use crate::client::{Client, Reply, Request};
use crate::error::AiError;
use crate::prompt::SYSTEM_PROMPT;
use crate::sse::{Block, Usage};
use crate::tools::{self, Approval, Change, Host, Outcome, Proposal, Sandbox};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// The user's answer to `Event::NeedsApproval`.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `Proposal::id`.
    pub id: String,
    pub approval: Approval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The model ended its turn.
    Finished,
    Cancelled,
    /// `max_steps` model calls were used.
    StepLimit,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// A text delta of the assistant message.
    Text(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// Waiting for a `Decision` with this proposal's id.
    NeedsApproval(Proposal),
    ToolResult {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// Tokens of one model call.
    Usage(Usage),
    /// Always the last event of a turn, unless `Error`.
    Done(Finish),
    /// Last event of a failed turn.
    Error(AiError),
}

/// What the app needs from an assistant provider: the API agent and the Claude Code driver
/// both run a turn on a worker thread, emit [`Event`]s and wait for [`Decision`]s.
pub trait Backend: Send {
    /// Set this flag (from the UI thread) to stop the running turn.
    fn cancel_handle(&self) -> Arc<AtomicBool>;
    /// Does the backend write approved edits to the disk itself (Claude Code's own Edit/Write
    /// tools)? Then the app must not apply an approved proposal locally: it makes sure the file
    /// is saved, answers `Approved`, and reloads the file when the tool's `ToolResult` arrives.
    /// The API backends return false: the app applies the edit, then answers `Approved`.
    fn writes_files_itself(&self) -> bool {
        false
    }
    /// A model id from [`crate::MODELS`].
    fn set_model(&mut self, model: &str);
    /// Forget the conversation.
    fn clear(&mut self);
    /// Run one user turn to completion; the last event sent is `Done` or `Error`.
    fn run_turn(&mut self, user_text: &str, events: &Sender<Event>, decisions: &Receiver<Decision>);
}

impl Backend for Agent {
    fn cancel_handle(&self) -> Arc<AtomicBool> {
        Agent::cancel_handle(self)
    }
    fn set_model(&mut self, model: &str) {
        Agent::set_model(self, model)
    }
    fn clear(&mut self) {
        Agent::clear(self)
    }
    fn run_turn(
        &mut self,
        user_text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) {
        Agent::run_turn(self, user_text, events, decisions)
    }
}

pub struct Agent {
    client: Client,
    runner: ToolRunner,
    tools: Vec<Value>,
    history: Vec<Value>,
    cancel: Arc<AtomicBool>,
    /// Model calls per user turn.
    pub max_steps: usize,
}

impl Agent {
    pub fn new(client: Client, host: Box<dyn Host>, sandbox: Sandbox) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        Agent {
            client,
            runner: ToolRunner::new(host, sandbox, cancel.clone()),
            tools: tools::tool_definitions(),
            history: Vec::new(),
            cancel,
            max_steps: 20,
        }
    }

    /// Set this flag (from the UI thread) to stop the running turn.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn set_model(&mut self, model: &str) {
        self.client.config.model = model.to_string();
    }

    /// Forget the conversation (and the "build approved" state).
    pub fn clear(&mut self) {
        self.history.clear();
        self.runner.build_approved = false;
    }

    pub fn history(&self) -> &[Value] {
        &self.history
    }

    /// Run one user turn to completion; blocks, so call it on a worker thread. The last event
    /// sent is `Done` or `Error`. The conversation keeps every finished step; a cancelled or
    /// failed step is dropped from it.
    pub fn run_turn(
        &mut self,
        user_text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) {
        self.cancel.store(false, Ordering::Relaxed);
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
            let reply: Reply = {
                let mut on_text = |t: &str| {
                    if events.send(Event::Text(t.to_string())).is_err() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                };
                self.client.stream(
                    &Request {
                        system: SYSTEM_PROMPT,
                        tools: &self.tools,
                        messages: &self.history,
                    },
                    &self.cancel,
                    &mut on_text,
                )?
            };
            let _ = events.send(Event::Usage(reply.usage));
            let uses: Vec<&Block> = reply
                .blocks
                .iter()
                .filter(|b| matches!(b, Block::ToolUse { .. }))
                .collect();
            if uses.is_empty() {
                if !reply.blocks.is_empty() {
                    self.history
                        .push(json!({"role": "assistant", "content": blocks_json(&reply.blocks)}));
                }
                return Ok(Finish::Finished);
            }
            let mut results = Vec::new();
            for b in uses {
                let Block::ToolUse { id, name, input } = b else {
                    continue;
                };
                let _ = events.send(Event::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                let (output, is_error) = self.runner.run(id, name, input, events, decisions)?;
                let _ = events.send(Event::ToolResult {
                    id: id.clone(),
                    name: name.clone(),
                    output: output.clone(),
                    is_error,
                });
                let mut r = json!({"type": "tool_result", "tool_use_id": id, "content": output});
                if is_error {
                    r["is_error"] = json!(true);
                }
                results.push(r);
            }
            self.history
                .push(json!({"role": "assistant", "content": blocks_json(&reply.blocks)}));
            self.history
                .push(json!({"role": "user", "content": results}));
        }
        Ok(Finish::StepLimit)
    }
}

