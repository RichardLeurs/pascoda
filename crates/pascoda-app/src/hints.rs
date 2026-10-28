//! Logic behind the editor's language-server features, kept free of window state so it can be
//! tested: dwell/debounce timing, pointer to identifier mapping, "is this code" checks for
//! completion, diagnostic marks and the squiggle geometry.

use eframe::egui::{self, Galley, Pos2, Rect, Vec2, text::CCursor};
use pascoda_fpc::Severity;
use std::ops::Range;

/// The pointer must rest this long on an identifier before hover is requested.
pub const HOVER_DELAY: f64 = 0.4;
/// Typing must pause this long before completion is requested.
pub const COMPLETE_DELAY: f64 = 0.15;

#[derive(Debug, PartialEq)]
pub enum Poll {
    /// Nothing to wait for.
    Idle,
    /// Check again in this many seconds.
    Wait(f64),
    /// The target has been stable for the delay; reported once per target.
    Ready,
}

/// Fires once after a target has stayed unchanged for a delay; a new target restarts it.
pub struct Dwell<T> {
    target: Option<T>,
    since: f64,
    fired: bool,
}

impl<T> Default for Dwell<T> {
    fn default() -> Self {
        Self {
            target: None,
            since: 0.0,
            fired: false,
        }
    }
}

impl<T: PartialEq> Dwell<T> {
    pub fn update(&mut self, target: Option<T>, now: f64, delay: f64) -> Poll {
        if self.target != target {
            self.since = now;
            self.fired = false;
            self.target = target;
        }
        if self.target.is_none() || self.fired {
            return Poll::Idle;
        }
        let left = self.since + delay - now;
        if left > 0.0 {
            Poll::Wait(left)
        } else {
            self.fired = true;
            Poll::Ready
        }
    }
}

pub fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Char range of the identifier containing the char at `idx`.
pub fn word_at(text: &str, idx: usize) -> Option<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    if !is_ident(*chars.get(idx)?) {
        return None;
    }
    let start = chars[..idx]
        .iter()
        .rposition(|c| !is_ident(*c))
        .map_or(0, |p| p + 1);
    let end = chars[idx..]
        .iter()
        .position(|c| !is_ident(*c))
        .map_or(chars.len(), |p| idx + p);
    Some(start..end)
}

/// Char index of 0-based (`line`, `col`), clamped to the end of that line.
pub fn char_index(text: &str, line: u32, col: u32) -> usize {
    let mut offset = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line as usize {
            let len = l.trim_end_matches(['\n', '\r']).chars().count();
            return offset + (col as usize).min(len);
        }
        offset += l.chars().count();
    }
    offset
}

/// Whether the end of `before` is ordinary code, not inside a comment or a string.
pub fn in_code(before: &str) -> bool {
    #[derive(PartialEq)]
    enum S {
        Code,
        Line,
        Brace,
        Paren,
        Str,
    }
    let chars: Vec<char> = before.chars().collect();
    let (mut s, mut i) = (S::Code, 0);
    while i < chars.len() {
        let (c, next) = (chars[i], chars.get(i + 1).copied());
        match s {
            S::Code => match (c, next) {
                ('/', Some('/')) => s = S::Line,
                ('(', Some('*')) => {
                    s = S::Paren;
                    i += 1;
                }
                ('{', _) => s = S::Brace,
                ('\'', _) => s = S::Str,
                _ => {}
            },
            S::Line if c == '\n' => s = S::Code,
            S::Brace if c == '}' => s = S::Code,
            S::Paren if c == '*' && next == Some(')') => {
                s = S::Code;
                i += 1;
            }
            S::Str if c == '\'' || c == '\n' => s = S::Code,
            _ => {}
        }
        i += 1;
    }
    s == S::Code
}

/// Whether typing that left the caret at char `cursor` should pop up completion: after a `.`
/// that follows an expression, or once the word before the caret has two characters.
pub fn should_complete(text: &str, cursor: usize) -> bool {
    let before: String = text.chars().take(cursor).collect();
    if !in_code(&before) {
        return false;
    }
    let chars: Vec<char> = before.chars().collect();
    match chars.as_slice() {
        [.., prev, '.'] => is_ident(*prev) || matches!(prev, ')' | ']'),
        _ => {
            let word = chars.iter().rev().take_while(|c| is_ident(**c)).count();
            word >= 2 && !chars[chars.len() - word].is_ascii_digit()
        }
    }
}

