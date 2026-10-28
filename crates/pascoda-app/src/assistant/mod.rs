//! The AI assistant: a chat panel on the right that talks to `pascoda-ai`.
//!
//! This file holds the state and the logic that does not draw anything: the conversation, the
//! worker thread that runs the agent, the host the agent uses to see the IDE, the context sent
//! with each message, and how an approved change is applied to the open files. `ui` draws it.
//! Everything that blocks (network, builds) runs off the UI thread; events come back over
//! channels and every one of them asks for a repaint.

pub mod diff;
pub mod markdown;
pub mod overview;
mod ui;

use super::*;
use pascoda_ai::claude_code::{self, ClaudeCode, CliStatus};
use pascoda_ai::codex::{self, Codex};
use pascoda_ai::{
    Agent, ApiKey, Approval, Backend, Change, Client, Config, Decision, Event, Finish, Host,
    Keystore, OpenAi, OpenAiConfig, OpenAiSettings, Proposal, Sandbox, Usage,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

/// A path inside the project as the overview names it (`/` separators).
fn rel_name(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// A file bigger than this is not pasted into the message; the model can `read_file` it.
const MAX_FILE_CHARS: usize = 12_000;
/// Build errors listed in the message.
const MAX_ERRORS: usize = 20;
/// One unsaved tab's text in the message, and all of them together.
const MAX_TAB_CHARS: usize = 6_000;
const MAX_TABS_CHARS: usize = 16_000;

// ---- what is sent with a message ------------------------------------------------------

/// Which parts of the IDE state go with the next message (the chips above the input).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chips {
    pub file: bool,
    pub selection: bool,
    pub errors: bool,
    /// The file tree and the unit summaries.
    pub project: bool,
    /// The other open tabs, with the text of unsaved ones.
    pub tabs: bool,
}

impl Default for Chips {
    fn default() -> Self {
        Chips {
            file: true,
            selection: true,
            errors: true,
            project: true,
            tabs: false,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot {
    /// Project-relative name, text, 1-based line of the caret.
    pub file: Option<(String, String, usize)>,
    /// Name, first and last line (1-based) and the selected text.
    pub selection: Option<(String, usize, usize, String)>,
    pub errors: Vec<String>,
    /// The project overview (see `overview`); `None` until the first scan is done.
    pub overview: Option<String>,
    /// The open tabs other than the active one.
    pub tabs: Vec<Tab>,
}

/// Another open tab: its project-relative name, and its text when it has unsaved changes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Tab {
    pub name: String,
    pub unsaved: Option<String>,
}

/// The names of the other tabs, and the text of the unsaved ones (the model reads files from
/// the disk and would miss those edits). Each text and the total are capped.
fn tabs_text(tabs: &[Tab]) -> String {
    let names: Vec<String> = tabs
        .iter()
        .map(|t| {
            if t.unsaved.is_some() {
                format!("{} (unsaved)", t.name)
            } else {
                t.name.clone()
            }
        })
        .collect();
    let mut out = format!("Other open tabs: {}\n", names.join(", "));
    let mut used = 0;
    for t in tabs {
        let Some(text) = &t.unsaved else { continue };
        let n = text.chars().count();
        let room = MAX_TABS_CHARS.saturating_sub(used).min(MAX_TAB_CHARS);
        if room == 0 {
            out.push_str(&format!(
                "Unsaved {}: text not included (size limit); use read_file for the saved version.\n",
                t.name
            ));
            continue;
        }
        let shown: String = text.chars().take(room).collect();
        used += shown.chars().count();
        out.push_str(&format!(
            "Unsaved text of {} (newer than the file on disk):\n```pascal\n{}\n```\n",
            t.name,
            shown.trim_end()
        ));
        if n > room {
            out.push_str(&format!("(cut after {room} of {n} characters)\n"));
        }
    }
    out
}

/// The text sent to the model and the labels of the context parts in it.
pub fn compose(chips: Chips, snap: &Snapshot, user: &str) -> (String, Vec<String>) {
    let mut parts = String::new();
    let mut labels = Vec::new();
    let mut after_file = Vec::new();
    if let (true, Some(text)) = (chips.project, &snap.overview) {
        parts.push_str("Project overview (read the relevant files before you rely on them):\n");
        parts.push_str(text);
        after_file.push("project".to_string());
    }
    if let (true, Some((name, text, line))) = (chips.file, &snap.file) {
        labels.push(name.clone());
        parts.push_str(&format!("Active file: {name} (caret at line {line})\n"));
        if text.chars().count() <= MAX_FILE_CHARS {
            parts.push_str(&format!("```pascal\n{}\n```\n", text.trim_end()));
        } else {
            parts.push_str("(too long to include here; use read_file to see it)\n");
        }
    }
    if let (true, Some((name, a, b, text))) = (chips.selection, &snap.selection) {
        labels.push(format!(
            "Selection, {} line{}",
            b - a + 1,
            if b > a { "s" } else { "" }
        ));
        let lines = if b > a {
            format!("lines {a}-{b}")
        } else {
            format!("line {a}")
        };
        parts.push_str(&format!(
            "Selected text ({lines} of {name}):\n```pascal\n{text}\n```\n"
        ));
    }
    labels.append(&mut after_file);
    if chips.tabs && !snap.tabs.is_empty() {
        let n = snap.tabs.len();
        labels.push(format!("{n} other tab{}", if n == 1 { "" } else { "s" }));
        parts.push_str(&tabs_text(&snap.tabs));
    }
    if chips.errors && !snap.errors.is_empty() {
        let n = snap.errors.len();
        labels.push(format!("{n} error{}", if n == 1 { "" } else { "s" }));
        parts.push_str("Current build errors:\n");
        for e in snap.errors.iter().take(MAX_ERRORS) {
            parts.push_str(e);
            parts.push('\n');
        }
        if n > MAX_ERRORS {
            parts.push_str(&format!("... and {} more\n", n - MAX_ERRORS));
        }
    }
    if parts.is_empty() {
        return (user.to_string(), labels);
    }
    (
        format!("[Context from the IDE]\n{parts}[End of context]\n\n{user}"),
        labels,
    )
}

// ---- the conversation -----------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub enum ToolState {
    Running,
    Done,
    Failed(String),
}

#[derive(Clone, PartialEq, Debug)]
pub struct ToolRow {
    pub id: String,
    pub label: String,
    pub state: ToolState,
    /// What the tool returned, shown as a tooltip.
    pub output: String,
}

#[derive(Clone, PartialEq, Debug)]
pub enum CardState {
    Pending,
    /// Approved and sent; the backend's own tool is writing the file (Claude Code) and the
    /// result has not arrived yet.
    Writing,
    Applied,
    /// An auto-applied edit that was taken back.
    Reverted,
    Rejected,
    Failed(String),
    /// The turn ended (stopped, failed) before the user decided.
    Cancelled,
}

/// What is needed to take an applied edit back.
#[derive(Clone, PartialEq, Debug)]
pub struct Undo {
    /// The text before the edit; `None` for a file the edit created.
    pub before: Option<String>,
    /// The text right after the edit, as the file had it then. Revert is only offered while the
    /// file still has exactly this text.
    pub after: String,
    /// The unit that was brought in step with an edited form: path, text before and after.
    pub unit: Option<(PathBuf, String, String)>,
    /// The edit went to the disk (the backend's tool, a file that was not open, a new file), so
    /// the revert writes the disk too; otherwise only the open tab/designer changed.
    pub on_disk: bool,
}

/// A change waiting for the user, with its diff. `local` ones come from a code block in an
/// answer, not from the agent, so nobody is waiting for the decision.
#[derive(Clone, PartialEq, Debug)]
pub struct Card {
    pub id: String,
    pub path: String,
    pub abs: PathBuf,
    pub change: Change,
    pub state: CardState,
    pub local: bool,
    /// Hunks and `(added, removed)`, or why the change cannot be applied.
    pub preview: Result<(Vec<diff::Row>, (usize, usize)), String>,
    /// Applied without a click ("Allow edits this session"); such a card keeps its diff and
    /// offers Revert.
    pub auto: bool,
    pub undo: Option<Box<Undo>>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    User {
        text: String,
        /// Exactly what went over the wire (context and message).
        sent: String,
        chips: Vec<String>,
    },
    Assistant(String),
    Tool(ToolRow),
    Card(Card),
    Error(String),
    Note(String),
}

/// How many hand-added Claude models the dropdown remembers.
const MAX_EXTRA_MODELS: usize = 10;

fn is_builtin_model(id: &str) -> bool {
    pascoda_ai::MODELS.iter().any(|(m, _)| *m == id)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyState {
    /// Not looked up yet.
    Unchecked,
    Missing,
    Keychain,
    /// From `ANTHROPIC_API_KEY` (or `OPENAI_API_KEY` for the OpenAI provider).
    Env,
}

/// Which service answers: the user's own Anthropic API key, their own OpenAI key (or any
/// OpenAI-compatible server), or their installed Claude Code (which uses their Claude
/// subscription and does its own login).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Provider {
    #[default]
    Api,
    OpenAi,
    ClaudeCode,
    /// The user's own installed `codex` (OpenAI Codex CLI), driven through its app server.
    Codex,
}

impl Provider {
    /// In the order of the chooser: the Claude providers together, then the OpenAI ones.
    pub const ALL: [Provider; 4] = [
        Provider::Api,
        Provider::ClaudeCode,
        Provider::OpenAi,
        Provider::Codex,
    ];

    /// Which company's agents these are; the chooser groups by it.
    pub fn vendor(self) -> &'static str {
        match self {
            Provider::Api | Provider::ClaudeCode => "Claude",
            Provider::OpenAi | Provider::Codex => "OpenAI",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Api => "Claude Console API",
            Provider::OpenAi => "OpenAI API",
            Provider::ClaudeCode => "Claude Code CLI",
            Provider::Codex => "Codex CLI",
        }
    }

    /// The agent's name for the panel title.
    pub fn short_name(self) -> &'static str {
        match self {
            Provider::Api => "Claude Console API",
            Provider::OpenAi => "OpenAI API",
            Provider::ClaudeCode => "Claude Code CLI",
            Provider::Codex => "Codex CLI",
        }
    }

    /// What is written to the settings file.
    fn token(self) -> &'static str {
        match self {
            Provider::Api => "api",
            Provider::OpenAi => "openai",
            Provider::ClaudeCode => "claude-code",
            Provider::Codex => "codex",
        }
    }

    pub fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(Path::new(&home).join("Library/Application Support/Pascoda/assistant-provider"))
    }

    /// The remembered choice (plain text); the API key provider when there is none.
    pub fn load(file: &Path) -> Provider {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        Provider::ALL
            .into_iter()
            .find(|p| p.token() == text.trim())
            .unwrap_or_default()
    }

    fn save(self, file: &Path) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, self.token());
    }
}

