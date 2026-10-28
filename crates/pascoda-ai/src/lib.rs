//! AI assistant backend for Pascoda: an Anthropic Messages API client with SSE streaming and
//! tool use, plus an agent loop whose file changes are only ever *proposed*.
//!
//! The crate has no UI. The app runs [`Agent::run_turn`] on a worker thread, reads [`Event`]s
//! from a channel, and answers [`Event::NeedsApproval`] with a [`Decision`].

mod agent;
pub mod claude_code;
mod client;
mod error;
mod keychain;
pub mod openai;
mod prompt;
mod sse;
mod tools;

pub use agent::{Agent, Backend, Decision, Event, Finish};
pub use client::{ApiKey, Client, Config, Reply, Request};
pub use error::AiError;
pub use keychain::{Keystore, resolve_key, resolve_openai_key};
pub use openai::{OpenAi, OpenAiConfig, OpenAiSettings};
pub use prompt::{CLAUDE_CODE_PROMPT, SYSTEM_PROMPT};
pub use sse::{Accumulator, Block, SseEvent, SseParser, Usage};
pub use tools::{Approval, Change, Host, Proposal, Sandbox, tool_definitions};

pub const MODEL_SONNET: &str = "claude-sonnet-5-5";
pub const MODEL_OPUS: &str = "claude-opus-5-5";
pub const MODEL_HAIKU: &str = "claude-haiku-5-5";
pub const MODEL_FABLE: &str = "claude-fable-5-1";
pub const DEFAULT_MODEL: &str = MODEL_SONNET;
/// `(id, display name)` for a model picker.
pub const MODELS: [(&str, &str); 4] = [
    (MODEL_SONNET, "Claude Sonnet 5.5"),
    (MODEL_OPUS, "Claude Opus 5.5"),
    (MODEL_HAIKU, "Claude Haiku 5.5"),
    (MODEL_FABLE, "Claude Fable 5.1"),
];
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
