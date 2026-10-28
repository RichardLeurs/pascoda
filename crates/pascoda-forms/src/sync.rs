//! Keeping a form's Pascal unit in step with its `.lfm`.
//!
//! The designer follows the Lazarus/Delphi contract: it edits only the class's *default
//! section* (from the class header to the first `private`/`protected`/`public`/`published`),
//! adds empty handler stubs, and never touches code the user wrote. It uses a small tolerant
//! scanner, not a full Pascal parser, and refuses when it cannot find its place unambiguously.

use crate::model::{Component, PropValue};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// The unit has no `TName = class(...)` declaration in its interface section.
    NoClass(String),
    NoImplementation,
    /// Something in the unit that the scanner will not guess about.
    Unsupported(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::NoClass(c) => {
                write!(f, "class {c} not found in the unit's interface section")
            }
            SyncError::NoImplementation => write!(f, "the unit has no `implementation` section"),
            SyncError::Unsupported(why) => write!(f, "cannot update the unit safely: {why}"),
        }
    }
}

impl std::error::Error for SyncError {}

/// Edits to apply, and things the designer left alone on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPlan {
    pub edits: Vec<TextEdit>,
    pub skipped: Vec<String>,
}

/// Applies non-overlapping edits to `source`.
pub fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.range.end)));
    let mut out = source.to_string();
    for e in sorted {
        out.replace_range(e.range.clone(), &e.text);
    }
    out
}

// ---- what the form needs ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handler {
    pub name: String,
    /// Parameter list without parentheses, e.g. `Sender: TObject`.
    pub params: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    /// `(component name, class)` for every named component below the form.
    pub fields: Vec<(String, String)>,
    pub handlers: Vec<Handler>,
}

/// The parameters Lazarus gives common events; anything unknown gets `Sender: TObject`.
pub fn event_params(event: &str) -> &'static str {
    match event {
        "OnKeyDown" | "OnKeyUp" => "Sender: TObject; var Key: Word; Shift: TShiftState",
        "OnKeyPress" => "Sender: TObject; var Key: char",
        "OnMouseDown" | "OnMouseUp" => {
            "Sender: TObject; Button: TMouseButton; Shift: TShiftState; X, Y: Integer"
        }
        "OnMouseMove" => "Sender: TObject; Shift: TShiftState; X, Y: Integer",
        "OnClose" => "Sender: TObject; var CloseAction: TCloseAction",
        "OnCloseQuery" => "Sender: TObject; var CanClose: Boolean",
        _ => "Sender: TObject",
    }
}

/// The parameters for `event` of a component of `class`; `is_form` for the form itself. Some
/// event names have another type on non-visual components than on the form or controls.
pub fn handler_params(class: &str, event: &str, is_form: bool) -> &'static str {
    let class = class.to_ascii_lowercase();
    match event {
        // TCommonDialog and TPopupMenu: a plain TNotifyEvent.
        "OnClose" if !is_form => "Sender: TObject",
        "OnExecute" | "OnUpdate" if class == "tactionlist" => {
            "AAction: TBasicAction; var Handled: Boolean"
        }
        "OnChange" if class == "tmainmenu" || class == "tpopupmenu" => {
            "Sender: TObject; Source: TMenuItem; Rebuild: Boolean"
        }
        _ => event_params(event),
    }
}

/// Reads the fields and event handlers a form file calls for.
pub fn wanted(form: &Component) -> Wanted {
    fn walk(c: &Component, w: &mut Wanted, is_root: bool) {
        if !is_root && !c.name.is_empty() {
            w.fields.push((c.name.clone(), c.class.clone()));
        }
        for p in &c.props {
            let is_event = p.name.starts_with("On") && !p.name.contains('.');
            if let (true, PropValue::Ident(h)) = (is_event, &p.value)
                && !w.handlers.iter().any(|x| x.name.eq_ignore_ascii_case(h))
            {
                w.handlers.push(Handler {
                    name: h.clone(),
                    params: handler_params(&c.class, &p.name, is_root).into(),
                });
            }
        }
        for child in &c.children {
            walk(child, w, false);
        }
    }
    let mut w = Wanted::default();
    walk(form, &mut w, true);
    w
}

// ---- scanning --------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Word,
    Sym(u8),
    Str,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

