//! A small line diff for showing proposals: longest common subsequence on the lines between the
//! common prefix and suffix, and unified-diff style hunks with a few lines of context.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Same,
    Add,
    Del,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    pub kind: Kind,
    pub text: String,
    /// 1-based line numbers in the old and the new text.
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    /// `@@ -a,b +c,d @@`
    Header(String),
    Line(Line),
}

/// Beyond this many cells the middle part is shown as one big replacement.
const MAX_CELLS: usize = 4_000_000;

fn split(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// Every line of both texts, marked as unchanged, added or removed.
pub fn lines(old: &str, new: &str) -> Vec<Line> {
    let (a, b) = (split(old), split(new));
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);

    // ops over the middle: (kind, index into am / bm)
    let mut ops: Vec<(Kind, usize)> = Vec::new();
    if am.len() * bm.len() > MAX_CELLS {
        ops.extend((0..am.len()).map(|i| (Kind::Del, i)));
        ops.extend((0..bm.len()).map(|j| (Kind::Add, j)));
    } else {
        let w = bm.len() + 1;
        let mut t = vec![0u32; (am.len() + 1) * w];
        for i in (0..am.len()).rev() {
            for j in (0..bm.len()).rev() {
                t[i * w + j] = if am[i] == bm[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < am.len() || j < bm.len() {
            if i < am.len() && j < bm.len() && am[i] == bm[j] {
                ops.push((Kind::Same, i));
                i += 1;
                j += 1;
            } else if i < am.len() && (j == bm.len() || t[(i + 1) * w + j] >= t[i * w + j + 1]) {
                ops.push((Kind::Del, i));
                i += 1;
            } else {
                ops.push((Kind::Add, j));
                j += 1;
            }
        }
    }

    let mut out = Vec::with_capacity(a.len().max(b.len()));
    let (mut o, mut n) = (0usize, 0usize);
    let mut push = |kind: Kind, text: &str, out: &mut Vec<Line>| {
        let (old_no, new_no) = match kind {
            Kind::Same => {
                o += 1;
                n += 1;
                (Some(o), Some(n))
            }
            Kind::Del => {
                o += 1;
                (Some(o), None)
            }
            Kind::Add => {
                n += 1;
                (None, Some(n))
            }
        };
        out.push(Line {
            kind,
            text: text.to_string(),
            old_no,
            new_no,
        });
    };
    for l in &a[..prefix] {
        push(Kind::Same, l, &mut out);
    }
    for (kind, idx) in ops {
        let text = if kind == Kind::Add { bm[idx] } else { am[idx] };
        push(kind, text, &mut out);
    }
    for l in &a[a.len() - suffix..] {
        push(Kind::Same, l, &mut out);
    }
    out
}

/// `(added, removed)` line counts.
pub fn stats(lines: &[Line]) -> (usize, usize) {
    let count = |k| lines.iter().filter(|l| l.kind == k).count();
    (count(Kind::Add), count(Kind::Del))
}

/// Hunks with `context` unchanged lines around each change, each led by a header row. Equal
/// texts give no rows.
pub fn hunks(all: &[Line], context: usize) -> Vec<Row> {
    let changed: Vec<usize> = (0..all.len())
        .filter(|&i| all[i].kind != Kind::Same)
        .collect();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        let (s, e) = (i.saturating_sub(context), (i + context + 1).min(all.len()));
        match ranges.last_mut() {
            Some(last) if s <= last.1 => last.1 = e,
            _ => ranges.push((s, e)),
        }
    }
    let mut rows = Vec::new();
    for (s, e) in ranges {
        let slice = &all[s..e];
        let old_start = slice.iter().find_map(|l| l.old_no);
        let new_start = slice.iter().find_map(|l| l.new_no);
        let old_n = slice.iter().filter(|l| l.kind != Kind::Add).count();
        let new_n = slice.iter().filter(|l| l.kind != Kind::Del).count();
        // A range with no old (or new) lines points at the line before it, as `diff -u` does.
        let start = |first: Option<usize>, before: fn(&Line) -> Option<usize>| {
            first.unwrap_or_else(|| all[..s].iter().rev().find_map(before).unwrap_or(0))
        };
        rows.push(Row::Header(format!(
            "@@ -{},{} +{},{} @@",
            start(old_start, |l| l.old_no),
            old_n,
            start(new_start, |l| l.new_no),
            new_n
        )));
        rows.extend(slice.iter().cloned().map(Row::Line));
    }
    rows
}

/// Convenience: hunks of the difference between two texts, with its `(added, removed)` counts.
pub fn unified(old: &str, new: &str, context: usize) -> (Vec<Row>, (usize, usize)) {
    let all = lines(old, new);
    (hunks(&all, context), stats(&all))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(old: &str, new: &str) -> String {
        lines(old, new)
            .iter()
            .map(|l| match l.kind {
                Kind::Same => ' ',
                Kind::Add => '+',
                Kind::Del => '-',
            })
            .collect()
    }

    #[test]
    fn equal_texts_have_no_hunks() {
        assert!(unified("a\nb\n", "a\nb\n", 3).0.is_empty());
        assert_eq!(unified("a\nb\n", "a\nb\n", 3).1, (0, 0));
    }

    #[test]
    fn marks_replaced_inserted_and_removed_lines() {
        assert_eq!(kinds("a\nb\nc\n", "a\nB\nc\n"), " -+ ");
        assert_eq!(kinds("a\nc\n", "a\nb\nc\n"), " + ");
        assert_eq!(kinds("a\nb\nc\n", "a\nc\n"), " - ");
        assert_eq!(kinds("", "x\ny\n"), "++");
        assert_eq!(kinds("x\n", ""), "-");
    }

    #[test]
    fn keeps_line_numbers_for_both_sides() {
        let l = lines("a\nb\nc\n", "a\nX\nY\nc\n");
        let num: Vec<_> = l.iter().map(|l| (l.old_no, l.new_no)).collect();
        assert_eq!(
            num,
            vec![
                (Some(1), Some(1)),
                (Some(2), None),
                (None, Some(2)),
                (None, Some(3)),
                (Some(3), Some(4))
            ]
        );
    }

    #[test]
    fn hunks_have_context_and_headers() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l10\n", "ten\n").replace("l2\n", "two\n");
        let (rows, (add, del)) = unified(&old, &new, 2);
        assert_eq!((add, del), (2, 2));
        let heads: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Header(h) => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(heads, vec!["@@ -1,4 +1,4 @@", "@@ -8,5 +8,5 @@"]);
    }

    #[test]
    fn nearby_changes_share_a_hunk() {
        let (rows, _) = unified("a\nb\nc\nd\n", "A\nb\nc\nD\n", 3);
        assert_eq!(
            rows.iter().filter(|r| matches!(r, Row::Header(_))).count(),
            1
        );
    }

    #[test]
    fn crlf_is_ignored_for_display_and_huge_inputs_do_not_hang() {
        assert_eq!(lines("a\r\nb\r\n", "a\r\nb\r\n")[0].text, "a");
        let old: String = (0..3000).map(|i| format!("a{i}\n")).collect();
        let new: String = (0..3000).map(|i| format!("b{i}\n")).collect();
        let (_, (add, del)) = unified(&old, &new, 3);
        assert_eq!((add, del), (3000, 3000));
    }
}