/// What is known about the installed Claude Code CLI.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum CliState {
    /// Not looked for yet.
    #[default]
    Unchecked,
    Checking,
    Missing,
    Found(CliStatus),
}

struct Run {
    events: std::sync::mpsc::Receiver<Event>,
    decisions: Sender<Decision>,
    cancel: Arc<AtomicBool>,
}

/// A request from the agent's worker for the UI thread to build.
pub struct BuildReq(pub Sender<String>);

pub struct Assistant {
    pub open: bool,
    pub key: KeyState,
    pub key_input: String,
    pub key_message: Option<String>,
    pub show_settings: bool,
    pub provider: Provider,
    pub cli: CliState,
    /// The same for the `codex` program.
    pub codex: CliState,
    /// Use this `codex` binary instead of looking for one (tests).
    pub codex_bin: Option<PathBuf>,
    /// The Codex model; empty = Codex's own default.
    pub codex_model: String,
    /// Where the Codex model is remembered (tests: `None`).
    pub codex_model_file: Option<PathBuf>,
    /// Use this `claude` binary instead of looking for one (tests).
    pub cli_bin: Option<PathBuf>,
    /// Where the provider choice is remembered; `None` = not remembered (tests).
    pub provider_file: Option<PathBuf>,
    /// Where the Claude model and the models added by hand are remembered (tests: `None`).
    pub model_file: Option<PathBuf>,
    /// Model ids the user typed that are not in the built-in list, newest first.
    pub extra_models: Vec<String>,
    pub items: Vec<Item>,
    pub input: String,
    pub model: String,
    pub chips: Chips,
    pub usage: Usage,
    /// Set when a suggestion or Apply wants the input to take focus.
    pub focus_input: bool,
    pub input_id: egui::Id,
    /// "Allow edits this session": proposed edits and new files are applied at once, no click.
    /// Off at every launch, never saved.
    pub auto_edits: bool,
    /// The one-line warning is showing (the first time the switch is turned on).
    /// The backend writes approved edits itself (Claude Code); known once the agent exists.
    pub(crate) backend_writes: bool,
    /// The last egui context, to put editor cursors right after a file was reloaded.
    ctx: Option<egui::Context>,
    /// Another API address (tests use a local server).
    pub base_url: Option<String>,
    api_key: Option<ApiKey>,
    keystore: Keystore,
    /// The OpenAI provider: base URL and model in use (saved in `openai_file`), the text
    /// fields of the key card, the key and where it lives.
    pub openai: OpenAiSettings,
    pub openai_edit: OpenAiSettings,
    pub openai_key_state: KeyState,
    /// Where `openai` is remembered; `None` = not remembered (tests).
    pub openai_file: Option<PathBuf>,
    openai_key: Option<ApiKey>,
    openai_keystore: Keystore,
    /// The project overview: index, cache.
    pub project: overview::Project,
    run: Option<Run>,
    agent: Option<Arc<Mutex<Box<dyn Backend>>>>,
    cli_rx: Option<std::sync::mpsc::Receiver<CliStatus>>,
    codex_rx: Option<std::sync::mpsc::Receiver<CliStatus>>,
    agent_root: PathBuf,
    data: Arc<Mutex<HostData>>,
    stop: Arc<AtomicBool>,
    build_tx: Sender<BuildReq>,
    build_rx: std::sync::mpsc::Receiver<BuildReq>,
}

impl Default for Assistant {
    fn default() -> Self {
        let (build_tx, build_rx) = channel();
        Assistant {
            open: false,
            key: KeyState::Unchecked,
            key_input: String::new(),
            key_message: None,
            show_settings: false,
            provider: Provider::default(),
            cli: CliState::default(),
            codex: CliState::default(),
            codex_bin: None,
            codex_model: String::new(),
            codex_model_file: None,
            cli_bin: None,
            provider_file: None,
            model_file: None,
            extra_models: Vec::new(),
            items: Vec::new(),
            input: String::new(),
            model: pascoda_ai::DEFAULT_MODEL.to_string(),
            chips: Chips::default(),
            usage: Usage::default(),
            focus_input: false,
            input_id: egui::Id::unique("assistant-input"),
            auto_edits: false,
            backend_writes: false,
            ctx: None,
            base_url: None,
            api_key: None,
            keystore: Keystore::app(),
            openai: OpenAiSettings::default(),
            openai_edit: OpenAiSettings::default(),
            openai_key_state: KeyState::Unchecked,
            openai_file: None,
            openai_key: None,
            openai_keystore: Keystore::openai(),
            project: overview::Project::default(),
            run: None,
            agent: None,
            cli_rx: None,
            codex_rx: None,
            agent_root: PathBuf::new(),
            data: Arc::default(),
            stop: Arc::default(),
            build_tx,
            build_rx,
        }
    }
}

impl Assistant {
    pub fn streaming(&self) -> bool {
        self.run.is_some()
    }

    /// Turns "Allow edits this session" on or off.
    pub fn set_auto_edits(&mut self, on: bool) {
        self.auto_edits = on;
    }

    pub fn ready(&self) -> bool {
        match self.provider {
            Provider::Api => self.api_key.is_some(),
            // A local server (Ollama, LM Studio) may work without a key.
            Provider::OpenAi => self.openai_key.is_some() || !self.openai.needs_key(),
            Provider::ClaudeCode => matches!(&self.cli,
                CliState::Found(s) if s.logged_in != Some(false)),
            Provider::Codex => matches!(&self.codex,
                CliState::Found(s) if s.logged_in != Some(false)),
        }
    }

    /// False while the key (or the Claude Code check) has not been looked up yet, so the panel
    /// shows nothing instead of a connect card that vanishes a moment later.
    pub fn settled(&self) -> bool {
        match self.provider {
            Provider::Api => self.key != KeyState::Unchecked,
            Provider::OpenAi => {
                !self.openai.needs_key() || self.openai_key_state != KeyState::Unchecked
            }
            Provider::ClaudeCode => !matches!(self.cli, CliState::Unchecked | CliState::Checking),
            Provider::Codex => !matches!(self.codex, CliState::Unchecked | CliState::Checking),
        }
    }

