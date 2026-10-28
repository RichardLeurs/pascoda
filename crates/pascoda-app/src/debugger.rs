//! Runs a debug session on its own thread and talks to the UI over channels.

use eframe::egui;
use pascoda_dap::{Frame, Session, Step, Variable, find_lldb_dap};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

pub enum Cmd {
    Resume,
    Step(Step),
    Stop,
    Breakpoints(PathBuf, Vec<u32>),
    /// The variable paths the UI shows open; they are fetched again at every stop.
    Expanded(HashSet<String>),
    /// Children of the variable at `path` (`reference` as in [`Var::reference`]) are wanted.
    Fetch(String, i64),
}

pub enum Event {
    Stopped(Paused),
    /// The children of the variable at the path, answering [`Cmd::Fetch`].
    Children(String, Vec<Var>),
    Output(String),
    Ended(String),
}

/// What a value is, for the badge in the Variables pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Integer,
    Float,
    Boolean,
    Char,
    String,
    Enum,
    Set,
    Record,
    Class,
    Array,
    Pointer,
    Other,
}

/// A variable ready to show: Pascal spelling of type and value, and the children if they were
/// fetched. Names stay as lldb reports them (capitals); the UI recovers the source's case.
#[derive(Clone, Debug)]
pub struct Var {
    pub name: String,
    pub type_name: String,
    pub value: String,
    pub kind: Kind,
    /// Non-zero when the variable can be expanded; valid only while the program is stopped.
    pub reference: i64,
    pub children: Option<Vec<Var>>,
}

/// The program is stopped: where, and what its variables hold.
pub struct Paused {
    pub frames: Vec<Frame>,
    pub vars: Vec<(String, Vec<Var>)>,
}

/// Separates scope and variable names in a path such as `Globals/POINT/X`.
pub const PATH_SEP: char = '/';

/// Children beyond this many are not fetched (a big array would stall the UI).
const MAX_CHILDREN: usize = 256;

impl Paused {
    /// Frames that belong to the user's sources; the rest is runtime and loader code
    /// whose "file" is a `binary`symbol` string.
    pub fn source_frames(&self) -> impl Iterator<Item = &Frame> {
        self.frames.iter().filter(|f| is_source(f))
    }

    pub fn location(&self) -> Option<(&Path, u32)> {
        self.source_frames()
            .next()
            .and_then(|f| f.file.as_deref().map(|p| (p, f.line)))
    }

    /// The variable at `path`: scope name, then variable names down the children.
    pub fn var_mut(&mut self, path: &str) -> Option<&mut Var> {
        fn find<'a>(vars: &'a mut [Var], names: &[&str]) -> Option<&'a mut Var> {
            let (first, rest) = names.split_first()?;
            let v = vars.iter_mut().find(|v| v.name == *first)?;
            if rest.is_empty() {
                Some(v)
            } else {
                find(v.children.as_mut()?, rest)
            }
        }
        let names: Vec<&str> = path.split(PATH_SEP).collect();
        let (scope, rest) = names.split_first()?;
        find(&mut self.vars.iter_mut().find(|(s, _)| s == scope)?.1, rest)
    }
}

fn is_source(f: &Frame) -> bool {
    f.file
        .as_ref()
        .is_some_and(|p| !p.to_string_lossy().contains('`') && p.exists())
}

pub struct Handle {
    pub cmds: Sender<Cmd>,
    pub events: Receiver<Event>,
}

pub fn spawn(
    exe: PathBuf,
    cwd: PathBuf,
    breakpoints: Vec<(PathBuf, Vec<u32>)>,
    ctx: egui::Context,
) -> Handle {
    let (cmds, cmd_rx) = channel();
    let (event_tx, events) = channel();
    std::thread::spawn(move || {
        let reason = match drive(&exe, &cwd, &breakpoints, &cmd_rx, &event_tx, &ctx) {
            Ok(()) => "program finished".to_string(),
            Err(e) => format!("debugger error: {e}"),
        };
        let _ = event_tx.send(Event::Ended(reason));
        ctx.request_repaint();
    });
    Handle { cmds, events }
}