/// A build diagnostic placed in a document (0-based line, column in chars).
#[derive(Clone, Debug)]
pub struct Mark {
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub message: String,
}

/// Marks for one file, valid only for the text the build saw.
pub struct FileMarks {
    pub path: std::path::PathBuf,
    pub text: String,
    pub items: Vec<Mark>,
}

impl Mark {
    /// Char range to underline: the identifier at the position, else the single char.
    pub fn range(&self, text: &str) -> Range<usize> {
        let at = char_index(text, self.line, self.col);
        word_at(text, at).unwrap_or(at..at + 1)
    }

    pub fn is_error(&self) -> bool {
        matches!(self.severity, Severity::Fatal | Severity::Error)
    }
}

/// What the pointer rests on: a char range and, when it is underlined, the mark.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub range: Range<usize>,
    pub mark: Option<usize>,
}

/// Screen-independent rectangle (galley space) covered by `range` on one row.
pub fn range_rect(galley: &Galley, range: &Range<usize>) -> Option<Rect> {
    let a = galley.pos_from_cursor(CCursor::new(range.start));
    let b = galley.pos_from_cursor(CCursor::new(range.end));
    (a.min.y == b.min.y && b.min.x > a.min.x).then(|| Rect::from_min_max(a.min, b.max))
}

/// The mark or identifier under `local` (a position relative to the galley's origin).
/// Marks win over plain identifiers.
pub fn target_at(galley: &Galley, text: &str, local: Vec2, marks: &[Mark]) -> Option<Target> {
    let p = local.to_pos2();
    let hit = |r: &Range<usize>| range_rect(galley, r).is_some_and(|rect| rect.contains(p));
    if let Some((i, m)) = marks.iter().enumerate().find(|(_, m)| hit(&m.range(text))) {
        return Some(Target {
            range: m.range(text),
            mark: Some(i),
        });
    }
    let idx = galley.cursor_from_pos(local).index.0;
    [Some(idx), idx.checked_sub(1)]
        .into_iter()
        .flatten()
        .filter_map(|i| word_at(text, i))
        .find(hit)
        .map(|range| Target { range, mark: None })
}

/// The zigzag along `x0..x1` at baseline `y` used for diagnostic underlines.
pub fn squiggle_points(x0: f32, x1: f32, y: f32) -> Vec<Pos2> {
    const STEP: f32 = 2.0;
    let n = ((x1 - x0) / STEP).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| {
            let x = (x0 + i as f32 * STEP).min(x1);
            Pos2::new(x, if i % 2 == 0 { y } else { y - 2.0 })
        })
        .collect()
}