    /// Switches the provider (not while a turn runs): the conversation starts over.
    pub fn set_provider(&mut self, p: Provider) {
        if p == self.provider || self.run.is_some() {
            return;
        }
        self.provider = p;
        self.agent = None;
        // Stays open (and opens from the key card): the user may want to change the model next.
        // Only the gear button closes it.
        self.show_settings = true;
        self.key_message = None;
        self.key_input.clear();
        self.openai_edit = self.openai.clone();
        if let Some(f) = &self.provider_file {
            p.save(f);
        }
        if !self.items.is_empty() {
            self.items.push(Item::Note(format!(
                "Switched to {}: the conversation starts over.",
                p.label()
            )));
        }
        if p == Provider::Api && self.key == KeyState::Unchecked {
            self.refresh_key();
        }
        if p == Provider::OpenAi && self.openai_key_state == KeyState::Unchecked {
            self.refresh_openai_key();
        }
    }

    /// The model name for the running provider.
    pub fn current_model(&self) -> &str {
        match self.provider {
            Provider::OpenAi => &self.openai.model,
            Provider::Codex => &self.codex_model,
            _ => &self.model,
        }
    }

    /// Looks for the OpenAI key: the Keychain first, then `OPENAI_API_KEY`. Only called while
    /// the OpenAI provider is chosen.
    pub fn refresh_openai_key(&mut self) {
        let found = match self.openai_keystore.load() {
            Ok(Some(k)) => Some((k, KeyState::Keychain)),
            _ => std::env::var("OPENAI_API_KEY")
                .ok()
                .map(ApiKey::new)
                .filter(|k| !k.is_empty())
                .map(|k| (k, KeyState::Env)),
        };
        match found {
            Some((k, s)) => self.set_openai_key(Some(k), s),
            None => self.set_openai_key(None, KeyState::Missing),
        }
    }

    fn set_openai_key(&mut self, key: Option<ApiKey>, state: KeyState) {
        self.openai_key = key;
        self.openai_key_state = state;
        self.agent = None;
    }

    /// Uses `key` for this session without touching the Keychain (tests).
    #[cfg(test)]
    pub fn use_openai_key(&mut self, key: &str) {
        self.set_openai_key(Some(ApiKey::new(key)), KeyState::Keychain);
    }

    /// Makes the text fields of the OpenAI card the settings in use, and remembers them.
    pub fn commit_openai(&mut self) {
        let edit = OpenAiSettings {
            base_url: self.openai_edit.base_url.trim().to_string(),
            model: self.openai_edit.model.trim().to_string(),
        };
        let edit = OpenAiSettings {
            base_url: if edit.base_url.is_empty() {
                OpenAiSettings::default().base_url
            } else {
                edit.base_url
            },
            ..edit
        };
        if edit != self.openai {
            self.agent = None; // the next message builds a client for the new endpoint
        }
        self.openai = edit.clone();
        self.openai_edit = edit;
        self.persist_openai();
    }

