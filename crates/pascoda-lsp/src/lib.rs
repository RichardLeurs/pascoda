//! Minimal Language Server Protocol client for `pasls` (pascal-language-server).
//!
//! Synchronous like `pascoda-dap`: requests block until answered. Run it on its own thread.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// Where `pasls` and the sources it needs live.
#[derive(Debug, Clone)]
pub struct Config {
    pub pasls: PathBuf,
    pub fpc: PathBuf,
    /// FPC source tree (`rtl`, `packages`), so the server can read standard units.
    pub fpc_src: PathBuf,
    /// Lazarus source tree, required by the server's CodeTools backend.
    pub lazarus_src: PathBuf,
}

impl Config {
    /// Looks for `vendor/pasls/dist/aarch64-darwin/pasls` in `start` and its parents, then in
    /// `$PASCODA_HOME`, then in the app bundle's `Resources` folder, and (debug builds only) in
    /// the source tree this crate was built from.
    pub fn find(start: &Path) -> Option<Self> {
        let mut dirs: Vec<PathBuf> = start.ancestors().map(Path::to_path_buf).collect();
        dirs.extend(std::env::var_os("PASCODA_HOME").map(PathBuf::from));
        if let Ok(exe) = std::env::current_exe()
            && let Some(contents) = exe.parent().and_then(Path::parent)
        {
            dirs.push(contents.join("Resources"));
        }
        if cfg!(debug_assertions) {
            dirs.extend(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .map(Path::to_path_buf),
            );
        }
        // `pasls` needs the compiler's path (`PP`); wherever fpc is installed, not only Homebrew.
        let fpc = pascoda_fpc::locate_fpc()?;
        Self::find_in(dirs.iter().map(PathBuf::as_path), &fpc)
    }