fn drive(
    exe: &Path,
    cwd: &Path,
    breakpoints: &[(PathBuf, Vec<u32>)],
    cmds: &Receiver<Cmd>,
    out: &Sender<Event>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let adapter = find_lldb_dap().ok_or("lldb-dap not found (install Xcode)")?;
    let mut dbg = Session::start(&adapter).map_err(|e| e.to_string())?;
    dbg.launch(exe, cwd, breakpoints)?;
    let mut thread = 0;
    // Variable references are valid only while stopped, in the frame we listed.
    let mut stopped_in: Option<i64> = None;
    let mut expanded = HashSet::new();
    let send = |e: Event| {
        let _ = out.send(e);
        ctx.request_repaint();
    };

    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Stop => {
                    dbg.stop();
                    return Ok(());
                }
                Cmd::Resume => {
                    stopped_in = None;
                    dbg.resume(thread)?
                }
                Cmd::Step(kind) => {
                    stopped_in = None;
                    dbg.step(thread, kind)?
                }
                Cmd::Breakpoints(file, lines) => dbg.set_breakpoints(&file, &lines)?,
                Cmd::Expanded(paths) => expanded = paths,
                Cmd::Fetch(path, reference) => {
                    if let Some(frame) = stopped_in {
                        let kids = children_of(&mut dbg, frame, reference, &path, &expanded)
                            .unwrap_or_default();
                        send(Event::Children(path, kids));
                    }
                }
            }
        }
        let Some(event) = dbg.next_event(Duration::from_millis(100)) else {
            // Also reached when the adapter died; avoid spinning.
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        match event.name.as_str() {
            "stopped" => {
                thread = event.body["threadId"].as_i64().unwrap_or(thread);
                let frames = dbg.stack(thread)?;
                let top = frames.iter().find(|f| is_source(f)).or(frames.first());
                let frame = top.map_or(0, |f| f.id);
                let vars = match top {
                    Some(f) => dbg.variables(f.id)?,
                    None => Vec::new(),
                };
                let vars = vars
                    .into_iter()
                    .filter(|(scope, _)| scope != "Registers")
                    .map(|(scope, vars)| {
                        let mut vars: Vec<Var> = vars
                            .into_iter()
                            .map(|v| build(&mut dbg, frame, v, &scope, &expanded))
                            .collect();
                        if scope == "Globals" {
                            // lldb lists globals in no particular order.
                            vars.sort_by_key(|v| v.name.to_lowercase());
                        }
                        (scope, vars)
                    })
                    .collect();
                stopped_in = Some(frame);
                send(Event::Stopped(Paused { frames, vars }));
            }
            "output" => {
                if let Some(text) = text_of(&event.body) {
                    send(Event::Output(text));
                }
            }
            "terminated" | "exited" => {
                dbg.stop();
                return Ok(());
            }
            _ => {}
        }
    }
}

/// Formats `v` (listed under the variable path `parent`) and, if its own path is open, fetches
/// its children too.
fn build(
    dbg: &mut Session,
    frame: i64,
    v: Variable,
    parent: &str,
    expanded: &HashSet<String>,
) -> Var {
    let path = format!("{parent}{PATH_SEP}{}", v.name);
    let mut var = show(dbg, frame, v);
    if var.reference > 0 && expanded.contains(&path) {
        var.children =
            Some(children_of(dbg, frame, var.reference, &path, expanded).unwrap_or_default());
    }
    var
}

fn children_of(
    dbg: &mut Session,
    frame: i64,
    reference: i64,
    path: &str,
    expanded: &HashSet<String>,
) -> Result<Vec<Var>, String> {
    let kids = dbg.children(reference)?;
    Ok(kids
        .into_iter()
        .take(MAX_CHILDREN)
        .map(|k| build(dbg, frame, k, path, expanded))
        .collect())
}