    /// Writes base URL and model (never the key) when a file is set.
    /// `~/Library/Application Support/Pascoda/claude-models`.
    pub fn claude_model_file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(Path::new(&home).join("Library/Application Support/Pascoda/claude-models"))
    }

    /// Reads the file: the chosen model on the first line, the added models after it.
    pub fn load_claude_models(&mut self, file: &Path) {
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        if let Some(first) = lines.next() {
            self.model = first.to_string();
        }
        self.extra_models = lines
            .filter(|l| !is_builtin_model(l))
            .map(String::from)
            .take(MAX_EXTRA_MODELS)
            .collect();
    }

    /// Saves the chosen Claude model and the added ones (not the key, not the OpenAI model).
    pub fn persist_claude_model(&self) {
        if let Some(f) = &self.model_file {
            let mut text = format!("{}\n", self.model.trim());
            for m in &self.extra_models {
                text.push_str(m);
                text.push('\n');
            }
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(f, text);
        }
    }

    /// A model typed by hand that is not built in joins the dropdown (newest first).
    pub fn remember_model(&mut self) {
        let m = self.model.trim().to_string();
        if m.is_empty() || is_builtin_model(&m) {
            return;
        }
        self.extra_models.retain(|e| *e != m);
        self.extra_models.insert(0, m);
        self.extra_models.truncate(MAX_EXTRA_MODELS);
        self.persist_claude_model();
    }

    /// `~/Library/Application Support/Pascoda/codex-model`.
    pub fn codex_model_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(Path::new(&home).join("Library/Application Support/Pascoda/codex-model"))
    }

    pub fn persist_codex_model(&self) {
        if let Some(f) = &self.codex_model_file {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(f, self.codex_model.trim());
        }
    }

    pub fn persist_openai(&self) {
        if let Some(f) = &self.openai_file {
            let _ = self.openai.save(f);
        }
    }

    /// Looks for the `claude` program and asks it for its version and login state, off the UI
    /// thread. Only the CLI itself is run; no credential is read.
    pub fn refresh_cli(&mut self, ctx: &egui::Context) {
        let bin = self.cli_bin.clone().or_else(claude_code::locate);
        let Some(bin) = bin else {
            self.cli = CliState::Missing;
            return;
        };
        self.cli = CliState::Checking;
        let (tx, rx) = channel();
        self.cli_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(claude_code::check(&bin));
            ctx.request_repaint();
        });
    }

    /// [`Assistant::refresh_cli`] for the `codex` program.
    pub fn refresh_codex(&mut self, ctx: &egui::Context) {
        let bin = self.codex_bin.clone().or_else(codex::locate);
        let Some(bin) = bin else {
            self.codex = CliState::Missing;
            return;
        };
        self.codex = CliState::Checking;
        let (tx, rx) = channel();
        self.codex_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(codex::check(&bin));
            ctx.request_repaint();
        });
    }

    /// Looks again for whichever CLI is chosen.
    pub fn refresh_chosen_cli(&mut self, ctx: &egui::Context) {
        match self.provider {
            Provider::Codex => self.refresh_codex(ctx),
            _ => self.refresh_cli(ctx),
        }
    }

    fn poll_cli(&mut self) {
        if let Some(rx) = &self.codex_rx {
            match rx.try_recv() {
                Ok(s) => {
                    self.codex = CliState::Found(s);
                    self.codex_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => {
                    self.codex = CliState::Missing;
                    self.codex_rx = None;
                }
            }
        }
        if let Some(rx) = &self.cli_rx {
            match rx.try_recv() {
                Ok(s) => {
                    self.cli = CliState::Found(s);
                    self.cli_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => {
                    self.cli = CliState::Missing;
                    self.cli_rx = None;
                }
            }
        }
    }

    /// Looks for a key: the Keychain first, then `ANTHROPIC_API_KEY`.
    pub fn refresh_key(&mut self) {
        match self.keystore.load() {
            Ok(Some(k)) => self.set_key(Some(k), KeyState::Keychain),
            _ => match std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .map(ApiKey::new)
                .filter(|k| !k.is_empty())
            {
                Some(k) => self.set_key(Some(k), KeyState::Env),
                None => self.set_key(None, KeyState::Missing),
            },
        }
    }

    fn set_key(&mut self, key: Option<ApiKey>, state: KeyState) {
        self.api_key = key;
        self.key = state;
        self.agent = None; // the next message builds a client with the new key
    }

    /// Uses `key` for this session without touching the Keychain (tests, and a Keychain that
    /// refuses).
    #[cfg(test)]
    pub fn use_key(&mut self, key: &str) {
        self.set_key(Some(ApiKey::new(key)), KeyState::Keychain);
    }

    /// Stores the typed key in the Keychain.
    pub fn save_key(&mut self) {
        if self.provider == Provider::OpenAi {
            return self.save_openai_key();
        }
        let key = ApiKey::new(std::mem::take(&mut self.key_input));
        if key.is_empty() {
            self.key_message = Some("Paste your API key first.".into());
            return;
        }
        match self.keystore.save(&key) {
            Ok(()) => {
                self.key_message = None;
                self.set_key(Some(key), KeyState::Keychain);
            }
            Err(e) => self.key_message = Some(format!("The Keychain refused the key: {e}")),
        }
    }

    /// The OpenAI card's Save: the endpoint and model, and the key when one was typed.
    fn save_openai_key(&mut self) {
        let typed = ApiKey::new(std::mem::take(&mut self.key_input));
        self.commit_openai();
        if typed.is_empty() {
            if self.openai_key.is_none() && self.openai.needs_key() {
                self.key_message = Some("Paste your OpenAI API key first.".into());
            } else {
                self.key_message = None;
            }
            return;
        }
        match self.openai_keystore.save(&typed) {
            Ok(()) => {
                self.key_message = None;
                self.set_openai_key(Some(typed), KeyState::Keychain);
            }
            Err(e) => self.key_message = Some(format!("The Keychain refused the key: {e}")),
        }
    }

    pub fn remove_key(&mut self) {
        if self.provider == Provider::OpenAi {
            if let Err(e) = self.openai_keystore.delete() {
                self.key_message = Some(format!("Could not remove the key: {e}"));
                return;
            }
            self.key_message = None;
            self.refresh_openai_key(); // an environment key may still apply
            return;
        }
        if let Err(e) = self.keystore.delete() {
            self.key_message = Some(format!("Could not remove the key: {e}"));
            return;
        }
        self.key_message = None;
        self.refresh_key(); // an environment key may still apply
    }

    /// Ends the running turn: the agent stops at the next network read or approval wait.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = &self.run {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets the conversation. Not while a turn runs.
    pub fn clear(&mut self) {
        if self.run.is_some() {
            return;
        }
        if let Some(a) = &self.agent {
            lock(a).clear();
        }
        self.items.clear();
        self.usage = Usage::default();
    }

    /// Applies one event of the running turn to the conversation. `current` gives the text of
    /// a file as the IDE has it (open tab, designer, or disk) for the diff of a proposal.
    pub fn on_event(&mut self, ev: Event, current: &dyn Fn(&Path) -> Option<String>) {
        match ev {
            Event::Text(t) => match self.items.last_mut() {
                Some(Item::Assistant(s)) => s.push_str(&t),
                _ => self.items.push(Item::Assistant(t)),
            },
            Event::ToolCall { id, name, input } => self.items.push(Item::Tool(ToolRow {
                id,
                label: tool_label(&name, &input),
                state: ToolState::Running,
                output: String::new(),
            })),
            Event::NeedsApproval(p) => {
                let preview = preview_of(&p, current(&p.abs_path));
                self.items.push(Item::Card(Card {
                    id: p.id,
                    path: p.path,
                    abs: p.abs_path,
                    change: p.change,
                    state: CardState::Pending,
                    local: false,
                    preview,
                    auto: false,
                    undo: None,
                }));
            }
            Event::ToolResult {
                id,
                output,
                is_error,
                ..
            } => {
                for it in self.items.iter_mut().rev() {
                    if let Item::Tool(t) = it
                        && t.id == id
                    {
                        t.state = if is_error {
                            ToolState::Failed(first_line(&output))
                        } else {
                            ToolState::Done
                        };
                        t.output = output;
                        break;
                    }
                }
            }
            Event::Usage(u) => self.usage.add(&u),
            Event::Done(f) => {
                match f {
                    Finish::Cancelled => self.items.push(Item::Note("Stopped.".into())),
                    Finish::StepLimit => self.items.push(Item::Note(
                        "Stopped: the step limit for one message was reached. Say \"continue\" to go on."
                            .into(),
                    )),
                    Finish::Finished => {}
                }
                self.end_run();
            }
            Event::Error(e) => {
                if e == pascoda_ai::AiError::NotLoggedIn
                    && let CliState::Found(s) = &mut self.cli
                {
                    s.logged_in = Some(false);
                    self.agent = None;
                }
                if e == pascoda_ai::AiError::CodexNotLoggedIn
                    && let CliState::Found(s) = &mut self.codex
                {
                    s.logged_in = Some(false);
                    self.agent = None;
                    self.show_settings = true;
                }
                if matches!(e, pascoda_ai::AiError::Auth(_)) {
                    self.key_message = Some(e.to_string());
                    if self.provider == Provider::OpenAi {
                        self.openai_key = None;
                        self.openai_key_state = KeyState::Missing;
                        self.openai_edit = self.openai.clone();
                        // A local server that wants a key: open the settings to type it.
                        self.show_settings = self.ready();
                    } else if self.provider == Provider::Api {
                        self.api_key = None;
                        self.key = KeyState::Missing;
                        self.show_settings = true;
                    }
                    self.agent = None;
                }
                self.items.push(Item::Error(e.to_string()));
                self.end_run();
            }
        }
    }

    fn end_run(&mut self) {
        self.run = None;
        for it in &mut self.items {
            match it {
                Item::Card(c) if c.state == CardState::Pending && !c.local => {
                    c.state = CardState::Cancelled
                }
                Item::Tool(t) if t.state == ToolState::Running => t.state = ToolState::Done,
                _ => {}
            }
        }
    }

    fn card_mut(&mut self, idx: usize) -> Option<&mut Card> {
        match self.items.get_mut(idx) {
            Some(Item::Card(c)) => Some(c),
            _ => None,
        }
    }

    fn send_decision(&self, id: &str, approval: Approval) {
        if let Some(r) = &self.run {
            let _ = r.decisions.send(Decision {
                id: id.to_string(),
                approval,
            });
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(200).collect()
}

fn tool_label(name: &str, input: &Value) -> String {
    let path = input.get("path").and_then(Value::as_str).unwrap_or("");
    match name {
        "list_files" if path.is_empty() || path == "." => "List files".into(),
        "list_files" => format!("List {path}"),
        "read_file" => {
            let range = match (
                input.get("start_line").and_then(Value::as_u64),
                input.get("end_line").and_then(Value::as_u64),
            ) {
                (Some(a), Some(b)) => format!(" (lines {a}-{b})"),
                (Some(a), None) => format!(" (from line {a})"),
                _ => String::new(),
            };
            format!("Read {path}{range}")
        }
        "read_form" => format!("Read form of {path}"),
        "get_diagnostics" => "Get build errors".into(),
        "build" => "Build".into(),
        "edit_file" => format!("Edit {path}"),
        "create_file" => format!("Create {path}"),
        "glob" => format!(
            "Find {}",
            input.get("pattern").and_then(Value::as_str).unwrap_or("")
        ),
        "grep" => format!(
            "Search for {}",
            input.get("pattern").and_then(Value::as_str).unwrap_or("")
        ),
        "shell" => {
            let c = input.get("command").and_then(Value::as_str).unwrap_or("");
            // Codex runs `/bin/zsh -lc "..."`; show what is inside.
            let inner = c.split_once(" -lc ").map_or(c, |(_, rest)| {
                rest.trim().trim_matches(|ch| ch == '"' || ch == '\'')
            });
            let one_line: String = inner.chars().take(80).collect();
            format!("Run {one_line}")
        }
        other => other.to_string(),
    }
}

/// The diff a proposal would make against `current`, or why it cannot be applied.
pub fn preview_of(
    p: &Proposal,
    current: Option<String>,
) -> Result<(Vec<diff::Row>, (usize, usize)), String> {
    if p.change == Change::Build {
        return Ok((Vec::new(), (0, 0)));
    }
    let after = p.apply_to(current.as_deref())?;
    Ok(diff::unified(current.as_deref().unwrap_or(""), &after, 3))
}

// ---- what the agent sees of the IDE ----------------------------------------------------

/// A copy of the IDE state for the agent's worker thread, refreshed by the UI thread.
#[derive(Default)]
struct HostData {
    diagnostics: String,
    /// Open tabs' text by absolute path (unsaved text wins over the disk).
    texts: HashMap<PathBuf, String>,
    /// Open form designers' `.lfm` text by absolute path.
    forms: HashMap<PathBuf, String>,
}

struct AppHost {
    data: Arc<Mutex<HostData>>,
    build: Sender<BuildReq>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
}

impl Host for AppHost {
    fn diagnostics(&mut self) -> String {
        lock(&self.data).diagnostics.clone()
    }

    /// The UI thread runs the build (with its progress bar and error list); this waits.
    fn build(&mut self) -> String {
        let (tx, rx) = channel();
        if self.build.send(BuildReq(tx)).is_err() {
            return "The IDE is closing; no build was run.".into();
        }
        self.ctx.request_repaint();
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(out) => return out,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self.stop.load(Ordering::Relaxed) {
                        return "The user stopped the turn before the build finished.".into();
                    }
                }
                Err(_) => return "The build was abandoned.".into(),
            }
        }
    }

    fn read_form(&mut self, lfm: &Path) -> Option<String> {
        lock(&self.data).forms.get(lfm).cloned()
    }

    fn open_text(&mut self, path: &Path) -> Option<String> {
        let d = lock(&self.data);
        d.forms.get(path).or_else(|| d.texts.get(path)).cloned()
    }
}