    fn find_in<'a>(dirs: impl Iterator<Item = &'a Path>, fpc: &Path) -> Option<Self> {
        dirs.into_iter().find_map(|dir| {
            let vendor = dir.join("vendor");
            let pasls = vendor.join("pasls/dist/aarch64-darwin/pasls");
            (pasls.exists() && fpc.exists()).then(|| Self {
                pasls,
                fpc: fpc.to_path_buf(),
                fpc_src: vendor.join("fpcsrc"),
                lazarus_src: vendor.join("lazarus"),
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: String,
}

/// A 0-based position in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    id: i64,
    versions: std::collections::HashMap<PathBuf, i64>,
    /// Last text sent per file, to convert columns between chars and UTF-16 units.
    texts: std::collections::HashMap<PathBuf, String>,
    encoding: Encoding,
    /// Latest `publishDiagnostics` per file.
    diagnostics: std::collections::HashMap<PathBuf, Vec<Diagnostic>>,
}

/// A problem the server reported; positions are 0-based, columns in chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    /// LSP severity: 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
}

/// How a server counts the `character` of a position. The spec says UTF-16 code units; pasls
/// (measured with an emoji and an accent before the word) counts UTF-8 bytes instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    /// Wire column for `chars` chars into `line`.
    pub fn to_wire(self, line: &str, chars: u32) -> u32 {
        match self {
            Self::Utf16 => utf16_col(line, chars),
            Self::Utf8 => line
                .chars()
                .take(chars as usize)
                .map(|c| c.len_utf8() as u32)
                .sum(),
        }
    }

    /// Chars into `line` for the wire column `units` (inside a char rounds up past it).
    pub fn from_wire(self, line: &str, units: u32) -> u32 {
        match self {
            Self::Utf16 => char_col(line, units),
            Self::Utf8 => {
                let mut used = 0;
                for (n, c) in line.chars().enumerate() {
                    if used >= units {
                        return n as u32;
                    }
                    used += c.len_utf8() as u32;
                }
                line.chars().count() as u32
            }
        }
    }
}

/// UTF-16 code units in the first `chars` chars of `line`.
pub fn utf16_col(line: &str, chars: u32) -> u32 {
    line.chars()
        .take(chars as usize)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// Chars covered by the first `units` UTF-16 code units of `line` (a position inside a
/// surrogate pair rounds up to the end of that char).
pub fn char_col(line: &str, units: u32) -> u32 {
    let mut used = 0;
    for (n, c) in line.chars().enumerate() {
        if used >= units {
            return n as u32;
        }
        used += c.len_utf16() as u32;
    }
    line.chars().count() as u32
}

fn nth_line(text: &str, line: u32) -> &str {
    text.split('\n')
        .nth(line as usize)
        .unwrap_or_default()
        .trim_end_matches('\r')
}

impl Client {
    /// Starts the server for the workspace at `root` and completes the handshake.
    pub fn start(config: &Config, root: &Path) -> Result<Self, String> {
        let mut child = Command::new(&config.pasls)
            .env("FPCDIR", &config.fpc_src)
            .env("LAZARUSDIR", &config.lazarus_src)
            .env("PP", &config.fpc)
            .env("FPCTARGET", "darwin")
            .env("FPCTARGETCPU", "aarch64")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start pasls: {e}"))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, messages) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(msg) = read_message(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            messages,
            id: 0,
            versions: Default::default(),
            texts: Default::default(),
            encoding: Encoding::Utf8,
            diagnostics: Default::default(),
        };
        // pasls crashes in `initialize` without `clientInfo`.
        client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": {"name": "pascoda", "version": env!("CARGO_PKG_VERSION")},
                "rootUri": uri(root),
                "workspaceFolders": [{"uri": uri(root), "name": "workspace"}],
                "capabilities": {"textDocument": {"completion": {"completionItem": {"snippetSupport": false}}}},
                "initializationOptions": {"fpcOptions": ["-Mobjfpc"]},
            }),
        )?;
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    fn write(&mut self, body: Value) -> Result<(), String> {
        let body = body.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
            .and_then(|_| self.stdin.flush())
            .map_err(|e| e.to_string())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.id += 1;
        let id = self.id;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self
                .messages
                .recv_timeout(left)
                .map_err(|_| format!("no answer to {method}"))?;
            if msg.get("method").is_some() {
                self.handle_server_message(&msg)?;
            } else if msg["id"] == id {
                return match msg.get("error") {
                    Some(e) => Err(format!(
                        "{method}: {}",
                        e["message"].as_str().unwrap_or("error")
                    )),
                    None => Ok(msg["result"].clone()),
                };
            }
        }
    }

    /// Notifications are recorded; requests from the server (progress, registration) just
    /// get an empty answer.
    fn handle_server_message(&mut self, msg: &Value) -> Result<(), String> {
        if let Some(req_id) = msg.get("id") {
            return self.write(json!({"jsonrpc": "2.0", "id": req_id, "result": null}));
        }
        if msg["method"] == "textDocument/publishDiagnostics" {
            let params = &msg["params"];
            let path = path_of(params["uri"].as_str().unwrap_or_default());
            let text = self.texts.get(&path).cloned().unwrap_or_default();
            let encoding = self.encoding;
            let list = params["diagnostics"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| parse_diagnostic(d, &text, encoding))
                .collect();
            self.diagnostics.insert(path, list);
        }
        Ok(())
    }

    /// Diagnostics the server has published for `path`, waiting up to `wait` for news.
    pub fn diagnostics(&mut self, path: &Path, wait: Duration) -> Vec<Diagnostic> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.messages.recv_timeout(left) {
                Ok(msg) if msg.get("method").is_some() => {
                    let _ = self.handle_server_message(&msg);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.diagnostics.get(path).cloned().unwrap_or_default()
    }

    /// Tells the server the full current text of `path`. Opens it on first use.
    pub fn sync(&mut self, path: &Path, text: &str) -> Result<(), String> {
        self.texts.insert(path.to_path_buf(), text.to_string());
        let version = self.versions.entry(path.to_path_buf()).or_insert(0);
        *version += 1;
        let version = *version;
        if version == 1 {
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": uri(path), "languageId": "pascal", "version": 1, "text": text}}),
            )
        } else {
            self.notify(
                "textDocument/didChange",
                json!({"textDocument": {"uri": uri(path), "version": version}, "contentChanges": [{"text": text}]}),
            )
        }
    }

    /// Request parameters for a position given as (line, column in chars).
    fn at(&self, path: &Path, line: u32, col: u32) -> Value {
        let text = self.texts.get(path).map(String::as_str).unwrap_or_default();
        let col = self.encoding.to_wire(nth_line(text, line), col);
        json!({"textDocument": {"uri": uri(path)}, "position": {"line": line, "character": col}})
    }

    pub fn completion(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
    ) -> Result<Vec<CompletionItem>, String> {
        let result = self.request("textDocument/completion", self.at(path, line, col))?;
        let items = result.get("items").unwrap_or(&result);
        Ok(items
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| CompletionItem {
                label: i["label"].as_str().unwrap_or_default().to_string(),
                detail: i["detail"].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    /// Hover text (markdown with the code fence removed), if the server has any.
    pub fn hover(&mut self, path: &Path, line: u32, col: u32) -> Result<Option<String>, String> {
        let result = self.request("textDocument/hover", self.at(path, line, col))?;
        let contents = &result["contents"];
        let text = contents["value"].as_str().or_else(|| contents.as_str());
        Ok(text.map(strip_fence))
    }

    pub fn definition(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
    ) -> Result<Option<Location>, String> {
        let result = self.request("textDocument/definition", self.at(path, line, col))?;
        let loc = if result.is_array() {
            &result[0]
        } else {
            &result
        };
        Ok(loc["uri"].as_str().map(|u| {
            let path = path_of(u);
            let line = loc["range"]["start"]["line"].as_u64().unwrap_or(0) as u32;
            let units = loc["range"]["start"]["character"].as_u64().unwrap_or(0) as u32;
            let col = self.char_col_in(&path, line, units);
            Location { path, line, col }
        }))
    }

    /// Converts a UTF-16 column in `path` to chars, using the synced text or else the file.
    fn char_col_in(&self, path: &Path, line: u32, units: u32) -> u32 {
        match self.texts.get(path) {
            Some(t) => self.encoding.from_wire(nth_line(t, line), units),
            None => {
                std::fs::read_to_string(path).map_or(units, |t| char_col(nth_line(&t, line), units))
            }
        }
    }

    pub fn stop(&mut self) {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn path_of(uri: &str) -> PathBuf {
    PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri))
}

fn parse_diagnostic(d: &Value, text: &str, encoding: Encoding) -> Diagnostic {
    let pos = |p: &Value| {
        let line = p["line"].as_u64().unwrap_or(0) as u32;
        let units = p["character"].as_u64().unwrap_or(0) as u32;
        (line, encoding.from_wire(nth_line(text, line), units))
    };
    let (line, col) = pos(&d["range"]["start"]);
    let (end_line, end_col) = pos(&d["range"]["end"]);
    Diagnostic {
        line,
        col,
        end_line,
        end_col,
        severity: d["severity"].as_u64().unwrap_or(1) as u8,
        message: d["message"].as_str().unwrap_or_default().to_string(),
    }
}

/// Removes the markdown code fence (``` with an optional language) around hover text.
pub fn strip_fence(markdown: &str) -> String {
    let t = markdown.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t.to_string();
    };
    // Drop the language tag on the opening line.
    let body = rest.split_once('\n').map_or("", |(_, b)| b);
    body.trim_end().trim_end_matches("```").trim().to_string()
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; length?];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn find_in_checks_each_directory_in_order() {
        let tmp = std::env::temp_dir().join(format!("pascoda-lsp-find-{}", std::process::id()));
        let bin = tmp.join("home/vendor/pasls/dist/aarch64-darwin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("pasls"), "").unwrap();
        let fpc = tmp.join("fpc");
        std::fs::write(&fpc, "").unwrap();
        let (empty, home) = (tmp.join("empty"), tmp.join("home"));
        let dirs = [empty.as_path(), home.as_path()];
        let found = Config::find_in(dirs.into_iter(), &fpc).unwrap();
        assert_eq!(found.lazarus_src, home.join("vendor/lazarus"));
        assert!(Config::find_in([empty.as_path()].into_iter(), &fpc).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    use super::*;

    #[test]
    fn strips_markdown_fence() {
        assert_eq!(
            strip_fence("```pascal\nvar x: Integer\n```"),
            "var x: Integer"
        );
        assert_eq!(strip_fence("```\na\nb\n```\n"), "a\nb");
        assert_eq!(strip_fence("plain text"), "plain text");
    }

    #[test]
    fn converts_columns_between_chars_and_utf16() {
        // '€' is one UTF-16 unit, the emoji is two.
        let line = "a€😀b";
        assert_eq!(utf16_col(line, 0), 0);
        assert_eq!(utf16_col(line, 2), 2);
        assert_eq!(utf16_col(line, 3), 4);
        assert_eq!(utf16_col(line, 4), 5);
        assert_eq!(utf16_col(line, 99), 5);
        assert_eq!(char_col(line, 4), 3);
        assert_eq!(char_col(line, 3), 3); // inside the pair: rounds up
        assert_eq!(char_col(line, 5), 4);
        assert_eq!(char_col(line, 99), 4);
        for n in 0..=4 {
            assert_eq!(char_col(line, utf16_col(line, n)), n);
        }
    }

    #[test]
    fn utf8_columns_count_bytes() {
        let line = "a€😀b";
        assert_eq!(Encoding::Utf8.to_wire(line, 3), 8);
        assert_eq!(Encoding::Utf8.from_wire(line, 8), 3);
        assert_eq!(Encoding::Utf8.from_wire(line, 99), 4);
        assert_eq!(Encoding::Utf16.to_wire(line, 3), 4);
    }

    #[test]
    fn parses_diagnostics_with_utf16_columns() {
        let d = json!({"range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 4}},
                       "severity": 2, "message": "m"});
        let got = parse_diagnostic(&d, "x\n😀ab\n", Encoding::Utf16);
        assert_eq!((got.col, got.end_col, got.severity), (1, 3, 2));
    }
}
