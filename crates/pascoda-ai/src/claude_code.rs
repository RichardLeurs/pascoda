//! The "Claude Code (subscription)" provider: drives the user's own installed `claude` CLI as a
//! subprocess, so a Claude subscription can be used without an API key.
//!
//! Pascoda only spawns the binary and talks to it over stdin/stdout (the CLI's stream-json
//! protocol with `--permission-prompt-tool stdio`). It never reads, copies or logs Claude
//! credentials, the keychain entry of Claude Code, or its settings: the CLI signs in and
//! authenticates itself. The one thing done with the environment is removing
//! `ANTHROPIC_API_KEY` from the child's, so a key meant for the other provider cannot silently
//! turn a subscription session into a billed API session.
//!
//! Protocol (observed with Claude Code 2.1.312, see `tests/fixtures/claude`):
//! - We write `{"type":"user","message":{"role":"user","content":TEXT}}` lines. One process
//!   serves a whole conversation; every turn starts with a `system/init` line and ends with a
//!   `result` line.
//! - With `--include-partial-messages` the answer arrives as `stream_event` lines (Anthropic
//!   stream events: `content_block_delta` / `text_delta`, `message_delta` with usage); complete
//!   blocks follow as `assistant` lines (tool calls with their full input) and tool results as
//!   `user` lines.
//! - A tool that needs permission produces `{"type":"control_request","request_id":ID,
//!   "request":{"subtype":"can_use_tool","tool_name","input","tool_use_id",..}}`; we answer
//!   `{"type":"control_response","response":{"subtype":"success","request_id":ID,"response":
//!   {"behavior":"allow","updatedInput":INPUT}}}` or `{"behavior":"deny","message":TEXT}`.
//! - `{"type":"control_request","request_id":ID,"request":{"subtype":"interrupt"}}` stops the
//!   running turn: the CLI acknowledges, ends the turn with an `is_error` `result` whose
//!   `terminal_reason` is `aborted_streaming`, and stays usable.
//! - Not logged in: an `assistant` line with `"error":"authentication_failed"` and an
//!   `is_error` `result` (`terminal_reason` `api_error`).
//!
//! Edits are never made by the CLI. For Edit/MultiEdit/Write the permission request becomes a
//! [`Proposal`]; the app applies an approved change itself, and we then answer the CLI with a
//! *deny* that says so, so the CLI does not write the file as well.

use crate::agent::{Backend, Decision, Event, Finish};
use crate::error::AiError;
use crate::prompt::CLAUDE_CODE_PROMPT;
use crate::sse::Usage;
use crate::tools::{Approval, Change, Host, Proposal, Sandbox};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Environment variable naming the `claude` binary to use (tests, unusual installs).
pub const ENV_BIN: &str = "PASCODA_CLAUDE_BIN";

/// Tools the CLI may use. No Bash, no web, no MCP.
pub const TOOLS: &str = "Read,Glob,Grep,Edit,MultiEdit,Write";

/// Removed from the child's environment (see the module comment).
const SCRUBBED_ENV: [&str; 1] = ["ANTHROPIC_API_KEY"];

// ---- finding the binary and its state ------------------------------------------------------

pub(crate) fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Finds `claude`: `override_path` (when given it is the only candidate), then `PATH`, then
/// the places the installers use (`~/.local/bin`, `~/.claude/local`, `/usr/local/bin`,
/// `/opt/homebrew/bin`).
pub fn locate_in(
    override_path: Option<OsString>,
    path: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(o) = override_path.filter(|o| !o.is_empty()) {
        let p = PathBuf::from(o);
        return is_executable(&p).then_some(p);
    }
    let mut dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(h) = home {
        let h = PathBuf::from(h);
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".claude/local"));
    }
    dirs.push("/usr/local/bin".into());
    dirs.push("/opt/homebrew/bin".into());
    dirs.into_iter()
        .map(|d| d.join("claude"))
        .find(|p| is_executable(p))
}

/// [`locate_in`] with the process environment.
pub fn locate() -> Option<PathBuf> {
    locate_in(
        std::env::var_os(ENV_BIN),
        std::env::var_os("PATH"),
        std::env::var_os("HOME"),
    )
}

/// What `claude --version` and `claude auth status` say. Nothing else from them is kept (the
/// status output also names the account; it is parsed for `loggedIn` and dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliStatus {
    pub binary: PathBuf,
    pub version: Option<String>,
    /// `None` when the CLI could not tell (old version, timeout).
    pub logged_in: Option<bool>,
}

fn scrub(cmd: &mut Command) {
    for k in SCRUBBED_ENV {
        cmd.env_remove(k);
    }
}

/// Runs `bin args` with a timeout and returns its stdout.
fn capture(bin: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    scrub(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    reader.join().ok()
}

/// Is the CLI there, which version, and is it logged in? Blocks for a second or two: call it
/// on a worker thread. No credential is read by us; the CLI answers.
pub fn check(bin: &Path) -> CliStatus {
    let version = capture(bin, &["--version"], Duration::from_secs(10))
        .and_then(|s| s.split_whitespace().next().map(str::to_string));
    let logged_in = capture(bin, &["auth", "status", "--json"], Duration::from_secs(15))
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|j| j["loggedIn"].as_bool());
    CliStatus {
        binary: bin.to_path_buf(),
        version,
        logged_in,
    }
}

