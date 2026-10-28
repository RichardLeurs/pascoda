//! Applies the unified-diff hunks Codex sends for an edited file (`@@ -a,b +c,d @@` plus
//! context, `-` and `+` lines) to a text, so the IDE can show and check the change itself.

/// `new` text of `old` after applying `diff`. Every context and removed line must match the
/// text (ignoring line-ending differences): when the file is not what Codex saw, the answer is an
/// error and nothing is guessed.
pub(crate) fn apply_unified(old: &str, diff: &str) -> Result<String, String> {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let crlf = old.contains("\r\n");
    let eol = if crlf { "\r\n" } else { "\n" };
    let same = |file_line: &str, diff_line: &str| {
        file_line.trim_end_matches(['\n', '\r']) == diff_line.trim_end_matches(['\n', '\r'])
    };
    let mut out = String::with_capacity(old.len() + diff.len());
    let mut idx = 0usize; // next unread line of `old_lines`
    let mut in_hunk = false;
    let mut hunks = 0;
    // What the previous diff line did, for a following `\ No newline at end of file`.
    let mut last: Option<char> = None;
    for line in diff.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if let Some(rest) = body.strip_prefix("@@") {
            let (a, b) = parse_header(rest).ok_or_else(|| format!("bad hunk header '{body}'"))?;
            let start = if b == 0 { a } else { a.saturating_sub(1) };
            if start < idx {
                return Err("the hunks overlap or are out of order".into());
            }
            if start > old_lines.len() {
                return Err("the change points past the end of the file".into());
            }
            for l in &old_lines[idx..start] {
                out.push_str(l);
            }
            idx = start;
            in_hunk = true;
            hunks += 1;
            last = None;
            continue;
        }
        if !in_hunk {
            continue; // `diff --git`, `---`, `+++` lines before the first hunk
        }
        match body.chars().next() {
            Some(' ') | None => {
                let text = body.get(1..).unwrap_or("");
                let cur = old_lines
                    .get(idx)
                    .ok_or("the file ends before the change does")?;
                if !same(cur, text) {
                    return Err(mismatch(idx, cur, text));
                }
                out.push_str(cur);
                idx += 1;
                last = Some(' ');
            }
            Some('-') => {
                let text = &body[1..];
                let cur = old_lines
                    .get(idx)
                    .ok_or("the file ends before the change does")?;
                if !same(cur, text) {
                    return Err(mismatch(idx, cur, text));
                }
                idx += 1;
                last = Some('-');
            }
            Some('+') => {
                out.push_str(&body[1..]);
                out.push_str(eol);
                last = Some('+');
            }
            Some('\\') => {
                // "\ No newline at end of file": the line before has no line ending.
                if matches!(last, Some('+' | ' ')) {
                    while out.ends_with('\n') || out.ends_with('\r') {
                        out.pop();
                    }
                }
            }
            Some(_) => return Err(format!("unexpected line in the change: '{body}'")),
        }
    }
    if hunks == 0 {
        return Err("the change has no hunks".into());
    }
    for l in &old_lines[idx.min(old_lines.len())..] {
        out.push_str(l);
    }
    Ok(out)
}

fn mismatch(idx: usize, file: &str, diff: &str) -> String {
    format!(
        "line {} is '{}' but the change expects '{}': the file is not what Codex saw",
        idx + 1,
        file.trim_end(),
        diff.trim_end()
    )
}

/// The old start line and line count of ` -a,b +c,d @@`.
fn parse_header(rest: &str) -> Option<(usize, usize)> {
    let minus = rest.split_whitespace().find(|t| t.starts_with('-'))?;
    let mut it = minus[1..].split(',');
    let a = it.next()?.parse().ok()?;
    let b = match it.next() {
        Some(n) => n.parse().ok()?,
        None => 1,
    };
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_line() {
        let old = "line one\nline two\nline three\n";
        let diff = "@@ -1,3 +1,3 @@\n line one\n-line two\n+line 2\n line three\n";
        assert_eq!(
            apply_unified(old, diff).unwrap(),
            "line one\nline 2\nline three\n"
        );
    }

    #[test]
    fn insertions_deletions_and_several_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let diff = "@@ -1,2 +1,3 @@\n a\n+a2\n b\n@@ -6,3 +7,2 @@\n f\n-g\n h\n";
        assert_eq!(
            apply_unified(old, diff).unwrap(),
            "a\na2\nb\nc\nd\ne\nf\nh\n"
        );
        // a pure insertion at the top and at the end
        assert_eq!(
            apply_unified("x\n", "@@ -0,0 +1,1 @@\n+first\n").unwrap(),
            "first\nx\n"
        );
        assert_eq!(
            apply_unified("x\n", "@@ -1,1 +1,2 @@\n x\n+last\n").unwrap(),
            "x\nlast\n"
        );
    }

    #[test]
    fn headers_before_the_first_hunk_are_ignored_and_counts_may_be_omitted() {
        let diff = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -2 +2 @@\n-b\n+B\n";
        assert_eq!(apply_unified("a\nb\nc\n", diff).unwrap(), "a\nB\nc\n");
    }

    #[test]
    fn a_file_that_differs_from_what_codex_saw_is_refused() {
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n";
        let e = apply_unified("a\nX\n", diff).unwrap_err();
        assert!(e.contains("not what Codex saw"), "{e}");
        assert!(apply_unified("a\n", diff).is_err(), "too short");
        assert!(apply_unified("a\n", "no hunks here").is_err());
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let old = "a\r\nb\r\nc\r\n";
        let diff = "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
        assert_eq!(apply_unified(old, diff).unwrap(), "a\r\nB\r\nc\r\n");
    }

    #[test]
    fn the_missing_final_newline_marker_is_honoured() {
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n";
        assert_eq!(apply_unified("a\nb", diff).unwrap(), "a\nB");
        // context after the change is copied as is
        let diff2 = "@@ -1,2 +1,2 @@\n-a\n+A\n b\n";
        assert_eq!(apply_unified("a\nb\nc\n", diff2).unwrap(), "A\nb\nc\n");
    }
}