/// lldb has no Pascal plugin, so structured values arrive as raw layouts. Replace the ones we
/// know how to read with what a Pascal programmer expects to see.
fn show(dbg: &mut Session, frame: i64, mut v: Variable) -> Var {
    if v.type_name == "SHORTSTRING"
        && v.reference > 0
        && let Ok(children) = dbg.children(v.reference)
        && let Some(st) = children.iter().find(|c| c.name == "st")
    {
        v.value = pascal_string(&st.value);
        v.reference = 0;
    }
    if let Some(elem) = dynamic_array_element(&v.type_name)
        && let Some(addr) = hex_token(&v.value)
    {
        // lldb sees a pointer to an unsized array. FPC keeps the highest index just before the
        // first element, so read it and have lldb view the memory as a fixed array.
        let length = dbg
            .evaluate(frame, &format!("((long*){addr})[-1]"))
            .ok()
            .and_then(|r| r.value.parse::<i64>().ok())
            .map_or(0, |high| high + 1);
        v.reference = 0;
        if length > 0
            && let Ok(arr) = dbg.evaluate(frame, &format!("*({elem}(*)[{length}]){addr}"))
        {
            v.reference = arr.reference;
        }
        v.value = format!("{length} elements");
    }
    let probe = kind_by_type(&v.type_name).is_none()
        && v.reference > 0
        && v.value.starts_with("0x")
        && !v.value.contains(" @ 0x");
    // A pointer's only child is what it points to (`*NAME`); a class instance lists its fields.
    let deref = probe
        && dbg
            .children(v.reference)
            .is_ok_and(|c| c.first().is_some_and(|c| c.name.starts_with('*')));
    present(v, deref)
}

/// The Pascal view of a variable lldb described. `deref` says whether its only child is the
/// value it points to (a typed pointer), as opposed to fields (a class instance).
pub fn present(v: Variable, deref: bool) -> Var {
    let kind = classify(&v.type_name, &v.value, v.reference, deref);
    let nil = matches!(kind, Kind::Pointer | Kind::Class) && address(&v.value) == Some(0);
    let expandable = match kind {
        Kind::Record | Kind::Array | Kind::Class => true,
        Kind::Pointer => deref,
        _ => false,
    };
    Var {
        name: v.name,
        type_name: display_type(&v.type_name),
        value: format_value(kind, &v.type_name, &v.value),
        kind,
        reference: if expandable && !nil { v.reference } else { 0 },
        children: None,
    }
}

/// The kind a type name says for sure (lldb reports Pascal's own types in capitals).
fn kind_by_type(type_name: &str) -> Option<Kind> {
    let u = type_name.to_ascii_uppercase();
    Some(match u.as_str() {
        "LONGINT" | "SMALLINT" | "SHORTINT" | "BYTE" | "WORD" | "LONGWORD" | "INT64" | "QWORD"
        | "CARDINAL" | "INTEGER" | "UINT64" | "PTRINT" | "PTRUINT" | "SIZEINT" | "NATIVEINT"
        | "NATIVEUINT" => Kind::Integer,
        "DOUBLE" | "SINGLE" | "EXTENDED" | "REAL" | "COMP" | "CURRENCY" => Kind::Float,
        "BOOLEAN" | "BYTEBOOL" | "WORDBOOL" | "LONGBOOL" | "QWORDBOOL" => Kind::Boolean,
        "CHAR" | "ANSICHAR" | "WIDECHAR" => Kind::Char,
        "SHORTSTRING" | "ANSISTRING" | "STRING" | "UNICODESTRING" | "WIDESTRING" | "PCHAR"
        | "PANSICHAR" => Kind::String,
        "POINTER" => Kind::Pointer,
        _ if u.ends_with("(*)[]") || (u.ends_with(']') && u.contains('[')) => Kind::Array,
        _ => return None,
    })
}