/// The CLI's model alias for a model id of the panel's picker.
pub fn cli_model(id: &str) -> String {
    match id {
        crate::MODEL_SONNET => "sonnet",
        crate::MODEL_OPUS => "opus",
        crate::MODEL_HAIKU => "haiku",
        crate::MODEL_FABLE => "fable",
        other => other,
    }
    .to_string()
}

// ---- configuration and the process -------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Config {
    pub binary: PathBuf,
    /// Model id of the picker or a CLI alias.
    pub model: String,
    /// How long a cancelled turn may take to end before the process is killed.
    pub interrupt_grace: Duration,
}

impl Config {
    pub fn new(binary: PathBuf) -> Self {
        Config {
            binary,
            model: crate::DEFAULT_MODEL.to_string(),
            interrupt_grace: Duration::from_secs(5),
        }
    }

    /// The command line (without the program).
    pub fn args(&self, resume: Option<&str>) -> Vec<String> {
        let mut a: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
            "--permission-mode",
            "manual",
            "--permission-prompt-tool",
            "stdio",
            "--tools",
            TOOLS,
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--name",
            "Pascoda",
            "--model",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.push(cli_model(&self.model));
        a.push("--append-system-prompt".into());
        a.push(CLAUDE_CODE_PROMPT.into());
        if let Some(id) = resume {
            a.push("--resume".into());
            a.push(id.into());
        }
        a
    }

    fn command(&self, root: &Path, resume: Option<&str>) -> Command {
        let mut c = Command::new(&self.binary);
        c.args(self.args(resume))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        scrub(&mut c);
        c
    }
}

enum Msg {
    Line(String),
    Eof,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Msg>,
    stderr: Arc<Mutex<String>>,
    model: String,
}

