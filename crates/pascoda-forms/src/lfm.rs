//! Reading and writing Lazarus form files (`.lfm`, the same grammar as Delphi's text `.dfm`).
//!
//! The parser records where every component and property sits in the source, so edits can be
//! text patches and everything we do not touch stays byte-for-byte as it was.

use crate::model::{Component, Kind, PropValue, Property};
use std::fmt::Write;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based.
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Whether `bytes` is a binary Delphi form (`.dfm` written as a binary stream). Those start
/// with `0xFF` (Delphi 2 onwards, then the `TFORM1` header of Delphi 1) or with `TPF0`. Text
/// forms start with `object` or `inherited`.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.first() == Some(&0xFF) || bytes.starts_with(b"TPF0") || bytes.starts_with(b"TFORM1")
}

pub fn parse(source: &str) -> Result<Component, ParseError> {
    let mut p = Parser {
        src: source,
        b: source.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let root = p.component()?;
    p.skip_ws();
    if p.pos < p.b.len() {
        return Err(p.error("text after the form's closing `end`"));
    }
    Ok(root)
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
}

type Res<T> = Result<T, ParseError>;

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

impl<'a> Parser<'a> {
    fn error(&self, message: &str) -> ParseError {
        let before = &self.src[..self.pos.min(self.src.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        ParseError {
            line,
            col,
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Res<()> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected `{}`", c as char)))
        }
    }

    /// An identifier, with dots between parts (`Font.Style`, `Unit1.Foo`).
    fn ident(&mut self) -> Res<(&'a str, Range<usize>)> {
        self.skip_ws();
        let start = self.pos;
        if !self.peek().is_some_and(is_ident_start) {
            return Err(self.error("expected a name"));
        }
        loop {
            while self.peek().is_some_and(is_ident_char) {
                self.pos += 1;
            }
            let dotted = self.peek() == Some(b'.')
                && self
                    .b
                    .get(self.pos + 1)
                    .copied()
                    .is_some_and(is_ident_start);
            if !dotted {
                break;
            }
            self.pos += 1;
        }
        Ok((&self.src[start..self.pos], start..self.pos))
    }

    fn peek_ident(&mut self) -> Option<&'a str> {
        self.skip_ws();
        let save = self.pos;
        let r = self.ident().ok().map(|(s, _)| s);
        self.pos = save;
        r
    }

    fn component(&mut self) -> Res<Component> {
        self.skip_ws();
        let start = self.pos;
        let (kw, _) = self.ident()?;
        let kind = match kw.to_ascii_lowercase().as_str() {
            "object" => Kind::Object,
            "inherited" => Kind::Inherited,
            "inline" => Kind::Inline,
            _ => {
                self.pos = start;
                return Err(self.error("expected `object`, `inherited` or `inline`"));
            }
        };
        // `object Name: TClass`, or just `object TClass` for an unnamed component.
        let (first, first_span) = self.ident()?;
        self.skip_ws();
        let (name, name_span, class) = if self.peek() == Some(b':') {
            self.pos += 1;
            let (class, _) = self.ident()?;
            (first, first_span, class)
        } else {
            ("", first_span.start..first_span.start, first)
        };
        let mut header_end = self.pos;
        let mut index = None;
        self.skip_ws();
        if self.peek() == Some(b'[') {
            self.pos += 1;
            let v = self.number()?;
            index = v.as_int();
            self.expect(b']')?;
            header_end = self.pos;
        }
        let line_start = self.src[..start].rfind('\n').map_or(0, |i| i + 1);
        let indent = &self.src[line_start..start];
        let indent = if indent.trim().is_empty() { indent } else { "" };

        let mut c = Component {
            kind,
            name: name.into(),
            class: class.into(),
            index,
            props: Vec::new(),
            children: Vec::new(),
            span: start..start,
            name_span,
            header_end,
            end_start: 0,
            indent: indent.into(),
        };
        loop {
            self.skip_ws();
            let Some(word) = self.peek_ident() else {
                return Err(self.error("expected a property, a child object or `end`"));
            };
            match word.to_ascii_lowercase().as_str() {
                "end" => {
                    c.end_start = self.pos;
                    self.pos += 3;
                    c.span = start..self.pos;
                    return Ok(c);
                }
                "object" | "inherited" | "inline" => c.children.push(self.component()?),
                _ => c.props.push(self.property()?),
            }
        }
    }

    fn property(&mut self) -> Res<Property> {
        let (name, name_span) = self.ident()?;
        self.expect(b'=')?;
        self.skip_ws();
        let value_start = self.pos;
        let value = self.value()?;
        Ok(Property {
            name: name.into(),
            value,
            span: name_span.start..self.pos,
            value_span: value_start..self.pos,
        })
    }

    fn value(&mut self) -> Res<PropValue> {
        self.skip_ws();
        match self.peek() {
            Some(b'\'' | b'#') => self.string(),
            Some(b'-' | b'$') => self.number(),
            Some(c) if c.is_ascii_digit() => self.number(),
            Some(b'[') => self.set(),
            Some(b'(') => self.list(),
            Some(b'<') => self.collection(),
            Some(b'{') => self.binary(),
            Some(c) if is_ident_start(c) => Ok(PropValue::Ident(self.ident()?.0.into())),
            _ => Err(self.error("expected a value")),
        }
    }

    /// One or more quoted pieces and `#code` characters, with optional `+` between them.
    fn string(&mut self) -> Res<PropValue> {
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            match self.peek() {
                Some(b'\'') => {
                    self.pos += 1;
                    loop {
                        match self.peek() {
                            None | Some(b'\n') => return Err(self.error("unterminated string")),
                            Some(b'\'') if self.b.get(self.pos + 1) == Some(&b'\'') => {
                                bytes.push(b'\'');
                                self.pos += 2;
                            }
                            Some(b'\'') => {
                                self.pos += 1;
                                break;
                            }
                            Some(c) => {
                                bytes.push(c);
                                self.pos += 1;
                            }
                        }
                    }
                }
                Some(b'#') => {
                    self.pos += 1;
                    let code = self.char_code()?;
                    match u8::try_from(code) {
                        Ok(b) => bytes.push(b),
                        Err(_) => {
                            let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                            let mut buf = [0; 4];
                            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
                _ => {
                    // A `+` continues the string with the next piece.
                    let save = self.pos;
                    self.skip_ws();
                    if self.peek() == Some(b'+') {
                        self.pos += 1;
                        self.skip_ws();
                        if matches!(self.peek(), Some(b'\'' | b'#')) {
                            continue;
                        }
                    }
                    self.pos = save;
                    break;
                }
            }
        }
        Ok(PropValue::Str(String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn char_code(&mut self) -> Res<u32> {
        let hex = self.peek() == Some(b'$');
        if hex {
            self.pos += 1;
        }
        let start = self.pos;
        while self.peek().is_some_and(|c| {
            if hex {
                c.is_ascii_hexdigit()
            } else {
                c.is_ascii_digit()
            }
        }) {
            self.pos += 1;
        }
        u32::from_str_radix(&self.src[start..self.pos], if hex { 16 } else { 10 })
            .map_err(|_| self.error("bad character code"))
    }

    fn number(&mut self) -> Res<PropValue> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        if self.peek() == Some(b'$') {
            self.pos += 1;
            let digits = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            let neg = self.src[start..].starts_with('-');
            return Ok(match i64::from_str_radix(&self.src[digits..self.pos], 16) {
                Ok(v) => PropValue::Int(if neg { -v } else { v }),
                Err(_) => PropValue::Raw(self.src[start..self.pos].into()),
            });
        }
        let digits = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits {
            return Err(self.error("expected a number"));
        }
        let mut float = false;
        if self.peek() == Some(b'.') && self.b.get(self.pos + 1).is_some_and(u8::is_ascii_digit) {
            float = true;
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mut p = self.pos + 1;
            if matches!(self.b.get(p), Some(b'+' | b'-')) {
                p += 1;
            }
            if self.b.get(p).is_some_and(u8::is_ascii_digit) {
                float = true;
                self.pos = p;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let text = &self.src[start..self.pos];
        Ok(if float {
            text.parse()
                .map_or_else(|_| PropValue::Raw(text.into()), PropValue::Float)
        } else {
            text.parse()
                .map_or_else(|_| PropValue::Raw(text.into()), PropValue::Int)
        })
    }

    fn set(&mut self) -> Res<PropValue> {
        self.pos += 1; // [
        let mut members = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(PropValue::Set(members));
                }
                Some(b',') => self.pos += 1,
                Some(c) if is_ident_start(c) => members.push(self.ident()?.0.into()),
                Some(c) if c.is_ascii_digit() || c == b'-' => {
                    let v = self.number()?;
                    members.push(match v {
                        PropValue::Int(i) => i.to_string(),
                        PropValue::Raw(r) => r,
                        other => format!("{other:?}"),
                    });
                }
                _ => return Err(self.error("expected a set member or `]`")),
            }
        }
    }

    fn list(&mut self) -> Res<PropValue> {
        self.pos += 1; // (
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    return Ok(PropValue::List(items));
                }
                None => return Err(self.error("unterminated list")),
                _ => items.push(self.value()?),
            }
        }
    }

    fn collection(&mut self) -> Res<PropValue> {
        self.pos += 1; // <
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'>') => {
                    self.pos += 1;
                    return Ok(PropValue::Collection(items));
                }
                Some(c) if is_ident_start(c) => {
                    let (kw, _) = self.ident()?;
                    if !kw.eq_ignore_ascii_case("item") {
                        return Err(self.error("expected `item` or `>`"));
                    }
                    let mut props = Vec::new();
                    loop {
                        self.skip_ws();
                        if self
                            .peek_ident()
                            .is_some_and(|w| w.eq_ignore_ascii_case("end"))
                        {
                            self.ident()?;
                            break;
                        }
                        let p = self.property()?;
                        props.push((p.name, p.value));
                    }
                    items.push(props);
                }
                _ => return Err(self.error("expected `item` or `>`")),
            }
        }
    }

    fn binary(&mut self) -> Res<PropValue> {
        self.pos += 1; // {
        let mut digits = Vec::new();
        loop {
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(c) if c.is_ascii_hexdigit() => {
                    digits.push(c);
                    self.pos += 1;
                }
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                _ => return Err(self.error("bad binary data")),
            }
        }
        if digits.len() % 2 != 0 {
            return Err(self.error("binary data has an odd number of hex digits"));
        }
        let bytes = digits
            .chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or("0"), 16).unwrap_or(0))
            .collect();
        Ok(PropValue::Binary(bytes))
    }
}

