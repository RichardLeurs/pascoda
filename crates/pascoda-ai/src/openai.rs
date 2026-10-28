//! The "OpenAI API key" provider: the Chat Completions API (`POST {base}/chat/completions`) with
//! streaming and tool calls. It works with api.openai.com and with any server that speaks the
//! same protocol (Ollama, LM Studio, ...) through a custom base URL.
//!
//! It shares the tools, the sandbox and the approval flow with the Anthropic agent
//! ([`crate::agent::ToolRunner`]): edits and builds are only proposed, the app applies them.

use crate::agent::{Backend, Decision, Event, Finish, ToolRunner};
use crate::client::ApiKey;
use crate::error::AiError;
use crate::prompt::SYSTEM_PROMPT;
use crate::sse::{SseParser, Usage};
use crate::tools::{self, Host, Sandbox};
use serde_json::{Value, json};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Model names offered as one-click choices in the panel (the model field stays free text).
pub const SUGGESTED_MODELS: [&str; 3] = ["gpt-6-luna", "gpt-6.1-terra", "gpt-6.1-sol"];

/// The host part of a base URL, lower case, without credentials and port.
fn host_of(url: &str) -> String {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    // IPv6 literals are in brackets; otherwise cut a `:port`.
    let host = if let Some(end) = authority.find(']') {
        &authority[..=end]
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

/// True when the endpoint is OpenAI itself, which cannot be used without a key. Other
/// endpoints (a local Ollama, LM Studio) may not need one.
pub fn needs_key(base_url: &str) -> bool {
    host_of(base_url) == "api.openai.com"
}

/// True for an endpoint on this Mac (localhost, 127.x, ::1): nothing leaves the machine.
pub fn is_local(base_url: &str) -> bool {
    let h = host_of(base_url);
    h == "localhost" || h == "[::1]" || h.starts_with("127.") || h.ends_with(".localhost")
}

/// What the user chose for the OpenAI provider (never the key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiSettings {
    pub base_url: String,
    /// Free text; empty until the user types one.
    pub model: String,
}

impl Default for OpenAiSettings {
    fn default() -> Self {
        OpenAiSettings {
            base_url: DEFAULT_OPENAI_BASE_URL.into(),
            model: String::new(),
        }
    }
}

impl OpenAiSettings {
    /// `~/Library/Application Support/Pascoda/openai-settings`.
    pub fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(Path::new(&home).join("Library/Application Support/Pascoda/openai-settings"))
    }

    /// The saved settings; defaults for a missing or unreadable file.
    pub fn load(file: &Path) -> Self {
        let mut s = OpenAiSettings::default();
        let Ok(text) = std::fs::read_to_string(file) else {
            return s;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return s;
        };
        if let Some(u) = v.get("base_url").and_then(Value::as_str)
            && !u.trim().is_empty()
        {
            s.base_url = u.trim().to_string();
        }
        if let Some(m) = v.get("model").and_then(Value::as_str) {
            s.model = m.trim().to_string();
        }
        s
    }

    /// Writes base URL and model as JSON. The key is never part of this file.
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&json!({
            "base_url": self.base_url,
            "model": self.model,
        }))
        .unwrap_or_default();
        std::fs::write(file, text + "\n")
    }

    pub fn needs_key(&self) -> bool {
        needs_key(&self.base_url)
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// `None` (or empty) sends no `Authorization` header: for local servers only.
    pub api_key: Option<ApiKey>,
    /// Up to and including `/v1`, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub model: String,
    /// Retries for 429/overloaded before any output arrived.
    pub max_retries: u32,
    pub retry_delay: Duration,
    pub timeout: Duration,
}

impl OpenAiConfig {
    pub fn new(api_key: Option<ApiKey>, base_url: &str, model: &str) -> Self {
        OpenAiConfig {
            api_key: api_key.filter(|k| !k.is_empty()),
            base_url: if base_url.trim().is_empty() {
                DEFAULT_OPENAI_BASE_URL.into()
            } else {
                base_url.trim().to_string()
            },
            model: model.trim().to_string(),
            max_retries: 2,
            retry_delay: Duration::from_secs(1),
            timeout: Duration::from_secs(600),
        }
    }
}

/// Anthropic-shaped tool schemas as Chat Completions function tools.
pub fn openai_tools(defs: &[Value]) -> Vec<Value> {
    defs.iter()
        .map(|d| {
            json!({"type": "function", "function": {
                "name": d["name"],
                "description": d["description"],
                "parameters": d["input_schema"],
            }})
        })
        .collect()
}

