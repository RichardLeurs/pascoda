//! Just enough Markdown for chat answers: headings, paragraphs, bullet and numbered lists,
//! quotes, tables (shown as they are), rules, fenced code blocks and inline `code`, **bold**
//! and *italic*. Streaming text is parsed again each frame, so an unfinished construct must
//! come out sensibly (an open code fence is a code block that is not `closed` yet).

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// `•` or `1.`
    pub marker: String,
    /// Nesting level from the indentation (0, 1, 2).
    pub depth: usize,
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    List(Vec<Item>),
    Quote(Vec<Span>),
    /// Rows of a table, kept as typed (drawn in a monospace font).
    Table(Vec<String>),
    Rule,
    Code {
        lang: String,
        code: String,
        closed: bool,
    },
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut list: Vec<Item> = Vec::new();
    let mut table: Vec<String> = Vec::new();
    let mut lines = text.lines().peekable();

    fn flush(
        blocks: &mut Vec<Block>,
        para: &mut Vec<&str>,
        list: &mut Vec<Item>,
        table: &mut Vec<String>,
    ) {
        if !para.is_empty() {
            blocks.push(Block::Paragraph(inline(&para.join(" "))));
            para.clear();
        }
        if !list.is_empty() {
            blocks.push(Block::List(std::mem::take(list)));
        }
        if !table.is_empty() {
            blocks.push(Block::Table(std::mem::take(table)));
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if let Some(fence) = fence_of(trimmed) {
            flush(&mut blocks, &mut para, &mut list, &mut table);
            let lang = trimmed.trim_start_matches(fence).trim().to_string();
            let mut code: Vec<&str> = Vec::new();
            let mut closed = false;
            for l in lines.by_ref() {
                let t = l.trim();
                if t.starts_with(fence) && t.trim_start_matches(fence).trim().is_empty() {
                    closed = true;
                    break;
                }
                code.push(l);
            }
            blocks.push(Block::Code {
                lang,
                code: code.join("\n"),
                closed,
            });
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut blocks, &mut para, &mut list, &mut table);
            continue;
        }
        if let Some((level, rest)) = heading(trimmed) {
            flush(&mut blocks, &mut para, &mut list, &mut table);
            blocks.push(Block::Heading(level, inline(rest)));
            continue;
        }
        if is_rule(trimmed) {
            flush(&mut blocks, &mut para, &mut list, &mut table);
            blocks.push(Block::Rule);
            continue;
        }
        if trimmed.starts_with('|') {
            if !table.is_empty() || para.is_empty() {
                if !para.is_empty() || !list.is_empty() {
                    flush(&mut blocks, &mut para, &mut list, &mut table);
                }
                table.push(trimmed.to_string());
                continue;
            }
        } else if !table.is_empty() {
            flush(&mut blocks, &mut para, &mut list, &mut table);
        }
        if let Some((marker, rest)) = list_marker(trimmed) {
            if !para.is_empty() {
                flush(&mut blocks, &mut para, &mut list, &mut table);
            }
            let indent = line.len() - line.trim_start().len();
            list.push(Item {
                marker,
                depth: (indent / 2).min(3),
                spans: inline(rest),
            });
            continue;
        }
        if let Some(q) = trimmed.strip_prefix('>') {
            flush(&mut blocks, &mut para, &mut list, &mut table);
            blocks.push(Block::Quote(inline(q.trim())));
            continue;
        }
        if let Some(last) = list.last_mut().filter(|_| line.starts_with(' ')) {
            // A wrapped list item.
            last.spans.push(Span {
                text: " ".into(),
                ..Default::default()
            });
            last.spans.extend(inline(trimmed));
            continue;
        }
        flush(&mut blocks, &mut Vec::new(), &mut list, &mut table);
        para.push(trimmed);
    }
    flush(&mut blocks, &mut para, &mut list, &mut table);
    blocks
}

fn fence_of(t: &str) -> Option<&'static str> {
    ["```", "~~~"].into_iter().find(|f| t.starts_with(f))
}

fn heading(t: &str) -> Option<(u8, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    let rest = t[n..].strip_prefix(' ')?;
    (1..=6).contains(&n).then_some((n as u8, rest.trim()))
}

fn is_rule(t: &str) -> bool {
    t.len() >= 3
        && ["-", "*", "_"]
            .iter()
            .any(|c| t.chars().all(|x| x.to_string() == *c || x == ' '))
        && t.chars().filter(|c| !c.is_whitespace()).count() >= 3
}

fn list_marker(t: &str) -> Option<(String, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some(("•".into(), rest.trim()));
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &t[digits..];
        for sep in [". ", ") "] {
            if let Some(r) = rest.strip_prefix(sep) {
                return Some((format!("{}.", &t[..digits]), r.trim()));
            }
        }
    }
    None
}

/// Inline spans of one paragraph.
pub fn inline(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    inline_into(s, false, false, &mut out);
    merge(out)
}

fn push(out: &mut Vec<Span>, text: &str, bold: bool, italic: bool, code: bool) {
    if !text.is_empty() {
        out.push(Span {
            text: text.to_string(),
            bold,
            italic,
            code,
        });
    }
}

