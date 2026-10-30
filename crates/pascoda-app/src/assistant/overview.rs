//! The project overview the assistant gets with each message: a file tree and a one-line
//! summary of every Pascal unit (kind, name, `uses`, form class).
//!
//! Nothing here draws. The disk is read by [`Index::scan`] on a worker thread ([`Project::poll`]);
//! the text is rendered from that index plus the text of the open tabs (so unsaved edits count)
//! and cached under a fingerprint, so an unchanged project costs nothing.

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

/// Entries (files and folders) in the tree.
pub const MAX_TREE_ENTRIES: usize = 150;
/// Characters in the tree.
pub const MAX_TREE_CHARS: usize = 4_000;
/// Unit summary lines and their characters.
pub const MAX_SUMMARIES: usize = 60;
pub const MAX_SUMMARY_CHARS: usize = 5_000;
/// One summary line.
pub const MAX_LINE_CHARS: usize = 220;
/// More entries than this in one folder are cut with a "... N more" line.
const MAX_PER_DIR: usize = 25;
/// The walk stops after this many files, and at this depth.
const MAX_FILES: usize = 2_000;
const MAX_DEPTH: usize = 8;
/// Pascal files bigger than this are listed but not scanned.
const MAX_SCAN_BYTES: u64 = 1_000_000;

const IGNORED_DIRS: &[&str] = &[
    "lib",
    "node_modules",
    "target",
    "vendor",
    "backup",
    "__history",
    "__recovery",
];
const IGNORED_EXTS: &[&str] = &[
    "o", "ppu", "a", "res", "compiled", "rsj", "or", "dylib", "so", "dsym", "app",
];
const IGNORED_FILES: &[&str] = &["link.res", "ppas.sh"];
const PASCAL_EXTS: &[&str] = &["pas", "pp", "lpr", "dpr"];

// ---- the scanner ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Program,
    Unit,
    Library,
    Package,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Program => "program",
            Kind::Unit => "unit",
            Kind::Library => "library",
            Kind::Package => "package",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Summary {
    pub kind: Kind,
    pub name: String,
    /// The `uses` clause of the program, or of the unit's interface section.
    pub uses: Vec<String>,
    /// The unit's implementation `uses`.
    pub impl_uses: Vec<String>,
}

#[derive(PartialEq, Debug)]
enum Tok {
    Word(String),
    Sym(char),
    Str,
}

/// Words, symbols and strings; comments, directives and the insides of strings are skipped.
fn tokens(src: &str) -> Vec<Tok> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '{' {
            while i < c.len() && c[i] != '}' {
                i += 1;
            }
            i += 1;
        } else if ch == '(' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&')')) {
                i += 1;
            }
            i += 2;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '\'' {
            i += 1;
            while i < c.len() && c[i] != '\n' {
                if c[i] == '\'' {
                    if c.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            out.push(Tok::Str);
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len()
                && (c[i].is_alphanumeric()
                    || c[i] == '_'
                    || (c[i] == '.' && c.get(i + 1).is_some_and(|n| n.is_alphabetic())))
            {
                i += 1;
            }
            out.push(Tok::Word(c[start..i].iter().collect()));
        } else {
            if !ch.is_whitespace() {
                out.push(Tok::Sym(ch));
            }
            i += 1;
        }
    }
    out
}