// ---- parsing the stream ---------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq)]
struct CallSlot {
    id: String,
    name: String,
    args: String,
}

/// A tool call of the finished reply.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Folds the `data:` payloads of one streamed reply.
#[derive(Debug, Default)]
pub struct ChunkAccumulator {
    pub text: String,
    slots: Vec<(Option<u64>, CallSlot)>,
    pub finish_reason: Option<String>,
    pub usage: Usage,
    pub done: bool,
}

impl ChunkAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot_for(&mut self, index: Option<u64>, has_new_info: bool) -> &mut CallSlot {
        let pos = match index {
            Some(i) => self.slots.iter().position(|(k, _)| *k == Some(i)),
            // Servers that do not number their calls send each call whole, with a name or an id.
            None if has_new_info => None,
            None => self.slots.len().checked_sub(1),
        };
        let pos = pos.unwrap_or_else(|| {
            self.slots.push((index, CallSlot::default()));
            self.slots.len() - 1
        });
        &mut self.slots[pos].1
    }

    /// Feeds the data of one SSE event. `on_text` receives every text delta.
    pub fn feed(&mut self, data: &str, on_text: &mut dyn FnMut(&str)) -> Result<(), AiError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| AiError::Protocol(format!("bad JSON in the stream: {e}")))?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(error_from(0, e, None));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |k: &str| u.get(k).and_then(Value::as_u64);
            let prompt = n("prompt_tokens").or(n("input_tokens")).unwrap_or(0);
            let cached = u
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(prompt);
            self.usage = Usage {
                input_tokens: prompt - cached,
                output_tokens: n("completion_tokens").or(n("output_tokens")).unwrap_or(0),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: cached,
            };
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(());
        };
        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(r.to_string());
        }
        let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) else {
            return Ok(());
        };
        if let Some(t) = delta.get("content").and_then(Value::as_str)
            && !t.is_empty()
        {
            self.text.push_str(t);
            on_text(t);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                let index = c.get("index").and_then(Value::as_u64);
                let id = c.get("id").and_then(Value::as_str).unwrap_or("");
                let name = c
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = match c.pointer("/function/arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(o @ Value::Object(_)) => o.to_string(), // some servers send an object
                    _ => String::new(),
                };
                let slot = self.slot_for(index, !id.is_empty() || !name.is_empty());
                if !id.is_empty() && slot.id.is_empty() {
                    slot.id = id.to_string();
                }
                if !name.is_empty() && slot.name.is_empty() {
                    slot.name = name.to_string();
                }
                slot.args.push_str(&args);
            }
        }
        Ok(())
    }

    /// The tool calls in order. Arguments that are not valid JSON (cut off by a length limit)
    /// become `{"_invalid_json": "<raw>"}`. Missing or repeated ids are replaced by new ones
    /// (`fresh_id`); calls without a name are dropped.
    pub fn tool_calls(&self, fresh_id: &mut dyn FnMut() -> String) -> Vec<ToolCall> {
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (_, s) in &self.slots {
            if s.name.is_empty() {
                continue;
            }
            let mut id = s.id.clone();
            if id.is_empty() || seen.contains(&id) {
                id = fresh_id();
            }
            seen.push(id.clone());
            let input = if s.args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&s.args).unwrap_or_else(|_| json!({"_invalid_json": s.args}))
            };
            out.push(ToolCall {
                id,
                name: s.name.clone(),
                input,
            });
        }
        out
    }
}

/// An `error` object (HTTP body or in-stream) as an [`AiError`].
fn error_from(status: u16, e: &Value, retry_after: Option<u64>) -> AiError {
    let (kind, msg) = match e {
        Value::String(s) => (String::new(), s.clone()),
        _ => {
            let code = e.get("code").map(|c| match c {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            });
            let kind = e
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|t| !t.is_empty())
                .or(code.clone().filter(|c| !c.is_empty()))
                .unwrap_or_default();
            (
                if code.as_deref() == Some("insufficient_quota") {
                    "insufficient_quota".to_string()
                } else {
                    kind
                },
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        }
    };
    map_error(status, &kind, &msg, retry_after)
}

