//! Inherited forms. A form whose `.lfm` starts with `inherited` builds on an ancestor form, whose
//! components it has. The designer works on the merged form: the ancestor's components with the
//! form's own changes applied, and the form's own components. Saving writes what Lazarus writes: an
//! `inherited` component for each ancestor component with the properties that differ from it, and the
//! form's own components in full.

use crate::lfm::write_component;
use crate::model::{Component, Kind, Property};

/// The properties of `base` with those of `over` applied: a property of `over` replaces the one of
/// the same name, and a new one is added.
fn overlay(base: &[Property], over: &[Property]) -> Vec<Property> {
    let mut out: Vec<Property> = base.to_vec();
    for p in over {
        match out.iter_mut().find(|q| q.name.eq_ignore_ascii_case(&p.name)) {
            Some(q) => *q = p.clone(),
            None => out.push(p.clone()),
        }
    }
    out
}

/// The properties of `over` whose value differs from the one `base` gives them (or that `base` lacks).
fn differing(base: &[Property], over: &[Property]) -> Vec<Property> {
    over.iter()
        .filter(|p| {
            base.iter()
                .find(|q| q.name.eq_ignore_ascii_case(&p.name))
                .is_none_or(|q| q.value != p.value)
        })
        .cloned()
        .collect()
}

fn find<'a>(list: &'a [Component], name: &str) -> Option<&'a Component> {
    list.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// The merged children of an ancestor's `children` and the form's own: each ancestor component with the
/// form's changes applied, then the form's own components (those the ancestor does not have).
fn merge_children(ancestor: &[Component], own: &[Component]) -> Vec<Component> {
    let mut out: Vec<Component> = ancestor
        .iter()
        .map(|a| {
            let mut m = a.clone();
            m.kind = Kind::Object;
            if let Some(c) = find(own, &a.name) {
                m.props = overlay(&a.props, &c.props);
                m.children = merge_children(&a.children, &c.children);
            }
            m
        })
        .collect();
    for c in own {
        if find(ancestor, &c.name).is_none() {
            let mut n = c.clone();
            n.kind = Kind::Object;
            out.push(n);
        }
    }
    out
}

/// The form as the designer edits it: the ancestor merged with the form's own root and components.
pub fn merge(own: &Component, ancestor: &Component) -> Component {
    let mut m = ancestor.clone();
    m.kind = Kind::Object;
    m.name = own.name.clone();
    m.class = own.class.clone();
    m.props = overlay(&ancestor.props, &own.props);
    m.children = merge_children(&ancestor.children, &own.children);
    m
}

/// The components of `merged` that the ancestor does not declare (the form's own).
fn own_part(merged: &Component, ancestor: &Component) -> Component {
    let mut own = merged.clone();
    own.children = merged
        .children
        .iter()
        .filter(|c| find(&ancestor.children, &c.name).is_none())
        .cloned()
        .collect();
    own
}

/// The `inherited` form Lazarus would save for `merged` on top of `ancestor`: a root `inherited` with its
/// differing properties, each ancestor component as an `inherited` entry with only its differing
/// properties (none at all when nothing differs), and the form's own components in full.
pub fn inherited_text(merged: &Component, ancestor: &Component, nl: &str) -> String {
    let mut out = Component::new(&merged.name, &merged.class);
    out.kind = Kind::Inherited;
    out.props = differing(&ancestor.props, &merged.props);
    out.children = inherited_children(&merged.children, &ancestor.children);
    let mut text = write_component(&out, "", nl);
    if !text.ends_with('\n') {
        text.push_str(nl);
    }
    text
}

fn inherited_children(merged: &[Component], ancestor: &[Component]) -> Vec<Component> {
    let mut out = Vec::new();
    for c in merged {
        match find(ancestor, &c.name) {
            Some(a) => {
                let mut i = Component::new(&c.name, &c.class);
                i.kind = Kind::Inherited;
                i.props = differing(&a.props, &c.props);
                i.children = inherited_children(&c.children, &a.children);
                if !i.props.is_empty() || !i.children.is_empty() {
                    out.push(i);
                }
            }
            None => out.push(c.clone()),
        }
    }
    out
}

/// The components of `merged` the form declares itself (not the ancestor's): what a unit's
/// declarations and handlers are made from.
pub fn own_components(merged: &Component, ancestor: &Component) -> Component {
    own_part(merged, ancestor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lfm::parse;
    use crate::model::PropValue;

    const BASE: &str = "object Form1: TForm1\n  Caption = 'Base'\n  object Button1: TButton\n    Left = 16\n    Caption = 'OK'\n  end\nend\n";
    const CHILD: &str = "inherited Form2: TForm2\n  Caption = 'Child'\n  inherited Button1: TButton\n    Left = 40\n  end\n  object Label9: TLabel\n    Caption = 'extra'\n  end\nend\n";

    #[test]
    fn the_merged_form_has_the_ancestor_components_with_the_changes_applied() {
        let base = parse(BASE).unwrap();
        let child = parse(CHILD).unwrap();
        let m = merge(&child, &base);
        assert_eq!(m.kind, Kind::Object);
        assert_eq!(m.name, "Form2");
        assert_eq!(m.class, "TForm2");
        assert_eq!(m.props.iter().find(|p| p.name == "Caption").unwrap().value, PropValue::Str("Child".into()));
        let button = m.children.iter().find(|c| c.name == "Button1").expect("inherited button is there");
        assert_eq!(button.props.iter().find(|p| p.name == "Left").unwrap().value, PropValue::Int(40), "the child's change");
        assert_eq!(button.props.iter().find(|p| p.name == "Caption").unwrap().value, PropValue::Str("OK".into()), "the ancestor's value");
        assert!(m.children.iter().any(|c| c.name == "Label9"), "the own component");
    }

    #[test]
    fn saving_writes_only_what_differs_from_the_ancestor() {
        let base = parse(BASE).unwrap();
        let child = parse(CHILD).unwrap();
        let merged = merge(&child, &base);
        let text = inherited_text(&merged, &base, "\n");
        assert!(text.starts_with("inherited Form2: TForm2\n"), "{text}");
        assert!(text.contains("Caption = 'Child'"), "the form's own caption");
        assert!(text.contains("inherited Button1: TButton\n    Left = 40\n  end"), "only the changed property: {text}");
        assert!(!text.contains("'OK'"), "unchanged ancestor values are not written: {text}");
        assert!(text.contains("object Label9: TLabel"), "own components in full");
        // Writing and reading it back gives the same merged form.
        let back = merge(&parse(&text).unwrap(), &base);
        assert_eq!(inherited_text(&back, &base, "\n"), text, "a second save is the same");
    }

    #[test]
    fn an_unchanged_ancestor_component_is_not_written_at_all() {
        let base = parse(BASE).unwrap();
        let child = parse("inherited Form2: TForm2\nend\n").unwrap();
        let merged = merge(&child, &base);
        let text = inherited_text(&merged, &base, "\n");
        assert_eq!(text, "inherited Form2: TForm2\nend\n");
    }
}
