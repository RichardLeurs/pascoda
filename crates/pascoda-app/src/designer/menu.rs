//! Editing the items inside non-visual components: the `TMenuItem` tree of a `TMainMenu` or
//! `TPopupMenu`, the `TAction` list of a `TActionList`, and the order of the component tray.
//! Pure logic; the editor UI is in `ui::menu_editor`.

use super::{Designer, DesignerError, TRAY_LEFT, TRAY_STEP, TRAY_TOP, is_nonvisual};
use pascoda_forms::{Component, EditError, PropValue};

/// A menu (its children are `TMenuItem`s, which may nest).
pub fn is_menu(class: &str) -> bool {
    ["tmainmenu", "tpopupmenu"].contains(&class.to_ascii_lowercase().as_str())
}

/// A component whose items the item editor edits: a menu or an action list.
pub fn has_item_editor(class: &str) -> bool {
    is_menu(class) || class.eq_ignore_ascii_case("TActionList")
}

/// The class of the items inside `owner` (a menu, a menu item or an action list).
pub fn item_class(owner: &str) -> &'static str {
    if owner.eq_ignore_ascii_case("TActionList") {
        "TAction"
    } else {
        "TMenuItem"
    }
}

pub fn is_separator(c: &Component) -> bool {
    c.prop("Caption").and_then(PropValue::as_str) == Some("-")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewItem {
    /// After the selected item, at its level (into the menu when the menu is selected).
    Item,
    /// The last child of the selected item.
    SubItem,
    /// A `-` item after the selected one.
    Separator,
}

// ---- shortcuts ---------------------------------------------------------------------------

/// LCL's `TShortCut` modifier bits (`lcltype.pp`).
const SC_SHIFT: i64 = 0x2000;
const SC_CTRL: i64 = 0x4000;
const SC_ALT: i64 = 0x8000;
const SC_META: i64 = 0x1000;

/// Named keys and their LCL virtual key codes.
const KEYS: &[(&str, i64)] = &[
    ("Backspace", 8),
    ("Tab", 9),
    ("Enter", 13),
    ("Esc", 27),
    ("Space", 32),
    ("PgUp", 33),
    ("PgDn", 34),
    ("End", 35),
    ("Home", 36),
    ("Left", 37),
    ("Up", 38),
    ("Right", 39),
    ("Down", 40),
    ("Ins", 45),
    ("Del", 46),
];

fn key_name(vk: i64) -> Option<String> {
    match vk {
        48..=57 | 65..=90 => Some(char::from(vk as u8).to_string()),
        112..=123 => Some(format!("F{}", vk - 111)),
        _ => KEYS
            .iter()
            .find(|(_, v)| *v == vk)
            .map(|(n, _)| n.to_string()),
    }
}

fn key_code(name: &str) -> Option<i64> {
    let up = name.to_ascii_uppercase();
    let b = up.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Some(b[0] as i64);
    }
    if let Some(n) = up.strip_prefix('F').and_then(|n| n.parse::<i64>().ok())
        && (1..=12).contains(&n)
    {
        return Some(111 + n);
    }
    let alias = match up.as_str() {
        "RETURN" => "Enter",
        "ESCAPE" => "Esc",
        "DELETE" => "Del",
        "INSERT" => "Ins",
        "PAGEUP" => "PgUp",
        "PAGEDOWN" => "PgDn",
        _ => name,
    };
    KEYS.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(alias))
        .map(|(_, v)| *v)
}

/// `"Cmd+Shift+S"` (also `Meta`, `Ctrl`, `Alt`/`Option`, in any order and case) as the integer
/// LCL stores in `ShortCut`. `None` for text that is not a shortcut.
pub fn shortcut_from_text(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last()?;
    let mut v = key_code(key)?;
    for m in mods {
        v |= match m.to_ascii_lowercase().as_str() {
            "shift" => SC_SHIFT,
            "ctrl" | "control" => SC_CTRL,
            "alt" | "option" | "opt" => SC_ALT,
            "cmd" | "command" | "meta" => SC_META,
            _ => return None,
        };
    }
    Some(v)
}

