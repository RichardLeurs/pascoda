//! The "Codex CLI" provider: drives the user's own installed `codex` as a subprocess, so a
//! ChatGPT/Codex subscription (or whatever login Codex has) works without Pascoda handling a key.
//!
//! Pascoda only spawns `codex app-server` and talks JSON-RPC over its stdin/stdout. It never
//! reads, copies or logs Codex's credentials or config files: Codex authenticates itself. (The
//! login check runs `codex login status` and keeps only whether it succeeded: the command also
//! prints a masked key.)
//!
//! Protocol (observed with codex-cli 0.165.0; recorded in `tests/fixtures/codex`):
//! - Lines of JSON, no `jsonrpc` member. We send `initialize` (clientInfo), then the notification
//!   `initialized`, then `thread/start` {cwd, approvalPolicy "on-request", sandbox "read-only",
//!   developerInstructions} and per user message `turn/start` {threadId, input [{type text}]}.
//! - The answer streams as notifications: `item/agentMessage/delta`, `item/started` /
//!   `item/completed` (commandExecution, fileChange, ...), `thread/tokenUsage/updated`, and
//!   `turn/completed` {turn {status completed | interrupted | failed, error}}.
//! - In a read-only sandbox, Codex reads with shell commands (no question asked) and changes files
//!   with `apply_patch`: an `item/started` fileChange item carries `changes` [{path (absolute),
//!   kind {type add | update | delete, move_path}, diff}] (add: the whole new text; update: unified
//!   hunks), followed by the server request `item/fileChange/requestApproval` {itemId}. We answer
//!   {"decision":"accept"} (Codex then writes the file itself) or "decline". Any command that
//!   needs permission arrives as `item/commandExecution/requestApproval`; we decline those.
//! - `turn/interrupt` {threadId, turnId} stops a turn; `turn/completed` follows with `interrupted`.

use crate::agent::{Backend, Decision, Event, Finish};
use crate::claude_code::{current_text, is_executable};
use crate::error::AiError;
use crate::prompt::CODEX_PROMPT;
use crate::sse::Usage;
use crate::tools::{Approval, Change, Host, Proposal, Sandbox};
use crate::udiff::apply_unified;
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

/// Environment variable naming the `codex` binary to use (tests, unusual installs).
pub const ENV_BIN: &str = "PASCODA_CODEX_BIN";

/// Codex's own optional tools that Pascoda does not want in an IDE assistant. The real barrier
/// is the read-only sandbox plus the approval requests, which the IDE answers; this only makes
/// the tool set smaller (computer use, browser, plugins, sub-agents ...).
const LOCKDOWN: [&str; 14] = [
    "-c",
    "mcp_servers={}",
    "--disable",
    "plugins",
    "--disable",
    "apps",
    "--disable",
    "computer_use",
    "--disable",
    "browser_use",
    "--disable",
    "hooks",
    "--disable",
    "multi_agent",
];

// ---- finding the binary and its state ------------------------------------------------------

/// Finds `codex`: `override_path` (when given it is the only candidate), then `PATH`, then the
/// usual places (`~/.local/bin`, `~/.npm-global/bin`, `/usr/local/bin`, `/opt/homebrew/bin`).
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
        dirs.push(h.join(".npm-global/bin"));
    }
    dirs.push("/usr/local/bin".into());
    dirs.push("/opt/homebrew/bin".into());
    dirs.into_iter()
        .map(|d| d.join("codex"))
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

/// What `codex --version` and `codex login status` say (the same shape as for Claude Code). Nothing
/// else is kept.
pub use crate::claude_code::CliStatus;

/// Runs `bin args` with a timeout: (exit status success, stdout).
fn capture(bin: &Path, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    Some((ok, reader.join().ok()?))
}

/// Is the CLI there, which version, and is it logged in? Blocks for a moment: call it on a
/// worker thread. The text of `login status` (it shows a masked key) is dropped.
pub fn check(bin: &Path) -> CliStatus {
    let version = capture(bin, &["--version"], Duration::from_secs(10))
        .and_then(|(_, s)| s.split_whitespace().last().map(str::to_string));
    let logged_in = capture(bin, &["login", "status"], Duration::from_secs(15))
        .map(|(ok, text)| ok && !text.to_ascii_lowercase().contains("not logged in"));
    CliStatus {
        binary: bin.to_path_buf(),
        version,
        logged_in,
    }
}