/// Decides what a variable is from its type, or failing that from the shape of the value.
pub fn classify(type_name: &str, value: &str, reference: i64, deref: bool) -> Kind {
    if let Some(kind) = kind_by_type(type_name) {
        return kind;
    }
    if value.starts_with("<error") {
        // fpc describes sets in DWARF, which lldb cannot size.
        return if value.contains("could not get size") {
            Kind::Set
        } else {
            Kind::Other
        };
    }
    if reference > 0 && value.contains(" @ 0x") {
        Kind::Record
    } else if value.starts_with("0x") {
        if deref || reference == 0 {
            Kind::Pointer
        } else {
            Kind::Class
        }
    } else if reference == 0
        && !value.is_empty()
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !value.starts_with(|c: char| c.is_ascii_digit())
    {
        Kind::Enum
    } else {
        Kind::Other
    }
}

/// The value as Pascal writes it.
pub fn format_value(kind: Kind, type_name: &str, value: &str) -> String {
    match kind {
        Kind::Integer => match char_code(value) {
            // lldb shows one-byte integers (Byte, ShortInt) as characters.
            Some(c) if type_name.eq_ignore_ascii_case("SHORTINT") => (c as u8 as i8).to_string(),
            Some(c) => c.to_string(),
            None => value.to_string(),
        },
        Kind::Float => trim_float(value, type_name.eq_ignore_ascii_case("SINGLE")),
        Kind::Boolean => match value {
            "true" | "1" => "True".into(),
            "false" | "0" => "False".into(),
            other => other.to_string(),
        },
        Kind::Char => char_code(value).map_or_else(|| value.to_string(), pascal_char),
        Kind::String => {
            if value.starts_with('\'') {
                value.to_string()
            } else if let (Some(a), Some(b)) = (value.find('"'), value.rfind('"'))
                && b > a
            {
                pascal_string(&value[a..=b])
            } else if address(value).is_some() {
                // A nil string pointer is the empty string.
                "''".into()
            } else {
                value.to_string()
            }
        }
        Kind::Pointer | Kind::Class => match address(value) {
            Some(0) => "nil".into(),
            Some(a) => format!("${a:X}"),
            None => value.to_string(),
        },
        Kind::Record => "(...)".into(),
        Kind::Array => match element_count(type_name) {
            Some(n) => format!("{n} elements"),
            None => value.to_string(),
        },
        Kind::Enum | Kind::Set | Kind::Other => value.to_string(),
    }
}

/// `'a'` for printable ASCII, `#7` otherwise.
pub fn pascal_char(code: u32) -> String {
    match code {
        0x27 => "''''".into(),
        0x20..=0x7e => format!("'{}'", char::from_u32(code).unwrap_or('?')),
        _ => format!("#{code}"),
    }
}

/// The code of a character as lldb prints it: `'a'`, `'\a'`, `'\0'`, `'\xc8'`.
pub fn char_code(lldb: &str) -> Option<u32> {
    let inner = lldb
        .trim_start_matches(['u', 'U', 'L'])
        .strip_prefix('\'')?
        .strip_suffix('\'')?;
    let Some(esc) = inner.strip_prefix('\\') else {
        let mut chars = inner.chars();
        return chars
            .next()
            .filter(|_| chars.next().is_none())
            .map(u32::from);
    };
    if let Some(hex) = esc.strip_prefix('x') {
        return u32::from_str_radix(hex, 16).ok();
    }
    if esc.starts_with(|c: char| c.is_digit(8)) {
        return u32::from_str_radix(esc, 8).ok();
    }
    Some(match esc {
        "a" => 7,
        "b" => 8,
        "t" => 9,
        "n" => 10,
        "v" => 11,
        "f" => 12,
        "r" => 13,
        "e" => 27,
        "\\" | "'" | "\"" => esc.chars().next().map_or(0, u32::from),
        _ => return None,
    })
}