/// The text form of an LCL `ShortCut` value: `Cmd+Shift+S`. Empty for 0.
pub fn shortcut_text(v: i64) -> String {
    if v == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (bit, name) in [
        (SC_META, "Cmd"),
        (SC_CTRL, "Ctrl"),
        (SC_ALT, "Alt"),
        (SC_SHIFT, "Shift"),
    ] {
        if v & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out + &key_name(v & 0xFFF).unwrap_or_else(|| format!("#{}", v & 0xFFF))
}

/// The shortcut as macOS shows it in a menu: `⌃⌥⇧⌘S`.
pub fn shortcut_symbols(v: i64) -> String {
    if v == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (bit, sym) in [
        (SC_CTRL, '\u{2303}'),
        (SC_ALT, '\u{2325}'),
        (SC_SHIFT, '\u{21E7}'),
        (SC_META, '\u{2318}'),
    ] {
        if v & bit != 0 {
            out.push(sym);
        }
    }
    out + &key_name(v & 0xFFF).unwrap_or_default()
}

// ---- the item tree -------------------------------------------------------------------------

impl Designer {
    /// The items of the menu or action list at `owner`, depth first, with their depth (0 for
    /// the owner's own children).
    pub fn item_rows(&self, owner: &[usize]) -> Vec<(Vec<usize>, usize)> {
        fn walk(
            c: &Component,
            path: &mut Vec<usize>,
            depth: usize,
            out: &mut Vec<(Vec<usize>, usize)>,
        ) {
            for (i, ch) in c.children.iter().enumerate() {
                path.push(i);
                out.push((path.clone(), depth));
                walk(ch, path, depth + 1, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        if let Some(c) = self.doc.component(owner) {
            walk(c, &mut owner.to_vec(), 0, &mut out);
        }
        out
    }

    /// The menu or action list that the component at `path` belongs to (or is).
    pub fn item_owner(&self, path: &[usize]) -> Option<Vec<usize>> {
        (0..=path.len())
            .rev()
            .map(|n| &path[..n])
            .find(|p| {
                self.doc
                    .component(p)
                    .is_some_and(|c| has_item_editor(&c.class))
            })
            .map(<[usize]>::to_vec)
    }

    /// Adds a menu item, separator or action next to (or under) the component at `at`, which is
    /// a menu, an action list, or an item in one. Lazarus names: `MenuItem1`, `Separator1`,
    /// `Action1`; captions `New Item1` / `-` / the name. One undo step; selects it.
    pub fn add_item(&mut self, at: &[usize], kind: NewItem) -> Result<Vec<usize>, DesignerError> {
        let owner = self.item_owner(at).ok_or(EditError::NoSuchComponent)?;
        let owner_class = self
            .doc
            .component(&owner)
            .map(|c| c.class.clone())
            .unwrap_or_default();
        let class = item_class(&owner_class);
        let menu = is_menu(&owner_class);
        let (parent, index) = if at == owner.as_slice() || (kind == NewItem::SubItem && menu) {
            let n = self.doc.component(at).map_or(0, |c| c.children.len());
            (at.to_vec(), n)
        } else {
            let (last, parent) = at.split_last().expect("an item is below its owner");
            (parent.to_vec(), last + 1)
        };
        let (name, caption) = match kind {
            NewItem::Separator if menu => {
                let name = self.free_name("TSeparator");
                (name, "-".to_string())
            }
            _ if menu => {
                let name = self.free_name(class);
                let n = name
                    .trim_start_matches(|c: char| !c.is_ascii_digit())
                    .to_string();
                (name, format!("New Item{n}"))
            }
            _ => {
                let name = self.free_name(class);
                (name.clone(), name)
            }
        };
        let item = Component::new(&name, class).with_prop("Caption", PropValue::Str(caption));
        self.batch(None, |d| d.doc.insert_child(&parent, index, item))?;
        let path = [parent.as_slice(), &[index]].concat();
        self.select(path.clone());
        Ok(path)
    }

    /// Moves an item up (`-1`) or down (`1`) among its siblings. One undo step; keeps it
    /// selected. Returns its new path (unchanged at the ends).
    pub fn move_item(&mut self, path: &[usize], by: isize) -> Result<Vec<usize>, DesignerError> {
        let Some((&last, parent)) = path.split_last() else {
            return Ok(path.to_vec());
        };
        let n = self.doc.component(parent).map_or(0, |c| c.children.len());
        let to = last as isize + by;
        if to < 0 || to as usize >= n {
            return Ok(path.to_vec());
        }
        let new = self.batch(None, |d| d.doc.move_component(path, parent, to as usize))?;
        self.select(new.clone());
        Ok(new)
    }

    /// Makes a menu item the last sub-item of the item before it (not into a separator).
    pub fn indent_item(&mut self, path: &[usize]) -> Result<Vec<usize>, DesignerError> {
        let Some((&last, parent)) = path.split_last() else {
            return Ok(path.to_vec());
        };
        let prev = [parent, &[last.wrapping_sub(1)]].concat();
        let ok = last > 0
            && self
                .doc
                .component(&prev)
                .is_some_and(|c| c.class.eq_ignore_ascii_case("TMenuItem") && !is_separator(c));
        if !ok {
            return Ok(path.to_vec());
        }
        let n = self.doc.component(&prev).map_or(0, |c| c.children.len());
        let new = self.batch(None, |d| d.doc.move_component(path, &prev, n))?;
        self.select(new.clone());
        Ok(new)
    }

    /// Moves a sub-item out of its parent item, to just after it.
    pub fn outdent_item(&mut self, path: &[usize]) -> Result<Vec<usize>, DesignerError> {
        let ok = path.len() >= 2
            && self
                .doc
                .component(&path[..path.len() - 1])
                .is_some_and(|c| c.class.eq_ignore_ascii_case("TMenuItem"));
        if !ok {
            return Ok(path.to_vec());
        }
        let parent = &path[..path.len() - 1];
        let (&pi, grand) = parent.split_last().expect("checked above");
        let new = self.batch(None, |d| d.doc.move_component(path, grand, pi + 1))?;
        self.select(new.clone());
        Ok(new)
    }

    /// Deletes an item (with its sub-items) and selects the next one, the previous one, or the
    /// parent.
    pub fn delete_item(&mut self, path: &[usize]) -> Result<(), DesignerError> {
        let Some((&last, parent)) = path.split_last() else {
            return Ok(());
        };
        self.select(path.to_vec());
        self.delete_selected()?;
        let n = self.doc.component(parent).map_or(0, |c| c.children.len());
        self.select(if last < n {
            path.to_vec()
        } else if last > 0 {
            [parent, &[last - 1]].concat()
        } else {
            parent.to_vec()
        });
        Ok(())
    }

    /// A Boolean property written only when it differs from `default` (as LCL streams them).
    pub fn set_flag(
        &mut self,
        path: &[usize],
        name: &str,
        on: bool,
        default: bool,
    ) -> Result<(), DesignerError> {
        let has = self
            .doc
            .component(path)
            .ok_or(EditError::NoSuchComponent)?
            .prop(name)
            .is_some();
        if on == default {
            if has {
                self.batch(None, |d| d.doc.remove_prop(path, name))?;
            }
            Ok(())
        } else {
            self.set_prop(path, name, PropValue::bool(on))
        }
    }

    /// Sets `ShortCut` from text such as `Cmd+S`; empty text removes it. Returns `false` (and
    /// changes nothing) when the text is not a shortcut.
    pub fn set_shortcut(&mut self, path: &[usize], text: &str) -> Result<bool, DesignerError> {
        let text = text.trim();
        if text.is_empty() {
            if self
                .doc
                .component(path)
                .is_some_and(|c| c.prop("ShortCut").is_some())
            {
                self.batch(None, |d| d.doc.remove_prop(path, "ShortCut"))?;
            }
            return Ok(true);
        }
        let Some(v) = shortcut_from_text(text) else {
            return Ok(false);
        };
        self.set_prop(path, "ShortCut", PropValue::Int(v))?;
        Ok(true)
    }

    // ---- the tray ----------------------------------------------------------------------

    /// Moves the tray component at position `from` of [`Designer::tray`] to position `to`,
    /// rewriting the `Left`/`Top` slots of those whose place changed. One undo step.
    pub fn move_in_tray(&mut self, from: usize, to: usize) -> Result<(), DesignerError> {
        let mut order = self.tray();
        if from >= order.len() || from == to {
            return Ok(());
        }
        let p = order.remove(from);
        order.insert(to.min(order.len()), p);
        self.batch(None, |d| {
            order.iter().enumerate().try_for_each(|(k, p)| {
                let c = d.doc.component(p).ok_or(EditError::NoSuchComponent)?;
                debug_assert!(is_nonvisual(&c.class));
                for (name, v) in [
                    ("Left", TRAY_LEFT + TRAY_STEP * k as i64),
                    ("Top", TRAY_TOP),
                ] {
                    if d.doc.component(p).and_then(|c| c.prop(name)) != Some(&PropValue::Int(v)) {
                        d.doc.set_prop(p, name, PropValue::Int(v))?;
                    }
                }
                Ok::<(), DesignerError>(())
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{PAS, designer};
    use super::*;

    #[test]
    fn shortcuts_read_and_write_like_lcl() {
        assert_eq!(shortcut_from_text("Ctrl+N"), Some(16462)); // as in Lazarus's own forms
        assert_eq!(shortcut_from_text("Shift+Del"), Some(8238));
        assert_eq!(
            shortcut_from_text("cmd + shift + s"),
            Some(0x1000 | 0x2000 | 83)
        );
        assert_eq!(shortcut_from_text("F5"), Some(116));
        assert_eq!(shortcut_from_text("Hyper+S"), None);
        assert_eq!(shortcut_from_text("Cmd+"), None);
        assert_eq!(shortcut_text(16462), "Ctrl+N");
        assert_eq!(shortcut_text(0x1000 | 0x2000 | 83), "Cmd+Shift+S");
        assert_eq!(shortcut_text(9), "Tab");
        assert_eq!(shortcut_text(0), "");
        assert_eq!(shortcut_symbols(0x1000 | 0x2000 | 83), "\u{21E7}\u{2318}S");
        for v in [16462, 8238, 0x1000 | 79, 0x8000 | 116, 13] {
            assert_eq!(shortcut_from_text(&shortcut_text(v)), Some(v), "{v}");
        }
    }

    fn captions(d: &Designer, owner: &[usize]) -> Vec<(String, usize)> {
        d.item_rows(owner)
            .into_iter()
            .map(|(p, depth)| {
                let c = d.doc.component(&p).unwrap();
                (
                    format!("{}:{}", c.name, super::super::Designer::caption_of(c)),
                    depth,
                )
            })
            .collect()
    }

    #[test]
    fn builds_a_menu_tree_with_lazarus_names() {
        let mut d = designer();
        let menu = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let file = d.add_item(&menu, NewItem::Item).unwrap();
        let open = d.add_item(&file, NewItem::SubItem).unwrap();
        let sep = d.add_item(&open, NewItem::Separator).unwrap();
        let quit = d.add_item(&sep, NewItem::Item).unwrap();
        let edit = d.add_item(&file, NewItem::Item).unwrap();
        assert_eq!(d.selected, edit);
        assert_eq!(
            captions(&d, &menu),
            [
                ("MenuItem1:New Item1".to_string(), 0),
                ("MenuItem2:New Item2".to_string(), 1),
                ("Separator1:-".to_string(), 1),
                ("MenuItem3:New Item3".to_string(), 1),
                ("MenuItem4:New Item4".to_string(), 0),
            ]
        );
        assert_eq!(d.item_owner(&quit), Some(menu.clone()));
        assert!(d.doc.source().contains(
            "  object MainMenu1: TMainMenu\n    Left = 16\n    Top = 16\n    object MenuItem1: TMenuItem\n      Caption = 'New Item1'\n      object MenuItem2: TMenuItem\n"
        ), "{}", d.doc.source());
        // Each add is one undo step.
        assert!(d.undo());
        assert_eq!(d.item_rows(&menu).len(), 4);
    }

    #[test]
    fn reorders_indents_and_outdents_items_as_single_undo_steps() {
        let mut d = designer();
        let menu = d.add_component("TPopupMenu", &[], 0, 0).unwrap();
        let a = d.add_item(&menu, NewItem::Item).unwrap();
        let b = d.add_item(&a, NewItem::Item).unwrap();
        let c = d.add_item(&b, NewItem::Item).unwrap();
        let start = d.doc.source().to_string();

        let c_up = d.move_item(&c, -1).unwrap();
        assert_eq!(c_up, b);
        assert_eq!(d.selected, c_up);
        let names: Vec<_> = d
            .item_rows(&menu)
            .iter()
            .map(|(p, _)| d.doc.component(p).unwrap().name.clone())
            .collect();
        assert_eq!(names, ["MenuItem1", "MenuItem3", "MenuItem2"]);
        assert_eq!(d.move_item(&a, -1).unwrap(), a, "the first stays first");
        assert!(d.undo());
        assert_eq!(d.doc.source(), start);

        // MenuItem2 into MenuItem1, then MenuItem3 too (after it), then out again.
        let b_in = d.indent_item(&b).unwrap();
        assert_eq!(b_in, [a.as_slice(), &[0]].concat());
        let c_in = d.indent_item(&[menu.as_slice(), &[1]].concat()).unwrap();
        assert_eq!(c_in, [a.as_slice(), &[1]].concat());
        assert_eq!(
            captions(&d, &menu).iter().map(|r| r.1).collect::<Vec<_>>(),
            [0, 1, 1]
        );
        assert_eq!(
            d.indent_item(&b_in).unwrap(),
            b_in,
            "nothing before it to go into"
        );
        let c_out = d.outdent_item(&c_in).unwrap();
        assert_eq!(c_out, [menu.as_slice(), &[1]].concat());
        assert_eq!(d.outdent_item(&c_out).unwrap(), c_out, "already at the top");
        assert!(d.undo() && d.undo() && d.undo());
        assert_eq!(d.doc.source(), start);

        // Separators take no sub-items.
        let sep = d.add_item(&a, NewItem::Separator).unwrap();
        let after = d.add_item(&sep, NewItem::Item).unwrap();
        assert_eq!(d.indent_item(&after).unwrap(), after);
    }

    #[test]
    fn deleting_an_item_selects_a_neighbour() {
        let mut d = designer();
        let menu = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let a = d.add_item(&menu, NewItem::Item).unwrap();
        let b = d.add_item(&a, NewItem::Item).unwrap();
        d.add_item(&b, NewItem::SubItem).unwrap();
        d.delete_item(&a).unwrap();
        assert_eq!(d.selected, a, "the next one moved up into its place");
        assert_eq!(d.doc.component(&a).unwrap().name, "MenuItem2");
        d.delete_item(&a).unwrap(); // with its sub-item
        assert_eq!(d.selected, menu);
        assert!(d.item_rows(&menu).is_empty());
    }

    #[test]
    fn item_properties_follow_lcl_defaults() {
        let mut d = designer();
        let menu = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let a = d.add_item(&menu, NewItem::Item).unwrap();
        d.set_flag(&a, "Checked", true, false).unwrap();
        d.set_flag(&a, "Enabled", false, true).unwrap();
        assert!(d.set_shortcut(&a, "Cmd+O").unwrap());
        assert!(!d.set_shortcut(&a, "nonsense").unwrap());
        let c = d.doc.component(&a).unwrap();
        assert_eq!(c.prop("ShortCut"), Some(&PropValue::Int(0x1000 | 79)));
        assert!(
            d.doc.source().contains("Checked = True") && d.doc.source().contains("Enabled = False")
        );
        d.set_flag(&a, "Checked", false, false).unwrap();
        d.set_flag(&a, "Enabled", true, true).unwrap();
        d.set_shortcut(&a, "").unwrap();
        let c = d.doc.component(&a).unwrap();
        assert!(
            c.prop("Checked").is_none()
                && c.prop("Enabled").is_none()
                && c.prop("ShortCut").is_none()
        );
    }

    #[test]
    fn menu_items_and_actions_get_fields_and_handlers_in_the_unit() {
        let mut d = designer();
        let mut unit = PAS.to_string();
        let menu = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let file = d.add_item(&menu, NewItem::Item).unwrap();
        let open = d.add_item(&file, NewItem::SubItem).unwrap();
        d.select(open.clone());
        assert_eq!(
            d.ensure_handler("OnClick").unwrap().as_deref(),
            Some("MenuItem2Click")
        );
        let list = d.add_component("TActionList", &[], 0, 0).unwrap();
        let act = d.add_item(&list, NewItem::Item).unwrap();
        assert_eq!(d.doc.component(&act).unwrap().class, "TAction");
        assert_eq!(
            super::super::Designer::caption_of(d.doc.component(&act).unwrap()),
            "Action1"
        );
        assert_eq!(
            d.ensure_handler(super::super::default_event("TAction"))
                .unwrap()
                .as_deref(),
            Some("Action1Execute")
        );
        d.sync_unit(&mut unit);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        for line in [
            "    MainMenu1: TMainMenu;",
            "    MenuItem1: TMenuItem;",
            "    MenuItem2: TMenuItem;",
            "    ActionList1: TActionList;",
            "    Action1: TAction;",
            "procedure TForm1.MenuItem2Click(Sender: TObject);",
            "procedure TForm1.Action1Execute(Sender: TObject);",
        ] {
            assert!(unit.contains(line), "{line}\n{unit}");
        }
        assert!(unit.contains("Menus") && unit.contains("ActnList"));

        // Renaming an item renames its handler; deleting it drops the field.
        d.select(open.clone());
        let handlers = d.rename_selected("OpenItem").unwrap();
        d.sync_rename(&mut unit, "MenuItem2", "OpenItem", &handlers);
        assert!(unit.contains("    OpenItem: TMenuItem;") && unit.contains("TForm1.OpenItemClick"));
        d.delete_item(&open).unwrap();
        d.sync_unit(&mut unit);
        assert!(!unit.contains("OpenItem: TMenuItem;"));
    }

    #[test]
    fn tray_chips_reorder_by_rewriting_their_slots() {
        let mut d = designer();
        let t = d.add_component("TTimer", &[], 0, 0).unwrap();
        let m = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let o = d.add_component("TOpenDialog", &[], 0, 0).unwrap();
        let before = d.doc.source().to_string();
        d.move_in_tray(2, 0).unwrap();
        assert_eq!(d.tray(), vec![o.clone(), t.clone(), m.clone()]);
        let left = |d: &Designer, p: &[usize]| {
            super::super::geometry::bounds(d.doc.component(p).unwrap()).left
        };
        assert_eq!((left(&d, &o), left(&d, &t), left(&d, &m)), (16, 56, 96));
        assert!(d.undo());
        assert_eq!(d.doc.source(), before, "one undo step");
        d.move_in_tray(0, 9).unwrap();
        assert_eq!(d.tray(), vec![m, o, t], "past the end goes last");
    }

    #[test]
    fn copied_items_paste_into_the_selected_menu_only() {
        let mut d = designer();
        let menu = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let a = d.add_item(&menu, NewItem::Item).unwrap();
        d.add_item(&a, NewItem::SubItem).unwrap();
        let b = d.add_item(&a, NewItem::Item).unwrap();
        d.select(a.clone());
        let text = d.copy_selection().unwrap();
        d.select(a.clone());
        let pasted = d.paste(&text).unwrap();
        assert_eq!(pasted, vec![b.clone()], "right after the selected item");
        let c = d.doc.component(&b).unwrap();
        assert_eq!(
            (c.name.as_str(), c.children[0].name.as_str()),
            ("MenuItem4", "MenuItem5")
        );
        assert_eq!(d.doc.component(&[0, 2]).unwrap().name, "MenuItem3");
        // Not into an action list, nor onto the form.
        let list = d.add_component("TActionList", &[], 0, 0).unwrap();
        d.select(list);
        assert!(d.paste(&text).unwrap().is_empty());
        d.select(Vec::new());
        assert!(d.paste(&text).unwrap().is_empty());
        // Cmd+D on an item duplicates it into the same menu.
        d.select(a);
        assert_eq!(d.duplicate().unwrap().len(), 1);
        assert_eq!(d.item_rows(&menu).iter().filter(|r| r.1 == 0).count(), 4);
    }
}