// ---- writing ---------------------------------------------------------------------------

/// Text for a value, as Lazarus would write it. `indent` is the indentation of the property.
pub fn write_value(v: &PropValue, indent: &str, nl: &str) -> String {
    let inner = format!("{indent}  ");
    match v {
        PropValue::Int(i) => i.to_string(),
        PropValue::Float(f) => {
            let s = f.to_string();
            if s.contains(['.', 'e', 'E']) || !f.is_finite() {
                s
            } else {
                format!("{s}.0")
            }
        }
        PropValue::Str(s) => write_string(s),
        PropValue::Ident(s) | PropValue::Raw(s) => s.clone(),
        PropValue::Set(m) => format!("[{}]", m.join(", ")),
        PropValue::List(items) if items.is_empty() => "()".into(),
        PropValue::List(items) => {
            let lines: Vec<String> = items.iter().map(|i| write_value(i, &inner, nl)).collect();
            format!("({nl}{inner}{})", lines.join(&format!("{nl}{inner}")))
        }
        PropValue::Collection(items) if items.is_empty() => "<>".into(),
        PropValue::Collection(items) => {
            let mut out = format!("<{nl}");
            for (n, item) in items.iter().enumerate() {
                let _ = write!(out, "{inner}item{nl}");
                for (name, value) in item {
                    let _ = write!(
                        out,
                        "{inner}  {name} = {}{nl}",
                        write_value(value, &format!("{inner}  "), nl)
                    );
                }
                let _ = write!(out, "{inner}end");
                out.push_str(if n + 1 == items.len() { ">" } else { nl });
            }
            out
        }
        PropValue::Binary(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
            let mut out = format!("{{{nl}");
            for chunk in hex.as_bytes().chunks(64) {
                let _ = write!(
                    out,
                    "{inner}{}{nl}",
                    std::str::from_utf8(chunk).unwrap_or("")
                );
            }
            let _ = write!(out, "{indent}}}");
            out
        }
    }
}