/// The text of `abs` as the IDE has it: the open designer for a `.lfm`, an open tab, else the disk.
pub fn current_text(docs: &[Doc], designers: &[designer::Designer], abs: &Path) -> Option<String> {
    if let Some(d) = designers.iter().find(|d| d.lfm_path == abs) {
        return Some(d.doc.source().to_string());
    }
    if let Some(d) = docs.iter().find(|d| d.path == abs) {
        return Some(d.text.clone());
    }
    std::fs::read_to_string(abs).ok()
}

/// Build output as the model should read it.
fn build_report(result: &BuildResult) -> String {
    let mut s = String::from(if result.success {
        "Build succeeded.\n"
    } else {
        "Build failed.\n"
    });
    s.push_str(&result.raw_output);
    s
}

fn diagnostic_line(d: &Diagnostic) -> String {
    let sev = match d.severity {
        Severity::Fatal => "Fatal",
        Severity::Error => "Error",
        Severity::Warning => "Warning",
        Severity::Note => "Note",
        Severity::Hint => "Hint",
    };
    format!(
        "{}({},{}) {sev}: {}",
        d.file.display(),
        d.line,
        d.col,
        d.message
    )
}

/// What a reload from the disk found.
struct Reloaded {
    text: String,
    /// The unit that was brought in step with a reloaded form: path, text before and after.
    unit: Option<(PathBuf, String, String)>,
    note: Option<String>,
}

/// Where a position `idx` (in chars) of `old` goes when `old` becomes `new`: unchanged before the
/// edit, shifted after it, and at the start of the edit when it was inside the replaced part.
pub fn map_cursor(old: &str, new: &str, idx: usize) -> usize {
    let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let max_suffix = o.len().min(n.len()) - prefix;
    let suffix = o
        .iter()
        .rev()
        .zip(n.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    let mapped = if idx <= prefix {
        idx
    } else if idx >= o.len() - suffix {
        idx + n.len() - o.len()
    } else {
        prefix
    };
    mapped.min(n.len())
}

/// Puts `text` into a tab (`saved`: it is also what the disk has) and keeps the caret where it
/// was in the text.
pub(crate) fn set_doc_text(doc: &mut Doc, text: String, saved: bool, ctx: Option<&egui::Context>) {
    if let Some(ctx) = ctx
        && doc.text != text
        && let Some(mut state) = egui::TextEdit::load_state(ctx, doc.editor_id)
        && let Some(r) = state.cursor.char_range()
    {
        let (a, b) = (r.primary.index.0, r.secondary.index.0);
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(map_cursor(&doc.text, &text, b)),
            CCursor::new(map_cursor(&doc.text, &text, a)),
        )));
        state.store(ctx, doc.editor_id);
    }
    if saved {
        doc.saved = text.clone();
    }
    doc.text = text;
}

// ---- App glue ---------------------------------------------------------------------------

/// What a code block in an answer may be applied to.
#[derive(Clone, Debug)]
pub struct ActiveFile {
    pub name: String,
    pub text: String,
}

/// The open file a code block clearly replaces: it names the file in its info string, or it is a
/// whole unit/program/form whose header equals the open file's. Anything else is only copied or
/// inserted.
pub fn block_targets_file(lang: &str, code: &str, file: &ActiveFile) -> bool {
    let names_file = lang
        .split(|c: char| c.is_whitespace() || c == ':' || c == ',')
        .any(|t| t.eq_ignore_ascii_case(&file.name));
    let header = |s: &str| -> Option<String> {
        let first = s.lines().map(str::trim).find(|l| {
            !l.is_empty() && !l.starts_with("//") && !l.starts_with('{') && !l.starts_with("(*")
        })?;
        let lower = first.to_lowercase();
        let words: Vec<&str> = lower
            .split(|c: char| c.is_whitespace() || c == ';' || c == ':' || c == '(')
            .filter(|w| !w.is_empty())
            .collect();
        match words.as_slice() {
            [kw @ ("unit" | "program" | "library"), name, ..] => Some(format!("{kw} {name}")),
            [kw @ ("object" | "inherited"), name, class, ..] => {
                Some(format!("{kw} {name}: {class}"))
            }
            _ => None,
        }
    };
    let same_header = header(code).is_some() && header(code) == header(&file.text);
    !code.trim().is_empty() && (names_file || same_header)
}

impl App {
    /// The caret's selection as char indices, when there is one.
    fn selection_range(&self, ctx: &egui::Context) -> Option<(usize, usize)> {
        if self.form_showing() {
            return None;
        }
        let doc = self.docs.get(self.active)?;
        let r = egui::TextEdit::load_state(ctx, doc.editor_id)?
            .cursor
            .char_range()?;
        let (a, b) = (r.primary.index.0, r.secondary.index.0);
        (a != b).then_some((a.min(b), a.max(b)))
    }

    fn display_name(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    /// Everything the chips can attach right now.
    pub(crate) fn assistant_snapshot(&self, ctx: &egui::Context) -> Snapshot {
        let mut snap = Snapshot::default();
        if let Some(doc) = self.docs.get(self.active) {
            let name = self.display_name(&doc.path);
            let caret = self
                .cursor_index(ctx)
                .map_or(1, |i| line_col(&doc.text, i).0 as usize + 1);
            snap.file = Some((name.clone(), doc.text.clone(), caret));
            if let Some((a, b)) = self.selection_range(ctx) {
                let text: String = doc.text.chars().skip(a).take(b - a).collect();
                let first = line_col(&doc.text, a).0 as usize + 1;
                let last = first + text.matches('\n').count();
                snap.selection = Some((name, first, last, text));
            }
        }
        snap.errors = self
            .diagnostics
            .iter()
            .filter(|d| matches!(d.severity, Severity::Fatal | Severity::Error))
            .map(diagnostic_line)
            .collect();
        if self.assistant.chips.project {
            // The open tabs' text wins over the disk, so unsaved edits show in the overview.
            let mut overlay: Vec<(String, String)> = self
                .docs
                .iter()
                .filter_map(|d| Some((rel_name(&self.root, &d.path)?, d.text.clone())))
                .collect();
            overlay.extend(self.designers.iter().filter_map(|f| {
                Some((
                    rel_name(&self.root, &f.lfm_path)?,
                    f.doc.source().to_string(),
                ))
            }));
            snap.overview = self.assistant.project.text(&self.root, &overlay);
        }
        snap.tabs = self
            .docs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active)
            .map(|(_, d)| Tab {
                name: self.display_name(&d.path),
                unsaved: d.dirty().then(|| d.text.clone()),
            })
            .collect();
        snap
    }

    /// Refreshes what the agent's worker thread can see.
    pub(crate) fn refresh_host_data(&self) {
        let mut d = lock(&self.assistant.data);
        d.diagnostics = self
            .diagnostics
            .iter()
            .map(diagnostic_line)
            .collect::<Vec<_>>()
            .join("\n");
        d.texts = self
            .docs
            .iter()
            .map(|x| (x.path.clone(), x.text.clone()))
            .collect();
        d.forms = self
            .designers
            .iter()
            .map(|f| (f.lfm_path.clone(), f.doc.source().to_string()))
            .collect();
    }

    /// Sends the typed message. Returns false when nothing was sent.
    pub(crate) fn assistant_send(&mut self, ctx: &egui::Context) -> bool {
        let user = self.assistant.input.trim().to_string();
        if user.is_empty() || self.assistant.streaming() || !self.assistant.ready() {
            return false;
        }
        if matches!(
            self.assistant.provider,
            Provider::Api | Provider::ClaudeCode
        ) && self.assistant.model.trim().is_empty()
        {
            self.assistant.model = pascoda_ai::DEFAULT_MODEL.to_string();
        }
        self.assistant.remember_model();
        if self.assistant.provider == Provider::OpenAi && self.assistant.openai.model.is_empty() {
            self.assistant.items.push(Item::Error(
                "No model is set. Type the model name your endpoint serves into the Model field below the message box, then send again.".into(),
            ));
            return false;
        }
        // Make sure a rescan is under way (the answer of the last one is used now).
        self.assistant
            .project
            .poll(&self.root, Some(ctx), std::time::Duration::from_secs(1));
        let snap = self.assistant_snapshot(ctx);
        let (sent, labels) = compose(self.assistant.chips, &snap, &user);
        self.refresh_host_data();
        match self.assistant_start(ctx, sent.clone()) {
            Ok(()) => {
                self.assistant.items.push(Item::User {
                    text: user,
                    sent,
                    chips: labels,
                });
                self.assistant.input.clear();
                true
            }
            Err(e) => {
                self.assistant.items.push(Item::Error(e));
                false
            }
        }
    }