fn inline_into(s: &str, bold: bool, italic: bool, out: &mut Vec<Span>) {
    let chars: Vec<char> = s.chars().collect();
    let mut plain = String::new();
    let mut i = 0;
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&k| chars[k..].starts_with(pat))
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() && "\\`*_[]()#".contains(chars[i + 1]) => {
                plain.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                let run = chars[i..].iter().take_while(|c| **c == '`').count();
                let pat = vec!['`'; run];
                if let Some(end) = find(i + run, &pat) {
                    push(out, &std::mem::take(&mut plain), bold, italic, false);
                    let body: String = chars[i + run..end].iter().collect();
                    push(out, body.trim_matches(' '), bold, italic, true);
                    i = end + run;
                } else {
                    plain.extend(&chars[i..i + run]);
                    i += run;
                }
            }
            '*' if chars.get(i + 1) == Some(&'*') => {
                match find(i + 2, &['*', '*']).filter(|&e| e > i + 2) {
                    Some(end) => {
                        push(out, &std::mem::take(&mut plain), bold, italic, false);
                        let inner: String = chars[i + 2..end].iter().collect();
                        inline_into(&inner, true, italic, out);
                        i = end + 2;
                    }
                    None => {
                        plain.push_str("**");
                        i += 2;
                    }
                }
            }
            '*' if chars
                .get(i + 1)
                .is_some_and(|n| !n.is_whitespace() && *n != '*') =>
            {
                match find(i + 1, &['*']).filter(|&e| e > i + 1 && !chars[e - 1].is_whitespace()) {
                    Some(end) => {
                        push(out, &std::mem::take(&mut plain), bold, italic, false);
                        let inner: String = chars[i + 1..end].iter().collect();
                        inline_into(&inner, bold, true, out);
                        i = end + 1;
                    }
                    None => {
                        plain.push('*');
                        i += 1;
                    }
                }
            }
            '[' => {
                // [text](url): keep the text.
                let close = find(i + 1, &[']']);
                match close.filter(|&c| chars.get(c + 1) == Some(&'(')) {
                    Some(c) => match find(c + 2, &[')']) {
                        Some(end) => {
                            push(out, &std::mem::take(&mut plain), bold, italic, false);
                            let inner: String = chars[i + 1..c].iter().collect();
                            inline_into(&inner, bold, italic, out);
                            i = end + 1;
                        }
                        None => {
                            plain.push('[');
                            i += 1;
                        }
                    },
                    None => {
                        plain.push('[');
                        i += 1;
                    }
                }
            }
            _ => {
                plain.push(c);
                i += 1;
            }
        }
    }
    push(out, &plain, bold, italic, false);
}

fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        match out.last_mut() {
            Some(l) if !l.code && !s.code && l.bold == s.bold && l.italic == s.italic => {
                l.text.push_str(&s.text)
            }
            _ => out.push(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(t: &str) -> Span {
        Span {
            text: t.into(),
            ..Default::default()
        }
    }

    #[test]
    fn inline_styles() {
        let s = inline("Use `WriteLn` or **bold `x`** and *it* now");
        assert_eq!(s[0], plain("Use "));
        assert_eq!(
            s[1],
            Span {
                text: "WriteLn".into(),
                code: true,
                ..Default::default()
            }
        );
        assert_eq!(s[2], plain(" or "));
        assert!(s[3].bold && !s[3].code && s[3].text == "bold ");
        assert!(s[4].bold && s[4].code && s[4].text == "x");
        assert!(s[6].italic && s[6].text == "it");
        assert_eq!(s.last().unwrap(), &plain(" now"));
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(inline("a * b ** c ` d"), vec![plain("a * b ** c ` d")]);
        assert_eq!(inline("2*3"), vec![plain("2*3")]);
        assert_eq!(inline(r"\*no\*"), vec![plain("*no*")]);
    }

    #[test]
    fn links_keep_their_text() {
        assert_eq!(
            inline("see [the docs](http://x.y) now"),
            vec![plain("see the docs now")]
        );
    }

    #[test]
    fn blocks() {
        let md = "# Title\nSome text\nmore text\n\n- one\n- two `x`\n  wrapped\n\n1. first\n2. second\n\n> quote\n\n---\n";
        let b = parse(md);
        assert!(matches!(&b[0], Block::Heading(1, s) if s[0].text == "Title"));
        assert!(matches!(&b[1], Block::Paragraph(s) if s[0].text == "Some text more text"));
        let Block::List(items) = &b[2] else {
            panic!("{b:?}")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].marker, "•");
        assert!(items[1].spans.iter().any(|s| s.code));
        assert!(items[1].spans.last().unwrap().text.contains("wrapped"));
        let Block::List(nums) = &b[3] else { panic!() };
        assert_eq!(nums[1].marker, "2.");
        assert!(matches!(&b[4], Block::Quote(_)));
        assert_eq!(b[5], Block::Rule);
    }

    #[test]
    fn code_fences_closed_and_open() {
        let b = parse("Before\n```pascal\nbegin\n  x := 1;\nend;\n```\nAfter");
        assert_eq!(b.len(), 3);
        assert_eq!(
            b[1],
            Block::Code {
                lang: "pascal".into(),
                code: "begin\n  x := 1;\nend;".into(),
                closed: true
            }
        );
        // streaming: the closing fence has not arrived
        let open = parse("Text\n```pascal\nbegin\n");
        assert_eq!(
            open[1],
            Block::Code {
                lang: "pascal".into(),
                code: "begin".into(),
                closed: false
            }
        );
        // markdown inside a fence is not parsed
        let b = parse("```\n# not a heading\n- nor a list\n```");
        assert!(matches!(&b[0], Block::Code { code, .. } if code.contains("# not")));
    }

    #[test]
    fn tables_are_kept_verbatim() {
        let b = parse("| a | b |\n|---|---|\n| 1 | 2 |\n\ntext");
        assert!(matches!(&b[0], Block::Table(r) if r.len() == 3));
        assert!(matches!(&b[1], Block::Paragraph(_)));
    }

    #[test]
    fn unicode_does_not_panic() {
        let _ = parse("**é** `ü` [ö](x) *ä* \u{1F600} ```");
    }
}