/// pasls ends its hover text with the declaration's location as `/full/path/unit.pas(4,3)`;
/// keep just the file name so the tooltip stays short.
pub fn tidy_hover(text: &str) -> String {
    text.lines()
        .map(|line| match (line.starts_with('/'), line.rfind('(')) {
            (true, Some(open)) if line.ends_with(')') => {
                let name = line[..open].rsplit('/').next().unwrap_or_default();
                format!("{name}{}", &line[open..])
            }
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tooltip shown near the code.
pub struct Popup {
    pub text: String,
    pub severity: Option<Severity>,
    /// Screen position of its top-left corner.
    pub pos: Pos2,
}

/// Hover bookkeeping for the active editor.
#[derive(Default)]
pub struct Hover {
    pub dwell: Dwell<(usize, usize, Option<usize>)>,
    pub target: Option<Target>,
    /// Position (0-based line, column) asked from the server and not yet answered.
    pub requested: Option<(u32, u32)>,
    pub popup: Option<Popup>,
}

impl Hover {
    pub fn clear(&mut self) {
        self.target = None;
        self.requested = None;
        self.popup = None;
    }

    /// Follows the pointer: a different target drops the popup and any pending answer.
    pub fn set_target(&mut self, target: Option<Target>) {
        if self.target != target {
            self.requested = None;
            self.popup = None;
            self.target = target;
        }
    }

    /// Accepts a server answer only if it is for the position still being asked about.
    pub fn accept(&mut self, line: u32, col: u32, text: Option<String>, pos: Pos2) {
        if self.requested != Some((line, col)) {
            return;
        }
        self.requested = None;
        self.popup = text.map(|text| Popup {
            text: tidy_hover(&text),
            severity: None,
            pos,
        });
    }
}

/// Draws a popup; it never takes the pointer, so hovering stays on the editor.
pub fn show_popup(
    ctx: &egui::Context,
    popup: &Popup,
    error: egui::Color32,
    warning: egui::Color32,
) {
    egui::Area::new(egui::Id::unique("editor-hover"))
        .order(egui::Order::Tooltip)
        .fixed_pos(popup.pos)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(560.0);
                let mut text = egui::RichText::new(&popup.text).monospace();
                match popup.severity {
                    Some(Severity::Fatal | Severity::Error) => text = text.color(error),
                    Some(Severity::Warning) => text = text.color(warning),
                    _ => {}
                }
                ui.label(text);
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwell_fires_once_after_the_delay_and_restarts_on_a_new_target() {
        let mut d = Dwell::default();
        assert_eq!(d.update(None::<u32>, 0.0, 0.4), Poll::Idle);
        assert!(matches!(d.update(Some(1), 1.0, 0.4), Poll::Wait(w) if (w - 0.4).abs() < 1e-9));
        assert!(matches!(d.update(Some(1), 1.3, 0.4), Poll::Wait(w) if (w - 0.1).abs() < 1e-9));
        assert_eq!(d.update(Some(1), 1.4, 0.4), Poll::Ready);
        assert_eq!(d.update(Some(1), 2.0, 0.4), Poll::Idle);
        // Moving to another target restarts the clock.
        assert!(matches!(d.update(Some(2), 2.1, 0.4), Poll::Wait(w) if (w - 0.4).abs() < 1e-9));
        assert_eq!(d.update(None, 2.2, 0.4), Poll::Idle);
        assert!(matches!(d.update(Some(2), 2.3, 0.4), Poll::Wait(w) if (w - 0.4).abs() < 1e-9));
    }

    #[test]
    fn finds_the_word_around_a_char() {
        let t = "x := foo_1 + 2;";
        assert_eq!(word_at(t, 5), Some(5..10));
        assert_eq!(word_at(t, 9), Some(5..10));
        assert_eq!(word_at(t, 10), None);
        assert_eq!(word_at(t, 0), Some(0..1));
        assert_eq!(word_at(t, 99), None);
        assert_eq!(word_at("é€ab", 2), Some(2..4)); // € is not an identifier char
    }

    #[test]
    fn maps_line_and_column_to_a_char_index() {
        let t = "ab\n😀cd\nef";
        assert_eq!(char_index(t, 0, 1), 1);
        assert_eq!(char_index(t, 1, 2), 5);
        assert_eq!(char_index(t, 1, 99), 6); // clamped to the line end
        assert_eq!(char_index(t, 2, 1), 8);
        assert_eq!(char_index(t, 9, 0), 9); // past the end
        // Round trip with the crate's own (line, col) of an index.
        for i in 0..=t.chars().count() {
            let (l, c) = crate::line_col(t, i);
            assert_eq!(char_index(t, l, c), i);
        }
    }

    #[test]
    fn tells_code_from_comments_and_strings() {
        assert!(in_code("begin x := 1; "));
        assert!(!in_code("x := 1; // note"));
        assert!(in_code("// note\nx"));
        assert!(!in_code("{ open"));
        assert!(in_code("{ closed } x"));
        assert!(!in_code("(* open"));
        assert!(in_code("(* a *) x"));
        assert!(!in_code("s := 'hel"));
        assert!(in_code("s := 'it''s' + x"));
        assert!(in_code("s := 'unterminated\nx"));
    }

    #[test]
    fn completes_after_two_chars_or_a_dot() {
        let ok = |t: &str| should_complete(t, t.chars().count());
        assert!(!ok("t"));
        assert!(ok("to"));
        assert!(ok("x := tot"));
        assert!(!ok("x := 12"));
        assert!(ok("Memo1."));
        assert!(ok("Items[2]."));
        assert!(ok("f(x)."));
        assert!(!ok("for i := 1 .. "));
        assert!(!ok("1..")); // range operator
        assert!(!ok("x := "));
        assert!(!ok("// to"));
        assert!(!ok("{ to"));
        assert!(!ok("s := 'to"));
        assert!(!ok("'a'.")); // after a string literal
    }

    #[test]
    fn marks_underline_the_identifier_or_one_char() {
        let m = |line, col| Mark {
            line,
            col,
            severity: Severity::Error,
            message: String::new(),
        };
        let t = "x := nope;\n;";
        assert_eq!(m(0, 5).range(t), 5..9);
        assert_eq!(m(0, 9).range(t), 9..10);
        assert_eq!(m(1, 0).range(t), 11..12);
    }

    #[test]
    fn squiggle_zigzags_between_two_heights_and_ends_at_the_edge() {
        let pts = squiggle_points(10.0, 17.0, 20.0);
        assert_eq!(pts.first().unwrap().x, 10.0);
        assert_eq!(pts.last().unwrap().x, 17.0);
        assert!(pts.iter().all(|p| p.y == 20.0 || p.y == 18.0));
        assert!(pts.windows(2).all(|w| w[0].y != w[1].y));
        assert_eq!(squiggle_points(5.0, 5.0, 0.0).len(), 2);
    }

    #[test]
    fn hover_text_keeps_only_the_file_name_of_the_location() {
        assert_eq!(
            tidy_hover("var total: Integer = longint\n/a/b/debug.pas(3,6)"),
            "var total: Integer = longint\ndebug.pas(3,6)"
        );
        assert_eq!(
            tidy_hover("procedure Foo(x: Integer)"),
            "procedure Foo(x: Integer)"
        );
    }

    #[test]
    fn hover_ignores_answers_for_an_old_position() {
        let mut h = Hover {
            requested: Some((3, 4)),
            ..Default::default()
        };
        h.accept(3, 5, Some("old".into()), Pos2::ZERO);
        assert!(h.popup.is_none());
        assert_eq!(h.requested, Some((3, 4)));
        h.accept(3, 4, Some("var x".into()), Pos2::ZERO);
        assert_eq!(h.popup.as_ref().unwrap().text, "var x");
        // After the pointer moves on, even a matching late answer is dropped.
        h.requested = Some((1, 1));
        h.set_target(Some(Target {
            range: 0..1,
            mark: None,
        }));
        h.accept(1, 1, Some("late".into()), Pos2::ZERO);
        assert!(h.popup.is_none());
    }

    /// Lays out real monospace text headlessly and finds the identifier under a point.
    #[test]
    fn maps_pointer_positions_to_identifiers_with_a_real_galley() {
        let text = "begin\n  total := total + 1;\nend.";
        let ctx = egui::Context::default();
        let out = ctx.run_pass(Default::default(), |ctx| {
            let pal = crate::style::Palette::of(ctx);
            let job =
                crate::highlight::layout(text, &pal, crate::editor_font(), crate::shell::LINE_H);
            let galley = ctx.fonts_mut(|f| f.layout_job(job));
            // Centre of char `idx` on its row.
            let centre = |idx: usize| {
                let r = galley.pos_from_cursor(CCursor::new(idx));
                Vec2::new(r.min.x + 2.0, r.center().y)
            };
            let first = text.find("total").unwrap();
            let t = target_at(&galley, text, centre(first + 2), &[]).unwrap();
            assert_eq!(t.range, first..first + 5);
            // Rows are LINE_H apart, so the second line is hit by its y.
            assert_eq!(
                galley.pos_from_cursor(CCursor::new(first)).min.y,
                crate::shell::LINE_H
            );
            // Whitespace between words, and the space past the end of a line, are no target.
            assert!(target_at(&galley, text, centre(first + 5), &[]).is_none());
            assert!(target_at(&galley, text, Vec2::new(900.0, 27.0), &[]).is_none());
            // A mark on `:=` wins even though it is not an identifier.
            let mark = Mark {
                line: 1,
                col: 8,
                severity: Severity::Error,
                message: "m".into(),
            };
            let at = text.find(":=").unwrap();
            let t = target_at(&galley, text, centre(at), std::slice::from_ref(&mark)).unwrap();
            assert_eq!((t.range, t.mark), (at..at + 1, Some(0)));
        });
        out.drop_without_applying_deltas();
    }
}
