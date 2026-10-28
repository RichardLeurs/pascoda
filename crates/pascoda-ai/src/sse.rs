use crate::error::{AiError, from_api};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental server-sent-events parser; chunks may split anywhere (even inside a UTF-8 char).
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut cur = SseEvent::default();
        let mut have = false;
        let mut consumed = 0;
        let mut start = 0;
        while let Some(rel) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + rel;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            start = end + 1;
            if line.is_empty() {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
                consumed = start;
                continue;
            }
            if line[0] == b':' {
                continue;
            }
            let text = String::from_utf8_lossy(line);
            let (field, value) = match text.split_once(':') {
                Some((f, v)) => (f.to_string(), v.strip_prefix(' ').unwrap_or(v).to_string()),
                None => (text.to_string(), String::new()),
            };
            match field.as_str() {
                "event" => {
                    cur.event = value;
                    have = true;
                }
                "data" => {
                    if !cur.data.is_empty() {
                        cur.data.push('\n');
                    }
                    cur.data.push_str(&value);
                    have = true;
                }
                _ => {}
            }
        }
        // Keep the incomplete tail, plus the lines of an event whose blank line has not arrived.
        self.buf.drain(..consumed);
        out
    }
}

/// Token counts of a reply (summed over steps by the agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_creation_input_tokens += o.cache_creation_input_tokens;
        self.cache_read_input_tokens += o.cache_read_input_tokens;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

impl Block {
    pub fn to_json(&self) -> Value {
        match self {
            Block::Text(t) => json!({"type": "text", "text": t}),
            Block::ToolUse { id, name, input } => {
                json!({"type": "tool_use", "id": id, "name": name, "input": input})
            }
        }
    }
}

#[derive(Debug)]
enum Partial {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    Other,
}

/// Folds SSE events of one message into content blocks.
#[derive(Debug, Default)]
pub struct Accumulator {
    parts: BTreeMap<usize, Partial>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub finished: bool,
}

fn u(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}

impl Accumulator {
    pub fn new() -> Self {
        Self::default()
    }

    fn absorb_usage(&mut self, usage: &Value) {
        if let Some(x) = u(usage, "input_tokens") {
            self.usage.input_tokens = x;
        }
        if let Some(x) = u(usage, "output_tokens") {
            self.usage.output_tokens = x;
        }
        if let Some(x) = u(usage, "cache_creation_input_tokens") {
            self.usage.cache_creation_input_tokens = x;
        }
        if let Some(x) = u(usage, "cache_read_input_tokens") {
            self.usage.cache_read_input_tokens = x;
        }
    }

