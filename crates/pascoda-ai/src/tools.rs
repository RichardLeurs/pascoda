use serde_json::{Value, json};
use std::fs;
use std::path::{Component, Path, PathBuf};

const MAX_READ_BYTES: u64 = 2_000_000;
const MAX_OUTPUT_CHARS: usize = 100_000;
const MAX_WRITE_BYTES: usize = 1_000_000;
const MAX_LIST: usize = 400;
const MAX_DEPTH: usize = 6;

/// What the app provides to the agent. Everything here may run on the worker thread.
pub trait Host: Send {
    /// The last build's diagnostics, formatted for the model (empty text when there are none).
    fn diagnostics(&mut self) -> String;
    /// Run the project build (blocking on the worker thread) and return its output.
    fn build(&mut self) -> String;
    /// Current text of a form file (`.lfm`, absolute path), e.g. unsaved designer state.
    /// `None` makes the agent read it from disk.
    fn read_form(&mut self, _lfm: &Path) -> Option<String> {
        None
    }
    /// Unsaved editor text for an open file (absolute path); `None` = read from disk.
    fn open_text(&mut self, _path: &Path) -> Option<String> {
        None
    }
}

/// Confines paths to a project root: rejects `..`, absolute paths elsewhere and symlink escapes.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Sandbox {
            root: root.canonicalize()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path for a project-relative (or absolute inside the root) path. The target
    /// need not exist, but its deepest existing ancestor must resolve inside the root.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        let rel = rel.trim();
        if rel.is_empty() {
            return Err("empty path".into());
        }
        if rel.contains('\0') {
            return Err("invalid path".into());
        }
        let p = Path::new(rel);
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!(
                "path '{rel}' leaves the project folder ('..' is not allowed)"
            ));
        }
        let cand = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };
        let mut existing = cand.clone();
        let mut rest: Vec<std::ffi::OsString> = Vec::new();
        while fs::symlink_metadata(&existing).is_err() {
            match (
                existing.file_name().map(|n| n.to_owned()),
                existing.parent(),
            ) {
                (Some(name), Some(parent)) => {
                    rest.push(name);
                    existing = parent.to_path_buf();
                }
                _ => return Err(format!("path '{rel}' is outside the project folder")),
            }
        }
        let canon = existing
            .canonicalize()
            .map_err(|_| format!("path '{rel}' cannot be resolved (broken symlink?)"))?;
        if !canon.starts_with(&self.root) {
            return Err(format!("path '{rel}' is outside the project folder"));
        }
        let mut out = canon;
        for name in rest.into_iter().rev() {
            out.push(name);
        }
        Ok(out)
    }

    /// Project-relative display form of an absolute path inside the root.
    pub fn relative(&self, abs: &Path) -> String {
        abs.strip_prefix(&self.root)
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned()
    }
}

/// A change the agent wants but must not make itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Replace the single occurrence of `old_text`.
    Edit {
        old_text: String,
        new_text: String,
    },
    Create {
        content: String,
    },
    /// Run the build.
    Build,
}

/// Shown to the user; answered with a `Decision`. The app applies the change *before*
/// answering `Approved` (so the agent's next tool call sees the new file).
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// The tool call id; use it in the `Decision`.
    pub id: String,
    /// Project-relative path ("" for a build).
    pub path: String,
    pub abs_path: PathBuf,
    pub change: Change,
}

impl Proposal {
    /// The file text after applying this proposal to `current` (`None` = file does not exist).
    pub fn apply_to(&self, current: Option<&str>) -> Result<String, String> {
        match (&self.change, current) {
            (Change::Edit { old_text, new_text }, Some(cur)) => {
                replace_once(cur, old_text, new_text)
            }
            (Change::Edit { .. }, None) => Err("file does not exist".into()),
            (Change::Create { content }, None) => Ok(content.clone()),
            (Change::Create { .. }, Some(_)) => Err("file already exists".into()),
            (Change::Build, _) => Err("a build changes no file".into()),
        }
    }
}