fn map_error(status: u16, kind: &str, msg: &str, retry_after: Option<u64>) -> AiError {
    let msg: String = msg.chars().take(500).collect();
    match (status, kind) {
        (401 | 403, _) | (_, "authentication_error" | "invalid_api_key" | "permission_error") => {
            AiError::Auth(msg)
        }
        // No money left is not worth retrying.
        (_, "insufficient_quota") => AiError::Api {
            status: if status == 0 { 429 } else { status },
            kind: kind.into(),
            message: msg,
        },
        (429, _) | (_, "rate_limit_error" | "rate_limit_exceeded") => AiError::RateLimited {
            retry_after,
            message: msg,
        },
        (502..=504 | 529, _) | (_, "overloaded_error" | "server_is_overloaded") => {
            AiError::Overloaded(msg)
        }
        _ => AiError::Api {
            status,
            kind: kind.into(),
            message: msg,
        },
    }
}

/// An HTTP error body as an error.
fn http_error(status: u16, body: &str, retry_after: Option<u64>) -> AiError {
    match serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").cloned())
    {
        Some(e) => error_from(status, &e, retry_after),
        None => map_error(status, "", body.trim(), retry_after),
    }
}

// ---- the backend ----------------------------------------------------------------------------

enum Msg {
    Status {
        status: u16,
        retry_after: Option<u64>,
    },
    Chunk(Vec<u8>),
    End,
    Failed(String),
}

/// One finished streamed reply.
struct Reply {
    text: String,
    calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    usage: Usage,
}

pub struct OpenAi {
    pub config: OpenAiConfig,
    http: ureq::Agent,
    runner: ToolRunner,
    tools: Vec<Value>,
    history: Vec<Value>,
    cancel: Arc<AtomicBool>,
    next_id: u64,
    /// Model calls per user turn.
    pub max_steps: usize,
}

impl fmt::Debug for OpenAi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAi")
            .field("config", &self.config)
            .finish()
    }
}