/// Splits Pascal source into words, symbols and strings, skipping comments and directives.
fn tokenize(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'{' {
            while i < b.len() && b[i] != b'}' {
                i += 1;
            }
            i = (i + 1).min(b.len());
        } else if c == b'(' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b')')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != b'\n' {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            toks.push(Tok {
                kind: Kind::Str,
                start,
                end: i,
            });
        } else if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            toks.push(Tok {
                kind: Kind::Word,
                start,
                end: i,
            });
        } else {
            i += 1;
            toks.push(Tok {
                kind: Kind::Sym(c),
                start,
                end: i,
            });
        }
    }
    toks
}

struct Scan<'a> {
    src: &'a str,
    toks: Vec<Tok>,
}

impl<'a> Scan<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            toks: tokenize(src),
        }
    }

    fn text(&self, i: usize) -> &'a str {
        let t = self.toks[i];
        &self.src[t.start..t.end]
    }

    fn is_word(&self, i: usize, w: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == Kind::Word)
            && self.text(i).eq_ignore_ascii_case(w)
    }

    fn is_sym(&self, i: usize, c: u8) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == Kind::Sym(c))
    }

    fn line_start(&self, pos: usize) -> usize {
        self.src[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    fn nl(&self) -> &'static str {
        if self.src.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// `pos` moved past any comments that trail it on the same line, so text inserted there
    /// does not come between a declaration and its comment.
    fn after_trailing_comments(&self, pos: usize) -> usize {
        let b = self.src.as_bytes();
        let (mut i, mut end) = (pos, pos);
        loop {
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
            let rest = &self.src[i.min(b.len())..];
            if rest.starts_with("//") {
                i += rest.find(['\r', '\n']).unwrap_or(rest.len());
            } else if rest.starts_with('{') {
                i += rest.find('}').map_or(rest.len(), |j| j + 1);
            } else if rest.starts_with("(*") {
                i += rest.find("*)").map_or(rest.len(), |j| j + 2);
            } else {
                return end;
            }
            end = i;
        }
    }

    fn indent_of_line(&self, pos: usize) -> &'a str {
        let ls = self.line_start(pos);
        let line = &self.src[ls..pos];
        &line[..line.len() - line.trim_start().len()]
    }

    /// Whole lines holding `span` including the line break, if nothing else shares them.
    fn line_range(&self, span: Range<usize>) -> Range<usize> {
        let start = self.line_start(span.start);
        let line_end = self.src[span.end..]
            .find('\n')
            .map_or(self.src.len(), |i| span.end + i + 1);
        let alone = self.src[start..span.start].trim().is_empty()
            && self.src[span.end..line_end].trim().is_empty();
        if alone { start..line_end } else { span }
    }
}

#[derive(Debug)]
struct Member {
    /// Names before the colon (fields) or the method name.
    names: Vec<(String, Range<usize>)>,
    span: Range<usize>,
    in_default_section: bool,
}

#[derive(Debug)]
struct ClassInfo {
    header_end: usize,
    fields: Vec<Member>,
    methods: Vec<Member>,
    /// Token index of `implementation`.
    implementation: usize,
}

const VISIBILITY: [&str; 5] = ["private", "protected", "public", "published", "automated"];
const METHOD_WORDS: [&str; 4] = ["procedure", "function", "constructor", "destructor"];