/// Replace `old` by `new` when it occurs exactly once.
pub(crate) fn replace_once(text: &str, old: &str, new: &str) -> Result<String, String> {
    if old.is_empty() {
        return Err("old_text is empty".into());
    }
    let n = text.matches(old).count();
    match n {
        0 => Err(
            "old_text was not found in the file (it must match exactly, including whitespace)"
                .into(),
        ),
        1 => Ok(text.replacen(old, new, 1)),
        n => Err(format!(
            "old_text matches {n} places; include more surrounding lines to make it unique"
        )),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Approval {
    Approved,
    Rejected(Option<String>),
}

pub(crate) enum Outcome {
    Text(String),
    Error(String),
    Propose {
        path: String,
        abs: PathBuf,
        change: Change,
    },
    Build,
}

/// Tool schemas for the API (`name`, `description`, `input_schema`).
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_files",
            "description": "List files and folders in the project (recursive, hidden entries skipped). Folders end with '/'.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string", "description": "Project-relative folder; default '.'"}}}
        }),
        json!({
            "name": "read_file",
            "description": "Read a text file from the project. Optional 1-based inclusive line range. Reflects unsaved editor text.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer"},
                "end_line": {"type": "integer"}}, "required": ["path"]}
        }),
        json!({
            "name": "get_diagnostics",
            "description": "Errors, warnings and notes from the last fpc build.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "build",
            "description": "Compile the project with fpc and return the output. Needs the user's approval the first time.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "read_form",
            "description": "Read the current .lfm form text for a unit (pass the unit's .pas or the .lfm path).",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}
        }),
        json!({
            "name": "edit_file",
            "description": "Propose replacing text in an existing file. old_text must match exactly once. The user must approve; nothing changes until then.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "old_text": {"type": "string", "description": "Exact existing text, unique in the file"},
                "new_text": {"type": "string"}}, "required": ["path", "old_text", "new_text"]}
        }),
        json!({
            "name": "create_file",
            "description": "Propose a new file (fails if it exists). The user must approve; nothing changes until then.",
            "input_schema": {"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}}, "required": ["path", "content"]}
        }),
    ]
}