impl OpenAi {
    pub fn new(config: OpenAiConfig, host: Box<dyn Host>, sandbox: Sandbox) -> Self {
        let http: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(config.timeout))
            .build()
            .into();
        let cancel = Arc::new(AtomicBool::new(false));
        OpenAi {
            config,
            http,
            runner: ToolRunner::new(host, sandbox, cancel.clone()),
            tools: openai_tools(&tools::tool_definitions()),
            history: Vec::new(),
            cancel,
            next_id: 0,
            max_steps: 20,
        }
    }

    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn history(&self) -> &[Value] {
        &self.history
    }

    /// The JSON body of `POST {base}/chat/completions`. No token limit is sent: OpenAI's newer
    /// models reject `max_tokens` and older servers reject `max_completion_tokens`.
    pub fn request_body(&self) -> Value {
        let mut messages = vec![json!({"role": "system", "content": SYSTEM_PROMPT})];
        messages.extend(self.history.iter().cloned());
        let mut body = json!({
            "model": self.config.model,
            "stream": true,
            "stream_options": {"include_usage": true},
            "messages": messages,
        });
        if !self.tools.is_empty() {
            body["tools"] = Value::Array(self.tools.clone());
        }
        body
    }

    pub fn run_turn(
        &mut self,
        user_text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) {
        self.cancel.store(false, Ordering::Relaxed);
        if self.config.model.trim().is_empty() {
            let _ = events.send(Event::Error(AiError::Config(
                "No model is set. Type the model name your endpoint serves into the Model field (OpenAI lists the names in its documentation; a local server shows them in its own model list).".into(),
            )));
            return;
        }
        if self.config.api_key.is_none() && needs_key(&self.config.base_url) {
            let _ = events.send(Event::Error(AiError::Auth(
                "no API key for api.openai.com".into(),
            )));
            return;
        }
        self.history
            .push(json!({"role": "user", "content": user_text}));
        let last = match self.turn(events, decisions) {
            Ok(f) => Event::Done(f),
            Err(AiError::Cancelled) => Event::Done(Finish::Cancelled),
            Err(e) => Event::Error(e),
        };
        let _ = events.send(last);
    }

    fn turn(
        &mut self,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) -> Result<Finish, AiError> {
        for _ in 0..self.max_steps {
            let cancel = self.cancel.clone();
            let reply = {
                let mut on_text = |t: &str| {
                    if events.send(Event::Text(t.to_string())).is_err() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                };
                self.stream(&mut on_text)?
            };
            let _ = events.send(Event::Usage(reply.usage));
            if reply.calls.is_empty() {
                if reply.finish_reason.as_deref() == Some("length") {
                    let _ = events.send(Event::Text(
                        "\n\n(The reply was cut off by the model's length limit.)".into(),
                    ));
                }
                if !reply.text.is_empty() {
                    self.history
                        .push(json!({"role": "assistant", "content": reply.text}));
                }
                return Ok(Finish::Finished);
            }
            let mut results = Vec::new();
            for c in &reply.calls {
                let _ = events.send(Event::ToolCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    input: c.input.clone(),
                });
                let (output, is_error) = self
                    .runner
                    .run(&c.id, &c.name, &c.input, events, decisions)?;
                let _ = events.send(Event::ToolResult {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    output: output.clone(),
                    is_error,
                });
                results.push(json!({
                    "role": "tool",
                    "tool_call_id": c.id,
                    "content": if is_error { format!("Error: {output}") } else { output },
                }));
            }
            let calls: Vec<Value> = reply
                .calls
                .iter()
                .map(|c| {
                    json!({"id": c.id, "type": "function", "function": {
                        "name": c.name,
                        // Always a string of valid JSON, even when the model's was cut off.
                        "arguments": if c.input.get("_invalid_json").is_some() {
                            "{}".to_string()
                        } else {
                            c.input.to_string()
                        },
                    }})
                })
                .collect();
            self.history.push(json!({
                "role": "assistant",
                "content": if reply.text.is_empty() { Value::Null } else { json!(reply.text) },
                "tool_calls": calls,
            }));
            self.history.extend(results);
        }
        Ok(Finish::StepLimit)
    }

    /// One streamed reply, retrying rate limits and overload before any output.
    fn stream(&mut self, on_text: &mut dyn FnMut(&str)) -> Result<Reply, AiError> {
        let body = self.request_body().to_string();
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut attempt = 0;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            match self.attempt(&url, &body, on_text) {
                Err((AiError::RateLimited { retry_after, .. }, false))
                    if attempt < self.config.max_retries =>
                {
                    self.backoff(attempt, retry_after)?;
                }
                Err((AiError::Overloaded(_), false)) if attempt < self.config.max_retries => {
                    self.backoff(attempt, None)?;
                }
                Err((e, _)) => return Err(e),
                Ok(reply) => return Ok(reply),
            }
            attempt += 1;
        }
    }

    fn backoff(&self, attempt: u32, retry_after: Option<u64>) -> Result<(), AiError> {
        let wait = match retry_after {
            Some(s) => Duration::from_secs(s.min(30)),
            None => self.config.retry_delay * 2u32.pow(attempt),
        };
        let end = std::time::Instant::now() + wait;
        while std::time::Instant::now() < end {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(20).min(wait));
        }
        Ok(())
    }

    /// One request. The error carries whether output had already reached the caller. The
    /// network runs on a helper thread so that a stop works while the server is still loading
    /// a model or thinking and sends nothing.
    fn attempt(
        &mut self,
        url: &str,
        body: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<Reply, (AiError, bool)> {
        let (tx, rx) = channel::<Msg>();
        {
            let (http, url, body) = (self.http.clone(), url.to_string(), body.to_string());
            let key = self
                .config
                .api_key
                .as_ref()
                .map(|k| format!("Bearer {}", k.expose()));
            std::thread::spawn(move || fetch(&http, &url, &body, key.as_deref(), &tx));
        }
        let mut output = false;
        let mut parser = SseParser::new();
        let mut acc = ChunkAccumulator::new();
        let mut error_body: Option<(u16, Option<u64>, Vec<u8>)> = None;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err((AiError::Cancelled, output)); // the helper ends with its connection
            }
            let msg = match rx.recv_timeout(Duration::from_millis(30)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err((AiError::Network("the connection was lost".into()), output));
                }
            };
            match msg {
                Msg::Status {
                    status,
                    retry_after,
                } => {
                    if status != 200 {
                        error_body = Some((status, retry_after, Vec::new()));
                    }
                }
                Msg::Failed(e) => {
                    return Err((
                        AiError::Network(format!("{e} (endpoint {})", self.config.base_url.trim())),
                        output,
                    ));
                }
                Msg::Chunk(bytes) => {
                    if let Some((_, _, buf)) = &mut error_body {
                        buf.extend_from_slice(&bytes);
                        continue;
                    }
                    for ev in parser.push(&bytes) {
                        let mut wrapped = |t: &str| {
                            output = true;
                            on_text(t);
                        };
                        acc.feed(&ev.data, &mut wrapped).map_err(|e| (e, output))?;
                    }
                }
                Msg::End => {
                    if let Some((status, retry_after, buf)) = error_body {
                        let text = String::from_utf8_lossy(&buf).to_string();
                        return Err((http_error(status, &text, retry_after), false));
                    }
                    break;
                }
            }
        }
        if !acc.done && acc.finish_reason.is_none() {
            return Err((
                AiError::Protocol("the stream ended before the reply was complete".into()),
                output,
            ));
        }
        let mut next = self.next_id;
        let calls = acc.tool_calls(&mut || {
            next += 1;
            format!("call_pascoda_{next}")
        });
        self.next_id = next;
        Ok(Reply {
            text: acc.text,
            calls,
            finish_reason: acc.finish_reason,
            usage: acc.usage,
        })
    }
}