    /// Starts the worker thread for one turn.
    fn assistant_start(&mut self, ctx: &egui::Context, text: String) -> Result<(), String> {
        let a = &mut self.assistant;
        let root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        if a.agent.is_some() && a.agent_root != root {
            a.agent = None;
            a.items
                .push(Item::Note("New project folder: a new conversation.".into()));
        }
        if a.agent.is_none() {
            let sandbox = Sandbox::new(&root).map_err(|e| format!("project folder: {e}"))?;
            let host = AppHost {
                data: a.data.clone(),
                build: a.build_tx.clone(),
                ctx: ctx.clone(),
                stop: a.stop.clone(),
            };
            let backend: Box<dyn Backend> = match a.provider {
                Provider::Api => {
                    let key = a.api_key.clone().ok_or("No API key.")?;
                    let mut cfg = Config::new(key);
                    cfg.model = a.model.clone();
                    if let Some(u) = &a.base_url {
                        cfg.base_url = u.clone();
                    }
                    Box::new(Agent::new(Client::new(cfg), Box::new(host), sandbox))
                }
                Provider::OpenAi => {
                    let cfg = OpenAiConfig::new(
                        a.openai_key.clone(),
                        &a.openai.base_url,
                        &a.openai.model,
                    );
                    Box::new(OpenAi::new(cfg, Box::new(host), sandbox))
                }
                Provider::Codex => {
                    let bin = match (&a.codex_bin, &a.codex) {
                        (Some(b), _) => b.clone(),
                        (None, CliState::Found(s)) => s.binary.clone(),
                        _ => return Err("Codex was not found.".into()),
                    };
                    let mut cfg = codex::Config::new(bin);
                    cfg.model = a.codex_model.clone();
                    Box::new(Codex::new(cfg, Box::new(host), sandbox))
                }
                Provider::ClaudeCode => {
                    let bin = match (&a.cli_bin, &a.cli) {
                        (Some(b), _) => b.clone(),
                        (None, CliState::Found(s)) => s.binary.clone(),
                        _ => return Err("Claude Code was not found.".into()),
                    };
                    let mut cfg = claude_code::Config::new(bin);
                    cfg.model = a.model.clone();
                    Box::new(ClaudeCode::new(cfg, Box::new(host), sandbox))
                }
            };
            a.backend_writes = backend.writes_files_itself();
            a.agent = Some(Arc::new(Mutex::new(backend)));
            a.agent_root = root;
        }
        let agent = a.agent.clone().ok_or("No agent.")?;
        a.stop.store(false, Ordering::Relaxed);
        let cancel = lock(&agent).cancel_handle();
        let model = a.current_model().to_string();
        let (raw_tx, raw_rx) = channel::<Event>();
        let (ui_tx, ui_rx) = channel::<Event>();
        let (dec_tx, dec_rx) = channel::<Decision>();
        // The agent sends plain channel events; this relay wakes the UI for each of them.
        let wake = ctx.clone();
        std::thread::spawn(move || {
            for ev in raw_rx {
                if ui_tx.send(ev).is_err() {
                    break;
                }
                wake.request_repaint();
            }
        });
        std::thread::spawn(move || {
            let mut agent = lock(&agent);
            agent.set_model(&model);
            agent.run_turn(&text, &raw_tx, &dec_rx);
        });
        a.run = Some(Run {
            events: ui_rx,
            decisions: dec_tx,
            cancel,
        });
        Ok(())
    }

