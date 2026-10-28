//! A form file plus edits to it. Every edit is a text patch on the original source, so the
//! parts of the file we do not touch (formatting, unknown properties) stay byte-for-byte.

use crate::lfm::{self, ParseError, write_component, write_value};
use crate::model::{Component, PropValue};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    NoSuchComponent,
    NoSuchProperty(String),
    /// The form itself cannot be removed.
    CannotRemoveRoot,
    /// An edit produced text that does not parse. This is a bug; the document is unchanged.
    Invalid(ParseError),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NoSuchComponent => write!(f, "no such component"),
            EditError::NoSuchProperty(p) => write!(f, "no property `{p}`"),
            EditError::CannotRemoveRoot => write!(f, "cannot remove the form itself"),
            EditError::Invalid(e) => write!(f, "edit produced invalid text: {e}"),
        }
    }
}

impl std::error::Error for EditError {}

/// Lazarus writes the geometry first, in this order.
const GEOMETRY: [&str; 4] = ["Left", "Height", "Top", "Width"];

fn geometry_rank(name: &str) -> Option<usize> {
    GEOMETRY.iter().position(|g| g.eq_ignore_ascii_case(name))
}

#[derive(Debug, Clone)]
pub struct FormDoc {
    source: String,
    root: Component,
}

impl FormDoc {
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        Ok(Self {
            root: lfm::parse(source)?,
            source: source.to_string(),
        })
    }

    /// The text to save. Identical to what was read when nothing was edited.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn root(&self) -> &Component {
        &self.root
    }

    /// The component at `path`: child indexes from the root, `[]` being the form itself.
    pub fn component(&self, path: &[usize]) -> Option<&Component> {
        path.iter().try_fold(&self.root, |c, &i| c.children.get(i))
    }

    fn nl(&self) -> &'static str {
        if self.source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Sets a property of the component at `path`, replacing its value or adding it.
    pub fn set_prop(
        &mut self,
        path: &[usize],
        name: &str,
        value: PropValue,
    ) -> Result<(), EditError> {
        let nl = self.nl();
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let indent = format!("{}  ", c.indent);
        let text = write_value(&value, &indent, nl);
        let edit = if let Some(p) = c.props.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
            (p.value_span.clone(), text)
        } else {
            // Geometry goes right after the previous geometry property (or the header);
            // anything else goes after the last property.
            let after = match geometry_rank(name) {
                Some(rank) => c
                    .props
                    .iter()
                    .rfind(|p| geometry_rank(&p.name).is_some_and(|r| r < rank))
                    .map_or(c.header_end, |p| p.span.end),
                None => c.props.last().map_or(c.header_end, |p| p.span.end),
            };
            (after..after, format!("{nl}{indent}{name} = {text}"))
        };
        self.apply(edit)
    }

    pub fn remove_prop(&mut self, path: &[usize], name: &str) -> Result<(), EditError> {
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let p = c
            .props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| EditError::NoSuchProperty(name.into()))?;
        let range = self.line_range(&p.span);
        self.apply((range, String::new()))
    }

    /// Adds `child` as the last child of the component at `path`.
    pub fn add_child(&mut self, path: &[usize], child: Component) -> Result<(), EditError> {
        let nl = self.nl();
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let text = write_component(&child, &format!("{}  ", c.indent), nl);
        let line_start = self.line_start(c.end_start);
        let edit = if self.source[line_start..c.end_start].trim().is_empty() {
            (line_start..line_start, format!("{text}{nl}"))
        } else {
            // `end` shares a line with other text: start a new line for the child.
            (
                c.end_start..c.end_start,
                format!("{nl}{text}{nl}{}", c.indent),
            )
        };
        self.apply(edit)
    }

    /// Inserts `child` as child number `index` of the component at `path` (at the end when
    /// `index` is past the last child).
    pub fn insert_child(
        &mut self,
        path: &[usize],
        index: usize,
        child: Component,
    ) -> Result<(), EditError> {
        let nl = self.nl();
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let indent = format!("{}  ", c.indent);
        let text = write_component(&child, &indent, nl);
        let (pos, prefix, suffix) = self.insertion_point(path, index)?;
        self.apply((pos..pos, format!("{prefix}{text}{nl}{suffix}")))
    }

    /// Where whole lines for a new child number `index` of `path` go: the position, and text
    /// to put before and after them when the spot does not start a line of its own.
    fn insertion_point(
        &self,
        path: &[usize],
        index: usize,
    ) -> Result<(usize, String, String), EditError> {
        let nl = self.nl();
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let at = c.children.get(index).map_or(c.end_start, |s| s.span.start);
        let line_start = self.line_start(at);
        Ok(if self.source[line_start..at].trim().is_empty() {
            (line_start, String::new(), String::new())
        } else {
            let after = c.children.get(index).map_or(&c.indent, |s| &s.indent);
            (at, nl.to_string(), after.clone())
        })
    }

    /// Moves the component at `from` (with everything inside it) to be child number `index` of
    /// the component at `to_parent`; `index` counts the new parent's children after the
    /// component has been taken out, and `to_parent` is a path in the document as it is now.
    /// The component's own text moves unchanged apart from its indentation. Returns its new
    /// path.
    pub fn move_component(
        &mut self,
        from: &[usize],
        to_parent: &[usize],
        index: usize,
    ) -> Result<Vec<usize>, EditError> {
        if from.is_empty() {
            return Err(EditError::CannotRemoveRoot);
        }
        if to_parent.starts_with(from) {
            // Into itself.
            return Err(EditError::NoSuchComponent);
        }
        let c = self
            .component(from)
            .ok_or(EditError::NoSuchComponent)?
            .clone();
        self.component(to_parent)
            .ok_or(EditError::NoSuchComponent)?;
        // The target parent's path once `from` is gone: a later sibling of `from` (or one of
        // its descendants) moves up by one.
        let mut target = to_parent.to_vec();
        let depth = from.len() - 1;
        if target.len() > depth && target[..depth] == from[..depth] && target[depth] > from[depth] {
            target[depth] -= 1;
        }
        let nl = self.nl();
        let range = self.line_range(&c.span);
        let owns_lines = range != c.span;
        let mut moved = self.clone();
        moved.apply((range.clone(), String::new()))?;
        let parent = moved.component(&target).ok_or(EditError::NoSuchComponent)?;
        let new_indent = format!("{}  ", parent.indent);
        let index = index.min(parent.children.len());
        let body = if owns_lines {
            let text = self.source[range].trim_end_matches(['\r', '\n']);
            reindent(text, &c.indent, &new_indent)
        } else {
            None
        }
        .unwrap_or_else(|| write_component(&c, &new_indent, nl));
        let (pos, prefix, suffix) = moved.insertion_point(&target, index)?;
        moved.apply((pos..pos, format!("{prefix}{body}{nl}{suffix}")))?;
        *self = moved;
        target.push(index);
        Ok(target)
    }

    pub fn remove_component(&mut self, path: &[usize]) -> Result<(), EditError> {
        if path.is_empty() {
            return Err(EditError::CannotRemoveRoot);
        }
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let range = self.line_range(&c.span);
        self.apply((range, String::new()))
    }

    /// Renames a component. Event handler names and code that refer to it are not changed.
    pub fn rename_component(&mut self, path: &[usize], new_name: &str) -> Result<(), EditError> {
        let c = self.component(path).ok_or(EditError::NoSuchComponent)?;
        let edit = if c.name.is_empty() {
            (c.name_span.clone(), format!("{new_name}: "))
        } else {
            (c.name_span.clone(), new_name.to_string())
        };
        self.apply(edit)
    }

    fn line_start(&self, pos: usize) -> usize {
        self.source[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    /// The whole lines holding `span`, including the line break after it, when nothing else
    /// shares those lines; otherwise just `span`.
    fn line_range(&self, span: &Range<usize>) -> Range<usize> {
        let start = self.line_start(span.start);
        let rest = &self.source[span.end..];
        let line_end = rest
            .find('\n')
            .map_or(self.source.len(), |i| span.end + i + 1);
        let before_blank = self.source[start..span.start].trim().is_empty();
        let after_blank = self.source[span.end..line_end].trim().is_empty();
        if before_blank && after_blank {
            start..line_end
        } else {
            span.clone()
        }
    }

    fn apply(&mut self, (range, text): (Range<usize>, String)) -> Result<(), EditError> {
        let mut source = self.source.clone();
        source.replace_range(range, &text);
        let root = lfm::parse(&source).map_err(EditError::Invalid)?;
        self.source = source;
        self.root = root;
        Ok(())
    }
}

/// `text` (whole lines) with the leading `old` indentation of every line replaced by `new`.
/// `None` when a non-blank line does not start with `old`.
fn reindent(text: &str, old: &str, new: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        if body.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        out.push_str(new);
        out.push_str(line.strip_prefix(old)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = "object Form1: TForm1\n  Left = 200\n  Top = 100\n  Caption = 'Form1'\n  object Button1: TButton\n    Left = 16\n    Width = 100\n    OnClick = Button1Click\n  end\nend\n";

    #[test]
    fn untouched_documents_are_byte_identical() {
        assert_eq!(FormDoc::parse(FORM).unwrap().source(), FORM);
    }

    #[test]
    fn replaces_one_value_and_nothing_else() {
        let mut d = FormDoc::parse(FORM).unwrap();
        d.set_prop(&[0], "Width", PropValue::Int(120)).unwrap();
        assert_eq!(d.source(), FORM.replace("Width = 100", "Width = 120"));
        assert_eq!(
            d.component(&[0]).unwrap().prop("width"),
            Some(&PropValue::Int(120))
        );
    }

    #[test]
    fn inserts_geometry_in_lazarus_order() {
        let mut d = FormDoc::parse(FORM).unwrap();
        d.set_prop(&[], "Height", PropValue::Int(300)).unwrap(); // between Left and Top
        d.set_prop(&[], "Width", PropValue::Int(400)).unwrap(); // after Top
        d.set_prop(&[], "Color", PropValue::Ident("clWhite".into()))
            .unwrap(); // at the end
        let names: Vec<_> = d.root().props.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Left", "Height", "Top", "Width", "Caption", "Color"]
        );
        assert!(
            d.source()
                .contains("  Height = 300\n  Top = 100\n  Width = 400\n")
        );
    }

    #[test]
    fn inserts_into_a_component_without_properties() {
        let mut d = FormDoc::parse("object F: TF\nend\n").unwrap();
        d.set_prop(&[], "Caption", PropValue::Str("Hi".into()))
            .unwrap();
        assert_eq!(d.source(), "object F: TF\n  Caption = 'Hi'\nend\n");
    }

    #[test]
    fn removes_a_property_with_its_line() {
        let mut d = FormDoc::parse(FORM).unwrap();
        d.remove_prop(&[0], "OnClick").unwrap();
        assert_eq!(d.source(), FORM.replace("    OnClick = Button1Click\n", ""));
        assert_eq!(
            d.remove_prop(&[0], "Nope"),
            Err(EditError::NoSuchProperty("Nope".into()))
        );
    }

    #[test]
    fn adds_and_removes_components() {
        let mut d = FormDoc::parse(FORM).unwrap();
        let label = Component::new("Label1", "TLabel")
            .with_prop("Left", PropValue::Int(16))
            .with_prop("Caption", PropValue::Str("Label1".into()));
        d.add_child(&[], label).unwrap();
        assert_eq!(
            d.source(),
            FORM.replace(
                "  end\nend\n",
                "  end\n  object Label1: TLabel\n    Left = 16\n    Caption = 'Label1'\n  end\nend\n"
            )
        );
        assert_eq!(d.root().children.len(), 2);
        d.remove_component(&[1]).unwrap();
        assert_eq!(d.source(), FORM);
        assert_eq!(d.remove_component(&[]), Err(EditError::CannotRemoveRoot));
        assert_eq!(d.remove_component(&[7]), Err(EditError::NoSuchComponent));
    }

    #[test]
    fn renames_only_the_component_name() {
        let mut d = FormDoc::parse(FORM).unwrap();
        d.rename_component(&[0], "OkButton").unwrap();
        assert_eq!(
            d.source(),
            FORM.replace("object Button1:", "object OkButton:")
        );
    }

    #[test]
    fn keeps_windows_line_endings() {
        let crlf = FORM.replace('\n', "\r\n");
        let mut d = FormDoc::parse(&crlf).unwrap();
        d.set_prop(&[], "Color", PropValue::Ident("clWhite".into()))
            .unwrap();
        d.add_child(&[], Component::new("L", "TLabel")).unwrap();
        assert!(
            !d.source().replace("\r\n", "").contains('\n'),
            "a bare LF slipped in"
        );
    }

    #[test]
    fn multi_line_values_are_indented() {
        let mut d = FormDoc::parse("object F: TF\n  object M: TMemo\n  end\nend\n").unwrap();
        let lines = PropValue::List(vec![PropValue::Str("a".into()), PropValue::Str("b".into())]);
        d.set_prop(&[0], "Lines.Strings", lines.clone()).unwrap();
        assert_eq!(
            d.source(),
            "object F: TF\n  object M: TMemo\n    Lines.Strings = (\n      'a'\n      'b')\n  end\nend\n"
        );
        assert_eq!(
            d.component(&[0]).unwrap().prop("Lines.Strings"),
            Some(&lines)
        );
    }

    const MENU: &str = "object F: TF\n  object M: TMainMenu\n    Left = 16\n    object A: TMenuItem\n      Caption = 'A'\n      object A1: TMenuItem\n        Caption = 'A1'\n      end\n    end\n    object B: TMenuItem\n      Caption  =  'B'\n    end\n  end\nend\n";

    fn names(d: &FormDoc, path: &[usize]) -> Vec<String> {
        d.component(path)
            .unwrap()
            .children
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    #[test]
    fn inserts_a_child_at_a_position() {
        let mut d = FormDoc::parse(MENU).unwrap();
        let item =
            Component::new("N", "TMenuItem").with_prop("Caption", PropValue::Str("-".into()));
        d.insert_child(&[0], 1, item.clone()).unwrap();
        assert_eq!(names(&d, &[0]), ["A", "N", "B"]);
        assert!(d.source().contains(
            "    end\n    object N: TMenuItem\n      Caption = '-'\n    end\n    object B"
        ));
        d.insert_child(&[0], 9, Component::new("Z", "TMenuItem"))
            .unwrap();
        assert_eq!(names(&d, &[0]), ["A", "N", "B", "Z"]);
    }

    #[test]
    fn moves_keep_the_text_and_undo_exactly() {
        let mut d = FormDoc::parse(MENU).unwrap();
        // B up before A: its comment and spacing travel with it.
        assert_eq!(d.move_component(&[0, 1], &[0], 0).unwrap(), vec![0, 0]);
        assert_eq!(names(&d, &[0]), ["B", "A"]);
        assert!(
            d.source()
                .contains("    object B: TMenuItem\n      Caption  =  'B'\n    end\n    object A")
        );
        assert_eq!(d.move_component(&[0, 0], &[0], 1).unwrap(), vec![0, 1]);
        assert_eq!(d.source(), MENU);

        // Indent B into A (after A1), then out again.
        let p = d.move_component(&[0, 1], &[0, 0], 1).unwrap();
        assert_eq!(p, vec![0, 0, 1]);
        assert!(
            d.source()
                .contains("      object B: TMenuItem\n        Caption  =  'B'\n      end\n")
        );
        assert_eq!(names(&d, &[0, 0]), ["A1", "B"]);
        assert_eq!(d.move_component(&p, &[0], 1).unwrap(), vec![0, 1]);
        assert_eq!(d.source(), MENU);

        // A1 out of A, to the front of the menu: the target path is given as it was before.
        assert_eq!(d.move_component(&[0, 0, 0], &[0], 0).unwrap(), vec![0, 0]);
        assert_eq!(names(&d, &[0]), ["A1", "A", "B"]);
        // Into a later sibling's subtree: the target path shifts once the item is out.
        let p = d.move_component(&[0, 0], &[0, 2], 0).unwrap();
        assert_eq!(p, vec![0, 1, 0]);
        assert_eq!(d.component(&p).unwrap().name, "A1");
        assert_eq!(
            d.move_component(&[0], &[0, 0], 0),
            Err(EditError::NoSuchComponent)
        );
    }
}
