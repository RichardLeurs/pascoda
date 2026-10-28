//! Minimal Debug Adapter Protocol client for driving `lldb-dap`.
//!
//! The session is synchronous: requests block until their response arrives, and events
//! that show up in the meantime are queued for [`Session::next_event`]. Run it on its own
//! thread so the UI never waits on the debugger.

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub name: String,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub file: Option<PathBuf>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    /// Non-zero when the variable has children (fields, elements) to fetch with [`Session::children`].
    pub reference: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Over,
    Into,
    Out,
}

/// Where `lldb-dap` lives. It ships inside Xcode and is not on PATH.
pub fn find_lldb_dap() -> Option<PathBuf> {
    let found = Command::new("xcrun")
        .args(["--find", "lldb-dap"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    // `xcrun` refuses to answer until the Xcode license is accepted (after an Xcode update);
    // the tool itself works, so fall back to where Xcode puts it.
    found.or_else(|| {
        let p = PathBuf::from("/Applications/Xcode.app/Contents/Developer/usr/bin/lldb-dap");
        p.is_file().then_some(p)
    })
}

/// lldb hangs at `run` on a program that lives in one of macOS's privacy-protected folders
/// (Documents, Desktop, Downloads), although the same file runs fine from a shell. Programs
/// there are debugged from a copy in the cache folder; the sources stay where they are, so
/// breakpoints still resolve. App bundles are left alone (they need their own folder).
pub fn stage_outside_protected_folders(program: &Path) -> PathBuf {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return program.to_path_buf();
    };
    let protected = ["Documents", "Desktop", "Downloads"]
        .iter()
        .any(|d| program.starts_with(home.join(d)));
    let in_bundle = program
        .components()
        .any(|c| c.as_os_str().to_str().is_some_and(|c| c.ends_with(".app")));
    let Some(name) = program.file_name().filter(|_| protected && !in_bundle) else {
        return program.to_path_buf();
    };
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    program.hash(&mut h);
    let dir = home
        .join("Library/Caches/Pascoda/debug")
        .join(format!("{:016x}", h.finish()));
    let copy = dir.join(name);
    if std::fs::create_dir_all(&dir).is_ok() && std::fs::copy(program, &copy).is_ok() {
        copy
    } else {
        program.to_path_buf()
    }
}

pub struct Session {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    seq: i64,
    events: VecDeque<Event>,
    /// Responses that arrived while waiting for a different one.
    responses: Vec<Value>,
}

impl Session {
    pub fn start(lldb_dap: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(lldb_dap)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
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
        Ok(Self {
            child,
            stdin,
            messages,
            seq: 0,
            events: VecDeque::new(),
            responses: Vec::new(),
        })
    }

    /// Sends a request without waiting; pair with [`Session::wait`].
    pub fn send(&mut self, command: &str, arguments: Value) -> std::io::Result<i64> {
        self.seq += 1;
        let body =
            json!({"seq": self.seq, "type": "request", "command": command, "arguments": arguments})
                .to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        self.stdin.flush()?;
        Ok(self.seq)
    }

    /// Waits for the response to request `seq`, returning its body or the error message.
    pub fn wait(&mut self, seq: i64, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(i) = self.responses.iter().position(|r| r["request_seq"] == seq) {
                let r = self.responses.remove(i);
                return if r["success"] == true {
                    Ok(r["body"].clone())
                } else {
                    let detail = r["body"]["error"]["format"]
                        .as_str()
                        .or(r["message"].as_str())
                        .unwrap_or("request failed");
                    Err(format!(
                        "{}: {detail}",
                        r["command"].as_str().unwrap_or("?")
                    ))
                };
            }
            self.pump(deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| format!("{e} while waiting for response {seq}"))?;
        }
    }

    pub fn request(&mut self, command: &str, arguments: Value) -> Result<Value, String> {
        let seq = self.send(command, arguments).map_err(|e| e.to_string())?;
        self.wait(seq, Duration::from_secs(20))
    }

    /// Returns the next event, waiting up to `timeout`.
    pub fn next_event(&mut self, timeout: Duration) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(e) = self.events.pop_front() {
                return Some(e);
            }
            self.pump(deadline.saturating_duration_since(Instant::now()))
                .ok()?;
        }
    }

    /// Waits for an event called `name`; other events are discarded.
    pub fn wait_event(&mut self, name: &str, timeout: Duration) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            let e = self.next_event(deadline.saturating_duration_since(Instant::now()))?;
            if e.name == name {
                return Some(e);
            }
        }
    }

    fn pump(&mut self, timeout: Duration) -> Result<(), RecvTimeoutError> {
        let msg = self.messages.recv_timeout(timeout)?;
        match msg["type"].as_str() {
            Some("response") => self.responses.push(msg),
            Some("event") => self.events.push_back(Event {
                name: msg["event"].as_str().unwrap_or_default().to_string(),
                body: msg["body"].clone(),
            }),
            _ => {}
        }
        Ok(())
    }

    /// Starts `program` under the debugger, with line breakpoints set before it runs.
    /// `breakpoints` pairs a source file with 1-based lines.
    pub fn launch(
        &mut self,
        program: &Path,
        cwd: &Path,
        breakpoints: &[(PathBuf, Vec<u32>)],
    ) -> Result<(), String> {
        let staged = stage_outside_protected_folders(program);
        let program = staged.as_path();
        self.request(
            "initialize",
            json!({"adapterID": "pascoda", "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path"}),
        )?;
        let launch = self
            .send(
                "launch",
                json!({"program": program, "cwd": cwd, "stopOnEntry": false}),
            )
            .map_err(|e| e.to_string())?;
        self.wait_event("initialized", Duration::from_secs(20))
            .ok_or("debugger never signalled it was ready")?;
        for (file, lines) in breakpoints {
            self.set_breakpoints(file, lines)?;
        }
        self.request("configurationDone", json!({}))?;
        self.wait(launch, Duration::from_secs(20)).map(|_| ())
    }

    pub fn set_breakpoints(&mut self, file: &Path, lines: &[u32]) -> Result<(), String> {
        self.request(
            "setBreakpoints",
            json!({"source": {"path": file}, "breakpoints": lines.iter().map(|l| json!({"line": l})).collect::<Vec<_>>()}),
        )
        .map(|_| ())
    }

    pub fn resume(&mut self, thread: i64) -> Result<(), String> {
        self.request("continue", json!({"threadId": thread}))
            .map(|_| ())
    }

    pub fn step(&mut self, thread: i64, kind: Step) -> Result<(), String> {
        let command = match kind {
            Step::Over => "next",
            Step::Into => "stepIn",
            Step::Out => "stepOut",
        };
        self.request(command, json!({"threadId": thread}))
            .map(|_| ())
    }

    pub fn stack(&mut self, thread: i64) -> Result<Vec<Frame>, String> {
        let body = self.request("stackTrace", json!({"threadId": thread}))?;
        Ok(body["stackFrames"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|f| Frame {
                        id: f["id"].as_i64().unwrap_or(0),
                        name: f["name"].as_str().unwrap_or_default().to_string(),
                        file: f["source"]["path"].as_str().map(PathBuf::from),
                        line: f["line"].as_u64().unwrap_or(0) as u32,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Variables of every scope (locals, globals, ...) of `frame`, as `(scope, variables)`.
    pub fn variables(&mut self, frame: i64) -> Result<Vec<(String, Vec<Variable>)>, String> {
        let scopes = self.request("scopes", json!({"frameId": frame}))?;
        let mut out = Vec::new();
        for scope in scopes["scopes"].as_array().into_iter().flatten() {
            let reference = scope["variablesReference"].as_i64().unwrap_or(0);
            let vars = self.children(reference)?;
            out.push((scope["name"].as_str().unwrap_or_default().to_string(), vars));
        }
        Ok(out)
    }

    /// The variables under a scope or a structured variable.
    pub fn children(&mut self, reference: i64) -> Result<Vec<Variable>, String> {
        let body = self.request("variables", json!({"variablesReference": reference}))?;
        Ok(body["variables"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| Variable {
                name: v["name"].as_str().unwrap_or_default().to_string(),
                value: v["value"].as_str().unwrap_or_default().to_string(),
                type_name: v["type"].as_str().unwrap_or_default().to_string(),
                reference: v["variablesReference"].as_i64().unwrap_or(0),
            })
            .collect())
    }

    /// Evaluates a (C-style, lldb has no Pascal expression parser) `expression` in `frame`.
    /// The result's `name` is the expression; its children are valid until the program resumes.
    pub fn evaluate(&mut self, frame: i64, expression: &str) -> Result<Variable, String> {
        let body = self.request(
            "evaluate",
            json!({"expression": expression, "frameId": frame, "context": "watch"}),
        )?;
        Ok(Variable {
            name: expression.to_string(),
            value: body["result"].as_str().unwrap_or_default().to_string(),
            type_name: body["type"].as_str().unwrap_or_default().to_string(),
            reference: body["variablesReference"].as_i64().unwrap_or(0),
        })
    }

    pub fn stop(&mut self) {
        let _ = self.request("disconnect", json!({"terminateDebuggee": true}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads one `Content-Length`-framed JSON message; `None` at end of stream.
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
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; length?];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_framed_messages() {
        let raw = b"Content-Length: 16\r\n\r\n{\"type\":\"event\"}Content-Length: 2\r\n\r\n{}";
        let mut r = std::io::Cursor::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap()["type"], "event");
        assert!(read_message(&mut r).is_some());
        assert!(read_message(&mut r).is_none());
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    #[test]
    fn programs_in_protected_folders_are_copied_and_others_are_not() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let elsewhere = Path::new("/tmp/prog");
        assert_eq!(stage_outside_protected_folders(elsewhere), elsewhere);
        let bundled = home.join("Documents/x/App.app/Contents/MacOS/app");
        assert_eq!(stage_outside_protected_folders(&bundled), bundled);
        let dir = home.join("Documents/pascoda-stage-test");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("prog");
        std::fs::write(&exe, b"x").unwrap();
        let staged = stage_outside_protected_folders(&exe);
        assert!(staged.starts_with(home.join("Library/Caches/Pascoda/debug")));
        assert_eq!(std::fs::read(&staged).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