fn rejection(reason: Option<String>) -> String {
    match reason {
        Some(r) if !r.trim().is_empty() => format!("The user rejected this change: {r}"),
        _ => "The user rejected this change.".into(),
    }
}

fn blocks_json(b: &[Block]) -> Vec<Value> {
    b.iter().map(Block::to_json).collect()
}

/// Runs the tools of a model's tool call for any chat-completion backend: read-only ones at once,
/// edits and builds only after the user's decision. Shared by the Anthropic agent and the OpenAI
/// one, so both obey the same approval rules.
pub(crate) struct ToolRunner {
    host: Box<dyn Host>,
    sandbox: Sandbox,
    pub(crate) build_approved: bool,
    cancel: Arc<AtomicBool>,
}

impl ToolRunner {
    pub(crate) fn new(host: Box<dyn Host>, sandbox: Sandbox, cancel: Arc<AtomicBool>) -> Self {
        ToolRunner {
            host,
            sandbox,
            build_approved: false,
            cancel,
        }
    }

    pub(crate) fn run(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) -> Result<(String, bool), AiError> {
        match tools::execute(name, input, &self.sandbox, self.host.as_mut()) {
            Outcome::Text(t) => Ok((t, false)),
            Outcome::Error(e) => Ok((e, true)),
            Outcome::Propose { path, abs, change } => {
                let p = Proposal {
                    id: id.to_string(),
                    path,
                    abs_path: abs,
                    change,
                };
                Ok(match self.ask(p, events, decisions)? {
                    Approval::Approved => (
                        "The user approved the change and it was applied.".into(),
                        false,
                    ),
                    Approval::Rejected(r) => (rejection(r), true),
                })
            }
            Outcome::Build => {
                if !self.build_approved {
                    let p = Proposal {
                        id: id.to_string(),
                        path: String::new(),
                        abs_path: self.sandbox.root().to_path_buf(),
                        change: Change::Build,
                    };
                    match self.ask(p, events, decisions)? {
                        Approval::Approved => self.build_approved = true,
                        Approval::Rejected(r) => return Ok((rejection(r), true)),
                    }
                }
                let out = self.host.build();
                Ok((
                    if out.trim().is_empty() {
                        "Build finished with no output.".into()
                    } else {
                        out
                    },
                    false,
                ))
            }
        }
    }

    fn ask(
        &self,
        p: Proposal,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) -> Result<Approval, AiError> {
        let id = p.id.clone();
        events
            .send(Event::NeedsApproval(p))
            .map_err(|_| AiError::Cancelled)?;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            match decisions.recv_timeout(Duration::from_millis(25)) {
                Ok(d) if d.id == id => return Ok(d.approval),
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(AiError::Cancelled),
            }
        }
    }
}