/// Shortest text that reads back as the same number, without lldb's `0.10000000000000001`.
pub fn trim_float(lldb: &str, single: bool) -> String {
    let long = |x: f64, text: String| {
        if x != 0.0 && !(1e-4..1e15).contains(&x.abs()) {
            format!("{x:e}")
        } else {
            text
        }
    };
    if single && let Ok(x) = lldb.parse::<f32>() {
        return if !x.is_finite() {
            lldb.to_string()
        } else if x != 0.0 && !(1e-4..1e15).contains(&x.abs()) {
            format!("{x:e}")
        } else {
            format!("{x}")
        };
    }
    match lldb.parse::<f64>() {
        Ok(x) if x.is_finite() => long(x, format!("{x}")),
        _ => lldb.to_string(),
    }
}

/// The first `0x...` token of `value` (lldb appends the pointed-to text for strings).
fn hex_token(value: &str) -> Option<&str> {
    let t = value.split_whitespace().next()?;
    (t.starts_with("0x") && t[2..].chars().all(|c| c.is_ascii_hexdigit())).then_some(t)
}

fn address(value: &str) -> Option<u64> {
    u64::from_str_radix(hex_token(value)?.trim_start_matches("0x"), 16).ok()
}

/// `LONGINT` for a dynamic array's `LONGINT (*)[]`.
fn dynamic_array_element(type_name: &str) -> Option<&str> {
    type_name.strip_suffix(" (*)[]")
}

/// The `3` of `LONGINT[3]`.
fn element_count(type_name: &str) -> Option<usize> {
    let (_, n) = type_name.strip_suffix(']')?.rsplit_once('[')?;
    n.parse().ok()
}

/// How the type is written in Pascal. lldb counts array elements from 0, whatever the declared
/// lower bound was, so fixed arrays are shown as `array[0..n-1]`.
pub fn display_type(name: &str) -> String {
    if let Some(elem) = dynamic_array_element(name) {
        format!("array of {}", pascal_type(elem))
    } else if let Some(n) = element_count(name)
        && let Some((elem, _)) = name.rsplit_once('[')
    {
        format!("array[0..{}] of {}", n.saturating_sub(1), pascal_type(elem))
    } else {
        pascal_type(name).to_string()
    }
}

/// lldb prints text as `"abc"`; Pascal writes `'abc'`.
pub fn pascal_string(lldb: &str) -> String {
    let inner = lldb.trim().trim_matches('"');
    format!("'{}'", inner.replace('\'', "''"))
}

/// Pascal's spelling of a type lldb reports in capitals.
pub fn pascal_type(name: &str) -> &str {
    match name {
        "LONGINT" => "LongInt",
        "SMALLINT" => "SmallInt",
        "SHORTINT" => "ShortInt",
        "BYTE" => "Byte",
        "WORD" => "Word",
        "LONGWORD" => "LongWord",
        "INT64" => "Int64",
        "QWORD" => "QWord",
        "BOOLEAN" => "Boolean",
        "CHAR" => "Char",
        "DOUBLE" => "Double",
        "SINGLE" => "Single",
        "EXTENDED" => "Extended",
        "SHORTSTRING" => "ShortString",
        "ANSISTRING" => "AnsiString",
        "UNICODESTRING" => "UnicodeString",
        "POINTER" => "Pointer",
        other => other,
    }
}

/// lldb reports identifiers in capitals. Recover the spelling used in `source`.
pub fn source_spelling(name: &str, source: &str) -> String {
    source
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|w| w.eq_ignore_ascii_case(name))
        .unwrap_or(name)
        .to_string()
}