impl Proc {
    fn spawn(cfg: &Config, root: &Path, resume: Option<&str>) -> Result<Proc, AiError> {
        let mut child = cfg.command(root, resume).spawn().map_err(|e| {
            AiError::Cli(match e.kind() {
                std::io::ErrorKind::NotFound => format!(
                    "Claude Code was not found at {}. Install it from claude.com/claude-code.",
                    cfg.binary.display()
                ),
                _ => format!("Claude Code could not be started: {e}"),
            })
        })?;
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(i), Some(o), Some(e)) => (i, o, e),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AiError::Cli("Claude Code started without pipes.".into()));
                }
            };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(Msg::Line(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(Msg::Eof);
        });
        let tail = Arc::new(Mutex::new(String::new()));
        let t2 = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = t2.lock().unwrap_or_else(|e| e.into_inner());
                t.push_str(&line);
                t.push('\n');
                let excess = t.len().saturating_sub(1500);
                if excess > 0 {
                    let cut = (excess..t.len())
                        .find(|i| t.is_char_boundary(*i))
                        .unwrap_or(t.len());
                    t.drain(..cut);
                }
            }
        });
        Ok(Proc {
            child,
            stdin,
            rx,
            stderr: tail,
            model: cfg.model.clone(),
        })
    }

    fn send(&mut self, v: &Value) -> std::io::Result<()> {
        writeln!(self.stdin, "{v}")?;
        self.stdin.flush()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- the driver ----------------------------------------------------------------------------------

pub struct ClaudeCode {
    cfg: Config,
    host: Box<dyn Host>,
    sandbox: Sandbox,
    proc: Option<Proc>,
    session: Option<String>,
    /// A turn finished in `session`, so `--resume` has something to resume.
    resumable: bool,
    cancel: Arc<AtomicBool>,
}

impl ClaudeCode {
    pub fn new(cfg: Config, host: Box<dyn Host>, sandbox: Sandbox) -> Self {
        ClaudeCode {
            cfg,
            host,
            sandbox,
            proc: None,
            session: None,
            resumable: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The CLI's session id, once a turn has started.
    pub fn session_id(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Is a CLI process alive?
    pub fn running(&self) -> bool {
        self.proc.is_some()
    }

    fn stop_process(&mut self) {
        self.proc = None; // Drop kills it
    }

    fn turn(
        &mut self,
        text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) -> Result<Finish, AiError> {
        let mut proc = match self.proc.take() {
            Some(p) => p,
            None => {
                let resume = self.resumable.then(|| self.session.clone()).flatten();
                Proc::spawn(&self.cfg, self.sandbox.root(), resume.as_deref())?
            }
        };
        let mut t = Turn {
            proc: &mut proc,
            host: self.host.as_mut(),
            sandbox: &self.sandbox,
            events,
            decisions,
            cancel: &self.cancel,
            grace: self.cfg.interrupt_grace,
            session: self.session.clone(),
            healthy: true,
            gone: false,
            cancelled: false,
            streamed: HashSet::new(),
            cur_msg: String::new(),
            in_text: false,
            block_had_text: false,
            need_break: false,
            start_usage: Usage::default(),
            announced: HashMap::new(),
            suppressed: HashSet::new(),
            pending: HashMap::new(),
            api_error: None,
            rate_limited: false,
        };
        let result = t.run(text);
        let (healthy, session) = (t.healthy, t.session.take());
        if session.is_some() {
            self.session = session;
        }
        if matches!(result, Ok(Finish::Finished | Finish::StepLimit)) {
            self.resumable = self.session.is_some();
        }
        if healthy {
            self.proc = Some(proc);
        }
        result
    }
}

impl Backend for ClaudeCode {
    fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn writes_files_itself(&self) -> bool {
        true
    }

    fn set_model(&mut self, model: &str) {
        if cli_model(model) != cli_model(&self.cfg.model) {
            self.cfg.model = model.to_string();
        }
        // The process was started with another model: restart it (the next turn resumes).
        if self
            .proc
            .as_ref()
            .is_some_and(|p| cli_model(&p.model) != cli_model(&self.cfg.model))
        {
            self.stop_process();
        }
    }

    fn clear(&mut self) {
        self.stop_process();
        self.session = None;
        self.resumable = false;
    }

    fn run_turn(
        &mut self,
        user_text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) {
        self.cancel.store(false, Ordering::Relaxed);
        let last = match self.turn(user_text, events, decisions) {
            Ok(f) => Event::Done(f),
            Err(AiError::Cancelled) => Event::Done(Finish::Cancelled),
            Err(e) => Event::Error(e),
        };
        let _ = events.send(last);
    }
}

/// State of one running user turn.
struct Turn<'a> {
    proc: &'a mut Proc,
    host: &'a mut dyn Host,
    sandbox: &'a Sandbox,
    events: &'a Sender<Event>,
    decisions: &'a Receiver<Decision>,
    cancel: &'a AtomicBool,
    grace: Duration,
    session: Option<String>,
    /// The process can serve another turn.
    healthy: bool,
    /// The receiver of our events is gone.
    gone: bool,
    cancelled: bool,
    /// Messages whose text came as stream deltas (their `assistant` lines are not repeated).
    streamed: HashSet<String>,
    cur_msg: String,
    in_text: bool,
    block_had_text: bool,
    /// A text block ended; the next one starts a new paragraph.
    need_break: bool,
    start_usage: Usage,
    /// Tool call id -> name sent as `ToolCall`.
    announced: HashMap<String, String>,
    /// Tool calls whose result we report ourselves (edits asked of the user).
    suppressed: HashSet<String>,
    /// Tool call id -> (control request id, the tool's input), for edits waiting for the user.
    pending: HashMap<String, (String, Value)>,
    api_error: Option<(String, String)>,
    rate_limited: bool,
}

fn u64_of(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}

fn usage_of(v: &Value, fallback: &Usage) -> Usage {
    Usage {
        input_tokens: u64_of(v, "input_tokens").unwrap_or(fallback.input_tokens),
        output_tokens: u64_of(v, "output_tokens").unwrap_or(fallback.output_tokens),
        cache_creation_input_tokens: u64_of(v, "cache_creation_input_tokens")
            .unwrap_or(fallback.cache_creation_input_tokens),
        cache_read_input_tokens: u64_of(v, "cache_read_input_tokens")
            .unwrap_or(fallback.cache_read_input_tokens),
    }
}

fn text_of_blocks(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn rejection(reason: Option<String>) -> String {
    match reason {
        Some(r) if !r.trim().is_empty() => format!("The user rejected this change: {r}"),
        _ => "The user rejected this change.".into(),
    }
}

impl Turn<'_> {
    fn emit(&mut self, ev: Event) {
        if self.events.send(ev).is_err() {
            self.gone = true;
        }
    }

    fn write(&mut self, v: &Value) -> Result<(), AiError> {
        self.proc.send(v).map_err(|_| self.died())
    }

    fn died(&mut self) -> AiError {
        self.healthy = false;
        let status = (0..20)
            .find_map(|_| match self.proc.child.try_wait() {
                Ok(Some(s)) => Some(Some(s)),
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(25));
                    None
                }
                Err(_) => Some(None),
            })
            .flatten();
        // The stderr reader may still be a moment behind the exit.
        let read_tail = |p: &Proc| {
            p.stderr
                .lock()
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let mut tail = read_tail(self.proc);
        for _ in 0..10 {
            if !tail.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            tail = read_tail(self.proc);
        }
        let mut m = String::from("The Claude Code process ended unexpectedly");
        if let Some(s) = status {
            m.push_str(&format!(" ({s})"));
        }
        m.push('.');
        if !tail.is_empty() {
            m.push(' ');
            m.push_str(
                &tail
                    .chars()
                    .rev()
                    .take(400)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<String>(),
            );
        }
        AiError::Cli(m)
    }

    fn respond(&mut self, request_id: &str, body: Value) -> Result<(), AiError> {
        self.write(&json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": body}
        }))
    }

    fn deny(&mut self, request_id: &str, message: &str) -> Result<(), AiError> {
        self.respond(request_id, json!({"behavior": "deny", "message": message}))
    }

    fn run(&mut self, text: &str) -> Result<Finish, AiError> {
        self.write(&json!({"type": "user", "message": {"role": "user", "content": text}}))?;
        loop {
            if self.cancel.load(Ordering::Relaxed) || self.gone {
                return self.abort();
            }
            while let Ok(d) = self.decisions.try_recv() {
                self.answer(d)?;
            }
            match self.proc.rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Msg::Line(l)) => {
                    if let Some(f) = self.line(&l)? {
                        return Ok(f);
                    }
                }
                Ok(Msg::Eof) | Err(RecvTimeoutError::Disconnected) => return Err(self.died()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Stops the turn: pending questions are denied, then the CLI is asked to interrupt. A CLI
    /// that does not end the turn within the grace period is killed (the next turn resumes).
    fn abort(&mut self) -> Result<Finish, AiError> {
        self.cancelled = true;
        let pending: Vec<String> = self.pending.drain().map(|(_, (rid, _))| rid).collect();
        for rid in pending {
            if self.deny(&rid, "The user stopped the turn.").is_err() {
                return Ok(Finish::Cancelled);
            }
        }
        let int = json!({"type": "control_request", "request_id": "pascoda-interrupt",
            "request": {"subtype": "interrupt"}});
        if self.proc.send(&int).is_err() {
            self.healthy = false;
            return Ok(Finish::Cancelled);
        }
        let end = Instant::now() + self.grace;
        loop {
            match self.proc.rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Msg::Line(l)) => {
                    let Ok(j) = serde_json::from_str::<Value>(&l) else {
                        continue;
                    };
                    match j["type"].as_str() {
                        Some("result") => return Ok(Finish::Cancelled),
                        Some("control_request") if j["request"]["subtype"] == "can_use_tool" => {
                            let rid = j["request_id"].as_str().unwrap_or("").to_string();
                            if self.deny(&rid, "The user stopped the turn.").is_err() {
                                return Ok(Finish::Cancelled);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Msg::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.healthy = false;
                    return Ok(Finish::Cancelled);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() > end {
                        self.healthy = false;
                        return Ok(Finish::Cancelled);
                    }
                }
            }
        }
    }

    /// The user's answer to an edit proposal. Approved: the CLI's own tool writes the file (the
    /// permission request is allowed with its original input) and its tool result, which says
    /// whether the write worked, is passed on. Rejected: denied with the reason.
    fn answer(&mut self, d: Decision) -> Result<(), AiError> {
        let Some((rid, input)) = self.pending.remove(&d.id) else {
            return Ok(());
        };
        match d.approval {
            Approval::Approved => {
                // The CLI's result for this call is now the real one: let it through.
                self.suppressed.remove(&d.id);
                self.respond(&rid, json!({"behavior": "allow", "updatedInput": input}))
            }
            Approval::Rejected(r) => {
                let m = rejection(r);
                self.deny(&rid, &m)?;
                let name = self.announced.get(&d.id).cloned().unwrap_or_default();
                self.emit(Event::ToolResult {
                    id: d.id,
                    name,
                    output: m,
                    is_error: true,
                });
                Ok(())
            }
        }
    }

    fn announce(&mut self, id: &str, name: &str, input: &Value) {
        if self.announced.contains_key(id) {
            return;
        }
        let (n, i) = normalize(name, input, self.sandbox);
        self.announced.insert(id.to_string(), n.clone());
        self.need_break = false;
        self.emit(Event::ToolCall {
            id: id.to_string(),
            name: n,
            input: i,
        });
    }

    fn text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        if self.need_break {
            self.need_break = false;
            self.emit(Event::Text("\n\n".into()));
        }
        self.emit(Event::Text(t.to_string()));
    }

    /// Handles one stdout line. `Some` ends the turn.
    fn line(&mut self, l: &str) -> Result<Option<Finish>, AiError> {
        let Ok(j) = serde_json::from_str::<Value>(l) else {
            return Ok(None); // not protocol (a stray log line)
        };
        let nested = !j["parent_tool_use_id"].is_null();
        match j["type"].as_str().unwrap_or("") {
            "system" => {
                if j["subtype"] == "init"
                    && let Some(s) = j["session_id"].as_str()
                {
                    self.session = Some(s.to_string());
                }
            }
            "stream_event" if !nested => self.stream_event(&j["event"]),
            "assistant" if !nested => self.assistant(&j),
            "user" if !nested => self.user(&j),
            "control_request" => self.control_request(&j)?,
            "control_cancel_request" => {
                if let Some(rid) = j["request_id"].as_str() {
                    self.pending.retain(|_, (r, _)| r != rid);
                }
            }
            "rate_limit_event" => {
                if j["rate_limit_info"]["status"] == "rejected" {
                    self.rate_limited = true;
                }
            }
            "result" => return self.result(&j).map(Some),
            _ => {}
        }
        Ok(None)
    }

    fn stream_event(&mut self, e: &Value) {
        match e["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.cur_msg = e["message"]["id"].as_str().unwrap_or("").to_string();
                self.start_usage = usage_of(&e["message"]["usage"], &Usage::default());
            }
            "content_block_start" => {
                self.in_text = e["content_block"]["type"] == "text";
                self.block_had_text = false;
            }
            "content_block_delta" if e["delta"]["type"] == "text_delta" => {
                if let Some(t) = e["delta"]["text"].as_str() {
                    self.streamed.insert(self.cur_msg.clone());
                    self.block_had_text |= !t.is_empty();
                    self.text(t);
                }
            }
            "content_block_stop" => {
                if self.in_text && self.block_had_text {
                    self.need_break = true;
                }
                self.in_text = false;
            }
            "message_delta" => {
                let u = usage_of(&e["usage"], &self.start_usage);
                if u != Usage::default() {
                    self.emit(Event::Usage(u));
                }
            }
            _ => {}
        }
    }

    fn assistant(&mut self, j: &Value) {
        let m = &j["message"];
        if let Some(code) = j["error"].as_str() {
            // The CLI's own error message (not logged in, limits ...): kept for the result.
            self.api_error = Some((code.to_string(), text_of_blocks(&m["content"])));
            return;
        }
        let id = m["id"].as_str().unwrap_or("").to_string();
        let Some(blocks) = m["content"].as_array() else {
            return;
        };
        for b in blocks {
            match b["type"].as_str() {
                Some("tool_use") => {
                    let (tid, name) = (
                        b["id"].as_str().unwrap_or(""),
                        b["name"].as_str().unwrap_or(""),
                    );
                    self.announce(tid, name, &b["input"]);
                }
                Some("text") if !self.streamed.contains(&id) => {
                    if let Some(t) = b["text"].as_str() {
                        self.text(t);
                        self.need_break = true;
                    }
                }
                _ => {}
            }
        }
    }

    fn user(&mut self, j: &Value) {
        let Some(blocks) = j["message"]["content"].as_array() else {
            return;
        };
        for b in blocks {
            if b["type"] != "tool_result" {
                continue;
            }
            let id = b["tool_use_id"].as_str().unwrap_or("").to_string();
            if self.suppressed.contains(&id) {
                continue;
            }
            let Some(name) = self.announced.get(&id).cloned() else {
                continue;
            };
            let mut output = text_of_blocks(&b["content"]);
            if output.chars().count() > 4000 {
                output = output.chars().take(4000).collect::<String>() + "...";
            }
            self.emit(Event::ToolResult {
                id,
                name,
                output,
                is_error: b["is_error"].as_bool().unwrap_or(false),
            });
        }
    }

    fn control_request(&mut self, j: &Value) -> Result<(), AiError> {
        let rid = j["request_id"].as_str().unwrap_or("").to_string();
        let req = &j["request"];
        if req["subtype"] != "can_use_tool" {
            return self.write(&json!({"type": "control_response", "response":
                {"subtype": "error", "request_id": rid, "error": "not supported by Pascoda"}}));
        }
        let name = req["tool_name"].as_str().unwrap_or("").to_string();
        let tid = req["tool_use_id"].as_str().unwrap_or("").to_string();
        let input = req["input"].clone();
        self.announce(&tid, &name, &input);
        match name.as_str() {
            "Read" | "Glob" | "Grep" => match read_scope(self.sandbox, &name, &input) {
                Ok(()) => self.respond(&rid, json!({"behavior": "allow", "updatedInput": input})),
                Err(m) => self.deny(&rid, &m),
            },
            "Edit" | "MultiEdit" | "Write" => {
                match edit_proposal(self.sandbox, self.host, &name, &input, &tid) {
                    Ok(p) => {
                        self.suppressed.insert(tid.clone());
                        self.pending.insert(tid, (rid, input));
                        self.emit(Event::NeedsApproval(p));
                        Ok(())
                    }
                    Err(m) => self.deny(&rid, &m),
                }
            }
            other => self.deny(
                &rid,
                &format!("Pascoda does not allow the {other} tool; only Read, Glob, Grep, Edit, MultiEdit and Write."),
            ),
        }
    }

    fn result(&mut self, j: &Value) -> Result<Finish, AiError> {
        let terminal = j["terminal_reason"].as_str().unwrap_or("");
        if self.cancelled || terminal.starts_with("aborted") {
            return Ok(Finish::Cancelled);
        }
        if j["subtype"] == "error_max_turns" {
            return Ok(Finish::StepLimit);
        }
        if !j["is_error"].as_bool().unwrap_or(false) {
            return Ok(Finish::Finished);
        }
        // A failed turn: start from a fresh process next time.
        self.healthy = false;
        let text = j["result"]
            .as_str()
            .map(str::to_string)
            .or_else(|| self.api_error.as_ref().map(|(_, t)| t.clone()))
            .unwrap_or_default();
        let code = self.api_error.as_ref().map_or("", |(c, _)| c.as_str());
        Err(classify(code, &text, self.rate_limited))
    }
}

/// Maps the CLI's error to ours.
pub fn classify(code: &str, text: &str, rate_limited: bool) -> AiError {
    let low = text.to_lowercase();
    if code == "authentication_failed"
        || low.contains("not logged in")
        || low.contains("/login")
        || low.contains("invalid api key")
        || low.contains("oauth token has expired")
    {
        return AiError::NotLoggedIn;
    }
    if rate_limited
        || code == "rate_limit"
        || low.contains("rate limit")
        || low.contains("usage limit")
        || low.contains("limit reached")
        || low.contains("limit will reset")
    {
        return AiError::RateLimited {
            retry_after: None,
            message: text.to_string(),
        };
    }
    if code == "overloaded" || low.contains("overloaded") {
        return AiError::Overloaded(text.to_string());
    }
    if text.trim().is_empty() {
        return AiError::Cli(format!(
            "Claude Code reported an error{}.",
            if code.is_empty() {
                String::new()
            } else {
                format!(" ({code})")
            }
        ));
    }
    AiError::Cli(format!("Claude Code reported an error: {text}"))
}

// ---- tools -------------------------------------------------------------------------------------

/// Tool call as the panel shows it, named like the API provider's tools where one exists.
fn normalize(name: &str, input: &Value, sandbox: &Sandbox) -> (String, Value) {
    let rel = |k: &str| -> Option<String> {
        let p = input[k].as_str()?;
        Some(
            sandbox
                .resolve(p)
                .map(|a| sandbox.relative(&a))
                .unwrap_or_else(|_| p.to_string()),
        )
    };
    match name {
        "Read" => {
            let mut v = json!({"path": rel("file_path").unwrap_or_default()});
            if let Some(off) = input["offset"].as_u64() {
                v["start_line"] = json!(off);
                if let Some(lim) = input["limit"].as_u64() {
                    v["end_line"] = json!(off + lim.saturating_sub(1));
                }
            }
            ("read_file".into(), v)
        }
        "Edit" | "MultiEdit" => (
            "edit_file".into(),
            json!({"path": rel("file_path").unwrap_or_default()}),
        ),
        "Write" => {
            let path = rel("file_path").unwrap_or_default();
            let exists = input["file_path"]
                .as_str()
                .and_then(|p| sandbox.resolve(p).ok())
                .is_some_and(|a| a.exists());
            (
                if exists { "edit_file" } else { "create_file" }.into(),
                json!({"path": path}),
            )
        }
        "Glob" => (
            "glob".into(),
            json!({"pattern": input["pattern"].as_str().unwrap_or("")}),
        ),
        "Grep" => (
            "grep".into(),
            json!({"pattern": input["pattern"].as_str().unwrap_or("")}),
        ),
        other => (other.to_string(), input.clone()),
    }
}

/// Reads may only touch the project folder.
fn read_scope(sandbox: &Sandbox, tool: &str, input: &Value) -> Result<(), String> {
    let check = |p: &str| {
        sandbox
            .resolve(p)
            .map(|_| ())
            .map_err(|e| format!("Pascoda only allows reading inside the project folder: {e}"))
    };
    match tool {
        "Read" => match input["file_path"].as_str() {
            Some(p) => check(p),
            None => Err("Read needs a file_path.".into()),
        },
        "Grep" => input["path"].as_str().map_or(Ok(()), check),
        "Glob" => {
            if let Some(p) = input["path"].as_str() {
                check(p)?;
            }
            let pat = input["pattern"].as_str().unwrap_or("");
            if Path::new(pat)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err("Glob patterns may not contain '..'.".into());
            }
            if pat.starts_with('/') {
                // Check the fixed part in front of the first wildcard.
                let fixed: String = pat
                    .split(['*', '?', '[', '{'])
                    .next()
                    .unwrap_or("")
                    .to_string();
                let dir = fixed.rfind('/').map_or("/", |i| &fixed[..=i]);
                check(dir)?;
            }
            Ok(())
        }
        _ => Err("not a read tool".into()),
    }
}