fn find_class(scan: &Scan, class: &str) -> Result<ClassInfo, SyncError> {
    let n = scan.toks.len();
    let implementation = (0..n)
        .find(|&i| scan.is_word(i, "implementation"))
        .ok_or(SyncError::NoImplementation)?;
    let decl = (0..implementation.saturating_sub(2))
        .find(|&i| {
            scan.is_word(i, class)
                && scan.is_sym(i + 1, b'=')
                && scan.is_word(i + 2, "class")
                && !scan.is_sym(i + 3, b';')
                && !scan.is_word(i + 3, "of")
        })
        .ok_or_else(|| SyncError::NoClass(class.into()))?;

    // Header: `class` or `class(TForm, IFoo)`.
    let mut k = decl + 3;
    let mut header_end = scan.toks[decl + 2].end;
    if scan.is_sym(k, b'(') {
        let close = (k..n)
            .find(|&j| scan.is_sym(j, b')'))
            .ok_or_else(|| SyncError::Unsupported("unterminated class header".into()))?;
        header_end = scan.toks[close].end;
        k = close + 1;
    }

    let (mut fields, mut methods) = (Vec::new(), Vec::new());
    let mut default_section = true;
    let mut closed = false;
    while k < implementation {
        if scan.is_word(k, "end") {
            closed = true;
            break;
        }
        if scan.is_word(k, "strict") && VISIBILITY.iter().any(|v| scan.is_word(k + 1, v)) {
            default_section = false;
            k += 2;
            continue;
        }
        if VISIBILITY.iter().any(|v| scan.is_word(k, v)) {
            default_section = false;
            k += 1;
            continue;
        }
        // One member: tokens up to a `;` outside parentheses, or up to the class's `end`.
        let first = k;
        let mut depth = 0;
        let mut last = k;
        while k < implementation {
            if scan.is_word(k, "record")
                || (scan.is_sym(k, b'=')
                    && ["class", "record", "object"]
                        .iter()
                        .any(|w| scan.is_word(k + 1, w)))
            {
                return Err(SyncError::Unsupported(
                    "nested types inside the form class".into(),
                ));
            }
            if scan.is_sym(k, b'(') || scan.is_sym(k, b'[') {
                depth += 1;
            } else if scan.is_sym(k, b')') || scan.is_sym(k, b']') {
                depth -= 1;
            } else if depth == 0 && scan.is_word(k, "end") {
                break;
            }
            last = k;
            k += 1;
            if depth == 0 && scan.is_sym(last, b';') {
                break;
            }
        }
        let span = scan.toks[first].start..scan.toks[last].end;
        let lead = if scan.is_word(first, "class") {
            first + 1
        } else {
            first
        };
        if METHOD_WORDS.iter().any(|w| scan.is_word(lead, w)) {
            if let Some(t) = scan.toks.get(lead + 1).filter(|t| t.kind == Kind::Word) {
                let name = scan.src[t.start..t.end].to_string();
                methods.push(Member {
                    names: vec![(name, t.start..t.end)],
                    span,
                    in_default_section: default_section,
                });
            }
        } else if lead == first && scan.toks[first].kind == Kind::Word {
            // `A, B: Type;` - names up to the colon.
            let mut names = Vec::new();
            let mut j = first;
            while j <= last && !scan.is_sym(j, b':') {
                if scan.toks[j].kind == Kind::Word {
                    let t = scan.toks[j];
                    names.push((scan.src[t.start..t.end].to_string(), t.start..t.end));
                }
                j += 1;
            }
            let is_field = j <= last
                && !["property", "var", "const", "type"]
                    .iter()
                    .any(|w| scan.is_word(first, w));
            if is_field && !names.is_empty() {
                fields.push(Member {
                    names,
                    span,
                    in_default_section: default_section,
                });
            }
        }
    }
    if !closed {
        return Err(SyncError::Unsupported(
            "could not find the end of the class".into(),
        ));
    }
    Ok(ClassInfo {
        header_end,
        fields,
        methods,
        implementation,
    })
}

/// Where new method bodies go: before `initialization`/`finalization`, else before the final `end.`.
fn stub_insertion_point(scan: &Scan, info: &ClassInfo) -> Result<usize, SyncError> {
    let n = scan.toks.len();
    let tail = (info.implementation + 1..n)
        .find(|&i| scan.is_word(i, "initialization") || scan.is_word(i, "finalization"))
        .or_else(|| {
            (info.implementation + 1..n.saturating_sub(1))
                .rev()
                .find(|&i| scan.is_word(i, "end") && scan.is_sym(i + 1, b'.'))
        })
        .ok_or_else(|| SyncError::Unsupported("no final `end.` in the unit".into()))?;
    let start = scan.toks[tail].start;
    let ls = scan.line_start(start);
    // Insert at the start of the line when the keyword starts it, else right before it.
    Ok(if scan.src[ls..start].trim().is_empty() {
        ls
    } else {
        start
    })
}

fn has_impl(scan: &Scan, info: &ClassInfo, class: &str, method: &str) -> bool {
    (info.implementation..scan.toks.len().saturating_sub(3))
        .any(|i| scan.is_word(i, class) && scan.is_sym(i + 1, b'.') && scan.is_word(i + 2, method))
}