/// Quoted text, with control characters as `#code`.
fn write_string(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let mut out = String::new();
    let mut quoted = false;
    for ch in s.chars() {
        if (ch as u32) < 32 {
            if quoted {
                out.push('\'');
                quoted = false;
            }
            let _ = write!(out, "#{}", ch as u32);
        } else {
            if !quoted {
                out.push('\'');
                quoted = true;
            }
            if ch == '\'' {
                out.push('\'');
            }
            out.push(ch);
        }
    }
    if quoted {
        out.push('\'');
    }
    out
}

/// Text for a component and everything in it. `indent` is the indentation of its first line;
/// the result has no trailing newline.
pub fn write_component(c: &Component, indent: &str, nl: &str) -> String {
    let inner = format!("{indent}  ");
    let mut out = if c.name.is_empty() {
        format!("{indent}{} {}", c.kind.keyword(), c.class)
    } else {
        format!("{indent}{} {}: {}", c.kind.keyword(), c.name, c.class)
    };
    if let Some(i) = c.index {
        let _ = write!(out, " [{i}]");
    }
    out.push_str(nl);
    for p in &c.props {
        let _ = write!(
            out,
            "{inner}{} = {}{nl}",
            p.name,
            write_value(&p.value, &inner, nl)
        );
    }
    for child in &c.children {
        out.push_str(&write_component(child, &inner, nl));
        out.push_str(nl);
    }
    let _ = write!(out, "{indent}end");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "object Form1: TForm1\n  Left = 200\n  Caption = 'It''s'#13#10'two'\n  Color = clBtnFace\n  object Button1: TButton\n    Width = -5\n    OnClick = Button1Click\n  end\nend\n";

    #[test]
    fn parses_structure_and_values() {
        let f = parse(SAMPLE).unwrap();
        assert_eq!((f.name.as_str(), f.class.as_str()), ("Form1", "TForm1"));
        assert_eq!(f.prop("left").and_then(PropValue::as_int), Some(200));
        assert_eq!(
            f.prop("Caption").and_then(PropValue::as_str),
            Some("It's\r\ntwo")
        );
        assert_eq!(
            f.prop("Color").and_then(PropValue::as_ident),
            Some("clBtnFace")
        );
        let b = f.child("button1").unwrap();
        assert_eq!(b.prop("Width").and_then(PropValue::as_int), Some(-5));
        assert_eq!(
            &SAMPLE[b.span.clone()],
            "object Button1: TButton\n    Width = -5\n    OnClick = Button1Click\n  end"
        );
        assert_eq!(b.indent, "  ");
    }

    #[test]
    fn parses_every_value_shape() {
        let src = "object F: TF\n  A = $FF\n  B = 1.5e+2\n  C = [fsBold, fsItalic]\n  D = ()\n  E = (\n    'a'\n    'b' +\n    'c')\n  F = <\n    item\n      Width = 10\n    end\n    item\n    end>\n  G = {\n    00FF\n    10 20\n  }\n  H.Style = [fsBold]\n  I = 99999999999999999999\nend";
        let f = parse(src).unwrap();
        assert_eq!(f.prop("A"), Some(&PropValue::Int(255)));
        assert_eq!(f.prop("B"), Some(&PropValue::Float(150.0)));
        assert_eq!(
            f.prop("C"),
            Some(&PropValue::Set(vec!["fsBold".into(), "fsItalic".into()]))
        );
        assert_eq!(f.prop("D"), Some(&PropValue::List(vec![])));
        assert_eq!(
            f.prop("E"),
            Some(&PropValue::List(vec![
                PropValue::Str("a".into()),
                PropValue::Str("bc".into())
            ]))
        );
        assert_eq!(
            f.prop("F"),
            Some(&PropValue::Collection(vec![
                vec![("Width".into(), PropValue::Int(10))],
                vec![]
            ]))
        );
        assert_eq!(
            f.prop("G"),
            Some(&PropValue::Binary(vec![0, 0xFF, 0x10, 0x20]))
        );
        assert!(f.prop("H.Style").is_some());
        assert_eq!(
            f.prop("I"),
            Some(&PropValue::Raw("99999999999999999999".into()))
        );
    }

    #[test]
    fn parses_unnamed_components() {
        let f =
            parse("object F: TF\n  inline TList\n    object TItem\n    end\n  end\nend").unwrap();
        let list = &f.children[0];
        assert_eq!(
            (list.name.as_str(), list.class.as_str(), list.kind),
            ("", "TList", Kind::Inline)
        );
        assert_eq!(list.children[0].class, "TItem");
        let again = parse(&write_component(&f, "", "\n")).unwrap();
        assert!(f.same_as(&again));
    }

    #[test]
    fn reports_errors_with_position() {
        let e = parse("object F: TF\n  Left = \nend").unwrap_err();
        assert_eq!(e.line, 3);
        let e = parse("object F: TF\n  Caption = 'open\nend").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(parse("object F: TF\nend\nstray").is_err());
    }

    #[test]
    fn written_text_parses_back_to_the_same_tree() {
        let f = parse(SAMPLE).unwrap();
        let again = parse(&write_component(&f, "", "\n")).unwrap();
        assert!(f.same_as(&again));
    }

    #[test]
    fn writes_strings_the_way_lazarus_does() {
        assert_eq!(write_string("It's"), "'It''s'");
        assert_eq!(write_string("a\r\nb"), "'a'#13#10'b'");
        assert_eq!(write_string(""), "''");
        assert_eq!(write_string("\ta"), "#9'a'");
    }
}
