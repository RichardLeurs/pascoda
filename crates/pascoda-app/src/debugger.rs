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
    Breakpoints(PathBuf, Vec<pascoda_dap::Breakpoint>),
    /// The variable paths the UI shows open; they are fetched again at every stop.
    Expanded(HashSet<String>),
    /// Children of the variable at `path` (`reference` as in [`Var::reference`]) are wanted.
    Fetch(String, i64),
    /// The watch expressions (Pascal text); their values are sent at every stop, and at once when
    /// the program is stopped.
    Watches(Vec<String>),
    /// Evaluates one expression in the stopped program's frame, once.
    Evaluate(String),
    /// Assigns `value` to the variable `target` (both lldb expressions) while the program is stopped.
    /// The variables are read again afterwards.
    SetValue(String, String),
    /// Stops (or stops no longer) where a Pascal exception is raised.
    ExceptionStops(bool),
    /// Runs until the program reaches `line` (1-based) of `file`, then stops there (a breakpoint
    /// that is only there for this run; the user's breakpoints stay as they were).
    RunTo(PathBuf, u32),
}

pub enum Event {
    Stopped(Paused),
    /// The children of the variable at the path, answering [`Cmd::Fetch`].
    Children(String, Vec<Var>),
    /// The values of the watch expressions, as the `Watches` scope of the variables, answering
    /// [`Cmd::Watches`] while the program is stopped.
    Watched(Vec<Var>),
    /// The result of [`Cmd::SetValue`]: done, or the error lldb gave.
    Assigned(Result<(), String>),
    /// The variables after an assignment, replacing the ones shown.
    Refreshed(Vec<(String, Vec<Var>)>),
    /// The result of [`Cmd::Evaluate`]: the expression, and its value or the error lldb gave.
    Evaluated(String, Result<Var, String>),
    Output(String),
    Ended(String),
}

/// The scope the watch values are listed under in the variables.
pub const WATCH_SCOPE: &str = "Watches";

/// Turns a Pascal expression into the C-style one lldb evaluates (lldb has no Pascal parser). Names
/// become upper case, as the debug information spells them; `=`, `<>`, `and`, `or`, `not`, `div`
/// and `mod` become their C operators; `true` and `false` become 1 and 0. String literals are kept.
pub fn lldb_expression(pascal: &str) -> String {
    let chars: Vec<char> = pascal.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut prev = ' ';
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // A string literal; a doubled quote inside it is one quote.
            out.push('\'');
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        out.push_str("\\'");
                        i += 2;
                        continue;
                    }
                    out.push('\'');
                    i += 1;
                    break;
                }
                out.push(chars[i]);
                i += 1;
            }
            prev = '\'';
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            match word.to_ascii_lowercase().as_str() {
                "and" => out.push_str("&&"),
                "or" => out.push_str("||"),
                "not" => out.push('!'),
                "div" => out.push('/'),
                "mod" => out.push('%'),
                "true" => out.push('1'),
                "false" => out.push('0'),
                _ => out.push_str(&word.to_uppercase()),
            }
            prev = 'a';
            continue;
        }
        let next = chars.get(i + 1).copied();
        match c {
            '<' if next == Some('>') => {
                out.push_str("!=");
                i += 2;
                prev = '=';
                continue;
            }
            // `=` alone is equality; `<=`, `>=` and `:=` keep their `=`.
            '=' if !matches!(prev, '<' | '>' | ':' | '!' | '=') => out.push_str("=="),
            _ => out.push(c),
        }
        prev = c;
        i += 1;
    }
    out
}

/// The error an evaluation gave, without lldb's notes and the echoed expression: `use of undeclared
/// identifier 'NOSUCH'` for an unknown name.
pub fn clean_error(raw: &str) -> String {
    let message = raw
        .lines()
        .find_map(|l| l.find("error: ").map(|i| &l[i + "error: ".len()..]))
        .map(|m| match m.split_once(": ") {
            Some((place, rest)) if place.starts_with("<user expression") => rest,
            _ => m,
        })
        .unwrap_or_else(|| raw.lines().next().unwrap_or_default().trim_start_matches("evaluate: "));
    message.trim().to_string()
}