/// Edits that add whatever `w` needs and the unit lacks. Only inserts; user code is untouched.
pub fn plan(unit: &str, class: &str, w: &Wanted) -> Result<SyncPlan, SyncError> {
    let scan = Scan::new(unit);
    let info = find_class(&scan, class)?;
    let nl = scan.nl();
    let mut out = SyncPlan::default();

    let has_field = |name: &str| {
        info.fields
            .iter()
            .any(|f| f.names.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
    };
    let has_method = |name: &str| {
        info.methods
            .iter()
            .any(|m| m.names[0].0.eq_ignore_ascii_case(name))
    };

    let default_fields = info.fields.iter().filter(|f| f.in_default_section);
    let default_methods = info.methods.iter().filter(|m| m.in_default_section);
    let indent = info
        .fields
        .iter()
        .chain(&info.methods)
        .filter(|m| m.in_default_section)
        .map(|m| scan.indent_of_line(m.span.start))
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}  ", scan.indent_of_line(scan.toks[0].start)) + "  ");

    let field_anchor = scan.after_trailing_comments(
        default_fields
            .map(|f| f.span.end)
            .max()
            .unwrap_or(info.header_end),
    );
    let method_anchor = default_methods
        .map(|m| scan.after_trailing_comments(m.span.end))
        .max()
        .unwrap_or(field_anchor);

    let new_fields: String = w
        .fields
        .iter()
        .filter(|(n, _)| !has_field(n))
        .map(|(n, c)| format!("{nl}{indent}{n}: {c};"))
        .collect();
    let new_decls: String = w
        .handlers
        .iter()
        .filter(|h| !has_method(&h.name))
        .map(|h| format!("{nl}{indent}procedure {}({});", h.name, h.params))
        .collect();

    if field_anchor == method_anchor {
        let text = format!("{new_fields}{new_decls}");
        if !text.is_empty() {
            out.edits.push(TextEdit {
                range: field_anchor..field_anchor,
                text,
            });
        }
    } else {
        if !new_fields.is_empty() {
            out.edits.push(TextEdit {
                range: field_anchor..field_anchor,
                text: new_fields,
            });
        }
        if !new_decls.is_empty() {
            out.edits.push(TextEdit {
                range: method_anchor..method_anchor,
                text: new_decls,
            });
        }
    }

    let stubs: String = w
        .handlers
        .iter()
        .filter(|h| !has_impl(&scan, &info, class, &h.name))
        .map(|h| {
            format!(
                "{nl}procedure {class}.{}({});{nl}begin{nl}end;{nl}",
                h.name, h.params
            )
        })
        .collect();
    if !stubs.is_empty() {
        let at = stub_insertion_point(&scan, &info)?;
        out.edits.push(TextEdit {
            range: at..at,
            text: stubs,
        });
    }
    Ok(out)
}

/// Adds the units in `units` that the interface `uses` clause lacks (a component's class lives
/// in a unit such as `ExtCtrls`). Creates the clause when there is none. Names already there
/// are matched case-insensitively and left alone.
pub fn plan_uses(unit: &str, units: &[&str]) -> Result<SyncPlan, SyncError> {
    let scan = Scan::new(unit);
    let iface = (0..scan.toks.len())
        .find(|&i| scan.is_word(i, "interface"))
        .ok_or_else(|| SyncError::Unsupported("no `interface` section".into()))?;
    let mut out = SyncPlan::default();
    if !scan.is_word(iface + 1, "uses") {
        let missing: Vec<&str> = units.to_vec();
        if !missing.is_empty() {
            let at = scan.after_trailing_comments(scan.toks[iface].end);
            let nl = scan.nl();
            out.edits.push(TextEdit {
                range: at..at,
                text: format!("{nl}{nl}uses{nl}  {};", missing.join(", ")),
            });
        }
        return Ok(out);
    }
    let mut i = iface + 2;
    let mut have: Vec<&str> = Vec::new();
    let mut last_end = None;
    while i < scan.toks.len() && !scan.is_sym(i, b';') {
        if scan.toks[i].kind == Kind::Word && !scan.is_word(i, "in") {
            have.push(scan.text(i));
            last_end = Some(scan.toks[i].end);
        }
        i += 1;
    }
    let missing: Vec<&str> = units
        .iter()
        .copied()
        .filter(|u| !have.iter().any(|h| h.eq_ignore_ascii_case(u)))
        .collect();
    if let (false, Some(at)) = (missing.is_empty(), last_end) {
        out.edits.push(TextEdit {
            range: at..at,
            text: format!(", {}", missing.join(", ")),
        });
    }
    Ok(out)
}