/// [`source_spelling`] for every identifier in `text` (`array of TFOO`, `*TEXT`, `[0]`), leaving
/// the Pascal keywords and the words we wrote ourselves alone.
pub fn respell(text: &str, source: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if matches!(word.as_str(), "array" | "of" | "elements" | "nil") {
            out.push_str(word);
        } else if !word.is_empty() {
            out.push_str(&source_spelling(word, source));
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn text_of(body: &Value) -> Option<String> {
    body["output"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_pascal_values() {
        assert_eq!(pascal_string("\"Pascoda\""), "'Pascoda'");
        assert_eq!(pascal_string("\"it's\""), "'it''s'");
        assert_eq!(pascal_type("LONGINT"), "LongInt");
        assert_eq!(pascal_type("TFOO"), "TFOO");
        assert_eq!(source_spelling("TOTAL", "var i, Total: Integer;"), "Total");
        assert_eq!(source_spelling("NOPE", "var i: Integer;"), "NOPE");
    }

    #[test]
    fn respells_identifiers_in_text() {
        let src = "type TThing = class Id: LongInt; end;";
        assert_eq!(respell("array of TTHING", src), "array of TThing");
        assert_eq!(respell("*ID", src), "*Id");
        assert_eq!(respell("[0]", src), "[0]");
        assert_eq!(respell("nil", "var Nil: Integer;"), "nil");
    }

    #[test]
    fn reads_lldb_characters() {
        assert_eq!(char_code("'a'"), Some(97));
        assert_eq!(char_code("'\\a'"), Some(7));
        assert_eq!(char_code("'\\0'"), Some(0));
        assert_eq!(char_code("'\\xc8'"), Some(200));
        assert_eq!(char_code("'\\''"), Some(39));
        assert_eq!(char_code("42"), None);
        assert_eq!(pascal_char(97), "'a'");
        assert_eq!(pascal_char(39), "''''");
        assert_eq!(pascal_char(7), "#7");
        assert_eq!(pascal_char(200), "#200");
    }

    #[test]
    fn trims_floats() {
        assert_eq!(trim_float("2.5", false), "2.5");
        assert_eq!(trim_float("0.10000000000000001", false), "0.1");
        assert_eq!(trim_float("3", false), "3");
        assert_eq!(trim_float("0.10000000149011612", true), "0.1");
        assert_eq!(trim_float("1e-10", false), "1e-10");
        assert_eq!(trim_float("inf", false), "inf");
    }

    #[test]
    fn describes_types_and_addresses() {
        assert_eq!(display_type("LONGINT (*)[]"), "array of LongInt");
        assert_eq!(display_type("LONGINT[3]"), "array[0..2] of LongInt");
        assert_eq!(display_type("TTHING"), "TTHING");
        assert_eq!(address("0x0000000100140040 \"x\""), Some(0x1_0014_0040));
        assert_eq!(address("TFOO @ 0x1"), None);
    }

    fn var(ty: &str, value: &str, reference: i64) -> Variable {
        Variable {
            name: "V".into(),
            value: value.into(),
            type_name: ty.into(),
            reference,
        }
    }

    #[test]
    fn presents_what_lldb_reports() {
        let show = |ty, value: &str, reference, deref| {
            let v = present(var(ty, value, reference), deref);
            (v.kind, v.value, v.reference)
        };
        let t = |k, v: &str, r| (k, v.to_string(), r);
        assert_eq!(
            show("BOOLEAN", "true", 0, false),
            t(Kind::Boolean, "True", 0)
        );
        assert_eq!(
            show("BYTE", "'\\xc8'", 0, false),
            t(Kind::Integer, "200", 0)
        );
        assert_eq!(
            show("SHORTINT", "'\\xfe'", 0, false),
            t(Kind::Integer, "-2", 0)
        );
        assert_eq!(show("CHAR", "'\\a'", 0, false), t(Kind::Char, "#7", 0));
        assert_eq!(show("TCOLOR", "GREEN", 0, false), t(Kind::Enum, "GREEN", 0));
        let set = "<error: could not get size of type TCOLORS>";
        assert_eq!(show("TCOLORS", set, 0, false).0, Kind::Set);
        let rec = show("TPOINT2", "TPOINT2 @ 0x10003cc0c", 11, false);
        assert_eq!(rec, t(Kind::Record, "(...)", 11));
        let class = show("TTHING", "0x0000000100140040", 8, false);
        assert_eq!(class, t(Kind::Class, "$100140040", 8));
        let nil = show("TTHING", "0x0000000000000000", 16, false);
        assert_eq!(nil, t(Kind::Class, "nil", 0));
        let typed = show("PTHING", "0x0000000100140040", 8, true);
        assert_eq!(typed, t(Kind::Pointer, "$100140040", 8));
        let raw = show("POINTER", "0x000000010003cbe8", 19, false);
        assert_eq!(raw, t(Kind::Pointer, "$10003CBE8", 0));
        let text = show("ANSISTRING", "0x000000010002d2f0 \"Hello\"", 28, false);
        assert_eq!(text, t(Kind::String, "'Hello'", 0));
        let empty = show("ANSISTRING", "0x0000000000000000", 17, false);
        assert_eq!(empty, t(Kind::String, "''", 0));
        let fixed = show("LONGINT[3]", "LONGINT[3] @ 0x10003cd30", 4, false);
        assert_eq!(fixed, t(Kind::Array, "3 elements", 4));
    }

    const WAIT: Duration = Duration::from_secs(30);

    fn next_stop(handle: &Handle) -> Paused {
        loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Stopped(p) => return p,
                Event::Ended(why) => panic!("ended early: {why}"),
                Event::Output(_) | Event::Children(..) => {}
            }
        }
    }

    fn find<'a>(p: &'a Paused, name: &str) -> &'a Var {
        p.vars
            .iter()
            .flat_map(|(_, v)| v)
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("variable {name}"))
    }

    /// Drives the debugger thread the way the UI does: breakpoint, inspect, resume, finish.
    #[test]
    fn stops_at_breakpoint_then_runs_to_end() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug.pas")
            .canonicalize()
            .unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);

        let handle = spawn(
            src.with_extension(""),
            src.parent().unwrap().to_path_buf(),
            vec![(src.clone(), vec![10])],
            egui::Context::default(),
        );
        let paused = next_stop(&handle);
        assert_eq!(paused.location(), Some((src.as_path(), 10)));
        assert_eq!(find(&paused, "TOTAL").value, "6");
        assert_eq!(find(&paused, "NAME").value, "'Pascora'");

        handle.cmds.send(Cmd::Resume).unwrap();
        loop {
            if let Event::Ended(_) = handle.events.recv_timeout(WAIT).expect("debugger event") {
                break;
            }
        }
    }

    /// Every kind of value in `debug_types.pas` as the pane gets it, and opening the structured
    /// ones: on demand, and kept open across a step.
    #[test]
    fn formats_every_kind_of_value() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug_types.pas")
            .canonicalize()
            .unwrap();
        // Build products go to the scratch folder, not into the fixtures.
        let work = crate::target_tmp().join("debug_types");
        std::fs::create_dir_all(&work).unwrap();
        let copy = work.join("debug_types.pas");
        let text = std::fs::read_to_string(&src).unwrap();
        std::fs::write(&copy, &text).unwrap();
        let built = pascoda_fpc::build(&copy, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let line = text.lines().position(|l| l.contains("WriteLn")).unwrap() as u32 + 1;
        let handle = spawn(
            copy.with_extension(""),
            work.clone(),
            vec![(copy.clone(), vec![line])],
            egui::Context::default(),
        );
        let mut paused = next_stop(&handle);

        let scalar = |name: &str| {
            let v = find(&paused, name);
            (v.kind, v.value.clone(), v.type_name.clone())
        };
        let s = |k, v: &str, t: &str| (k, v.to_string(), t.to_string());
        assert_eq!(scalar("COUNT"), s(Kind::Integer, "42", "LongInt"));
        assert_eq!(scalar("SMALL"), s(Kind::Integer, "200", "Byte"));
        assert_eq!(scalar("RATIO"), s(Kind::Float, "2.5", "Double"));
        assert_eq!(scalar("SINGLE1"), s(Kind::Float, "0.125", "Single"));
        assert_eq!(scalar("FLAG"), s(Kind::Boolean, "True", "Boolean"));
        assert_eq!(scalar("OFF"), s(Kind::Boolean, "False", "Boolean"));
        assert_eq!(scalar("LETTER"), s(Kind::Char, "'a'", "Char"));
        assert_eq!(scalar("BELL"), s(Kind::Char, "#7", "Char"));
        assert_eq!(scalar("COLOR"), s(Kind::Enum, "GREEN", "TCOLOR"));
        assert_eq!(
            scalar("TEXT"),
            s(Kind::String, "'Pascal text'", "AnsiString")
        );
        assert_eq!(scalar("EMPTY"), s(Kind::String, "''", "AnsiString"));
        assert_eq!(scalar("NOTHING"), s(Kind::Class, "nil", "TTHING"));
        assert_eq!(
            scalar("NUMBERS"),
            s(Kind::Array, "3 elements", "array of LongInt")
        );
        assert_eq!(
            scalar("FIXED"),
            s(Kind::Array, "3 elements", "array[0..2] of LongInt")
        );
        assert_eq!(scalar("POINT"), s(Kind::Record, "(...)", "TPOINT2"));
        // lldb cannot size Pascal sets: the raw error stays.
        let colors = find(&paused, "COLORS");
        assert_eq!(colors.kind, Kind::Set);
        assert!(colors.value.starts_with("<error"), "{}", colors.value);
        let ptr = find(&paused, "PTR");
        assert!(ptr.kind == Kind::Pointer && ptr.value.starts_with('$'));
        assert_eq!(find(&paused, "NOTHING").reference, 0);
        let globals = &paused.vars.iter().find(|(s, _)| s == "Globals").unwrap().1;
        let names: Vec<_> = globals.iter().map(|v| v.name.to_lowercase()).collect();
        assert!(names.windows(2).all(|w| w[0] <= w[1]), "{names:?}");

        // Open structured values on demand.
        let kids = |paused: &mut Paused, path: &str| {
            let reference = paused.var_mut(path).unwrap().reference;
            assert!(reference > 0, "{path} is not expandable");
            handle
                .cmds
                .send(Cmd::Fetch(path.into(), reference))
                .unwrap();
            let got = loop {
                match handle.events.recv_timeout(WAIT).expect("event") {
                    Event::Children(p, kids) if p == path => break kids,
                    Event::Ended(why) => panic!("ended: {why}"),
                    _ => {}
                }
            };
            paused.var_mut(path).unwrap().children = Some(got.clone());
            got.iter()
                .map(|k| format!("{}={}", k.name, k.value))
                .collect::<Vec<_>>()
        };
        let point = kids(&mut paused, "Globals/POINT");
        assert_eq!(point, ["X=3", "Y=-4", "TAG='origin'"]);
        let thing = kids(&mut paused, "Globals/THING");
        assert_eq!(thing, ["ID=7", "TITLE='Hello'"]);
        let numbers = kids(&mut paused, "Globals/NUMBERS");
        assert_eq!(numbers, ["[0]=10", "[1]=20", "[2]=30"]);
        let fixed = kids(&mut paused, "Globals/FIXED");
        assert_eq!(fixed, ["[0]=1", "[1]=2", "[2]=3"]);

        // Open paths are fetched again after a step.
        let open: HashSet<String> = ["Globals/POINT", "Globals/THING"].map(String::from).into();
        handle.cmds.send(Cmd::Expanded(open)).unwrap();
        handle.cmds.send(Cmd::Step(Step::Over)).unwrap();
        let mut again = next_stop(&handle);
        let thing = again.var_mut("Globals/THING").unwrap();
        assert_eq!(thing.children.as_ref().map(Vec::len), Some(2));
        assert!(
            again
                .var_mut("Globals/POINT/TAG")
                .is_some_and(|v| v.value == "'origin'")
        );
        assert!(again.var_mut("Globals/NUMBERS").unwrap().children.is_none());

        handle.cmds.send(Cmd::Stop).unwrap();
        let _ = std::fs::remove_dir_all(&work);
    }
}
