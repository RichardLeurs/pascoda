//! Minimal Pascal syntax highlighting. To be replaced by tree-sitter-pascal.

use crate::style::Palette;
use eframe::egui::{Color32, FontId, TextFormat, text::LayoutJob};

const KEYWORDS: &[&str] = &[
    "and",
    "array",
    "as",
    "begin",
    "case",
    "class",
    "const",
    "constructor",
    "destructor",
    "div",
    "do",
    "downto",
    "else",
    "end",
    "except",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implementation",
    "in",
    "inherited",
    "interface",
    "is",
    "mod",
    "nil",
    "not",
    "of",
    "or",
    "private",
    "procedure",
    "program",
    "property",
    "protected",
    "public",
    "record",
    "repeat",
    "set",
    "shl",
    "shr",
    "then",
    "to",
    "true",
    "try",
    "type",
    "unit",
    "until",
    "uses",
    "var",
    "while",
    "with",
    "xor",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    UserType,
    BuiltinType,
    Call,
}

fn color(kind: Kind, pal: &Palette) -> Color32 {
    match kind {
        Kind::Plain => pal.code_text,
        Kind::Keyword => pal.keyword,
        Kind::String => pal.string,
        Kind::Comment => pal.comment,
        Kind::Number => pal.number,
        Kind::UserType => pal.user_type,
        Kind::BuiltinType => pal.builtin_type,
        Kind::Call => pal.call,
    }
}

const BUILTIN_TYPES: &[&str] = &[
    "integer",
    "string",
    "boolean",
    "char",
    "byte",
    "word",
    "longint",
    "int64",
    "double",
    "single",
    "real",
    "extended",
    "pointer",
    "shortint",
    "smallint",
    "cardinal",
    "longword",
    "qword",
    "widestring",
    "ansistring",
    "shortstring",
    "variant",
    "pchar",
    "tobject",
    "tclass",
];

/// `TForm1`, `TButton`: a `T` followed by a capital, the convention for Pascal types.
fn looks_like_type(word: &str) -> bool {
    let mut c = word.chars();
    c.next() == Some('T') && c.next().is_some_and(|x| x.is_ascii_uppercase())
}

/// Splits `text` into (kind, byte range) tokens covering the whole string.
fn tokenize(text: &str) -> Vec<(Kind, usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let kind = match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                Kind::Comment
            }
            b'{' => {
                while i < b.len() && b[i] != b'}' {
                    i += 1;
                }
                i = (i + 1).min(b.len());
                Kind::Comment
            }
            b'(' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b')')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                Kind::Comment
            }
            b'\'' => {
                i += 1;
                while i < b.len() && b[i] != b'\'' && b[i] != b'\n' {
                    i += 1;
                }
                i = (i + 1).min(b.len());
                Kind::String
            }
            c if c.is_ascii_digit() => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                Kind::Number
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let word = &text[start..i];
                let lower = word.to_ascii_lowercase();
                if KEYWORDS.contains(&lower.as_str()) {
                    Kind::Keyword
                } else if BUILTIN_TYPES.contains(&lower.as_str()) {
                    Kind::BuiltinType
                } else if looks_like_type(word) {
                    Kind::UserType
                } else if text[i..].trim_start_matches([' ', '\t']).starts_with('(') {
                    Kind::Call
                } else {
                    Kind::Plain
                }
            }
            _ => {
                // Advance by a whole UTF-8 character so slices stay valid.
                i += text[i..].chars().next().map_or(1, char::len_utf8);
                Kind::Plain
            }
        };
        out.push((kind, start, i));
    }
    out
}

/// `line_height` spaces the lines out the way Xcode does (about 1.5 times the font size).
pub fn layout(text: &str, pal: &Palette, font: FontId, line_height: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (kind, s, e) in tokenize(text) {
        let mut format = TextFormat::simple(font.clone(), color(kind, pal));
        format.line_height = Some(line_height);
        job.append(&text[s..e], 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Kind, &str)> {
        tokenize(src)
            .into_iter()
            .map(|(k, s, e)| (k, &src[s..e]))
            .filter(|(k, _)| *k != Kind::Plain)
            .collect()
    }

    #[test]
    fn classifies_tokens() {
        let got = kinds("BEGIN WriteLn('hi'); // x\nend.");
        assert_eq!(
            got,
            vec![
                (Kind::Keyword, "BEGIN"),
                (Kind::Call, "WriteLn"),
                (Kind::String, "'hi'"),
                (Kind::Comment, "// x"),
                (Kind::Keyword, "end"),
            ]
        );
    }

    #[test]
    fn tells_types_and_calls_from_plain_names() {
        let got = kinds("var x: Integer; f: TForm1; begin Foo(x); y := z; end");
        assert!(got.contains(&(Kind::BuiltinType, "Integer")));
        assert!(got.contains(&(Kind::UserType, "TForm1")));
        assert!(got.contains(&(Kind::Call, "Foo")));
        assert!(!got.iter().any(|(_, w)| *w == "x" || *w == "y" || *w == "z"));
        assert!(!looks_like_type("Tab") && !looks_like_type("T") && looks_like_type("TX"));
    }

    #[test]
    fn survives_unterminated_and_unicode() {
        let _ = tokenize("{ open comment é");
        let _ = tokenize("'open string é");
        let _ = tokenize("(* open");
    }
}