/// The watch values for `exprs` in `frame`: each evaluated, with its children when they are open.
/// A value that cannot be evaluated shows the error in place of the value.
fn watch_vars(dbg: &mut Session, frame: i64, exprs: &[String], expanded: &HashSet<String>) -> Vec<Var> {
    exprs
        .iter()
        .map(|expr| match dbg.evaluate(frame, &lldb_expression(expr)) {
            Ok(mut v) => {
                v.name = expr.clone();
                build(dbg, frame, v, WATCH_SCOPE, expanded)
            }
            Err(e) => Var {
                name: expr.clone(),
                type_name: String::new(),
                value: clean_error(&e),
                kind: Kind::Other,
                reference: 0,
                children: None,
            },
        })
        .collect()
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

/// The condition and hit condition of a breakpoint as the user wrote them: a Pascal condition, and a
/// hit condition (`== 2`, `>= 3`). Both empty for a plain breakpoint.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Stop {
    pub condition: String,
    pub hits: String,
}

/// The breakpoints of a file for the debugger: each line of `lines` that is not `disabled`, with its
/// condition translated for lldb (`lldb_expression`) and its hit condition as written.
pub fn breakpoint_specs(
    lines: &std::collections::BTreeSet<u32>,
    stops: &std::collections::BTreeMap<u32, Stop>,
    disabled: &std::collections::BTreeSet<u32>,
) -> Vec<pascoda_dap::Breakpoint> {
    lines
        .iter()
        .filter(|line| !disabled.contains(line))
        .map(|&line| {
            let stop = stops.get(&line).cloned().unwrap_or_default();
            pascoda_dap::Breakpoint {
                line,
                condition: Some(stop.condition.trim().to_string()).filter(|c| !c.is_empty()).map(|c| lldb_expression(&c)),
                hit_condition: Some(stop.hits.trim().to_string()).filter(|h| !h.is_empty()),
            }
        })
        .collect()
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

    /// Replaces the variables listed under `scope`, adding the scope first when it is not there.
    pub fn set_scope(&mut self, scope: &str, vars: Vec<Var>) {
        match self.vars.iter().position(|(s, _)| s == scope) {
            Some(i) => self.vars[i].1 = vars,
            None => self.vars.insert(0, (scope.to_string(), vars)),
        }
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
    args: Vec<String>,
    breakpoints: Vec<(PathBuf, Vec<pascoda_dap::Breakpoint>)>,
    ctx: egui::Context,
) -> Handle {
    let (cmds, cmd_rx) = channel();
    let (event_tx, events) = channel();
    std::thread::spawn(move || {
        let reason = match drive(&exe, &cwd, &args, &breakpoints, &cmd_rx, &event_tx, &ctx) {
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
    args: &[String],
    breakpoints: &[(PathBuf, Vec<pascoda_dap::Breakpoint>)],
    cmds: &Receiver<Cmd>,
    out: &Sender<Event>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let adapter = find_lldb_dap().ok_or("lldb-dap not found (install Xcode)")?;
    let mut dbg = Session::start(&adapter).map_err(|e| e.to_string())?;
    dbg.launch(exe, cwd, args, breakpoints)?;
    let mut thread = 0;
    // Variable references are valid only while stopped, in the frame we listed.
    let mut stopped_in: Option<i64> = None;
    let mut expanded = HashSet::new();
    let send = |e: Event| {
        let _ = out.send(e);
        ctx.request_repaint();
    };

    // The watch expressions, set by the UI.
    let mut watches: Vec<String> = Vec::new();
    // The user's breakpoints per file (what the UI set), and the run-to target while it runs.
    let mut user_bps: std::collections::HashMap<PathBuf, Vec<pascoda_dap::Breakpoint>> =
        breakpoints.iter().cloned().collect();
    let mut run_to: Option<(PathBuf, u32)> = None;

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
                Cmd::Breakpoints(file, lines) => {
                    let mut all = lines.clone();
                    if let Some((target_file, target)) = &run_to
                        && *target_file == file
                    {
                        all.push(pascoda_dap::Breakpoint::at(*target));
                    }
                    user_bps.insert(file.clone(), lines);
                    dbg.set_breakpoints(&file, &all)?
                }
                Cmd::RunTo(file, line) => match stopped_in {
                    Some(_) => {
                        let mut lines = user_bps.get(&file).cloned().unwrap_or_default();
                        lines.push(pascoda_dap::Breakpoint::at(line));
                        dbg.set_breakpoints(&file, &lines)?;
                        run_to = Some((file, line));
                        stopped_in = None;
                        dbg.resume(thread)?;
                    }
                    None => send(Event::Output("Run to Cursor needs the program stopped\n".into())),
                },
                Cmd::Expanded(paths) => expanded = paths,
                Cmd::Fetch(path, reference) => {
                    if let Some(frame) = stopped_in {
                        let kids = children_of(&mut dbg, frame, reference, &path, &expanded)
                            .unwrap_or_default();
                        send(Event::Children(path, kids));
                    }
                }
                Cmd::Watches(list) => {
                    watches = list;
                    if let Some(frame) = stopped_in {
                        let values = watch_vars(&mut dbg, frame, &watches, &expanded);
                        send(Event::Watched(values));
                    }
                }
                Cmd::ExceptionStops(on) => {
                    // Free Pascal raises every exception through this routine.
                    let names: &[&str] = if on { &["FPC_RAISEEXCEPTION"] } else { &[] };
                    dbg.set_function_breakpoints(names)?;
                }
                Cmd::SetValue(target, value) => match stopped_in {
                    Some(frame) => match dbg.evaluate(frame, &format!("{target} = {value}")) {
                        Ok(_) => {
                            send(Event::Assigned(Ok(())));
                            let vars = read_vars(&mut dbg, frame, true, &expanded, &watches).unwrap_or_default();
                            send(Event::Refreshed(vars));
                        }
                        Err(e) => send(Event::Assigned(Err(clean_error(&e)))),
                    },
                    None => send(Event::Assigned(Err("the program is running".into()))),
                },
                Cmd::Evaluate(expr) => {
                    let result = match stopped_in {
                        Some(frame) => dbg
                            .evaluate(frame, &lldb_expression(&expr))
                            .map(|mut v| {
                                v.name = expr.clone();
                                show(&mut dbg, frame, v)
                            }),
                        None => Err("the program is running".to_string()),
                    };
                    send(Event::Evaluated(expr, result.map_err(|e| clean_error(&e))));
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
                // The run-to breakpoint is taken out again before anything else happens.
                if let Some((file, _)) = run_to.take() {
                    let lines = user_bps.get(&file).cloned().unwrap_or_default();
                    dbg.set_breakpoints(&file, &lines)?;
                }
                let frames = dbg.stack(thread)?;
                let top = frames.iter().find(|f| is_source(f)).or(frames.first());
                let frame = top.map_or(0, |f| f.id);
                let vars = read_vars(&mut dbg, frame, top.is_some(), &expanded, &watches)?;
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

/// The variables of `frame` by scope (none when there is no source frame), with the watches first
/// under their own scope. Open rows are filled in; globals are sorted by name.
fn read_vars(
    dbg: &mut Session,
    frame: i64,
    has_frame: bool,
    expanded: &HashSet<String>,
    watches: &[String],
) -> Result<Vec<(String, Vec<Var>)>, String> {
    let raw = if has_frame { dbg.variables(frame)? } else { Vec::new() };
    let mut vars: Vec<(String, Vec<Var>)> = raw
        .into_iter()
        .filter(|(scope, _)| scope != "Registers")
        .map(|(scope, vars)| {
            let mut vars: Vec<Var> = vars
                .into_iter()
                .map(|v| build(dbg, frame, v, &scope, expanded))
                .collect();
            if scope == "Globals" {
                // lldb lists globals in no particular order.
                vars.sort_by_key(|v| v.name.to_lowercase());
            }
            (scope, vars)
        })
        .collect();
    if !watches.is_empty() {
        let values = watch_vars(dbg, frame, watches, expanded);
        vars.insert(0, (WATCH_SCOPE.to_string(), values));
    }
    Ok(vars)
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
                Event::Output(_)
                | Event::Children(..)
                | Event::Watched(_)
                | Event::Evaluated(..)
                | Event::Assigned(_)
                | Event::Refreshed(_) => {}
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
            vec![],
            vec![(src.clone(), vec![pascoda_dap::Breakpoint::at(10)])],
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

    /// Run to cursor stops at a line inside a loop once, and the line is not a breakpoint afterwards:
    /// the program runs to its end.
    #[test]
    fn run_to_stops_at_the_line_once_and_leaves_the_breakpoints_alone() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug.pas").canonicalize().unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);

        let handle = spawn(
            src.with_extension(""),
            src.parent().unwrap().to_path_buf(),
            vec![],
            vec![(src.clone(), vec![pascoda_dap::Breakpoint::at(6)])],
            egui::Context::default(),
        );
        let paused = next_stop(&handle);
        assert_eq!(paused.location(), Some((src.as_path(), 6)));

        // Line 9 is in the loop: it would stop three times if it stayed a breakpoint.
        handle.cmds.send(Cmd::RunTo(src.clone(), 9)).unwrap();
        let paused = next_stop(&handle);
        assert_eq!(paused.location(), Some((src.as_path(), 9)));
        assert_eq!(find(&paused, "TOTAL").value, "0", "the first time through the loop");

        handle.cmds.send(Cmd::Resume).unwrap();
        loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Ended(_) => break,
                Event::Stopped(p) => panic!("stopped again at {:?}: the run-to line stayed", p.location()),
                _ => {}
            }
        }
    }

    /// The program gets the run arguments and the run folder, under the debugger as under Run.
    #[test]
    fn the_debugged_program_gets_its_arguments_and_folder() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("pascoda-debug-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let src = dir.join("argsdemo.pas");
        std::fs::write(
            &src,
            "program argsdemo;\nuses SysUtils;\nbegin\n  WriteLn(ParamStr(1), '|', GetCurrentDir);\nend.\n",
        )
        .unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let handle = spawn(
            src.with_extension(""),
            dir.join("data"),
            vec!["hello there".into()],
            vec![],
            egui::Context::default(),
        );
        let mut output = String::new();
        loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Output(text) => output.push_str(&text),
                Event::Ended(_) => break,
                _ => {}
            }
        }
        let data = dir.join("data").canonicalize().unwrap();
        assert!(output.contains(&format!("hello there|{}", data.display())), "{output}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A condition stops the program only when it holds, and a hit count stops it from that hit on: line
    /// 9 runs three times, and each case stops on the passes it should.
    #[test]
    fn a_condition_and_a_hit_condition_decide_when_a_breakpoint_stops() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug.pas").canonicalize().unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let stops_for = |bp: pascoda_dap::Breakpoint| {
            let handle = spawn(
                src.with_extension(""),
                src.parent().unwrap().to_path_buf(),
                vec![],
                vec![(src.clone(), vec![bp])],
                egui::Context::default(),
            );
            let mut stops = Vec::new();
            loop {
                match handle.events.recv_timeout(WAIT).expect("debugger event") {
                    Event::Stopped(p) => {
                        stops.push(find(&p, "I").value.clone());
                        handle.cmds.send(Cmd::Resume).unwrap();
                    }
                    Event::Ended(_) => break stops,
                    _ => {}
                }
            }
        };
        let by_condition = stops_for(pascoda_dap::Breakpoint {
            line: 9,
            condition: Some(lldb_expression("i = 2")),
            hit_condition: None,
        });
        assert_eq!(by_condition, ["2"], "only the pass where i = 2");
        // lldb-dap reads a plain number as "from that hit on": 3 is the third pass, 2 the second and third.
        let from_third = stops_for(pascoda_dap::Breakpoint {
            line: 9,
            condition: None,
            hit_condition: Some("3".into()),
        });
        assert_eq!(from_third, ["3"], "from the third hit on");
        let from_second = stops_for(pascoda_dap::Breakpoint {
            line: 9,
            condition: None,
            hit_condition: Some("2".into()),
        });
        assert_eq!(from_second, ["2", "3"], "from the second hit on");
    }

    /// A value changed while the program is stopped is the new value: the variables show it, and the
    /// program carries on with it (line 10 prints the total).
    #[test]
    fn a_value_can_be_changed_while_stopped() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug.pas").canonicalize().unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let handle = spawn(
            src.with_extension(""),
            src.parent().unwrap().to_path_buf(),
            vec![],
            vec![(src.clone(), vec![pascoda_dap::Breakpoint::at(10)])],
            egui::Context::default(),
        );
        assert_eq!(find(&next_stop(&handle), "TOTAL").value, "6");

        handle.cmds.send(Cmd::SetValue("TOTAL".into(), "9".into())).unwrap();
        let mut refreshed = None;
        let mut assigned = None;
        while refreshed.is_none() || assigned.is_none() {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Assigned(r) => assigned = Some(r),
                Event::Refreshed(vars) => refreshed = Some(vars),
                Event::Ended(why) => panic!("ended: {why}"),
                _ => {}
            }
        }
        assert_eq!(assigned.unwrap(), Ok(()));
        let shown = refreshed
            .unwrap()
            .into_iter()
            .flat_map(|(_, vars)| vars)
            .find(|v| v.name == "TOTAL")
            .expect("TOTAL is listed");
        assert_eq!(shown.value, "9", "the variables show the new value");

        handle.cmds.send(Cmd::Resume).unwrap();
        let mut output = String::new();
        loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Output(text) => output.push_str(&text),
                Event::Ended(_) => break,
                _ => {}
            }
        }
        assert!(output.contains("Pascora 9"), "the program prints the new total: {output}");
    }

    /// A name that is not a variable is refused, with lldb's reason, and the program keeps running.
    #[test]
    fn a_change_to_an_unknown_name_is_refused() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug.pas").canonicalize().unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let handle = spawn(
            src.with_extension(""),
            src.parent().unwrap().to_path_buf(),
            vec![],
            vec![(src.clone(), vec![pascoda_dap::Breakpoint::at(10)])],
            egui::Context::default(),
        );
        next_stop(&handle);
        handle.cmds.send(Cmd::SetValue("NOSUCH".into(), "1".into())).unwrap();
        let result = loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Assigned(r) => break r,
                Event::Ended(why) => panic!("ended: {why}"),
                _ => {}
            }
        };
        assert_eq!(result, Err("use of undeclared identifier 'NOSUCH'".to_string()));
        handle.cmds.send(Cmd::Stop).unwrap();
    }

    /// With exception stops on, the program stops on the line that raises a Pascal exception, and
    /// carries on when it is resumed. Without them it runs past the raise.
    #[test]
    fn exception_stops_land_on_the_raising_line() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("pascoda-exc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonical, as the debugger reports paths (`/private/var` for the temp folder on macOS).
        let dir = dir.canonicalize().unwrap();
        let src = dir.join("boom.pas");
        std::fs::write(
            &src,
            "program boom;\nuses SysUtils;\nprocedure Fail;\nbegin\n  raise Exception.Create('bad thing');\nend;\nbegin\n  try\n    Fail;\n  except\n    on E: Exception do WriteLn('caught ', E.Message);\n  end;\n  WriteLn('done');\nend.\n",
        )
        .unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::ObjFpc).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let handle = spawn(
            src.with_extension(""),
            dir.clone(),
            vec![],
            vec![],
            egui::Context::default(),
        );
        handle.cmds.send(Cmd::ExceptionStops(true)).unwrap();
        let paused = next_stop(&handle);
        assert_eq!(paused.location(), Some((src.as_path(), 5)), "the raise");
        handle.cmds.send(Cmd::Resume).unwrap();
        let mut output = String::new();
        loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Output(text) => output.push_str(&text),
                Event::Stopped(_) => panic!("stopped again after the raise"),
                Event::Ended(_) => break,
                _ => {}
            }
        }
        assert!(output.contains("caught bad thing"), "{output}");

        // Without exception stops the same program runs past the raise without stopping.
        let plain = spawn(src.with_extension(""), dir.clone(), vec![], vec![], egui::Context::default());
        let mut output = String::new();
        loop {
            match plain.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Output(text) => output.push_str(&text),
                Event::Stopped(_) => panic!("stopped without exception stops on"),
                Event::Ended(_) => break,
                _ => {}
            }
        }
        assert!(output.contains("caught bad thing"), "{output}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Watches are listed under their own scope at each stop, can change while stopped, and a
    /// one-off evaluation gives a value or lldb's error.
    #[test]
    fn watches_are_listed_at_each_stop_and_evaluated_on_demand() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/debug.pas").canonicalize().unwrap();
        let built = pascoda_fpc::build(&src, pascoda_fpc::Mode::default()).unwrap();
        assert!(built.success, "{}", built.raw_output);

        let handle = spawn(
            src.with_extension(""),
            src.parent().unwrap().to_path_buf(),
            vec![],
            vec![(src.clone(), vec![pascoda_dap::Breakpoint::at(10)])],
            egui::Context::default(),
        );
        handle
            .cmds
            .send(Cmd::Watches(vec!["total".into(), "name".into()]))
            .unwrap();
        let paused = next_stop(&handle);
        let (scope, watched) = &paused.vars[0];
        assert_eq!(scope, WATCH_SCOPE);
        assert_eq!(watched.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["total", "name"]);
        assert_eq!(watched[0].value, "6");
        assert_eq!(watched[1].value, "'Pascora'");

        // A watch that cannot be evaluated shows lldb's message as its value.
        handle.cmds.send(Cmd::Watches(vec!["total".into(), "nosuch".into()])).unwrap();
        let values = loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Watched(v) => break v,
                Event::Ended(why) => panic!("ended: {why}"),
                _ => {}
            }
        };
        assert_eq!(values[0].value, "6");
        assert_eq!(values[1].value, "use of undeclared identifier 'NOSUCH'", "{:?}", values[1]);

        // One-off evaluation, Pascal spelling and case.
        handle.cmds.send(Cmd::Evaluate("total * 2".into())).unwrap();
        let result = loop {
            match handle.events.recv_timeout(WAIT).expect("debugger event") {
                Event::Evaluated(expr, r) => break (expr, r),
                Event::Ended(why) => panic!("ended: {why}"),
                _ => {}
            }
        };
        assert_eq!(result.0, "total * 2");
        assert_eq!(result.1.expect("evaluates").value, "12");

        handle.cmds.send(Cmd::Stop).unwrap();
    }

    /// Every kind of value in `debug_types.pas` as the pane gets it, and opening the structured
    /// ones: on demand, and kept open across a step.
    #[test]
    fn cleans_lldb_errors_for_the_pane() {
        let raw = "evaluate: Expression evaluation in ISO Pascal not supported. Falling back to default language. Ran expression as 'Objective C++'.\nerror: <user expression 0>:1:1: use of undeclared identifier 'NOSUCH'\n    1 | NOSUCH\n      | ^~~~~~\n";
        assert_eq!(clean_error(raw), "use of undeclared identifier 'NOSUCH'");
        assert_eq!(clean_error("evaluate: no frame"), "no frame");
    }

    #[test]
    fn translates_pascal_expressions_for_lldb() {
        assert_eq!(lldb_expression("total + i"), "TOTAL + I");
        assert_eq!(lldb_expression("total = 3"), "TOTAL == 3");
        assert_eq!(lldb_expression("a <> b and not c"), "A != B && ! C");
        assert_eq!(lldb_expression("x >= 2"), "X >= 2");
        assert_eq!(lldb_expression("x <= 2 or y := 1"), "X <= 2 || Y := 1");
        assert_eq!(lldb_expression("x mod 2 div 3"), "X % 2 / 3");
        assert_eq!(lldb_expression("name = 'a = b'"), "NAME == 'a = b'", "string literals are kept");
        assert_eq!(lldb_expression("c = 'it''s'"), "C == 'it\\'s'");
        assert_eq!(lldb_expression("flag = true"), "FLAG == 1");
    }

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
            vec![],
            vec![(copy.clone(), vec![pascoda_dap::Breakpoint::at(line)])],
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