// ---- configuration and the process -------------------------------------------------------------

pub struct Config {
    pub binary: PathBuf,
    /// A Codex model name; empty = Codex's own default.
    pub model: String,
    /// How long an interrupted turn may take to end before the process is killed.
    pub interrupt_grace: Duration,
}

impl Config {
    pub fn new(binary: PathBuf) -> Self {
        Config {
            binary,
            model: String::new(),
            interrupt_grace: Duration::from_secs(5),
        }
    }

    /// The command line (without the program).
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = LOCKDOWN.iter().map(|s| s.to_string()).collect();
        a.push("app-server".into());
        a
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
    next_id: u64,
}

impl Proc {
    fn spawn(cfg: &Config, root: &Path) -> Result<Proc, AiError> {
        let mut child = Command::new(&cfg.binary)
            .args(cfg.args())
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                AiError::Cli(match e.kind() {
                    std::io::ErrorKind::NotFound => format!(
                        "Codex was not found at {}. Install it with `npm i -g @openai/codex`.",
                        cfg.binary.display()
                    ),
                    _ => format!("Codex could not be started: {e}"),
                })
            })?;
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(i), Some(o), Some(e)) => (i, o, e),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AiError::Cli("Codex started without pipes.".into()));
                }
            };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(Msg::Line(line)).is_err() {
                    return;
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
            next_id: 0,
        })
    }

    fn send(&mut self, v: &Value) -> std::io::Result<()> {
        writeln!(self.stdin, "{v}")?;
        self.stdin.flush()
    }

    /// Sends a request and returns its id.
    fn request(&mut self, method: &str, params: Value) -> std::io::Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    fn died(&mut self) -> AiError {
        let status = (0..20)
            .find_map(|_| match self.child.try_wait() {
                Ok(Some(s)) => Some(Some(s)),
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(25));
                    None
                }
                Err(_) => Some(None),
            })
            .flatten();
        let tail = self
            .stderr
            .lock()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let lower = tail.to_ascii_lowercase();
        if lower.contains("not logged in") || lower.contains("log in") && lower.contains("codex") {
            return AiError::CodexNotLoggedIn;
        }
        let mut m = match status {
            Some(s) => format!("Codex stopped unexpectedly ({s})."),
            None => "Codex stopped unexpectedly.".to_string(),
        };
        if !tail.is_empty() {
            m.push_str(&format!("\n{tail}"));
        }
        AiError::Cli(m)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- the driver ----------------------------------------------------------------------------------