    /// Feed one event. `on_text` receives every text delta.
    pub fn feed(&mut self, ev: &SseEvent, on_text: &mut dyn FnMut(&str)) -> Result<(), AiError> {
        if ev.data.is_empty() {
            return Ok(());
        }
        let v: Value = serde_json::from_str(&ev.data)
            .map_err(|e| AiError::Protocol(format!("bad JSON in '{}' event: {e}", ev.event)))?;
        let kind = v.get("type").and_then(Value::as_str).unwrap_or(&ev.event);
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match kind {
            "message_start" => {
                if let Some(usage) = v.pointer("/message/usage") {
                    self.absorb_usage(usage);
                }
            }
            "content_block_start" => {
                let b = &v["content_block"];
                let part = match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = b
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if !t.is_empty() {
                            on_text(&t);
                        }
                        Partial::Text(t)
                    }
                    Some("tool_use") => Partial::Tool {
                        id: b
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        name: b
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        json: String::new(),
                    },
                    _ => Partial::Other,
                };
                self.parts.insert(index, part);
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match (
                    d.get("type").and_then(Value::as_str),
                    self.parts.get_mut(&index),
                ) {
                    (Some("text_delta"), Some(Partial::Text(t))) => {
                        let s = d.get("text").and_then(Value::as_str).unwrap_or("");
                        t.push_str(s);
                        on_text(s);
                    }
                    (Some("input_json_delta"), Some(Partial::Tool { json, .. })) => {
                        json.push_str(d.get("partial_json").and_then(Value::as_str).unwrap_or(""));
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(r.to_string());
                }
                if let Some(usage) = v.get("usage") {
                    self.absorb_usage(usage);
                }
            }
            "message_stop" => self.finished = true,
            "error" => {
                let kind = v
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(from_api(0, kind, msg, None));
            }
            _ => {}
        }
        Ok(())
    }

    /// Content blocks in order. Tool input that is not valid JSON (e.g. cut off by
    /// `max_tokens`) becomes `{"_invalid_json": "<raw>"}`.
    pub fn blocks(&self) -> Vec<Block> {
        self.parts
            .values()
            .filter_map(|p| match p {
                Partial::Text(t) if !t.is_empty() => Some(Block::Text(t.clone())),
                Partial::Tool { id, name, json } => {
                    let input = if json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(json)
                            .unwrap_or_else(|_| json!({"_invalid_json": json}))
                    };
                    Some(Block::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input,
                    })
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = include_str!("../tests/fixtures/text.sse");
    const TOOL: &str = include_str!("../tests/fixtures/tool_use.sse");
    const ERR: &str = include_str!("../tests/fixtures/error.sse");

    fn run(sse: &str, chunk: usize) -> (Result<(), AiError>, Accumulator, String) {
        let mut p = SseParser::new();
        let mut acc = Accumulator::new();
        let mut text = String::new();
        for c in sse.as_bytes().chunks(chunk) {
            for ev in p.push(c) {
                if let Err(e) = acc.feed(&ev, &mut |t| text.push_str(t)) {
                    return (Err(e), acc, text);
                }
            }
        }
        (Ok(()), acc, text)
    }

    #[test]
    fn text_message_at_any_chunk_size() {
        for chunk in [1, 2, 3, 7, 64, 100_000] {
            let (r, acc, text) = run(TEXT, chunk);
            r.unwrap();
            assert_eq!(text, "Hello, Pascal \u{e9}!", "chunk {chunk}");
            assert_eq!(
                acc.blocks(),
                vec![Block::Text("Hello, Pascal \u{e9}!".into())]
            );
            assert_eq!(acc.stop_reason.as_deref(), Some("end_turn"));
            assert!(acc.finished);
            assert_eq!(acc.usage.input_tokens, 25);
            assert_eq!(acc.usage.output_tokens, 6);
            assert_eq!(acc.usage.cache_read_input_tokens, 3);
        }
    }

    #[test]
    fn tool_use_with_split_json() {
        for chunk in [1, 5, 33, 100_000] {
            let (r, acc, text) = run(TOOL, chunk);
            r.unwrap();
            assert_eq!(text, "Let me look.");
            let blocks = acc.blocks();
            assert_eq!(blocks.len(), 2);
            assert_eq!(
                blocks[1],
                Block::ToolUse {
                    id: "toolu_01".into(),
                    name: "read_file".into(),
                    input: json!({"path": "unit1.pas", "start_line": 10})
                }
            );
            assert_eq!(acc.stop_reason.as_deref(), Some("tool_use"));
        }
    }

    #[test]
    fn error_event_maps_to_overloaded() {
        let (r, _, _) = run(ERR, 9);
        assert!(matches!(r, Err(AiError::Overloaded(_))), "{r:?}");
    }

    #[test]
    fn crlf_comments_and_multiline_data() {
        let mut p = SseParser::new();
        let evs = p.push(b": hi\r\nevent: x\r\ndata: a\r\ndata: b\r\n\r\nevent: y\ndata: c\n\n");
        assert_eq!(evs.len(), 2);
        assert_eq!(
            evs[0],
            SseEvent {
                event: "x".into(),
                data: "a\nb".into()
            }
        );
        assert_eq!(evs[1].data, "c");
    }

    #[test]
    fn truncated_tool_json_is_flagged() {
        let mut acc = Accumulator::new();
        let mut p = SseParser::new();
        let s = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"n\",\"input\":{}}}\n\n\
                 event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n";
        for ev in p.push(s.as_bytes()) {
            acc.feed(&ev, &mut |_| {}).unwrap();
        }
        match &acc.blocks()[0] {
            Block::ToolUse { input, .. } => assert!(input.get("_invalid_json").is_some()),
            b => panic!("{b:?}"),
        }
    }
}