fn arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    if let Some(raw) = input.get("_invalid_json") {
        return Err(format!(
            "the tool input was not valid JSON (probably cut off); shorten it and retry. Received: {}",
            raw.as_str()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn cap(mut s: String) -> String {
    if s.chars().count() > MAX_OUTPUT_CHARS {
        s = s.chars().take(MAX_OUTPUT_CHARS).collect();
        s.push_str("\n[output truncated]");
    }
    s
}

fn current_text(sb: &Sandbox, host: &mut dyn Host, abs: &Path) -> Result<Option<String>, String> {
    let _ = sb;
    if let Some(t) = host.open_text(abs) {
        return Ok(Some(t));
    }
    match fs::symlink_metadata(abs) {
        Err(_) => Ok(None),
        Ok(m) if m.is_dir() => Err("it is a folder".into()),
        Ok(m) if m.len() > MAX_READ_BYTES => Err(format!("file is too large ({} bytes)", m.len())),
        Ok(_) => match fs::read(abs) {
            Ok(b) => String::from_utf8(b)
                .map(Some)
                .map_err(|_| "file is not UTF-8 text".into()),
            Err(e) => Err(e.to_string()),
        },
    }
}

pub(crate) fn execute(name: &str, input: &Value, sb: &Sandbox, host: &mut dyn Host) -> Outcome {
    match run(name, input, sb, host) {
        Ok(o) => o,
        Err(e) => Outcome::Error(e),
    }
}

fn run(name: &str, input: &Value, sb: &Sandbox, host: &mut dyn Host) -> Result<Outcome, String> {
    match name {
        "list_files" => {
            let rel = input
                .get("path")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(".");
            let dir = if rel.trim() == "." {
                sb.root().to_path_buf()
            } else {
                sb.resolve(rel)?
            };
            if !dir.is_dir() {
                return Err(format!("'{rel}' is not a folder"));
            }
            let mut out = Vec::new();
            walk(sb, &dir, 0, &mut out);
            let more = out.len() > MAX_LIST;
            out.truncate(MAX_LIST);
            let mut s = out.join("\n");
            if more {
                s.push_str("\n[list truncated]");
            }
            Ok(Outcome::Text(if s.is_empty() {
                "(empty)".into()
            } else {
                s
            }))
        }
        "read_file" => {
            let abs = sb.resolve(arg(input, "path")?)?;
            let text = current_text(sb, host, &abs)?.ok_or("file not found")?;
            let start = input
                .get("start_line")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as usize;
            let end = input
                .get("end_line")
                .and_then(Value::as_u64)
                .map(|e| e as usize);
            let lines: Vec<&str> = text.split('\n').collect();
            if start > lines.len() {
                return Err(format!(
                    "start_line {start} is past the end ({} lines)",
                    lines.len()
                ));
            }
            let end = end.unwrap_or(lines.len()).min(lines.len()).max(start);
            let body = lines[start - 1..end].join("\n");
            let head = format!(
                "[{} lines {start}-{end} of {}]\n",
                sb.relative(&abs),
                lines.len()
            );
            Ok(Outcome::Text(cap(head + &body)))
        }
        "get_diagnostics" => {
            let d = host.diagnostics();
            Ok(Outcome::Text(if d.trim().is_empty() {
                "No diagnostics.".into()
            } else {
                cap(d)
            }))
        }
        "build" => Ok(Outcome::Build),
        "read_form" => {
            let abs = sb.resolve(arg(input, "path")?)?;
            let lfm = if abs
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lfm"))
            {
                abs
            } else {
                abs.with_extension("lfm")
            };
            let lfm = sb.resolve(&lfm.to_string_lossy())?;
            if let Some(t) = host.read_form(&lfm) {
                return Ok(Outcome::Text(cap(t)));
            }
            let t = current_text(sb, host, &lfm)?.ok_or("no .lfm form file for that unit")?;
            Ok(Outcome::Text(cap(t)))
        }
        "edit_file" => {
            let rel = arg(input, "path")?;
            let (old, new) = (arg(input, "old_text")?, arg(input, "new_text")?);
            let abs = sb.resolve(rel)?;
            let cur = current_text(sb, host, &abs)?
                .ok_or("file not found (use create_file for new files)")?;
            if old == new {
                return Err("old_text and new_text are identical".into());
            }
            if new.len() > MAX_WRITE_BYTES {
                return Err("new_text is too large".into());
            }
            replace_once(&cur, old, new)?; // validate now so the user never sees an unappliable diff
            Ok(Outcome::Propose {
                path: sb.relative(&abs),
                abs,
                change: Change::Edit {
                    old_text: old.into(),
                    new_text: new.into(),
                },
            })
        }
        "create_file" => {
            let rel = arg(input, "path")?;
            let content = arg(input, "content")?;
            let abs = sb.resolve(rel)?;
            if abs == sb.root() {
                return Err("path is the project folder".into());
            }
            if content.len() > MAX_WRITE_BYTES {
                return Err("content is too large".into());
            }
            if current_text(sb, host, &abs)
                .map_err(|e| format!("cannot create: {e}"))?
                .is_some()
            {
                return Err(format!(
                    "'{rel}' already exists; use edit_file to change it"
                ));
            }
            Ok(Outcome::Propose {
                path: sb.relative(&abs),
                abs,
                change: Change::Create {
                    content: content.into(),
                },
            })
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

fn walk(sb: &Sandbox, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || out.len() > MAX_LIST {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") {
            continue;
        }
        let path = e.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let rel = sb.relative(&path);
        if meta.is_dir() {
            out.push(format!("{rel}/"));
            walk(sb, &path, depth + 1, out);
        } else if !(name.ends_with(".o") || name.ends_with(".ppu")) {
            out.push(rel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoHost;
    impl Host for NoHost {
        fn diagnostics(&mut self) -> String {
            String::new()
        }
        fn build(&mut self) -> String {
            String::new()
        }
    }

    fn setup() -> (tempfile::TempDir, Sandbox) {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.pas"), "line1\nline2\nline2\nend.\n").unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        fs::write(d.path().join("sub/b.pas"), "x").unwrap();
        let sb = Sandbox::new(d.path()).unwrap();
        (d, sb)
    }

    #[test]
    fn escapes_are_rejected() {
        let (d, sb) = setup();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), d.path().join("flink"))
            .unwrap();
        std::os::unix::fs::symlink("/nonexistent/zzz", d.path().join("dangling")).unwrap();
        for bad in [
            "../x",
            "sub/../../x",
            "a/../..",
            "/etc/passwd",
            outside.path().join("secret.txt").to_str().unwrap(),
            "link/secret.txt",
            "link/new.txt",
            "link",
            "flink",
            "dangling",
            "",
            "a\0b",
        ] {
            assert!(sb.resolve(bad).is_err(), "should reject {bad:?}");
        }
        for ok in ["a.pas", "sub/b.pas", "sub/new/deeper.pas", "./a.pas", "."] {
            assert!(sb.resolve(ok).unwrap().starts_with(sb.root()), "{ok}");
        }
        let abs = sb.root().join("a.pas");
        assert_eq!(sb.resolve(abs.to_str().unwrap()).unwrap(), abs);
    }

    #[test]
    fn tools_reject_escapes() {
        let (_d, sb) = setup();
        for (n, i) in [
            ("read_file", json!({"path": "../../etc/passwd"})),
            ("list_files", json!({"path": "/"})),
            ("read_form", json!({"path": "/etc/hosts"})),
            (
                "edit_file",
                json!({"path": "../a", "old_text": "a", "new_text": "b"}),
            ),
            (
                "create_file",
                json!({"path": "/tmp/pascoda-x", "content": "x"}),
            ),
        ] {
            assert!(
                matches!(execute(n, &i, &sb, &mut NoHost), Outcome::Error(_)),
                "{n}"
            );
        }
    }

    #[test]
    fn edit_validation() {
        let (_d, sb) = setup();
        let e = |old: &str| {
            execute(
                "edit_file",
                &json!({"path": "a.pas", "old_text": old, "new_text": "Z"}),
                &sb,
                &mut NoHost,
            )
        };
        match e("nothing") {
            Outcome::Error(m) => assert!(m.contains("not found"), "{m}"),
            _ => panic!(),
        }
        match e("line2") {
            Outcome::Error(m) => assert!(m.contains("2 places"), "{m}"),
            _ => panic!(),
        }
        match e("line1") {
            Outcome::Propose { path, change, .. } => {
                assert_eq!(path, "a.pas");
                let p = Proposal {
                    id: "x".into(),
                    path,
                    abs_path: PathBuf::new(),
                    change,
                };
                assert_eq!(p.apply_to(Some("line1\nq")).unwrap(), "Z\nq");
                assert!(p.apply_to(Some("none")).is_err());
            }
            _ => panic!(),
        }
        assert!(matches!(
            execute(
                "edit_file",
                &json!({"path": "missing.pas", "old_text": "a", "new_text": "b"}),
                &sb,
                &mut NoHost
            ),
            Outcome::Error(_)
        ));
        assert!(matches!(e(""), Outcome::Error(_)));
    }

    #[test]
    fn create_validation_and_nothing_is_written() {
        let (d, sb) = setup();
        let c = |p: &str| {
            execute(
                "create_file",
                &json!({"path": p, "content": "hi"}),
                &sb,
                &mut NoHost,
            )
        };
        assert!(matches!(c("a.pas"), Outcome::Error(_)));
        assert!(matches!(c("sub"), Outcome::Error(_)));
        assert!(matches!(c("new/x.pas"), Outcome::Propose { .. }));
        assert!(!d.path().join("new").exists());
    }

    #[test]
    fn read_and_list() {
        let (_d, sb) = setup();
        match execute(
            "read_file",
            &json!({"path": "a.pas", "start_line": 2, "end_line": 3}),
            &sb,
            &mut NoHost,
        ) {
            Outcome::Text(t) => assert!(t.ends_with("line2\nline2"), "{t}"),
            _ => panic!(),
        }
        match execute("list_files", &json!({}), &sb, &mut NoHost) {
            Outcome::Text(t) => assert_eq!(t, "a.pas\nsub/\nsub/b.pas"),
            _ => panic!(),
        }
    }
}