/// The helper thread: send the request, then forward the body until it ends.
fn fetch(http: &ureq::Agent, url: &str, body: &str, auth: Option<&str>, tx: &Sender<Msg>) {
    use std::io::Read;
    let mut req = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    let mut resp = match req.send(body) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Msg::Failed(e.to_string()));
            return;
        }
    };
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());
    if tx
        .send(Msg::Status {
            status,
            retry_after,
        })
        .is_err()
    {
        return;
    }
    let mut reader = resp.body_mut().as_reader();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(Msg::End);
                return;
            }
            Ok(n) => {
                if tx.send(Msg::Chunk(buf[..n].to_vec())).is_err() {
                    return; // the turn was stopped: dropping the response closes the connection
                }
            }
            Err(e) => {
                let _ = tx.send(Msg::Failed(e.to_string()));
                return;
            }
        }
    }
}

impl Backend for OpenAi {
    fn cancel_handle(&self) -> Arc<AtomicBool> {
        OpenAi::cancel_handle(self)
    }
    fn set_model(&mut self, model: &str) {
        self.config.model = model.trim().to_string();
    }
    fn clear(&mut self) {
        self.history.clear();
        self.runner.build_approved = false;
    }
    fn run_turn(
        &mut self,
        user_text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) {
        OpenAi::run_turn(self, user_text, events, decisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_openai_itself_needs_a_key() {
        for (u, k) in [
            ("https://api.openai.com/v1", true),
            ("https://API.OpenAI.com/v1/", true),
            ("https://api.openai.com:443/v1", true),
            ("https://user:pw@api.openai.com/v1", true),
            ("http://localhost:11434/v1", false),
            ("http://127.0.0.1:1234/v1", false),
            ("http://[::1]:8080/v1", false),
            ("https://api.openai.com.evil.example/v1", false),
            ("https://evil.example/api.openai.com/v1", false),
            ("https://openrouter.ai/api/v1", false),
        ] {
            assert_eq!(needs_key(u), k, "{u}");
        }
    }

    #[test]
    fn local_endpoints_are_recognised() {
        assert!(is_local("http://localhost:11434/v1"));
        assert!(is_local("http://127.0.0.1:1234/v1"));
        assert!(is_local("http://[::1]:8080/v1"));
        assert!(!is_local("https://api.openai.com/v1"));
        assert!(!is_local("http://localhost.evil.example/v1"));
        assert!(!is_local("http://192.168.1.5:11434/v1"));
    }

    #[test]
    fn settings_round_trip_and_never_hold_a_key() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("sub/openai-settings");
        assert_eq!(OpenAiSettings::load(&f), OpenAiSettings::default());
        let s = OpenAiSettings {
            base_url: "http://localhost:11434/v1".into(),
            model: "llama3.1".into(),
        };
        s.save(&f).unwrap();
        assert_eq!(OpenAiSettings::load(&f), s);
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.to_lowercase().contains("key"), "{text}");
        std::fs::write(&f, "not json").unwrap();
        assert_eq!(OpenAiSettings::load(&f), OpenAiSettings::default());
        std::fs::write(&f, r#"{"base_url":"  ","model":" m "}"#).unwrap();
        let l = OpenAiSettings::load(&f);
        assert_eq!(
            (l.base_url.as_str(), l.model.as_str()),
            (DEFAULT_OPENAI_BASE_URL, "m")
        );
    }

    #[test]
    fn tools_become_function_tools() {
        let t = openai_tools(&tools::tool_definitions());
        assert_eq!(t.len(), 7);
        assert_eq!(t[0]["type"], "function");
        assert_eq!(t[0]["function"]["name"], "list_files");
        assert_eq!(t[0]["function"]["parameters"]["type"], "object");
        assert!(
            t[0].get("input_schema").is_none() && t[0]["function"].get("input_schema").is_none()
        );
        assert_eq!(t[5]["function"]["name"], "edit_file");
        assert_eq!(t[5]["function"]["parameters"]["required"][1], "old_text");
    }

    fn feed_all(chunks: &[&str]) -> (ChunkAccumulator, String) {
        let mut acc = ChunkAccumulator::new();
        let mut text = String::new();
        for c in chunks {
            acc.feed(c, &mut |t| text.push_str(t)).unwrap();
        }
        (acc, text)
    }

    #[test]
    fn fragmented_calls_are_joined_by_index() {
        let (acc, text) = feed_all(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hi "}}]}"#,
            r#"{"choices":[{"delta":{"content":null,"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","type":"function","function":{"name":"build","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.pas\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":60}}}"#,
            "[DONE]",
        ]);
        assert_eq!(text, "Hi ");
        let calls = acc.tool_calls(&mut || "x".into());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].input, json!({"path": "a.pas"}));
        assert_eq!(calls[1].name, "build");
        assert_eq!(acc.finish_reason.as_deref(), Some("tool_calls"));
        assert!(acc.done);
        assert_eq!(
            acc.usage,
            Usage {
                input_tokens: 40,
                output_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 60
            }
        );
    }

    #[test]
    fn servers_without_ids_or_indexes_get_generated_ids() {
        let (acc, _) = feed_all(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"get_diagnostics","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_file","arguments":{"path":"u.pas"}}}]},"finish_reason":"stop"}]}"#,
        ]);
        let mut n = 0;
        let calls = acc.tool_calls(&mut || {
            n += 1;
            format!("gen{n}")
        });
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[0].id.as_str(), calls[1].id.as_str()),
            ("gen1", "gen2")
        );
        assert_eq!(calls[1].input, json!({"path": "u.pas"}));
        // repeated ids are replaced too
        let (acc, _) = feed_all(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"same","function":{"name":"build","arguments":"{}"}},{"index":1,"id":"same","function":{"name":"build","arguments":"{}"}}]}}]}"#,
        ]);
        let mut n = 0;
        let calls = acc.tool_calls(&mut || {
            n += 1;
            format!("gen{n}")
        });
        assert_eq!(
            (calls[0].id.as_str(), calls[1].id.as_str()),
            ("same", "gen1")
        );
    }

    #[test]
    fn cut_off_arguments_are_marked_and_errors_map() {
        let (acc, _) = feed_all(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"edit_file","arguments":"{\"path\":\"a"}}]},"finish_reason":"length"}]}"#,
        ]);
        let calls = acc.tool_calls(&mut || "x".into());
        assert_eq!(calls[0].input["_invalid_json"], "{\"path\":\"a");
        let mut acc = ChunkAccumulator::new();
        let e = acc
            .feed(
                r#"{"error":{"message":"Rate limit","type":"rate_limit_exceeded"}}"#,
                &mut |_| {},
            )
            .unwrap_err();
        assert!(matches!(e, AiError::RateLimited { .. }), "{e:?}");
        assert!(matches!(
            http_error(
                401,
                r#"{"error":{"message":"bad","type":"invalid_request_error","code":"invalid_api_key"}}"#,
                None
            ),
            AiError::Auth(_)
        ));
        assert!(matches!(
            http_error(
                429,
                r#"{"error":{"message":"quota","type":"insufficient_quota","code":"insufficient_quota"}}"#,
                Some(3)
            ),
            AiError::Api { status: 429, .. }
        ));
        assert!(matches!(http_error(503, "", None), AiError::Overloaded(_)));
        assert!(matches!(
            http_error(404, r#"{"error":"model 'x' not found"}"#, None),
            AiError::Api { status: 404, message, .. } if message.contains("not found")
        ));
    }

    #[test]
    fn debug_never_shows_the_key() {
        let cfg = OpenAiConfig::new(Some(ApiKey::new("sk-SECRET-1")), "", "m");
        assert_eq!(cfg.base_url, DEFAULT_OPENAI_BASE_URL);
        let d = tempfile::tempdir().unwrap();
        struct H;
        impl Host for H {
            fn diagnostics(&mut self) -> String {
                String::new()
            }
            fn build(&mut self) -> String {
                String::new()
            }
        }
        let ai = OpenAi::new(cfg.clone(), Box::new(H), Sandbox::new(d.path()).unwrap());
        for s in [format!("{cfg:?}"), format!("{cfg:#?}"), format!("{ai:?}")] {
            assert!(!s.contains("SECRET"), "{s}");
        }
        // an empty key counts as none
        assert!(
            OpenAiConfig::new(Some(ApiKey::new("  ")), "http://localhost:1/v1", "m")
                .api_key
                .is_none()
        );
    }
}