/// The text of a file as the IDE has it (unsaved tabs and open forms win over the disk).
pub(crate) fn current_text(host: &mut dyn Host, abs: &Path) -> Option<String> {
    host.open_text(abs)
        .or_else(|| {
            (abs.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lfm")))
            .then(|| host.read_form(abs))
            .flatten()
        })
        .or_else(|| std::fs::read_to_string(abs).ok())
}

fn replace_in(text: &str, old: &str, new: &str, all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("old_string is empty.".into());
    }
    if old == new {
        return Err("old_string and new_string are identical; nothing to change.".into());
    }
    match text.matches(old).count() {
        0 => Err("old_string was not found in the file as the IDE has it (it must match exactly, including whitespace). Read the file again and retry with the exact current text.".into()),
        1 => Ok(text.replacen(old, new, 1)),
        _ if all => Ok(text.replace(old, new)),
        n => Err(format!(
            "old_string matches {n} places; include more surrounding lines to make it unique, or set replace_all."
        )),
    }
}

/// Turns an Edit/MultiEdit/Write permission request into a proposal for the user. The new text
/// is computed here, against the IDE's text, through the same path the app uses to apply it.
fn edit_proposal(
    sandbox: &Sandbox,
    host: &mut dyn Host,
    tool: &str,
    input: &Value,
    id: &str,
) -> Result<Proposal, String> {
    let path = input["file_path"]
        .as_str()
        .ok_or("The tool call has no file_path.")?;
    let abs = sandbox
        .resolve(path)
        .map_err(|e| format!("Pascoda only allows changes inside the project folder: {e}"))?;
    let cur = current_text(host, &abs);
    let change = match tool {
        "Edit" => {
            let old = input["old_string"].as_str().unwrap_or("");
            let new = input["new_string"].as_str().unwrap_or("");
            let all = input["replace_all"].as_bool().unwrap_or(false);
            match &cur {
                None if old.is_empty() => Change::Create {
                    content: new.to_string(),
                },
                None => return Err("The file does not exist.".into()),
                Some(c) => {
                    let after = replace_in(c, old, new, all)?;
                    if all && c.matches(old).count() > 1 {
                        Change::Edit {
                            old_text: c.clone(),
                            new_text: after,
                        }
                    } else {
                        Change::Edit {
                            old_text: old.to_string(),
                            new_text: new.to_string(),
                        }
                    }
                }
            }
        }
        "MultiEdit" => {
            let c = cur.clone().ok_or("The file does not exist.")?;
            let edits = input["edits"]
                .as_array()
                .filter(|e| !e.is_empty())
                .ok_or("MultiEdit needs a list of edits.")?;
            let mut text = c.clone();
            for (i, e) in edits.iter().enumerate() {
                text = replace_in(
                    &text,
                    e["old_string"].as_str().unwrap_or(""),
                    e["new_string"].as_str().unwrap_or(""),
                    e["replace_all"].as_bool().unwrap_or(false),
                )
                .map_err(|m| format!("Edit {} of {}: {m}", i + 1, edits.len()))?;
            }
            Change::Edit {
                old_text: c,
                new_text: text,
            }
        }
        "Write" => {
            let content = input["content"].as_str().unwrap_or("").to_string();
            match &cur {
                None => Change::Create { content },
                Some(c) if *c == content => return Err("The file already has this content.".into()),
                Some(c) if c.is_empty() => {
                    return Err("The file exists but is empty; ask the user to type a character into it first, or create another file.".into());
                }
                Some(c) => Change::Edit {
                    old_text: c.clone(),
                    new_text: content,
                },
            }
        }
        _ => return Err("not an edit tool".into()),
    };
    let p = Proposal {
        id: id.to_string(),
        path: sandbox.relative(&abs),
        abs_path: abs,
        change,
    };
    p.apply_to(cur.as_deref())?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_aliases() {
        assert_eq!(cli_model(crate::MODEL_SONNET), "sonnet");
        assert_eq!(cli_model(crate::MODEL_OPUS), "opus");
        assert_eq!(cli_model(crate::MODEL_HAIKU), "haiku");
        assert_eq!(cli_model(crate::MODEL_FABLE), "fable");
        assert_eq!(cli_model("sonnet"), "sonnet");
        assert_eq!(cli_model("claude-x-1"), "claude-x-1");
    }

    #[test]
    fn command_line_is_locked_down() {
        let mut cfg = Config::new("/x/claude".into());
        cfg.model = crate::MODEL_OPUS.into();
        let a = cfg.args(None);
        let has = |s: &str| a.iter().any(|x| x == s);
        assert!(has("--permission-prompt-tool") && has("stdio") && has("manual"));
        assert!(has("--strict-mcp-config") && has(TOOLS) && has("--setting-sources"));
        assert!(has("--include-partial-messages") && has("opus") && has("--append-system-prompt"));
        for bad in [
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
            "bypassPermissions",
            "--bare",
            "--mcp-config",
        ] {
            assert!(!has(bad), "{bad}");
        }
        assert!(!a.iter().any(|x| x == "--resume"));
        let r = cfg.args(Some("abc"));
        assert_eq!(&r[r.len() - 2..], ["--resume", "abc"]);
        // no Bash in the tool list
        assert!(!TOOLS.contains("Bash") && !TOOLS.contains("Web"));
    }

    #[test]
    fn the_api_key_variable_is_removed_from_the_child() {
        let cfg = Config::new("/x/claude".into());
        let c = cfg.command(Path::new("/"), None);
        let removed: Vec<_> = c
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(removed, vec!["ANTHROPIC_API_KEY"]);
        assert!(c.get_envs().all(|(_, v)| v.is_none()), "nothing is added");
    }

    #[test]
    fn this_module_never_touches_credentials() {
        // The driver may only spawn the CLI; it must not name credential stores or settings.
        let src = include_str!("claude_code.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for needle in [
            ".credentials",
            "security find",
            "SecItem",
            "Keystore",
            "keychain",
            "settings.json",
            "~/.claude/",
            "OAUTH",
        ] {
            let hits: Vec<_> = code
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains(needle))
                .collect();
            assert!(hits.is_empty(), "{needle}: {hits:?}");
        }
    }

    #[test]
    fn locating_the_binary() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let bin = home.join(".local/bin/claude");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // not executable yet
        assert_eq!(locate_in(None, None, Some(home.clone().into())), None);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            locate_in(None, Some("/nonexistent".into()), Some(home.clone().into())),
            Some(bin.clone())
        );
        // PATH wins over the fallbacks
        let p = d.path().join("path");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("claude"), "x").unwrap();
        std::fs::set_permissions(p.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            locate_in(None, Some(p.clone().into()), Some(home.clone().into())),
            Some(p.join("claude"))
        );
        // an override is the only candidate
        assert_eq!(
            locate_in(Some(bin.clone().into()), Some(p.into()), None),
            Some(bin)
        );
        assert_eq!(
            locate_in(Some(d.path().join("none").into()), None, Some(home.into())),
            None
        );
    }

    #[test]
    fn errors_are_classified() {
        assert_eq!(
            classify(
                "authentication_failed",
                "Not logged in · Please run /login",
                false
            ),
            AiError::NotLoggedIn
        );
        assert!(
            AiError::NotLoggedIn
                .to_string()
                .contains("run `claude` once")
        );
        assert!(AiError::NotLoggedIn.to_string().contains("/login"));
        assert!(matches!(
            classify(
                "",
                "You've reached your usage limit. It will reset at 5pm",
                false
            ),
            AiError::RateLimited { .. }
        ));
        assert!(matches!(
            classify("", "something", true),
            AiError::RateLimited { .. }
        ));
        assert!(matches!(
            classify("", "API Error: overloaded_error", false),
            AiError::Overloaded(_)
        ));
        assert!(matches!(classify("x", "boom", false), AiError::Cli(m) if m.contains("boom")));
    }

    #[test]
    fn edit_requests_become_proposals_against_the_ides_text() {
        struct H(Option<String>);
        impl Host for H {
            fn diagnostics(&mut self) -> String {
                String::new()
            }
            fn build(&mut self) -> String {
                String::new()
            }
            fn open_text(&mut self, p: &Path) -> Option<String> {
                p.ends_with("a.pas").then(|| self.0.clone()).flatten()
            }
        }
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.pas"), "disk text\n").unwrap();
        let sb = Sandbox::new(d.path()).unwrap();
        let abs = sb.root().join("a.pas");
        let mut host = H(Some("unsaved one\nunsaved one two\n".into()));
        let ed = |host: &mut H, input: Value| edit_proposal(&sb, host, "Edit", &input, "t1");
        // unique inside the IDE text, even though the disk differs
        let p = ed(
            &mut host,
            json!({"file_path": abs, "old_string": "one two", "new_string": "X"}),
        )
        .unwrap();
        assert_eq!(p.path, "a.pas");
        assert_eq!(
            p.apply_to(host.0.as_deref()).unwrap(),
            "unsaved one\nunsaved X\n"
        );
        // not unique / not found / replace_all
        assert!(
            ed(
                &mut host,
                json!({"file_path": abs, "old_string": "unsaved", "new_string": "Y"})
            )
            .unwrap_err()
            .contains("2 places")
        );
        assert!(
            ed(
                &mut host,
                json!({"file_path": abs, "old_string": "disk", "new_string": "Y"})
            )
            .unwrap_err()
            .contains("not found")
        );
        let p = ed(
            &mut host,
            json!({"file_path": abs, "old_string": "unsaved", "new_string": "Y", "replace_all": true}),
        )
        .unwrap();
        assert_eq!(p.apply_to(host.0.as_deref()).unwrap(), "Y one\nY one two\n");
        // outside the project
        assert!(
            ed(
                &mut host,
                json!({"file_path": "/etc/hosts", "old_string": "a", "new_string": "b"})
            )
            .unwrap_err()
            .contains("inside the project")
        );
        // write: new file, existing file, identical
        let w = |host: &mut H, f: &str, c: &str| {
            edit_proposal(
                &sb,
                host,
                "Write",
                &json!({"file_path": f, "content": c}),
                "t2",
            )
        };
        let p = w(&mut host, "new.pas", "program n;\n").unwrap();
        assert!(matches!(&p.change, Change::Create { content } if content == "program n;\n"));
        // the unit exists only on disk here, but the host says it has text
        let p = w(&mut host, "a.pas", "whole\n").unwrap();
        assert_eq!(p.apply_to(host.0.as_deref()).unwrap(), "whole\n");
        assert!(w(&mut host, "a.pas", "unsaved one\nunsaved one two\n").is_err());
        // multi edit: sequential
        let p = edit_proposal(
            &sb,
            &mut host,
            "MultiEdit",
            &json!({"file_path": "a.pas", "edits": [
                {"old_string": "one two", "new_string": "2"},
                {"old_string": "2", "new_string": "3"}]}),
            "t3",
        )
        .unwrap();
        assert_eq!(
            p.apply_to(host.0.as_deref()).unwrap(),
            "unsaved one\nunsaved 3\n"
        );
        // an edit of a missing file without old_string creates it
        let mut none = H(None);
        let p = edit_proposal(
            &sb,
            &mut none,
            "Edit",
            &json!({"file_path": "b.pas", "old_string": "", "new_string": "x"}),
            "t4",
        )
        .unwrap();
        assert!(matches!(p.change, Change::Create { .. }));
    }

    #[test]
    fn reads_stay_inside_the_project() {
        let d = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(d.path()).unwrap();
        let root = sb.root().to_string_lossy().into_owned();
        assert!(read_scope(&sb, "Read", &json!({"file_path": format!("{root}/a.pas")})).is_ok());
        assert!(read_scope(&sb, "Read", &json!({"file_path": "/etc/hosts"})).is_err());
        assert!(read_scope(&sb, "Read", &json!({"file_path": "../x"})).is_err());
        assert!(read_scope(&sb, "Grep", &json!({"pattern": "x"})).is_ok());
        assert!(read_scope(&sb, "Grep", &json!({"pattern": "x", "path": "/etc"})).is_err());
        assert!(read_scope(&sb, "Glob", &json!({"pattern": "**/*.pas"})).is_ok());
        assert!(read_scope(&sb, "Glob", &json!({"pattern": "../**/*.pas"})).is_err());
        assert!(read_scope(&sb, "Glob", &json!({"pattern": "/etc/*"})).is_err());
        assert!(read_scope(&sb, "Glob", &json!({"pattern": format!("{root}/**/*.pas")})).is_ok());
        assert!(read_scope(&sb, "Glob", &json!({"pattern": "*", "path": "/"})).is_err());
        // a symlink out of the project does not help
        std::os::unix::fs::symlink("/etc", d.path().join("link")).unwrap();
        assert!(read_scope(&sb, "Read", &json!({"file_path": "link/hosts"})).is_err());
    }

    #[test]
    fn tool_calls_are_shown_like_the_api_tools() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.pas"), "x").unwrap();
        let sb = Sandbox::new(d.path()).unwrap();
        let root = sb.root().to_string_lossy().into_owned();
        let (n, i) = normalize(
            "Read",
            &json!({"file_path": format!("{root}/a.pas"), "offset": 3, "limit": 5}),
            &sb,
        );
        assert_eq!(n, "read_file");
        assert_eq!(i, json!({"path": "a.pas", "start_line": 3, "end_line": 7}));
        assert_eq!(
            normalize("Write", &json!({"file_path": "a.pas"}), &sb).0,
            "edit_file"
        );
        assert_eq!(
            normalize("Write", &json!({"file_path": "n.pas"}), &sb).0,
            "create_file"
        );
        assert_eq!(
            normalize("Edit", &json!({"file_path": "a.pas"}), &sb).1,
            json!({"path": "a.pas"})
        );
        assert_eq!(
            normalize("Glob", &json!({"pattern": "*.pas"}), &sb).0,
            "glob"
        );
    }
}