/// Removes the field lines for deleted components. Only a line holding exactly one field is
/// removed, and only from the default section; anything else is reported in `skipped`.
pub fn plan_remove_fields(
    unit: &str,
    class: &str,
    names: &[String],
) -> Result<SyncPlan, SyncError> {
    let scan = Scan::new(unit);
    let info = find_class(&scan, class)?;
    let mut out = SyncPlan::default();
    for name in names {
        let found = info
            .fields
            .iter()
            .find(|f| f.names.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)));
        match found {
            Some(f) if f.in_default_section && f.names.len() == 1 => {
                out.edits.push(TextEdit {
                    range: scan.line_range(f.span.clone()),
                    text: String::new(),
                });
            }
            Some(_) => out.skipped.push(format!(
                "field {name}: not a single field in the default section"
            )),
            None => {}
        }
    }
    Ok(out)
}

/// Renames a field declaration. Code that uses the field is the caller's concern.
pub fn plan_rename_field(
    unit: &str,
    class: &str,
    old: &str,
    new: &str,
) -> Result<SyncPlan, SyncError> {
    let scan = Scan::new(unit);
    let info = find_class(&scan, class)?;
    let mut out = SyncPlan::default();
    for f in &info.fields {
        for (n, span) in &f.names {
            if n.eq_ignore_ascii_case(old) {
                out.edits.push(TextEdit {
                    range: span.clone(),
                    text: new.into(),
                });
            }
        }
    }
    Ok(out)
}