pub struct Codex {
    cfg: Config,
    host: Box<dyn Host>,
    sandbox: Sandbox,
    proc: Option<Proc>,
    thread: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl Codex {
    pub fn new(cfg: Config, host: Box<dyn Host>, sandbox: Sandbox) -> Self {
        Codex {
            cfg,
            host,
            sandbox,
            proc: None,
            thread: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread.as_deref()
    }

    pub fn running(&self) -> bool {
        self.proc.is_some()
    }

    /// Waits for the response to request `id`, skipping everything else (used during setup).
    fn wait_response(proc: &mut Proc, id: u64, cancel: &AtomicBool) -> Result<Value, AiError> {
        let start = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            if start.elapsed() > Duration::from_secs(60) {
                return Err(AiError::Cli("Codex did not answer in time.".into()));
            }
            match proc.rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Msg::Line(l)) => {
                    let Ok(v) = serde_json::from_str::<Value>(&l) else {
                        continue;
                    };
                    if v["id"].as_u64() == Some(id) && v.get("method").is_none() {
                        if let Some(e) = v.get("error") {
                            return Err(AiError::Cli(format!(
                                "Codex refused the request: {}",
                                e["message"].as_str().unwrap_or("unknown error")
                            )));
                        }
                        return Ok(v["result"].clone());
                    }
                    // a server request during setup: nothing is asked then; refuse politely
                    if v.get("method").is_some()
                        && let Some(rid) = v.get("id")
                    {
                        let _ = proc.send(&json!({"id": rid, "error": {"code": -32601, "message": "not supported"}}));
                    }
                }
                Ok(Msg::Eof) | Err(RecvTimeoutError::Disconnected) => return Err(proc.died()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Starts the process and a thread (or resumes the one of an earlier process).
    fn ensure(&mut self) -> Result<(), AiError> {
        if self.proc.is_none() {
            let mut proc = Proc::spawn(&self.cfg, self.sandbox.root())?;
            let id = proc
                .request(
                    "initialize",
                    json!({"clientInfo": {"name": "pascoda", "title": "Pascoda", "version": env!("CARGO_PKG_VERSION")}}),
                )
                .map_err(|_| proc.died())?;
            Self::wait_response(&mut proc, id, &self.cancel)?;
            proc.send(&json!({"method": "initialized"}))
                .map_err(|_| proc.died())?;
            // a restarted process picks the conversation up again when Codex still has it
            if let Some(t) = self.thread.clone() {
                let id = proc
                    .request("thread/resume", json!({"threadId": t}))
                    .map_err(|_| proc.died())?;
                if Self::wait_response(&mut proc, id, &self.cancel).is_err() {
                    self.thread = None;
                }
            }
            self.proc = Some(proc);
        }
        if self.thread.is_none() {
            let root = self.sandbox.root().to_string_lossy().into_owned();
            let mut params = json!({
                "cwd": root,
                "approvalPolicy": "on-request",
                "sandbox": "read-only",
                "developerInstructions": CODEX_PROMPT,
            });
            if !self.cfg.model.trim().is_empty() {
                params["model"] = json!(self.cfg.model.trim());
            }
            let proc = self.proc.as_mut().expect("started above");
            let id = proc
                .request("thread/start", params)
                .map_err(|_| proc.died())?;
            let r = Self::wait_response(proc, id, &self.cancel)?;
            let t = r["thread"]["id"]
                .as_str()
                .ok_or_else(|| AiError::Protocol("thread/start returned no thread id".into()))?;
            self.thread = Some(t.to_string());
        }
        Ok(())
    }

    fn turn(
        &mut self,
        text: &str,
        events: &Sender<Event>,
        decisions: &Receiver<Decision>,
    ) -> Result<Finish, AiError> {
        if let Err(e) = self.ensure() {
            // a process that failed to set up is not reused
            if !matches!(e, AiError::Cancelled) {
                self.proc = None;
            }
            return Err(e);
        }
        let thread = self.thread.clone().expect("ensure made one");
        let mut proc = self.proc.take().expect("ensure made one");
        let mut t = Turn {
            proc: &mut proc,
            host: self.host.as_mut(),
            sandbox: &self.sandbox,
            events,
            decisions,
            cancel: &self.cancel,
            grace: self.cfg.interrupt_grace,
            thread: thread.clone(),
            turn_id: None,
            start_req: 0,
            healthy: true,
            gone: false,
            interrupted: None,
            cur_msg: String::new(),
            sent_text: false,
            files: HashMap::new(),
            notes: HashMap::new(),
            rejected: HashSet::new(),
        };
        let mut params = json!({
            "threadId": thread,
            "input": [{"type": "text", "text": text}],
        });
        if !self.cfg.model.trim().is_empty() {
            params["model"] = json!(self.cfg.model.trim());
        }
        let result = t.run(params);
        let healthy = t.healthy;
        if healthy {
            self.proc = Some(proc);
        }
        result
    }
}

impl Backend for Codex {
    fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn writes_files_itself(&self) -> bool {
        true
    }

    fn set_model(&mut self, model: &str) {
        self.cfg.model = model.trim().to_string();
    }

    fn clear(&mut self) {
        self.proc = None; // Drop kills it
        self.thread = None;
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

/// One file of a `fileChange` item.
#[derive(Clone)]
struct FileChg {
    /// The proposal id (and tool-row id).
    id: String,
    path: String,
    kind: String,
    moved: bool,
    diff: String,
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
    thread: String,
    turn_id: Option<String>,
    /// Id of our `turn/start` request.
    start_req: u64,
    healthy: bool,
    gone: bool,
    /// When `turn/interrupt` was sent.
    interrupted: Option<Instant>,
    cur_msg: String,
    sent_text: bool,
    /// `fileChange` items by item id.
    files: HashMap<String, Vec<FileChg>>,
    /// Why a change was not applied by us (shown on its row).
    notes: HashMap<String, String>,
    /// Changes the user rejected.
    rejected: HashSet<String>,
}

fn s_of<'v>(v: &'v Value, k: &str) -> &'v str {
    v[k].as_str().unwrap_or("")
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push_str("\n... (cut)");
        t
    }
}

fn usage_of(last: &Value) -> Usage {
    let n = |k: &str| last[k].as_u64().unwrap_or(0);
    let cached = n("cachedInputTokens");
    Usage {
        input_tokens: n("inputTokens").saturating_sub(cached),
        output_tokens: n("outputTokens"),
        cache_creation_input_tokens: n("cacheWriteInputTokens"),
        cache_read_input_tokens: cached,
    }
}

/// Maps a failed turn's error to ours.
fn classify(error: &Value) -> AiError {
    let message = s_of(error, "message").to_string();
    let info = error["codexErrorInfo"].as_str().unwrap_or("");
    match info {
        "unauthorized" => AiError::CodexNotLoggedIn,
        "usageLimitExceeded" | "rateLimitExceeded" | "sessionBudgetExceeded" => {
            AiError::RateLimited {
                retry_after: None,
                message,
            }
        }
        "serverOverloaded" => AiError::Overloaded(message),
        _ => {
            let l = message.to_ascii_lowercase();
            if l.contains("not logged in") || l.contains("401") || l.contains("unauthorized") {
                AiError::CodexNotLoggedIn
            } else if message.is_empty() {
                AiError::Cli("Codex reported an error.".into())
            } else {
                AiError::Cli(message)
            }
        }
    }
}

impl Turn<'_> {
    fn emit(&mut self, ev: Event) {
        if self.events.send(ev).is_err() {
            self.gone = true;
        }
    }

    fn write(&mut self, v: &Value) -> Result<(), AiError> {
        self.proc.send(v).map_err(|_| {
            self.healthy = false;
            self.proc.died()
        })
    }

    fn respond(&mut self, id: &Value, result: Value) -> Result<(), AiError> {
        self.write(&json!({"id": id, "result": result}))
    }

    fn run(&mut self, params: Value) -> Result<Finish, AiError> {
        self.start_req = self.proc.request("turn/start", params).map_err(|_| {
            self.healthy = false;
            self.proc.died()
        })?;
        loop {
            if self.gone {
                // The app dropped the receiver: stop the turn quietly.
                self.cancel.store(true, Ordering::Relaxed);
            }
            if self.cancel.load(Ordering::Relaxed) {
                match self.interrupted {
                    None => {
                        if let Some(turn) = self.turn_id.clone() {
                            let thread = self.thread.clone();
                            self.write(&json!({
                                "id": 900_000,
                                "method": "turn/interrupt",
                                "params": {"threadId": thread, "turnId": turn},
                            }))?;
                            self.interrupted = Some(Instant::now());
                        }
                    }
                    Some(at) if at.elapsed() > self.grace => {
                        // Codex ignored the interrupt: end the process; the next turn starts a
                        // new one and resumes the thread.
                        self.healthy = false;
                        return Err(AiError::Cancelled);
                    }
                    Some(_) => {}
                }
            }
            match self.proc.rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Msg::Line(l)) => {
                    let Ok(v) = serde_json::from_str::<Value>(&l) else {
                        continue;
                    };
                    if let Some(done) = self.handle(&v)? {
                        return Ok(done);
                    }
                }
                Ok(Msg::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.healthy = false;
                    return Err(self.proc.died());
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// One message from Codex. `Some` ends the turn.
    fn handle(&mut self, m: &Value) -> Result<Option<Finish>, AiError> {
        let method = m["method"].as_str();
        match (method, m.get("id")) {
            (Some(method), Some(id)) => {
                self.serve(id.clone(), method, &m["params"])?;
                Ok(None)
            }
            (Some(method), None) => self.notification(method, &m["params"]),
            (None, Some(id)) => {
                // a response: ours to `turn/start`, or to the interrupt
                if id.as_u64() == Some(self.start_req) {
                    if let Some(e) = m.get("error") {
                        return Err(AiError::Cli(format!(
                            "Codex refused the message: {}",
                            e["message"].as_str().unwrap_or("unknown error")
                        )));
                    }
                    if let Some(t) = m["result"]["turn"]["id"].as_str() {
                        self.turn_id.get_or_insert_with(|| t.to_string());
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn notification(&mut self, method: &str, p: &Value) -> Result<Option<Finish>, AiError> {
        match method {
            "turn/started" => {
                if let Some(t) = p["turn"]["id"].as_str() {
                    self.turn_id.get_or_insert_with(|| t.to_string());
                }
            }
            "item/agentMessage/delta" => {
                let id = s_of(p, "itemId");
                if id != self.cur_msg {
                    if self.sent_text {
                        self.emit(Event::Text("\n\n".into()));
                    }
                    self.cur_msg = id.to_string();
                }
                let d = s_of(p, "delta");
                if !d.is_empty() {
                    self.sent_text = true;
                    self.emit(Event::Text(d.to_string()));
                }
            }
            "item/started" => self.item_started(&p["item"]),
            "item/completed" => self.item_completed(&p["item"]),
            "thread/tokenUsage/updated" => {
                let u = usage_of(&p["tokenUsage"]["last"]);
                self.emit(Event::Usage(u));
            }
            "turn/completed" => {
                let turn = &p["turn"];
                if let (Some(mine), Some(id)) = (&self.turn_id, turn["id"].as_str())
                    && mine != id
                {
                    return Ok(None); // an earlier turn
                }
                return match s_of(turn, "status") {
                    "interrupted" => Ok(Some(Finish::Cancelled)),
                    "failed" => Err(classify(&turn["error"])),
                    _ => Ok(Some(Finish::Finished)),
                };
            }
            _ => {}
        }
        Ok(None)
    }

    fn item_started(&mut self, item: &Value) {
        let id = s_of(item, "id").to_string();
        match s_of(item, "type") {
            "commandExecution" => {
                let command = s_of(item, "command").to_string();
                self.emit(Event::ToolCall {
                    id,
                    name: "shell".into(),
                    input: json!({"command": command}),
                });
            }
            "fileChange" => {
                let changes = item["changes"].as_array().cloned().unwrap_or_default();
                let many = changes.len() > 1;
                let mut list = Vec::new();
                for (i, c) in changes.iter().enumerate() {
                    let cid = if many {
                        format!("{id}#{}", i + 1)
                    } else {
                        id.clone()
                    };
                    let kind = c["kind"]["type"].as_str().unwrap_or("update").to_string();
                    let chg = FileChg {
                        id: cid,
                        path: s_of(c, "path").to_string(),
                        kind,
                        moved: c["kind"]["move_path"].as_str().is_some(),
                        diff: s_of(c, "diff").to_string(),
                    };
                    let name = if chg.kind == "add" {
                        "create_file"
                    } else {
                        "edit_file"
                    };
                    let rel = self
                        .sandbox
                        .resolve(&chg.path)
                        .map(|a| self.sandbox.relative(&a))
                        .unwrap_or_else(|_| chg.path.clone());
                    self.emit(Event::ToolCall {
                        id: chg.id.clone(),
                        name: name.into(),
                        input: json!({"path": rel}),
                    });
                    list.push(chg);
                }
                self.files.insert(id, list);
            }
            _ => {}
        }
    }

    fn item_completed(&mut self, item: &Value) {
        let id = s_of(item, "id").to_string();
        let status = s_of(item, "status");
        match s_of(item, "type") {
            "commandExecution" => {
                let out = clip(s_of(item, "aggregatedOutput"), 1500);
                let failed = status != "completed" || item["exitCode"].as_i64().unwrap_or(0) != 0;
                let output = match (status, out.is_empty()) {
                    ("declined", _) => {
                        "Not run: Pascoda only allows commands that need no permission.".to_string()
                    }
                    (_, true) if !failed => "(no output)".to_string(),
                    _ => out,
                };
                self.emit(Event::ToolResult {
                    id,
                    name: "shell".into(),
                    output,
                    is_error: failed,
                });
            }
            "fileChange" => {
                for c in self.files.remove(&id).unwrap_or_default() {
                    let name = if c.kind == "add" {
                        "create_file"
                    } else {
                        "edit_file"
                    };
                    let (output, is_error) = match status {
                        "completed" => ("Applied.".to_string(), false),
                        _ if self.rejected.contains(&c.id) => {
                            ("The user rejected this change.".to_string(), false)
                        }
                        _ => (
                            self.notes
                                .remove(&c.id)
                                .map(|n| format!("Not applied: {n}"))
                                .unwrap_or_else(|| {
                                    format!("The change was not applied ({status}).")
                                }),
                            true,
                        ),
                    };
                    self.emit(Event::ToolResult {
                        id: c.id,
                        name: name.into(),
                        output,
                        is_error,
                    });
                }
            }
            _ => {}
        }
    }

    /// A request from Codex that needs our answer.
    fn serve(&mut self, id: Value, method: &str, p: &Value) -> Result<(), AiError> {
        match method {
            "item/fileChange/requestApproval" => self.file_change_approval(id, p),
            // Anything that needs permission to run is refused: the IDE assistant reads and
            // proposes patches, it does not run programs.
            "item/commandExecution/requestApproval" => {
                self.respond(&id, json!({"decision": "decline"}))
            }
            _ => self.write(&json!({"id": id, "error": {"code": -32601, "message": "not supported by this client"}})),
        }
    }

    fn proposal_for(&mut self, c: &FileChg) -> Result<Proposal, String> {
        let abs = self
            .sandbox
            .resolve(&c.path)
            .map_err(|e| format!("Pascoda only allows changes inside the project folder: {e}"))?;
        if c.moved {
            return Err("renaming files is not supported through the assistant; ask the user to do it in the IDE".into());
        }
        let cur = current_text(self.host, &abs);
        let change = match c.kind.as_str() {
            "add" => match &cur {
                None => Change::Create {
                    content: c.diff.clone(),
                },
                Some(old) if *old == c.diff => {
                    return Err("the file already has this content".into());
                }
                Some(old) => Change::Edit {
                    old_text: old.clone(),
                    new_text: c.diff.clone(),
                },
            },
            "update" => {
                let old = cur.clone().ok_or("the file does not exist")?;
                let new = apply_unified(&old, &c.diff)?;
                if new == old {
                    return Err("the change leaves the file as it is".into());
                }
                Change::Edit {
                    old_text: old,
                    new_text: new,
                }
            }
            _ => {
                return Err("deleting files is not supported through the assistant; ask the user to do it in the IDE".into());
            }
        };
        let p = Proposal {
            id: c.id.clone(),
            path: self.sandbox.relative(&abs),
            abs_path: abs,
            change,
        };
        p.apply_to(cur.as_deref())?;
        Ok(p)
    }

    /// Waits for the user's answer to `p`; `None` when the turn is being cancelled.
    fn ask(&mut self, p: Proposal) -> Option<Approval> {
        let id = p.id.clone();
        self.emit(Event::NeedsApproval(p));
        loop {
            if self.cancel.load(Ordering::Relaxed) || self.gone {
                return None;
            }
            match self.decisions.recv_timeout(Duration::from_millis(25)) {
                Ok(d) if d.id == id => return Some(d.approval),
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn file_change_approval(&mut self, id: Value, p: &Value) -> Result<(), AiError> {
        let item = s_of(p, "itemId").to_string();
        let changes = self.files.get(&item).cloned().unwrap_or_default();
        if changes.is_empty() {
            return self.respond(&id, json!({"decision": "decline"}));
        }
        // Every change must be understood before the user is asked about any.
        let mut proposals = Vec::new();
        let mut broken = false;
        for c in &changes {
            match self.proposal_for(c) {
                Ok(p) => proposals.push(p),
                Err(why) => {
                    self.notes.insert(c.id.clone(), why);
                    broken = true;
                }
            }
        }
        if broken {
            for c in &changes {
                self.notes.entry(c.id.clone()).or_insert_with(|| {
                    "another change of the same patch could not be applied".into()
                });
            }
            return self.respond(&id, json!({"decision": "decline"}));
        }
        for prop in proposals {
            let pid = prop.id.clone();
            match self.ask(prop) {
                Some(Approval::Approved) => {}
                Some(Approval::Rejected(reason)) => {
                    self.rejected.insert(pid.clone());
                    if let Some(r) = reason.filter(|r| !r.trim().is_empty()) {
                        self.notes.insert(pid, r);
                    }
                    return self.respond(&id, json!({"decision": "decline"}));
                }
                None => return self.respond(&id, json!({"decision": "cancel"})),
            }
        }
        self.respond(&id, json!({"decision": "accept"}))
    }
}