/// Kind, name and `uses` of a Pascal source. `None` when it has no program/unit header.
pub fn scan_pascal(src: &str) -> Option<Summary> {
    let toks = tokens(src);
    let mut sum: Option<Summary> = None;
    let mut in_impl = false;
    let mut i = 0;
    while i < toks.len() {
        let Tok::Word(w) = &toks[i] else {
            i += 1;
            continue;
        };
        let lw = w.to_lowercase();
        let Some(s) = sum.as_mut() else {
            let kind = match lw.as_str() {
                "program" => Some(Kind::Program),
                "unit" => Some(Kind::Unit),
                "library" => Some(Kind::Library),
                "package" => Some(Kind::Package),
                _ => None,
            };
            if let Some(kind) = kind {
                let name = match toks.get(i + 1) {
                    Some(Tok::Word(n)) => n.clone(),
                    _ => String::new(),
                };
                sum = Some(Summary {
                    kind,
                    name,
                    uses: Vec::new(),
                    impl_uses: Vec::new(),
                });
                i += 1;
            }
            i += 1;
            continue;
        };
        match lw.as_str() {
            "implementation" => in_impl = true,
            "begin" | "initialization" | "finalization" => break,
            "uses" => {
                i += 1;
                let mut after_in = false;
                while i < toks.len() && toks[i] != Tok::Sym(';') {
                    match &toks[i] {
                        Tok::Word(u) if u.eq_ignore_ascii_case("in") => after_in = true,
                        Tok::Word(u) if !after_in => {
                            let list = if in_impl {
                                &mut s.impl_uses
                            } else {
                                &mut s.uses
                            };
                            if !list.iter().any(|x| x == u) {
                                list.push(u.clone());
                            }
                        }
                        Tok::Sym(',') => after_in = false,
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    sum
}

/// The class of a form file's root: `object Form1: TForm1` -> `TForm1`.
pub fn lfm_class(src: &str) -> Option<String> {
    let line = src.lines().map(str::trim).find(|l| !l.is_empty())?;
    let rest = ["object ", "inherited ", "inline "]
        .iter()
        .find_map(|k| line.strip_prefix(k))?;
    let class = rest.split_once(':')?.1.trim();
    (!class.is_empty() && !class.contains(char::is_whitespace)).then(|| class.to_string())
}

fn has_ext(rel: &str, exts: &[&str]) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn is_pascal(rel: &str) -> bool {
    has_ext(rel, PASCAL_EXTS)
}

// ---- the disk ---------------------------------------------------------------------------

fn is_macho(path: &Path) -> bool {
    let mut b = [0u8; 4];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    if f.read_exact(&mut b).is_err() {
        return false;
    }
    matches!(
        b,
        [0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe]
    )
}

fn ignored_dir(name: &str) -> bool {
    name.starts_with('.')
        || IGNORED_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
        || has_ext(name, &["app", "dSYM"])
}

fn ignored_file(path: &Path, name: &str) -> bool {
    name.starts_with('.')
        || IGNORED_FILES.contains(&name)
        || has_ext(name, IGNORED_EXTS)
        // A compiled program has no extension; a README or Makefile is text.
        || (Path::new(name).extension().is_none() && is_macho(path))
}

/// Project-relative paths (with `/`), sorted; `true` when the walk was cut short.
pub fn walk(root: &Path) -> (Vec<String>, bool) {
    let mut files = Vec::new();
    let mut cut = false;
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            let rel = format!("{prefix}{name}");
            if ft.is_dir() {
                if depth + 1 < MAX_DEPTH && !ignored_dir(&name) {
                    stack.push((e.path(), format!("{rel}/"), depth + 1));
                }
            } else if ft.is_file() && !ignored_file(&e.path(), &name) {
                if files.len() >= MAX_FILES {
                    cut = true;
                } else {
                    files.push(rel);
                }
            }
        }
    }
    files.sort_by_key(|f| f.to_lowercase());
    (files, cut)
}

fn read_capped(path: &Path, max: u64) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    if f.metadata().ok()?.len() > max {
        return None;
    }
    let mut buf = Vec::new();
    f.take(max).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// What the disk says about the project.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    pub files: Vec<String>,
    pub units: HashMap<String, Summary>,
    /// `.lfm` path -> root class.
    pub forms: HashMap<String, String>,
    pub truncated: bool,
}

impl Index {
    pub fn scan(root: &Path) -> Index {
        let (files, truncated) = walk(root);
        let mut idx = Index {
            root: root.to_path_buf(),
            truncated,
            ..Index::default()
        };
        for rel in &files {
            if is_pascal(rel) {
                if let Some(s) =
                    read_capped(&root.join(rel), MAX_SCAN_BYTES).and_then(|t| scan_pascal(&t))
                {
                    idx.units.insert(rel.clone(), s);
                }
            } else if has_ext(rel, &["lfm"])
                && let Some(c) = read_capped(&root.join(rel), 64_000).and_then(|t| lfm_class(&t))
            {
                idx.forms.insert(rel.clone(), c);
            }
        }
        idx.files = files;
        idx
    }
}

// ---- rendering --------------------------------------------------------------------------

#[derive(Default)]
struct Node {
    dirs: std::collections::BTreeMap<String, Node>,
    files: Vec<String>,
}

fn tree_lines(node: &Node, depth: usize, out: &mut Vec<(String, bool)>) {
    let pad = "  ".repeat(depth);
    let mut shown = 0;
    let total = node.dirs.len() + node.files.len();
    for (name, child) in &node.dirs {
        if shown >= MAX_PER_DIR {
            break;
        }
        shown += 1;
        // A folder that holds only one folder is written as one line: a/b/c/.
        let (mut label, mut cur) = (name.clone(), child);
        while cur.files.is_empty() && cur.dirs.len() == 1 {
            let (n, c) = cur.dirs.iter().next().expect("one folder");
            label = format!("{label}/{n}");
            cur = c;
        }
        out.push((format!("{pad}{label}/"), true));
        tree_lines(cur, depth + 1, out);
    }
    for f in &node.files {
        if shown >= MAX_PER_DIR {
            break;
        }
        shown += 1;
        out.push((format!("{pad}{f}"), true));
    }
    if total > shown {
        out.push((format!("{pad}... {} more here", total - shown), false));
    }
}

/// The file tree as text, within `max_entries` lines and `max_chars` characters. Folders come
/// first, a folder with many entries is cut, and the end says how many entries were left out.
pub fn render_tree(files: &[String], max_entries: usize, max_chars: usize) -> String {
    let mut root = Node::default();
    for f in files {
        let mut parts: Vec<&str> = f.split('/').collect();
        let Some(file) = parts.pop() else { continue };
        let mut n = &mut root;
        for d in parts {
            n = n.dirs.entry(d.to_string()).or_default();
        }
        n.files.push(file.to_string());
    }
    fn sort(n: &mut Node) {
        n.files.sort_by_key(|f| f.to_lowercase());
        n.dirs.values_mut().for_each(sort);
    }
    sort(&mut root);
    let mut lines = Vec::new();
    tree_lines(&root, 0, &mut lines);
    let total = lines.iter().filter(|l| l.1).count();
    let (mut out, mut entries) = (String::new(), 0);
    for (text, is_entry) in &lines {
        if (*is_entry && entries >= max_entries) || out.len() + text.len() + 1 > max_chars {
            break;
        }
        entries += usize::from(*is_entry);
        out.push_str(text);
        out.push('\n');
    }
    if entries < total {
        out.push_str(&format!("... {} more\n", total - entries));
    }
    out
}

fn cap_line(s: String) -> String {
    if s.chars().count() <= MAX_LINE_CHARS {
        return s;
    }
    let mut t: String = s.chars().take(MAX_LINE_CHARS - 3).collect();
    t.push_str("...");
    t
}

fn summary_line(rel: &str, s: &Summary, form: Option<&str>) -> String {
    let head = format!("{rel}: {} {}", s.kind.word(), s.name);
    // The form class is worth more than the end of a long `uses` list: it is kept, and the
    // lists are cut with "..." when the line would be too long.
    let tail = form.map(|c| format!("; form {c}")).unwrap_or_default();
    let mut mid = String::new();
    if !s.uses.is_empty() {
        mid.push_str(&format!("; uses {}", s.uses.join(", ")));
    }
    if !s.impl_uses.is_empty() {
        mid.push_str(&format!("; implementation uses {}", s.impl_uses.join(", ")));
    }
    let budget = MAX_LINE_CHARS.saturating_sub(head.chars().count() + tail.chars().count());
    if mid.chars().count() > budget {
        let keep = budget.saturating_sub(3);
        mid = mid.chars().take(keep).collect::<String>() + "...";
    }
    cap_line(format!("{head}{mid}{tail}"))
}

/// The overview text. `overlay` holds the text of open tabs (project-relative path, text); it
/// replaces what the disk had, and adds files the index does not know yet.
pub fn render(idx: &Index, name: &str, overlay: &[(String, String)]) -> String {
    let mut files = idx.files.clone();
    let mut units = idx.units.clone();
    let mut forms = idx.forms.clone();
    for (rel, text) in overlay {
        if !files.contains(rel) {
            files.push(rel.clone());
        }
        if is_pascal(rel) {
            match scan_pascal(text) {
                Some(s) => {
                    units.insert(rel.clone(), s);
                }
                None => {
                    units.remove(rel);
                }
            }
        } else if has_ext(rel, &["lfm"]) {
            match lfm_class(text) {
                Some(c) => {
                    forms.insert(rel.clone(), c);
                }
                None => {
                    forms.remove(rel);
                }
            }
        }
    }
    files.sort_by_key(|f| f.to_lowercase());
    let mut out = format!("Project: {name}\nFiles:\n");
    out.push_str(&render_tree(&files, MAX_TREE_ENTRIES, MAX_TREE_CHARS));
    if idx.truncated {
        out.push_str("(the project has more files than this list covers)\n");
    }
    // The program first, then the rest alphabetically.
    let mut list: Vec<(&String, &Summary)> = units.iter().collect();
    list.sort_by_key(|(rel, s)| (s.kind != Kind::Program, rel.to_lowercase()));
    if !list.is_empty() {
        out.push_str("Pascal files:\n");
        let (mut n, mut chars) = (0, 0);
        for (rel, s) in &list {
            let stem = Path::new(rel.as_str()).with_extension("lfm");
            let form = forms
                .get(&stem.to_string_lossy().into_owned())
                .map(String::as_str);
            let line = summary_line(rel, s, form);
            if n >= MAX_SUMMARIES || chars + line.len() + 1 > MAX_SUMMARY_CHARS {
                break;
            }
            chars += line.len() + 1;
            n += 1;
            out.push_str(&line);
            out.push('\n');
        }
        if n < list.len() {
            out.push_str(&format!("... {} more\n", list.len() - n));
        }
    }
    out
}

// ---- keeping it fresh -------------------------------------------------------------------

#[derive(Default)]
struct Slot {
    index: Option<Arc<Index>>,
    /// Counts the scans that found something new.
    generation: u64,
    running: bool,
    started: Option<std::time::Instant>,
}

/// The project index (rescanned on a worker thread) and the cached overview text.
#[derive(Default)]
pub struct Project {
    slot: Arc<Mutex<Slot>>,
    cache: Mutex<Option<(u64, String)>>,
    /// How often the text was rendered (tests).
    pub renders: AtomicUsize,
}

impl Project {
    /// Starts a rescan when there is none for `root` yet or the last one is older than
    /// `max_age`. Cheap to call every frame; never blocks.
    pub fn poll(&self, root: &Path, ctx: Option<&egui::Context>, max_age: Duration) {
        {
            let mut s = lock(&self.slot);
            let fresh = s.index.as_ref().is_some_and(|i| i.root == root)
                && s.started.is_some_and(|t| t.elapsed() < max_age);
            if s.running || fresh {
                return;
            }
            s.running = true;
            s.started = Some(std::time::Instant::now());
        }
        let slot = self.slot.clone();
        let root = root.to_path_buf();
        let ctx = ctx.cloned();
        std::thread::spawn(move || {
            let idx = Index::scan(&root);
            let mut s = lock(&slot);
            if s.index.as_deref() != Some(&idx) {
                s.generation += 1;
                s.index = Some(Arc::new(idx));
            }
            s.running = false;
            drop(s);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    /// Replaces the index (tests).
    #[cfg(test)]
    pub fn set_index(&self, idx: Index) {
        let mut s = lock(&self.slot);
        s.generation += 1;
        s.index = Some(Arc::new(idx));
        s.started = Some(std::time::Instant::now());
    }

    /// The overview for `root`, from the cache when neither the index nor an open tab's text
    /// changed. `None` while no scan has finished.
    pub fn text(&self, root: &Path, overlay: &[(String, String)]) -> Option<String> {
        let (idx, generation) = {
            let s = lock(&self.slot);
            (s.index.clone()?, s.generation)
        };
        if idx.root != root {
            return None;
        }
        let mut h = DefaultHasher::new();
        (root, generation).hash(&mut h);
        for (rel, text) in overlay {
            (rel, text.len(), text).hash(&mut h);
        }
        let key = h.finish();
        let mut cache = lock(&self.cache);
        if let Some((k, t)) = cache.as_ref()
            && *k == key
        {
            return Some(t.clone());
        }
        self.renders.fetch_add(1, Ordering::Relaxed);
        let name = root.file_name().map_or_else(
            || "project".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let text = render(&idx, &name, overlay);
        *cache = Some((key, text.clone()));
        Some(text)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A scratch copy of the sources of `examples/gui-demo`, plus things that must be ignored.
    pub(crate) fn notes_copy(name: &str) -> PathBuf {
        let dir = crate::target_tmp().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("Notes.app/Contents")).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gui-demo");
        for e in std::fs::read_dir(src).unwrap().flatten() {
            let p = e.path();
            if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("pas" | "lpr" | "lfm")
            ) {
                std::fs::copy(&p, dir.join(e.file_name())).unwrap();
            }
        }
        std::fs::write(dir.join("lib/mainunit.o"), "x").unwrap();
        std::fs::write(dir.join("mainunit.o"), "x").unwrap();
        std::fs::write(dir.join("mainunit.ppu"), "x").unwrap();
        std::fs::write(dir.join(".git/config"), "x").unwrap();
        std::fs::write(dir.join("Notes.app/Contents/Info.plist"), "x").unwrap();
        std::fs::write(dir.join("link.res"), "x").unwrap();
        std::fs::write(dir.join("README"), "read me").unwrap();
        // A compiled program: no extension, Mach-O magic.
        std::fs::write(dir.join("notes"), [0xcf, 0xfa, 0xed, 0xfe, 0, 0]).unwrap();
        dir.canonicalize().unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_scanner_reads_kind_name_and_both_uses_clauses() {
        let src = "unit Main.Unit1;\n{$mode objfpc}\ninterface\nuses Classes, SysUtils, // Forms\n  Math;\ntype IFoo = interface end;\nimplementation\nuses Dialogs;\n{$R *.lfm}\nbegin\nend.\nuses Late;";
        let u = scan_pascal(src).unwrap();
        assert_eq!((u.kind, u.name.as_str()), (Kind::Unit, "Main.Unit1"));
        assert_eq!(u.uses, s(&["Classes", "SysUtils", "Math"]));
        assert_eq!(u.impl_uses, s(&["Dialogs"]));
    }

    #[test]
    fn the_scanner_skips_comments_strings_and_directives() {
        let src = "{ program Wrong; uses Bad; }\n(* uses Bad2; *)\n// unit Nope;\nProgram Real;\n{$IFDEF UNIX}\nuses cthreads,\n{$ENDIF}\n  Unit1 in 'sub/unit1.pas', Other;\nconst S = 'uses Fake; begin';\nbegin end.";
        let p = scan_pascal(src).unwrap();
        assert_eq!((p.kind, p.name.as_str()), (Kind::Program, "Real"));
        assert_eq!(p.uses, s(&["cthreads", "Unit1", "Other"]));
        assert!(p.impl_uses.is_empty());
        assert_eq!(scan_pascal("// nothing\nvar x: integer;"), None);
        // An unterminated string does not hang or panic.
        assert!(scan_pascal("unit u; interface uses 'oops").is_some());
    }

    #[test]
    fn a_form_file_names_its_class() {
        assert_eq!(
            lfm_class("\nobject Form1: TForm1\n  Left = 1\nend"),
            Some("TForm1".into())
        );
        assert_eq!(lfm_class("inherited F: TBase\nend"), Some("TBase".into()));
        assert_eq!(lfm_class("nonsense"), None);
    }

    #[test]
    fn the_tree_uses_relative_paths_and_collapses_chains() {
        let files = s(&[
            "a/b/c/x.pas",
            "main.lpr",
            "src/u.pas",
            "src/v.pas",
            "README",
        ]);
        let t = render_tree(&files, 150, 4000);
        assert_eq!(
            t,
            "a/b/c/\n  x.pas\nsrc/\n  u.pas\n  v.pas\nmain.lpr\nREADME\n"
        );
    }

    #[test]
    fn the_tree_is_capped_by_entries_and_characters() {
        let files: Vec<String> = (0..400).map(|i| format!("d{}/f{i}.pas", i % 20)).collect();
        let t = render_tree(&files, 150, 100_000);
        let lines = t.lines().count();
        assert!(lines <= 151 + 20, "{lines}");
        assert!(t.trim_end().lines().last().unwrap().starts_with("... "));
        let small = render_tree(&files, 150, 300);
        assert!(small.len() < 300 + 20 && small.contains("more"));
        // One crowded folder is cut on its own.
        let many: Vec<String> = (0..100).map(|i| format!("f{i:03}.pas")).collect();
        let t = render_tree(&many, 150, 4000);
        assert!(t.contains("... 75 more here") && !t.contains("f030"));
    }

    #[test]
    fn the_walk_ignores_hidden_build_products_and_programs() {
        let dir = notes_copy("overview-walk");
        let (files, cut) = walk(&dir);
        assert!(!cut);
        for f in &files {
            assert!(
                !f.starts_with("lib/")
                    && !f.starts_with(".git")
                    && !f.contains(".app")
                    && !f.ends_with(".o")
                    && !f.ends_with(".ppu")
                    && f != "link.res"
                    && f != "notes",
                "{f}"
            );
        }
        for want in ["README", "notes.lpr", "mainunit.pas", "mainunit.lfm"] {
            assert!(files.iter().any(|f| f == want), "{want} in {files:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_overview_lists_the_program_first_then_units() {
        let dir = notes_copy("overview-render");
        let idx = Index::scan(&dir);
        let text = render(&idx, "notes", &[]);
        assert!(text.starts_with("Project: notes\nFiles:\n"));
        let pas = text.split("Pascal files:\n").nth(1).unwrap();
        let lines: Vec<&str> = pas.lines().collect();
        assert!(lines[0].starts_with("notes.lpr: program "), "{lines:?}");
        let rest: Vec<&str> = lines[1..]
            .iter()
            .map(|l| l.split(':').next().unwrap())
            .collect();
        let mut sorted = rest.clone();
        sorted.sort();
        assert_eq!(rest, sorted);
        let main = lines
            .iter()
            .find(|l| l.starts_with("mainunit.pas"))
            .unwrap();
        assert!(
            main.contains("unit ") && main.contains("; uses ") && main.contains("; form T"),
            "{main}"
        );
        assert!(text.contains("mainunit.lfm") && !text.contains("mainunit.o"));
        assert!(text.len() < 3000, "{}", text.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_tab_text_wins_over_the_disk() {
        let dir = notes_copy("overview-overlay");
        let idx = Index::scan(&dir);
        let unsaved = "unit mainunit;\ninterface\nuses Brand_New_Unit;\nimplementation\nend.";
        let overlay = vec![
            ("mainunit.pas".to_string(), unsaved.to_string()),
            (
                "fresh.pas".to_string(),
                "unit fresh; interface implementation end.".into(),
            ),
        ];
        let text = render(&idx, "notes", &overlay);
        assert!(text.contains("mainunit.pas: unit mainunit; uses Brand_New_Unit"));
        assert!(text.contains("fresh.pas: unit fresh"));
        assert!(text.contains("\nfresh.pas\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summaries_are_capped() {
        let mut idx = Index::default();
        for i in 0..100 {
            let rel = format!("u{i:03}.pas");
            idx.files.push(rel.clone());
            idx.units.insert(
                rel,
                Summary {
                    kind: Kind::Unit,
                    name: format!("U{i}"),
                    uses: (0..80).map(|n| format!("Unit{n}")).collect(),
                    impl_uses: vec![],
                },
            );
        }
        let text = render(&idx, "big", &[]);
        let pas = text.split("Pascal files:\n").nth(1).unwrap();
        assert!(pas.lines().all(|l| l.chars().count() <= MAX_LINE_CHARS));
        assert!(pas.contains("... ") && pas.trim_end().ends_with("more"));
        assert!(pas.len() <= MAX_SUMMARY_CHARS + 40);
        assert!(
            text.len() <= MAX_TREE_CHARS + MAX_SUMMARY_CHARS + 200,
            "{}",
            text.len()
        );
    }

    #[test]
    fn an_unchanged_project_is_not_rendered_again() {
        let dir = notes_copy("overview-cache");
        let p = Project::default();
        assert_eq!(p.text(&dir, &[]), None, "nothing scanned yet");
        p.set_index(Index::scan(&dir));
        let a = p.text(&dir, &[]).unwrap();
        let b = p.text(&dir, &[]).unwrap();
        assert_eq!(
            (a.as_str(), p.renders.load(Ordering::Relaxed)),
            (b.as_str(), 1)
        );
        // An edit in an open tab: new text.
        let ov = vec![(
            "mainunit.pas".to_string(),
            "unit mainunit; interface uses Zed; implementation end.".to_string(),
        )];
        let c = p.text(&dir, &ov).unwrap();
        assert!(c.contains("uses Zed") && p.renders.load(Ordering::Relaxed) == 2);
        p.text(&dir, &ov);
        assert_eq!(p.renders.load(Ordering::Relaxed), 2);
        // A new index: new text.
        let mut idx = Index::scan(&dir);
        idx.files.push("extra.txt".into());
        p.set_index(idx);
        assert!(p.text(&dir, &ov).unwrap().contains("extra.txt"));
        assert_eq!(p.renders.load(Ordering::Relaxed), 3);
        // Another root: nothing.
        assert_eq!(p.text(Path::new("/elsewhere"), &[]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_background_scan_fills_the_index_and_notices_new_files() {
        let dir = notes_copy("overview-poll");
        let p = Project::default();
        p.poll(&dir, None, Duration::from_millis(0));
        let wait = |p: &Project, f: &dyn Fn(&str) -> bool| {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                p.poll(&dir, None, Duration::from_millis(0));
                if let Some(t) = p.text(&dir, &[])
                    && f(&t)
                {
                    return true;
                }
                if std::time::Instant::now() > end {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        assert!(wait(&p, &|t| t.contains("notes.lpr")));
        std::fs::write(
            dir.join("newunit.pas"),
            "unit newunit; interface uses Classes; implementation end.",
        )
        .unwrap();
        assert!(wait(&p, &|t| t
            .contains("newunit.pas: unit newunit; uses Classes")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