/// Renames a method in the class declaration and in its `TClass.Name` implementation header.
pub fn plan_rename_method(
    unit: &str,
    class: &str,
    old: &str,
    new: &str,
) -> Result<SyncPlan, SyncError> {
    let scan = Scan::new(unit);
    let info = find_class(&scan, class)?;
    let mut out = SyncPlan::default();
    for m in &info.methods {
        let (name, span) = &m.names[0];
        if name.eq_ignore_ascii_case(old) {
            out.edits.push(TextEdit {
                range: span.clone(),
                text: new.into(),
            });
        }
    }
    for i in info.implementation..scan.toks.len().saturating_sub(3) {
        if scan.is_word(i, class) && scan.is_sym(i + 1, b'.') && scan.is_word(i + 2, old) {
            let t = scan.toks[i + 2];
            out.edits.push(TextEdit {
                range: t.start..t.end,
                text: new.into(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lfm;

    const UNIT: &str = "unit unit1;\n{$mode objfpc}{$H+}\ninterface\nuses Classes, Forms;\ntype\n  TForm1 = class(TForm)\n    Button1: TButton;\n    procedure Button1Click(Sender: TObject);\n  private\n    FCount: Integer;\n  end;\nvar\n  Form1: TForm1;\nimplementation\n{$R *.lfm}\nprocedure TForm1.Button1Click(Sender: TObject);\nbegin\n  Inc(FCount);\nend;\nend.\n";

    fn form(src: &str) -> Component {
        lfm::parse(src).unwrap()
    }

    const LFM: &str = "object Form1: TForm1\n  object Button1: TButton\n    OnClick = Button1Click\n  end\n  object Edit1: TEdit\n    OnChange = Edit1Change\n  end\nend\n";

    #[test]
    fn adds_the_missing_field_declaration_and_stub() {
        let p = plan(UNIT, "TForm1", &wanted(&form(LFM))).unwrap();
        let got = apply_edits(UNIT, &p.edits);
        let expected = UNIT
            .replace(
                "    procedure Button1Click(Sender: TObject);\n",
                "    procedure Button1Click(Sender: TObject);\n    procedure Edit1Change(Sender: TObject);\n",
            )
            .replace("    Button1: TButton;\n", "    Button1: TButton;\n    Edit1: TEdit;\n")
            .replace("end;\nend.\n", "end;\n\nprocedure TForm1.Edit1Change(Sender: TObject);\nbegin\nend;\nend.\n");
        assert_eq!(got, expected);
    }

    #[test]
    fn only_inserts_and_is_idempotent() {
        let w = wanted(&form(LFM));
        let p = plan(UNIT, "TForm1", &w).unwrap();
        assert!(
            p.edits.iter().all(|e| e.range.is_empty()),
            "plan() must never remove or replace text"
        );
        let once = apply_edits(UNIT, &p.edits);
        assert!(plan(&once, "TForm1", &w).unwrap().edits.is_empty());
    }

    #[test]
    fn in_sync_units_need_no_edits() {
        let w = wanted(&form(
            "object Form1: TForm1\n  object Button1: TButton\n    OnClick = Button1Click\n  end\nend\n",
        ));
        assert!(plan(UNIT, "TForm1", &w).unwrap().edits.is_empty());
    }

    #[test]
    fn an_empty_class_gets_fields_then_methods() {
        let unit = "unit u;\ninterface\ntype\n  TF = class(TForm)\n  end;\nimplementation\nend.\n";
        let w = wanted(&form(
            "object F: TF\n  object B: TButton\n    OnClick = BClick\n  end\nend\n",
        ));
        let got = apply_edits(unit, &plan(unit, "TF", &w).unwrap().edits);
        assert_eq!(
            got,
            "unit u;\ninterface\ntype\n  TF = class(TForm)\n    B: TButton;\n    procedure BClick(Sender: TObject);\n  end;\nimplementation\n\nprocedure TF.BClick(Sender: TObject);\nbegin\nend;\nend.\n"
        );
    }

    #[test]
    fn a_handler_declared_elsewhere_in_the_class_is_not_declared_twice() {
        let unit = UNIT.replace(
            "    procedure Button1Click(Sender: TObject);\n  private\n",
            "  private\n    procedure Button1Click(Sender: TObject);\n",
        );
        let w = wanted(&form(LFM));
        let got = apply_edits(&unit, &plan(&unit, "TForm1", &w).unwrap().edits);
        assert_eq!(got.matches("Button1Click(Sender: TObject);").count(), 2); // declaration + implementation
    }

    #[test]
    fn a_missing_body_gets_a_stub_even_when_declared() {
        let unit = UNIT.replace(
            "procedure TForm1.Button1Click(Sender: TObject);\nbegin\n  Inc(FCount);\nend;\n",
            "",
        );
        let w = wanted(&form(LFM));
        let got = apply_edits(&unit, &plan(&unit, "TForm1", &w).unwrap().edits);
        assert!(got.contains("procedure TForm1.Button1Click(Sender: TObject);\nbegin\nend;"));
    }

    #[test]
    fn comments_and_strings_do_not_confuse_it() {
        let unit = "unit u;\ninterface\ntype\n  { TF = class(TOther) end; }\n  TF = class(TForm)\n    // end;\n    B: TButton; (* private *)\n    S: string; // 'implementation'\n  end;\nimplementation\nconst X = 'end.';\nend.\n";
        let w = wanted(&form("object F: TF\n  object C: TCheckBox\n  end\nend\n"));
        let got = apply_edits(unit, &plan(unit, "TF", &w).unwrap().edits);
        assert!(
            got.contains("    S: string; // 'implementation'\n    C: TCheckBox;\n  end;"),
            "{got}"
        );
        let c = "object F: TF\n  object C: TCheckBox\n  end\nend\n";
        let multi = "unit u;\ninterface\ntype\n  TF = class(TForm) // the form\n    B: TButton; { a note\n      over two lines }\n  end;\nimplementation\nend.\n";
        let got = apply_edits(multi, &plan(multi, "TF", &wanted(&form(c))).unwrap().edits);
        assert!(
            got.contains("over two lines }\n    C: TCheckBox;\n  end;"),
            "{got}"
        );
        let empty = "unit u;\ninterface\ntype\n  TF = class(TForm) // the form\n  end;\nimplementation\nend.\n";
        let got = apply_edits(empty, &plan(empty, "TF", &wanted(&form(c))).unwrap().edits);
        assert!(
            got.contains("class(TForm) // the form\n    C: TCheckBox;\n  end;"),
            "{got}"
        );
    }

    #[test]
    fn stubs_go_before_the_initialization_section() {
        let unit = UNIT.replace("end;\nend.\n", "end;\ninitialization\n  Randomize;\nend.\n");
        let w = wanted(&form(LFM));
        let got = apply_edits(&unit, &plan(&unit, "TForm1", &w).unwrap().edits);
        let stub = got.find("procedure TForm1.Edit1Change").unwrap();
        assert!(stub < got.find("initialization").unwrap());
    }

    #[test]
    fn keeps_windows_line_endings() {
        let unit = UNIT.replace('\n', "\r\n");
        let w = wanted(&form(LFM));
        let got = apply_edits(&unit, &plan(&unit, "TForm1", &w).unwrap().edits);
        assert!(!got.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn adds_missing_units_to_the_uses_clause_once() {
        let add = |src: &str| {
            let p = plan_uses(src, &["ExtCtrls", "stdctrls"]).unwrap();
            apply_edits(src, &p.edits)
        };
        let out = add(UNIT);
        assert!(
            out.contains("uses Classes, Forms, ExtCtrls, stdctrls;"),
            "{out}"
        );
        assert_eq!(add(&out), out);
        let bare = "unit u;\ninterface\ntype\n  T = class end;\nimplementation\nend.\n";
        let out = add(bare);
        assert!(
            out.contains("interface\n\nuses\n  ExtCtrls, stdctrls;\ntype"),
            "{out}"
        );
        assert_eq!(add(&out), out);
    }

    #[test]
    fn refuses_instead_of_guessing() {
        let w = Wanted::default();
        assert_eq!(
            plan(UNIT, "TNope", &w),
            Err(SyncError::NoClass("TNope".into()))
        );
        assert_eq!(
            plan("unit u;\ninterface\nend.\n", "TF", &w),
            Err(SyncError::NoImplementation)
        );
        let nested = "unit u;\ninterface\ntype\n  TF = class(TForm)\n    type TX = record a: Integer; end;\n  end;\nimplementation\nend.\n";
        assert!(matches!(
            plan(nested, "TF", &w),
            Err(SyncError::Unsupported(_))
        ));
        let unclosed = "unit u;\ninterface\ntype\n  TF = class(TForm)\n    B: TButton;\nimplementation\nend.\n";
        assert!(matches!(
            plan(unclosed, "TF", &w),
            Err(SyncError::Unsupported(_))
        ));
    }

    #[test]
    fn removes_only_simple_default_section_fields() {
        let got = plan_remove_fields(UNIT, "TForm1", &["Button1".into()]).unwrap();
        assert_eq!(
            apply_edits(UNIT, &got.edits),
            UNIT.replace("    Button1: TButton;\n", "")
        );
        let multi = UNIT.replace(
            "    Button1: TButton;\n",
            "    Button1, Button2: TButton;\n",
        );
        let got = plan_remove_fields(&multi, "TForm1", &["Button1".into()]).unwrap();
        assert!(got.edits.is_empty() && got.skipped.len() == 1);
        let private = plan_remove_fields(UNIT, "TForm1", &["FCount".into()]).unwrap();
        assert!(
            private.edits.is_empty() && private.skipped.len() == 1,
            "user-owned sections are never touched"
        );
    }

    #[test]
    fn renames_fields_and_methods_in_every_place() {
        let f = plan_rename_field(UNIT, "TForm1", "Button1", "OkButton").unwrap();
        assert_eq!(
            apply_edits(UNIT, &f.edits),
            UNIT.replace("    Button1: TButton;", "    OkButton: TButton;")
        );
        let m = plan_rename_method(UNIT, "TForm1", "Button1Click", "OkButtonClick").unwrap();
        let got = apply_edits(UNIT, &m.edits);
        assert_eq!(got.matches("OkButtonClick").count(), 2);
        assert!(!got.contains("Button1Click"));
    }

    #[test]
    fn derives_events_and_signatures_from_the_form() {
        let f = form(
            "object F: TF\n  OnClose = FormClose\n  object B: TButton\n    OnClick = Go\n    Font.Name = 'x'\n  end\n  object C: TButton\n    OnClick = Go\n  end\nend\n",
        );
        let w = wanted(&f);
        assert_eq!(
            w.fields,
            [
                ("B".to_string(), "TButton".to_string()),
                ("C".to_string(), "TButton".to_string())
            ]
        );
        assert_eq!(w.handlers.len(), 2, "a shared handler is declared once");
        assert_eq!(
            w.handlers[0],
            Handler {
                name: "FormClose".into(),
                params: "Sender: TObject; var CloseAction: TCloseAction".into()
            }
        );
    }

    #[test]
    fn non_visual_components_get_their_own_signatures() {
        let f = form(
            "object F: TF\n  object D: TOpenDialog\n    OnClose = DClose\n  end\n  object A: TActionList\n    OnExecute = AExecute\n  end\n  object T: TTimer\n    OnTimer = TTimer1\n  end\nend\n",
        );
        let params: Vec<String> = wanted(&f).handlers.into_iter().map(|h| h.params).collect();
        assert_eq!(
            params,
            [
                "Sender: TObject",
                "AAction: TBasicAction; var Handled: Boolean",
                "Sender: TObject"
            ]
        );
    }
}
