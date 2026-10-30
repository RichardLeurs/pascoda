//! Find and replace: the matching rules for the editor's find bar and for the project search. UI-free.
//! Positions are character indices, as egui's text cursor uses.

use std::ops::Range;
use std::path::PathBuf;

/// How a search matches: case-sensitive or not, and whole words only or any part of a word.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Options {
    pub case: bool,
    pub word: bool,
}

fn same(a: char, b: char, o: Options) -> bool {
    if o.case {
        a == b
    } else {
        a == b || a.to_lowercase().eq(b.to_lowercase())
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The character ranges of every match of `query` in `text`, in order and without overlap. An empty
/// query matches nothing.
pub fn matches(text: &str, query: &str, o: Options) -> Vec<Range<usize>> {
    let t: Vec<char> = text.chars().collect();
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() || q.len() > t.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + q.len() <= t.len() {
        let hit = q.iter().enumerate().all(|(k, &c)| same(t[i + k], c, o));
        let end = i + q.len();
        let whole = !o.word
            || ((i == 0 || !is_word(t[i - 1])) && (end == t.len() || !is_word(t[end])));
        if hit && whole {
            out.push(i..end);
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// The index (into `found`) of the next match after the caret at `from` (its selection end), or the
/// previous one before `from` (its selection start), wrapping at the ends.
pub fn step(found: &[Range<usize>], from: usize, forward: bool) -> Option<usize> {
    if found.is_empty() {
        return None;
    }
    if forward {
        Some(found.iter().position(|r| r.start >= from).unwrap_or(0))
    } else {
        Some(found.iter().rposition(|r| r.start < from).unwrap_or(found.len() - 1))
    }
}

/// `text` with every match replaced by `with`, and how many were replaced.
pub fn replace_all(text: &str, query: &str, with: &str, o: Options) -> (String, usize) {
    let found = matches(text, query, o);
    if found.is_empty() {
        return (text.to_string(), 0);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for r in &found {
        out.extend(&chars[at..r.start]);
        out.push_str(with);
        at = r.end;
    }
    out.extend(&chars[at..]);
    (out, found.len())
}

/// The result of replacing one match: the new text, the index of the match now current (the next
/// one after the replacement, if any) and its range to select.
pub struct Replaced {
    pub text: String,
    pub index: Option<usize>,
    pub selected: Option<Range<usize>>,
}

/// Replaces one match with `with`. That is the match `current` (when it is still a match), else the
/// first one after `caret`. Then the match after the replacement is selected. `None` when there is no
/// match at all.
pub fn replace_one(text: &str, query: &str, o: Options, current: Option<usize>, caret: usize, with: &str) -> Option<Replaced> {
    let found = matches(text, query, o);
    let i = current
        .filter(|&i| i < found.len())
        .or_else(|| step(&found, caret, true))?;
    let r = found[i].clone();
    let new = replace_at(text, r.clone(), with);
    let end = r.start + with.chars().count();
    let next = matches(&new, query, o);
    let index = step(&next, end, true);
    Some(Replaced {
        text: new,
        selected: index.map(|j| next[j].clone()),
        index,
    })
}

/// The identifier under the character `index` of `text` (the one just before it when the caret sits
/// at its end), with its character range. `None` when there is no identifier there.
pub fn word_at(text: &str, index: usize) -> Option<(Range<usize>, String)> {
    let t: Vec<char> = text.chars().collect();
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    let mut at = index.min(t.len());
    if at == t.len() || !is_id(t[at]) {
        if at > 0 && is_id(t[at - 1]) {
            at -= 1;
        } else {
            return None;
        }
    }
    let start = (0..=at).rev().take_while(|&i| is_id(t[i])).last()?;
    let end = (at..t.len()).take_while(|&i| is_id(t[i])).last()? + 1;
    if t[start].is_ascii_digit() {
        return None;
    }
    Some((start..end, t[start..end].iter().collect()))
}

/// Applies replacements given as (0-based line, character column) ranges, end exclusive. They must
/// not overlap. An edit that starts on a line the text does not have is left out (a wrong position
/// from the server must not write text at the end of the file), and a column past its line's end
/// is clamped to that end.
pub fn apply_edits(text: &str, edits: &[((u32, u32), (u32, u32), String)]) -> String {
    let mut line_start = vec![0usize];
    for (i, c) in text.chars().enumerate() {
        if c == '\n' {
            line_start.push(i + 1);
        }
    }
    let total = text.chars().count();
    let chars_on_line = |line: usize| {
        let end = line_start.get(line + 1).map_or(total, |next| next - 1);
        end - line_start[line]
    };
    let index = |(line, col): (u32, u32)| {
        let base = line_start[line as usize];
        base + (col as usize).min(chars_on_line(line as usize))
    };
    let mut chars: Vec<char> = text.chars().collect();
    let mut sorted: Vec<_> = edits
        .iter()
        .filter(|(s, _, _)| (s.0 as usize) < line_start.len())
        .filter(|(_, e, _)| (e.0 as usize) < line_start.len())
        .map(|(s, e, new)| (index(*s), index(*e).max(index(*s)), new))
        .collect();
    // Last first, so the earlier positions stay valid.
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, end, new) in sorted {
        chars.splice(start..end, new.chars());
    }
    chars.into_iter().collect()
}

/// `text` with the characters `r` replaced by `with`.
pub fn replace_at(text: &str, r: Range<usize>, with: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out: String = chars[..r.start].iter().collect();
    out.push_str(with);
    out.extend(&chars[r.end..]);
    out
}

/// What the editor's find bar holds between frames.
#[derive(Default)]
pub struct Bar {
    pub open: bool,
    /// The replace row is shown.
    pub replace: bool,
    pub query: String,
    pub with: String,
    pub opts: Options,
    /// Asks for keyboard focus in the query field on the next frame.
    pub focus: bool,
    /// The match the editor selected last (an index into the editor's matches).
    pub current: Option<usize>,
    /// A match to scroll into view on the next frame.
    pub scroll: Option<Range<usize>>,
    /// What the last replace did, shown in the bar.
    pub note: String,
    /// A next (true) or previous (false) step asked for by the keyboard this frame.
    pub pending: Option<bool>,
    /// The next frame seeds the query from the editor's selection (on Cmd+F).
    pub from_selection: bool,
}

impl Bar {
    /// Opens the bar (with the replace row when `replace`), focuses the query, and takes the query
    /// from the editor's selection.
    pub fn show(&mut self, replace: bool) {
        self.open = true;
        self.replace |= replace;
        self.focus = true;
        self.from_selection = true;
    }
}

/// One line of a file that matches, for the project search: the 1-based line and column of the
/// match and the whole line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hit {
    pub line: u32,
    pub col: u32,
    pub text: String,
    /// The match's character range inside `text`, from the start of the file.
    pub range: Range<usize>,
}

/// Every match in `text`, with its line and column.
pub fn hits(text: &str, query: &str, o: Options) -> Vec<Hit> {
    let found = matches(text, query, o);
    if found.is_empty() {
        return Vec::new();
    }
    // The first character of every line, so a match finds its line by binary search.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut line_start = Vec::with_capacity(lines.len());
    let mut pos = 0usize;
    for l in &lines {
        line_start.push(pos);
        pos += l.chars().count() + 1;
    }
    found
        .into_iter()
        .map(|r| {
            let idx = line_start.partition_point(|&s| s <= r.start).saturating_sub(1);
            Hit {
                line: idx as u32 + 1,
                col: (r.start - line_start[idx]) as u32 + 1,
                text: lines[idx].to_string(),
                range: r,
            }
        })
        .collect()
}

/// The matches of one file of the project.
pub struct FileHits {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
}

/// The matches in every file, in the order given, stopping after `limit` matches in all. The flag
/// says whether that limit cut the search short.
pub fn project_hits<'a>(
    files: impl IntoIterator<Item = (PathBuf, &'a str)>,
    query: &str,
    o: Options,
    limit: usize,
) -> (Vec<FileHits>, bool) {
    let mut out = Vec::new();
    let mut total = 0;
    for (path, text) in files {
        let mut hits = hits(text, query, o);
        if hits.is_empty() {
            continue;
        }
        let room = limit.saturating_sub(total);
        let cut = hits.len() > room;
        hits.truncate(room);
        total += hits.len();
        out.push(FileHits { path, hits });
        if cut || total >= limit {
            return (out, true);
        }
    }
    (out, false)
}

/// The files whose text changes under a replace of every match, with the new text and the number of
/// matches replaced in each. Files without a match are left out.
pub fn replace_in_files(
    files: impl IntoIterator<Item = (PathBuf, String)>,
    query: &str,
    with: &str,
    o: Options,
) -> Vec<(PathBuf, String, usize)> {
    files
        .into_iter()
        .filter_map(|(path, text)| {
            let (new, n) = replace_all(&text, query, with, o);
            (n > 0).then_some((path, new, n))
        })
        .collect()
}

/// What the project search tab holds between frames.
#[derive(Default)]
pub struct ProjectBar {
    pub query: String,
    pub with: String,
    pub opts: Options,
    /// Asks for keyboard focus in the query field on the next frame.
    pub focus: bool,
    pub results: Vec<FileHits>,
    /// What the results are, when they are not a search ("References to total").
    pub title: String,
    /// The limit cut the results short.
    pub truncated: bool,
    /// A search has run, so "no results" is true and not just "nothing typed yet".
    pub searched: bool,
    pub note: String,
}

/// The most matches the project search lists.
pub const PROJECT_LIMIT: usize = 1000;

/// The state of the Rename Symbol box, and of the rename it sent.
#[derive(Default)]
pub struct Rename {
    pub open: bool,
    /// The symbol being renamed and where it is (0-based line and column) in `path`.
    pub old: String,
    pub path: Option<std::path::PathBuf>,
    pub line: u32,
    pub col: u32,
    pub name: String,
    pub focus: bool,
    pub error: String,
    /// The rename on its way to the server.
    pub sent: Option<Sent>,
}

/// A rename asked of the server: the names, and the open files' text when it was asked. A change to
/// an open file since then makes the server's positions wrong, so nothing is applied.
pub struct Sent {
    pub old: String,
    pub new: String,
    pub open: Vec<(std::path::PathBuf, String)>,
}

/// The state of the Go to Line box.
#[derive(Default)]
pub struct GoTo {
    pub open: bool,
    pub text: String,
    /// Why the last try was refused (empty when none).
    pub error: String,
    /// Asks for keyboard focus in the field on the next frame.
    pub focus: bool,
}

/// Reads "line" or "line:column" (both 1-based; the column defaults to 1) for a file of `lines`
/// lines. A line past the end, or text that is not a number, is refused with the reason.
pub fn parse_position(input: &str, lines: u32) -> Result<(u32, u32), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Type a line number".into());
    }
    let (line_text, col_text) = match input.split_once(':') {
        Some((l, c)) => (l.trim(), Some(c.trim())),
        None => (input, None),
    };
    let line: u32 = line_text
        .parse()
        .map_err(|_| format!("“{line_text}” is not a line number"))?;
    if line == 0 || line > lines {
        return Err(format!("Line {line} is not in the file (it has {lines} lines)"));
    }
    let col = match col_text {
        None => 1,
        Some(c) => c
            .parse::<u32>()
            .ok()
            .filter(|&c| c >= 1)
            .ok_or_else(|| format!("“{c}” is not a column number"))?,
    };
    Ok((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASE: Options = Options { case: true, word: false };
    const ANY: Options = Options { case: false, word: false };
    const WORD: Options = Options { case: false, word: true };

    #[test]
    fn finds_every_match_without_overlap_and_respects_case() {
        assert_eq!(matches("aaaa", "aa", ANY), vec![0..2, 2..4]);
        assert_eq!(matches("Foo foo FOO", "foo", ANY).len(), 3);
        assert_eq!(matches("Foo foo FOO", "foo", CASE), vec![4..7]);
        assert!(matches("abc", "", ANY).is_empty());
        assert!(matches("ab", "abc", ANY).is_empty());
    }

    #[test]
    fn whole_words_skip_parts_of_identifiers() {
        assert_eq!(matches("count counter recount count_", "count", WORD), vec![0..5]);
        assert_eq!(matches("count", "count", WORD), vec![0..5]);
    }

    #[test]
    fn characters_are_counted_not_bytes() {
        // "é" is one character but two bytes: the range is in characters, as the editor cursor is.
        assert_eq!(matches("é x", "x", ANY), vec![2..3]);
    }

    #[test]
    fn steps_wrap_around_and_start_from_the_caret() {
        let found = vec![2..5, 10..13, 20..23];
        assert_eq!(step(&found, 0, true), Some(0));
        assert_eq!(step(&found, 5, true), Some(1), "from the end of the current match");
        assert_eq!(step(&found, 21, true), Some(0), "wraps forward");
        assert_eq!(step(&found, 2, false), Some(2), "wraps backward");
        assert_eq!(step(&found, 11, false), Some(1));
        assert_eq!(step(&[], 0, true), None);
    }

    #[test]
    fn replace_all_counts_and_keeps_the_rest() {
        assert_eq!(replace_all("a foo b foo", "foo", "bar", ANY), ("a bar b bar".to_string(), 2));
        assert_eq!(replace_all("Foo", "foo", "x", CASE), ("Foo".to_string(), 0));
        assert_eq!(replace_all("é foo", "foo", "ü", ANY), ("é ü".to_string(), 1));
    }

    #[test]
    fn replace_one_replaces_the_current_match_and_selects_the_next() {
        let r = replace_one("a foo b foo c", "foo", ANY, Some(0), 0, "bar").unwrap();
        assert_eq!(r.text, "a bar b foo c");
        assert_eq!(r.selected, Some(8..11), "the next match is selected");
        assert_eq!(r.index, Some(0), "an index into the matches after the replacement");
        // The last match: nothing left to select.
        let last = replace_one("foo", "foo", ANY, Some(0), 0, "x").unwrap();
        assert_eq!((last.text.as_str(), last.selected, last.index), ("x", None, None));
        // No current match: the one after the caret is used.
        let from_caret = replace_one("foo foo", "foo", ANY, None, 2, "y").unwrap();
        assert_eq!(from_caret.text, "foo y");
        assert!(replace_one("abc", "foo", ANY, None, 0, "y").is_none());
    }

    #[test]
    fn project_hits_group_by_file_and_stop_at_the_limit() {
        let a = PathBuf::from("/p/a.pas");
        let b = PathBuf::from("/p/b.pas");
        let files = vec![(a.clone(), "x\nx"), (b.clone(), "none"), (PathBuf::from("/p/c.pas"), "x")];
        let (all, cut) = project_hits(files.clone(), "x", ANY, 10);
        assert!(!cut);
        assert_eq!(all.iter().map(|f| (f.path.clone(), f.hits.len())).collect::<Vec<_>>(), vec![(a.clone(), 2), (PathBuf::from("/p/c.pas"), 1)]);
        let (few, cut) = project_hits(files, "x", ANY, 2);
        assert!(cut);
        assert_eq!(few.iter().map(|f| f.hits.len()).sum::<usize>(), 2);
    }

    #[test]
    fn replace_in_files_reports_only_the_files_that_change() {
        let out = replace_in_files(
            vec![(PathBuf::from("a"), "foo foo".to_string()), (PathBuf::from("b"), "bar".to_string())],
            "foo",
            "x",
            ANY,
        );
        assert_eq!(out, vec![(PathBuf::from("a"), "x x".to_string(), 2)]);
    }

    #[test]
    fn go_to_takes_a_line_or_a_line_and_column_within_the_file() {
        assert_eq!(parse_position("8", 12), Ok((8, 1)));
        assert_eq!(parse_position(" 3 : 5 ", 12), Ok((3, 5)));
        assert_eq!(parse_position("12", 12), Ok((12, 1)), "the last line");
        assert!(parse_position("13", 12).unwrap_err().contains("has 12 lines"));
        assert!(parse_position("0", 12).is_err());
        assert!(parse_position("", 12).is_err());
        assert!(parse_position("abc", 12).unwrap_err().contains("not a line number"));
        assert!(parse_position("4:0", 12).unwrap_err().contains("not a column number"));
        assert!(parse_position("4:x", 12).is_err());
    }

    #[test]
    fn word_at_finds_the_identifier_under_or_before_the_caret() {
        let text = "total := total + 1;";
        assert_eq!(word_at(text, 1), Some((0..5, "total".into())));
        assert_eq!(word_at(text, 5), Some((0..5, "total".into())), "caret at the end of a word");
        assert_eq!(word_at(text, 6), None, "a space is not a word");
        assert_eq!(word_at(text, 11), Some((9..14, "total".into())));
        assert_eq!(word_at("x := 12;", 6), None, "numbers are not names");
        assert_eq!(word_at("é := 1", 0), Some((0..1, "é".into())));
    }

    #[test]
    fn apply_edits_replaces_each_range_by_line_and_character() {
        let text = "a := 1;
b := a;
";
        let edits = vec![
            ((0, 0), (0, 1), "x".to_string()),
            ((1, 5), (1, 6), "x".to_string()),
        ];
        assert_eq!(apply_edits(text, &edits), "x := 1;
b := x;
");
        // Wide characters count as one.
        assert_eq!(apply_edits("é a", &[((0, 2), (0, 3), "bb".into())]), "é bb");
        assert_eq!(apply_edits("ab", &[((5, 0), (5, 1), "x".into())]), "ab", "a line past the end is left out");
        assert_eq!(apply_edits("ab\ncd", &[((0, 9), (0, 9), "!".into())]), "ab!\ncd", "a column past the line end is clamped");
    }

    #[test]
    fn replace_at_splices_by_character() {
        assert_eq!(replace_at("é foo", 2..5, "bar"), "é bar");
        assert_eq!(replace_at("ab", 1..1, "X"), "aXb");
    }

    #[test]
    fn hits_give_line_column_and_the_line_text() {
        let text = "program p;\nbegin\n  WriteLn('hi');\nend.\n";
        let h = hits(text, "hi", ANY);
        assert_eq!(h.len(), 1);
        assert_eq!((h[0].line, h[0].col), (3, 12));
        assert_eq!(h[0].text, "  WriteLn('hi');");
        let twice = hits("x\nx x", "x", ANY);
        assert_eq!(twice.iter().map(|h| (h.line, h.col)).collect::<Vec<_>>(), vec![(1, 1), (2, 1), (2, 3)]);
    }
}