    /// Called every frame: moves events from the worker into the conversation and serves
    /// build requests.
    pub(crate) fn poll_assistant(&mut self, ctx: &egui::Context) {
        self.assistant.ctx = Some(ctx.clone());
        if self.assistant.open {
            self.assistant
                .project
                .poll(&self.root, Some(ctx), std::time::Duration::from_secs(3));
        }
        self.assistant.poll_cli();
        if self.assistant.open
            && self.assistant.provider == Provider::ClaudeCode
            && self.assistant.cli == CliState::Unchecked
        {
            self.assistant.refresh_cli(ctx);
        }
        if self.assistant.open
            && self.assistant.provider == Provider::Codex
            && self.assistant.codex == CliState::Unchecked
        {
            self.assistant.refresh_codex(ctx);
        }
        if self.assistant.run.is_some() {
            self.refresh_host_data();
            let mut events = Vec::new();
            let mut gone = false;
            if let Some(r) = &self.assistant.run {
                loop {
                    match r.events.try_recv() {
                        Ok(e) => events.push(e),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            gone = true;
                            break;
                        }
                    }
                }
            }
            for ev in events {
                // The backend's own tool has written (or failed to write) an approved file.
                if let Event::ToolResult {
                    id,
                    output,
                    is_error,
                    ..
                } = &ev
                {
                    self.assistant_write_result(id, output, *is_error);
                }
                let asks = matches!(&ev, Event::NeedsApproval(_));
                let (docs, designers) = (&self.docs, &self.designers);
                self.assistant
                    .on_event(ev, &|p| current_text(docs, designers, p));
                if asks {
                    self.assistant_auto_decide();
                }
            }
            if gone && self.assistant.run.is_some() {
                self.assistant.items.push(Item::Error(
                    "The assistant's worker ended unexpectedly.".into(),
                ));
                self.assistant.end_run();
            }
            if self.assistant.run.is_none() {
                self.settle_writing_cards();
            }
        }
        while let Ok(BuildReq(reply)) = self.assistant.build_rx.try_recv() {
            if self.building.is_some() || self.session.is_some() || self.docs.is_empty() {
                let _ = reply.send(
                    "Cannot build now: a build or debug session is already running, or no file is open."
                        .into(),
                );
            } else {
                self.assistant_build_reply = Some(reply);
                self.start_build(ctx, false, false);
            }
        }
    }

    /// Called when a build ends: the waiting agent gets the result.
    pub(crate) fn assistant_build_done(&mut self, result: &BuildResult) {
        if let Some(reply) = self.assistant_build_reply.take() {
            self.refresh_host_data();
            let _ = reply.send(build_report(result));
        }
    }

    /// The user's answer to a card (Apply/Allow or Reject/Deny).
    pub(crate) fn assistant_decide(&mut self, idx: usize, approve: bool) {
        self.decide(idx, approve, false);
    }

    /// "Allow edits this session": answers the newest waiting edit or new-file card at once.
    /// Builds are never answered here.
    fn assistant_auto_decide(&mut self) {
        if !self.assistant.auto_edits {
            return;
        }
        let idx = self.assistant.items.iter().rposition(|it| {
            matches!(it, Item::Card(c) if c.state == CardState::Pending
                && !c.local && c.change != Change::Build)
        });
        if let Some(i) = idx {
            self.decide(i, true, true);
        }
    }

    /// Is `abs` inside the project folder? (The agents' sandbox already refuses anything else;
    /// this is the app's own check, so an automatic apply can never leave the folder.)
    fn inside_project(&self, abs: &Path) -> bool {
        let root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let clean = !abs
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        clean && (abs.starts_with(&root) || abs.starts_with(&self.root))
    }

    /// The file has changes the disk does not have: an edited tab, or a form edited in the
    /// designer.
    pub(crate) fn unsaved_in(&self, abs: &Path) -> bool {
        self.docs.iter().any(|d| d.path == abs && d.dirty())
            || self
                .designers
                .iter()
                .any(|f| f.lfm_path == abs && f.dirty())
    }

    /// Writes the unsaved state of `abs` (its tab and, for a unit, its form; or the designer of
    /// a `.lfm`) to the disk.
    pub(crate) fn save_target(&mut self, abs: &Path) {
        if let Some(i) = self.docs.iter().position(|d| d.path == abs) {
            self.save_doc(i);
        }
        if let Some(f) = self.designers.iter_mut().find(|f| f.lfm_path == abs)
            && f.save().is_ok()
            && let Some(doc) = self.docs.iter_mut().find(|d| d.path == abs)
            && !doc.dirty()
            && let Ok(text) = std::fs::read_to_string(abs)
        {
            doc.saved = text.clone();
            doc.text = text;
        }
    }

    /// Decides a card. `auto`: the switch decided, not a click.
    fn decide(&mut self, idx: usize, approve: bool, auto: bool) {
        let Some(card) = self
            .assistant
            .card_mut(idx)
            .filter(|c| c.state == CardState::Pending)
        else {
            return;
        };
        let (id, local, abs, change) = (
            card.id.clone(),
            card.local,
            card.abs.clone(),
            card.change.clone(),
        );
        if !approve {
            card.state = CardState::Rejected;
            self.assistant.send_decision(&id, Approval::Rejected(None));
            return;
        }
        let fail = |app: &mut App, e: String| {
            if let Some(c) = app.assistant.card_mut(idx) {
                c.state = CardState::Failed(e.clone());
            }
            if !local {
                app.assistant.send_decision(
                    &id,
                    Approval::Rejected(Some(format!("The change could not be applied: {e}"))),
                );
            }
        };
        if change == Change::Build {
            self.refresh_host_data();
            if let Some(c) = self.assistant.card_mut(idx) {
                c.state = CardState::Applied;
            }
            if !local {
                self.assistant.send_decision(&id, Approval::Approved);
            }
            return;
        }
        if !self.inside_project(&abs) {
            return fail(
                self,
                format!("{} is outside the project folder.", abs.display()),
            );
        }
        if self.assistant.backend_writes && !local {
            // The backend's own tool writes the file, and it checks the edit against the disk:
            // the file must be saved first. A click on Apply never saves silently (the card
            // offers "Save and apply"); the switch does.
            if self.unsaved_in(&abs) {
                if !auto {
                    return;
                }
                self.save_target(&abs);
                if self.unsaved_in(&abs) {
                    return fail(
                        self,
                        format!(
                            "Could not save {} before the edit.",
                            self.display_name(&abs)
                        ),
                    );
                }
            }
            let before = std::fs::read_to_string(&abs).ok();
            if let Some(c) = self.assistant.card_mut(idx) {
                c.state = CardState::Writing;
                c.auto = auto;
                c.undo = Some(Box::new(Undo {
                    before,
                    after: String::new(),
                    unit: None,
                    on_disk: true,
                }));
            }
            self.refresh_host_data();
            self.assistant.send_decision(&id, Approval::Approved);
            return;
        }
        let applied = self.apply_recorded(&abs, &change);
        // The agent's next tool call must see the new text.
        self.refresh_host_data();
        match applied {
            Ok((note, undo)) => {
                if let Some(c) = self.assistant.card_mut(idx) {
                    c.state = CardState::Applied;
                    c.auto = auto;
                    c.undo = auto.then(|| Box::new(undo));
                }
                if let Some(n) = note {
                    self.assistant.items.push(Item::Note(n));
                }
                if !local {
                    self.assistant.send_decision(&id, Approval::Approved);
                }
            }
            Err(e) => fail(self, e),
        }
    }

    /// The first "Save and apply" step: saves the card's file, then applies as a click would.
    pub(crate) fn assistant_save_and_apply(&mut self, idx: usize) {
        let Some(abs) = self
            .assistant
            .card_mut(idx)
            .filter(|c| c.state == CardState::Pending)
            .map(|c| c.abs.clone())
        else {
            return;
        };
        if !self.inside_project(&abs) {
            return;
        }
        self.save_target(&abs);
        self.decide(idx, true, false);
    }

    /// The backend's tool answered for a card in `Writing`: reload what it wrote.
    fn assistant_write_result(&mut self, id: &str, output: &str, is_error: bool) {
        let Some(idx) = self.assistant.items.iter().position(
            |it| matches!(it, Item::Card(c) if c.id == id && c.state == CardState::Writing),
        ) else {
            return;
        };
        if is_error {
            if let Some(c) = self.assistant.card_mut(idx) {
                c.state = CardState::Failed(format!("The write failed: {}", first_line(output)));
                c.undo = None;
            }
            return;
        }
        self.finish_write(idx);
    }

    /// Reads the file the backend wrote into the IDE and marks the card applied (or failed).
    fn finish_write(&mut self, idx: usize) {
        let Some((abs, create)) = self
            .assistant
            .card_mut(idx)
            .map(|c| (c.abs.clone(), matches!(c.change, Change::Create { .. })))
        else {
            return;
        };
        let res = self.reload_from_disk(&abs, create);
        self.refresh_host_data();
        let note = match &res {
            Ok(r) => r.note.clone(),
            Err(_) => None,
        };
        if let Some(c) = self.assistant.card_mut(idx) {
            match res {
                Ok(r) => {
                    c.state = CardState::Applied;
                    if let Some(u) = &mut c.undo {
                        u.after = r.text;
                        u.unit = r.unit;
                    }
                    if !c.auto {
                        c.undo = None;
                    }
                }
                Err(e) => {
                    c.state = CardState::Failed(format!("Written, but not reloaded: {e}"));
                    c.undo = None;
                }
            }
        }
        if let Some(n) = note {
            self.assistant.items.push(Item::Note(n));
        }
    }

    /// The turn is over: a card still in `Writing` never got its tool result. Look at the file.
    fn settle_writing_cards(&mut self) {
        let writing: Vec<usize> = self
            .assistant
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                matches!(it, Item::Card(c) if c.state == CardState::Writing).then_some(i)
            })
            .collect();
        for i in writing {
            let changed = match self.assistant.card_mut(i) {
                Some(c) => {
                    let now = std::fs::read_to_string(&c.abs).ok();
                    c.undo.as_ref().is_some_and(|u| u.before != now)
                }
                None => false,
            };
            if changed {
                self.finish_write(i);
            } else if let Some(c) = self.assistant.card_mut(i) {
                c.state = CardState::Failed("The turn ended before the file was written.".into());
                c.undo = None;
            }
        }
    }

    /// Puts the file as the disk has it into the open tab and form designer (after the backend's
    /// own tool wrote it), or opens it when it is new. Unopened files need nothing.
    fn reload_from_disk(&mut self, abs: &Path, create: bool) -> Result<Reloaded, String> {
        let text = std::fs::read_to_string(abs).map_err(|e| e.to_string())?;
        let mut out = Reloaded {
            text: text.clone(),
            unit: None,
            note: None,
        };
        let ctx = self.assistant.ctx.clone();
        if let Some(di) = self.designers.iter().position(|d| d.lfm_path == abs) {
            let form = &mut self.designers[di];
            if form.doc.source() != text {
                form.replace_source(&text).map_err(|e| e.to_string())?;
            }
            form.mark_saved();
            let pas = form.pas_path.clone();
            if let Some(doc) = self.docs.iter_mut().find(|d| d.path == pas) {
                let before = doc.text.clone();
                form.sync_unit(&mut doc.text);
                if doc.text != before {
                    out.unit = Some((pas, before, doc.text.clone()));
                }
            }
            out.note = form
                .sync_error
                .as_ref()
                .map(|e| format!("The unit could not be brought in step with the form: {e}"));
        }
        if let Some(doc) = self.docs.iter_mut().find(|d| d.path == abs) {
            set_doc_text(doc, text.clone(), true, ctx.as_ref());
        } else if create {
            self.open_path(abs.to_path_buf());
        }
        Ok(out)
    }

    /// Takes an auto-applied edit back, if the file still has the text the edit left.
    pub(crate) fn assistant_revert(&mut self, idx: usize) {
        let Some((abs, undo)) = self
            .assistant
            .card_mut(idx)
            .filter(|c| c.state == CardState::Applied && c.auto)
            .and_then(|c| c.undo.clone().map(|u| (c.abs.clone(), u)))
        else {
            return;
        };
        let fail = |app: &mut App, why: String| {
            app.assistant
                .items
                .push(Item::Note(format!("Could not revert: {why}")));
        };
        if self.revert_text(&abs).as_deref() != Some(undo.after.as_str())
            || !self.inside_project(&abs)
        {
            return fail(self, "the file has changed since the edit.".into());
        }
        let ctx = self.assistant.ctx.clone();
        let mut note = None;
        match &undo.before {
            None => {
                // The edit created the file: remove it and close its tab.
                if let Err(e) = std::fs::remove_file(&abs) {
                    return fail(self, e.to_string());
                }
                if let Some(i) = self.docs.iter().position(|d| d.path == abs) {
                    self.close_doc(i);
                }
            }
            Some(before) => {
                if let Some(di) = self.designers.iter().position(|d| d.lfm_path == abs) {
                    let form = &mut self.designers[di];
                    if let Err(e) = form.replace_source(before) {
                        return fail(self, e.to_string());
                    }
                    if undo.on_disk {
                        if let Err(e) = std::fs::write(&abs, before) {
                            return fail(self, e.to_string());
                        }
                        form.mark_saved();
                    }
                    if let Some((pas, ub, ua)) = &undo.unit {
                        match self.docs.iter_mut().find(|d| d.path == *pas) {
                            Some(doc) if doc.text == *ua => {
                                set_doc_text(doc, ub.clone(), false, ctx.as_ref())
                            }
                            Some(_) => {
                                note = Some(format!(
                                    "{} was edited since; it was not restored.",
                                    pas.file_name().map_or_else(String::new, |n| n
                                        .to_string_lossy()
                                        .into_owned())
                                ))
                            }
                            None => {}
                        }
                    }
                    if let Some(doc) = self.docs.iter_mut().find(|d| d.path == abs) {
                        set_doc_text(doc, before.clone(), undo.on_disk, ctx.as_ref());
                    }
                } else if let Some(doc) = self.docs.iter_mut().find(|d| d.path == abs) {
                    if undo.on_disk
                        && let Err(e) = std::fs::write(&abs, before)
                    {
                        return fail(self, e.to_string());
                    }
                    set_doc_text(doc, before.clone(), undo.on_disk, ctx.as_ref());
                } else if let Err(e) = std::fs::write(&abs, before) {
                    return fail(self, e.to_string());
                }
            }
        }
        if let Some(c) = self.assistant.card_mut(idx) {
            c.state = CardState::Reverted;
        }
        if let Some(n) = note {
            self.assistant.items.push(Item::Note(n));
        }
        self.refresh_host_data();
    }

    /// The text of `abs` as a revert compares it with the text the edit left.
    pub(crate) fn revert_text(&self, abs: &Path) -> Option<String> {
        current_text(&self.docs, &self.designers, abs)
    }

    /// Applies an approved edit or new file. Open files change in memory (so undo, save and the
    /// language server work as for typing); a form open in the designer is reloaded from the
    /// edited `.lfm` text and the unit brought in step; other files are written. Returns a note
    /// to show when the form and unit could not be kept in step.
    #[cfg(test)]
    pub(crate) fn apply_change(
        &mut self,
        abs: &Path,
        change: &Change,
    ) -> Result<Option<String>, String> {
        self.apply_recorded(abs, change).map(|(note, _)| note)
    }

    /// `apply_change`, also returning what a revert needs.
    fn apply_recorded(
        &mut self,
        abs: &Path,
        change: &Change,
    ) -> Result<(Option<String>, Undo), String> {
        match change {
            Change::Build => Ok((
                None,
                Undo {
                    before: None,
                    after: String::new(),
                    unit: None,
                    on_disk: false,
                },
            )),
            Change::Create { content } => {
                if abs.exists() || self.docs.iter().any(|d| d.path == abs) {
                    return Err("the file already exists".into());
                }
                if let Some(dir) = abs.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(abs, content).map_err(|e| e.to_string())?;
                self.open_path(abs.to_path_buf());
                Ok((
                    None,
                    Undo {
                        before: None,
                        after: content.clone(),
                        unit: None,
                        on_disk: true,
                    },
                ))
            }
            Change::Edit { old_text, new_text } => {
                let edit = |cur: &str| -> Result<String, String> {
                    Proposal {
                        id: String::new(),
                        path: String::new(),
                        abs_path: abs.to_path_buf(),
                        change: Change::Edit {
                            old_text: old_text.clone(),
                            new_text: new_text.clone(),
                        },
                    }
                    .apply_to(Some(cur))
                };
                // A form open in the designer: the designer's text is what the model saw.
                if let Some(di) = self.designers.iter().position(|d| d.lfm_path == abs) {
                    let before = self.designers[di].doc.source().to_string();
                    let after = edit(&before)?;
                    let form = &mut self.designers[di];
                    form.replace_source(&after).map_err(|e| e.to_string())?;
                    let pas = form.pas_path.clone();
                    let mut unit = None;
                    if let Some(doc) = self.docs.iter_mut().find(|d| d.path == pas) {
                        let ub = doc.text.clone();
                        form.sync_unit(&mut doc.text);
                        if doc.text != ub {
                            unit = Some((pas, ub, doc.text.clone()));
                        }
                    }
                    let note = form.sync_error.as_ref().map(|e| {
                        format!("The unit could not be brought in step with the form: {e}")
                    });
                    if let Some(doc) = self.docs.iter_mut().find(|d| d.path == abs) {
                        doc.text = after.clone();
                    }
                    return Ok((
                        note,
                        Undo {
                            before: Some(before),
                            after,
                            unit,
                            on_disk: false,
                        },
                    ));
                }
                if let Some(doc) = self.docs.iter_mut().find(|d| d.path == abs) {
                    let before = doc.text.clone();
                    doc.text = edit(&before)?;
                    return Ok((
                        None,
                        Undo {
                            before: Some(before),
                            after: doc.text.clone(),
                            unit: None,
                            on_disk: false,
                        },
                    ));
                }
                let cur = std::fs::read_to_string(abs).map_err(|e| e.to_string())?;
                let after = edit(&cur)?;
                std::fs::write(abs, &after).map_err(|e| e.to_string())?;
                Ok((
                    None,
                    Undo {
                        before: Some(cur),
                        after,
                        unit: None,
                        on_disk: true,
                    },
                ))
            }
        }
    }

    /// Puts a card for replacing the whole open file with `code` in the conversation.
    pub(crate) fn assistant_propose_block(&mut self, code: &str) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let old = doc.text.clone();
        let mut new = code.trim_end().to_string();
        if old.ends_with('\n') {
            new.push('\n');
        }
        let change = Change::Edit {
            old_text: old.clone(),
            new_text: new,
        };
        let p = Proposal {
            id: String::new(),
            path: self.display_name(&doc.path),
            abs_path: doc.path.clone(),
            change,
        };
        let preview = preview_of(&p, Some(old));
        self.assistant.items.push(Item::Card(Card {
            id: p.id,
            path: p.path,
            abs: p.abs_path,
            change: p.change,
            state: CardState::Pending,
            local: true,
            preview,
            auto: false,
            undo: None,
        }));
    }

    /// Inserts `text` at the caret of the active file.
    pub(crate) fn insert_at_cursor(&mut self, ctx: &egui::Context, text: &str) {
        let Some(at) = self.cursor_index(ctx) else {
            return;
        };
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        let byte = doc
            .text
            .char_indices()
            .nth(at)
            .map_or(doc.text.len(), |(b, _)| b);
        doc.text.insert_str(byte, text);
        let id = doc.editor_id;
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(
                at + text.chars().count(),
            ))));
        state.store(ctx, id);
    }

    pub(crate) fn toggle_assistant(&mut self) {
        self.assistant.open = !self.assistant.open;
        if self.assistant.open && self.assistant.provider_file.is_none() {
            // First time: the remembered provider.
            self.assistant.provider_file = Provider::file();
            if let Some(f) = &self.assistant.provider_file {
                self.assistant.provider = Provider::load(f);
            }
        }
        if self.assistant.open && self.assistant.model_file.is_none() {
            self.assistant.model_file = Assistant::claude_model_file();
            if let Some(f) = self.assistant.model_file.clone() {
                self.assistant.load_claude_models(&f);
            }
        }
        if self.assistant.open && self.assistant.codex_model_file.is_none() {
            self.assistant.codex_model_file = Assistant::codex_model_path();
            if let Some(f) = &self.assistant.codex_model_file {
                self.assistant.codex_model = std::fs::read_to_string(f)
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default();
            }
        }
        if self.assistant.open && self.assistant.openai_file.is_none() {
            self.assistant.openai_file = OpenAiSettings::file();
            if let Some(f) = &self.assistant.openai_file {
                self.assistant.openai = OpenAiSettings::load(f);
                self.assistant.openai_edit = self.assistant.openai.clone();
            }
        }
        if self.assistant.open
            && self.assistant.provider == Provider::Api
            && self.assistant.key == KeyState::Unchecked
        {
            self.assistant.refresh_key();
        }
        if self.assistant.open
            && self.assistant.provider == Provider::OpenAi
            && self.assistant.openai_key_state == KeyState::Unchecked
        {
            self.assistant.refresh_openai_key();
        }
        if let Some(m) = &self.menu {
            m.assistant.set_checked(self.assistant.open);
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{KEYCHAIN, mock as mock_server, text_reply};
