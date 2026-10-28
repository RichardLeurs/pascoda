use crate::error::{AiError, from_api};
use crate::sse::{Accumulator, Block, SseParser, Usage};
use serde_json::{Value, json};
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// An API key. `Debug` is redacted; the secret is only reachable inside this crate.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        ApiKey(key.into().trim().to_string())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: ApiKey,
    /// Without a trailing path, e.g. `https://api.anthropic.com` or `http://127.0.0.1:1234`.
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    /// Retries for 429/overloaded before any output arrived.
    pub max_retries: u32,
    /// First backoff; doubles each retry (a `retry-after` header up to 30 s wins).
    pub retry_delay: Duration,
    pub timeout: Duration,
}

impl Config {
    pub fn new(api_key: ApiKey) -> Self {
        Config {
            api_key,
            base_url: crate::DEFAULT_BASE_URL.to_string(),
            model: crate::DEFAULT_MODEL.to_string(),
            max_tokens: 8192,
            max_retries: 2,
            retry_delay: Duration::from_secs(1),
            timeout: Duration::from_secs(600),
        }
    }
}

/// One request: `messages` are API message objects (`{"role", "content"}`).
pub struct Request<'a> {
    pub system: &'a str,
    pub tools: &'a [Value],
    pub messages: &'a [Value],
}

/// A finished streamed reply.
#[derive(Debug, Clone)]
pub struct Reply {
    pub blocks: Vec<Block>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

pub struct Client {
    pub config: Config,
    agent: ureq::Agent,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("config", &self.config)
            .finish()
    }
}

impl Client {
    pub fn new(config: Config) -> Self {
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(config.timeout))
            .build();
        Client {
            agent: cfg.into(),
            config,
        }
    }

    /// The JSON body sent to `/v1/messages`. Cache breakpoints sit on the last tool and on
    /// the system prompt, so tools + system are cached as a prefix.
    pub fn request_body(&self, req: &Request) -> Value {
        let mut tools = req.tools.to_vec();
        if let Some(Value::Object(last)) = tools.last_mut() {
            last.insert("cache_control".into(), json!({"type": "ephemeral"}));
        }
        let mut body = json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "stream": true,
            "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
            "messages": req.messages,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        body
    }

    /// Stream one reply. `on_text` gets text deltas as they arrive. `cancel` is checked between
    /// network reads (the API sends pings every few seconds, so a stop takes effect quickly);
    /// on cancel the connection is dropped and `Err(Cancelled)` returned.
    pub fn stream(
        &self,
        req: &Request,
        cancel: &Arc<AtomicBool>,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<Reply, AiError> {
        let body = self.request_body(req).to_string();
        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
        let mut attempt = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            let result = self.attempt(&url, &body, cancel, on_text);
            match result {
                Err(AiError::RateLimited { retry_after, .. })
                    if attempt < self.config.max_retries =>
                {
                    self.backoff(attempt, retry_after, cancel)?;
                }
                Err(AiError::Overloaded(_)) if attempt < self.config.max_retries => {
                    self.backoff(attempt, None, cancel)?;
                }
                other => return other,
            }
            attempt += 1;
        }
    }

    fn backoff(
        &self,
        attempt: u32,
        retry_after: Option<u64>,
        cancel: &AtomicBool,
    ) -> Result<(), AiError> {
        let wait = match retry_after {
            Some(s) => Duration::from_secs(s.min(30)),
            None => self.config.retry_delay * 2u32.pow(attempt),
        };
        let end = std::time::Instant::now() + wait;
        while std::time::Instant::now() < end {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(20).min(wait));
        }
        Ok(())
    }

    fn attempt(
        &self,
        url: &str,
        body: &str,
        cancel: &AtomicBool,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<Reply, AiError> {
        let mut resp = self
            .agent
            .post(url)
            .header("x-api-key", self.config.api_key.expose())
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .send(body)
            .map_err(|e| AiError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let kind = v
                .pointer("/error/type")
                .and_then(Value::as_str)
                .unwrap_or("");
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or(text.trim());
            return Err(from_api(
                status,
                kind,
                &msg.chars().take(500).collect::<String>(),
                retry_after,
            ));
        }
        let mut reader = resp.body_mut().as_reader();
        let mut parser = SseParser::new();
        let mut acc = Accumulator::new();
        let mut buf = [0u8; 4096];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled); // dropping `resp` closes the connection
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Err(AiError::Network(e.to_string())),
            };
            for ev in parser.push(&buf[..n]) {
                acc.feed(&ev, on_text)?;
            }
            if acc.finished {
                break;
            }
        }
        if !acc.finished {
            return Err(AiError::Protocol(
                "the stream ended before message_stop".into(),
            ));
        }
        Ok(Reply {
            blocks: acc.blocks(),
            stop_reason: acc.stop_reason.clone(),
            usage: acc.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_key() {
        let secret = "sk-ant-SECRET-123";
        let key = ApiKey::new(secret);
        let cfg = Config::new(key.clone());
        let client = Client::new(cfg.clone());
        for s in [
            format!("{key:?}"),
            format!("{cfg:?}"),
            format!("{client:?}"),
            format!("{:#?}", cfg),
        ] {
            assert!(!s.contains("SECRET"), "{s}");
        }
        assert!(format!("{key:?}").contains("redacted"));
    }

    #[test]
    fn body_has_cache_control_on_system_and_last_tool() {
        let client = Client::new(Config::new(ApiKey::new("k")));
        let tools = vec![json!({"name": "a"}), json!({"name": "b"})];
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let b = client.request_body(&Request {
            system: "sys",
            tools: &tools,
            messages: &msgs,
        });
        assert_eq!(b["stream"], true);
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(b["tools"][0].get("cache_control").is_none());
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["model"], "claude-sonnet-5-5");
    }
}
