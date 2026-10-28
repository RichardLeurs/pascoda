//! The inline item editor for menus (`TMainMenu`, `TPopupMenu`) and action lists. It replaces
//! the design canvas while open (egui windows would draw under the native controls), so the
//! native views are hidden meanwhile. Selecting an item makes it the designer's selection, so
//! the inspector, Delete, undo and rename-with-handlers all apply to it.

use super::menu::{self, NewItem, is_separator, shortcut_symbols, shortcut_text};
use super::ui::{Outcome, UiState, create_handler, section};
use super::{Designer, default_event};
use crate::style::{Palette, paint_disclosure, pill, segment};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Rect, RichText, Sense,
    Stroke, StrokeKind, pos2, vec2,
};
use pascoda_forms::{Component, PropValue};

const ROW_H: f32 = 24.0;

/// The menu or action list called `name`, if the form still has one.
pub(super) fn find_owner(d: &Designer, name: &str) -> Option<Vec<usize>> {
    d.all_paths().into_iter().find(|p| {
        d.doc
            .component(p)
            .is_some_and(|c| c.name.eq_ignore_ascii_case(name) && menu::has_item_editor(&c.class))
    })
}

/// A caption as a menu shows it: `&File` -> `File`, `&&` -> `&`.
pub fn display_caption(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                out.push('&');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn caption(c: &Component) -> String {
    display_caption(
        c.prop("Caption")
            .and_then(PropValue::as_str)
            .unwrap_or_default(),
    )
}

fn flag(c: &Component, name: &str, default: bool) -> bool {
    c.prop(name).and_then(PropValue::as_bool).unwrap_or(default)
}

fn shortcut(c: &Component) -> i64 {
    c.prop("ShortCut").and_then(PropValue::as_int).unwrap_or(0)
}

#[derive(Clone, Copy)]
enum Act {
    Add(NewItem),
    Move(isize),
    Indent,
    Outdent,
    Delete,
}

/// Width of the item list, and the gap between it and the properties.
const LIST_W: f32 = 300.0;
const GAP: f32 = 20.0;

/// Draws the editor in the space the canvas would take. Returns `false` once it is closed.
pub(super) fn show(
    d: &mut Designer,
    state: &mut UiState,
    ui: &mut egui::Ui,
    unit: &mut String,
    out: &mut Outcome,
) -> bool {
    let Some(owner) = state.item_edit.as_deref().and_then(|n| find_owner(d, n)) else {
        state.item_edit = None;
        return false;
    };
    if !(d.selected.starts_with(&owner) && d.selection().len() <= 1) {
        d.select(owner.clone());
    }
    let pal = Palette::of(ui.ctx());
    let owner_c = d.doc.component(&owner).cloned().expect("found above");
    let is_menu = menu::is_menu(&owner_c.class);
    let main_menu = owner_c.class.eq_ignore_ascii_case("TMainMenu");
    let rows = d.item_rows(&owner);
    let sel = d.selected.clone();
    let at_item = sel.len() > owner.len();
    let cur = at_item.then(|| d.doc.component(&sel).cloned()).flatten();
    let mut act: Option<Act> = None;
    let mut close = false;

    let avail = ui.available_rect_before_wrap();
    ui.painter().rect_filled(avail, 0.0, pal.editor);
    Frame::new()
        .inner_margin(Margin::symmetric(20, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Header: what is being edited, and Done.
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                crate::icons::paint(ui.painter(), r, &owner_c.class);
                ui.label(
                    RichText::new(&owner_c.name)
                        .size(15.0)
                        .strong()
                        .color(pal.text),
                );
                let what = if is_menu { "items" } else { "actions" };
                ui.label(
                    RichText::new(format!("{} \u{00B7} {} {what}", owner_c.class, rows.len()))
                        .color(pal.text_secondary),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button("Done")
                        .on_hover_text("Back to the form (Esc)")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            ui.add_space(10.0);
            toolbar(ui, &pal, is_menu, cur.as_ref(), &sel, &rows, &mut act);
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // List and properties side by side when there is room, else stacked: the
                    // editor must stay inside the area between the navigator and the inspector.
                    let avail_w = ui.available_width();
                    let wide = avail_w >= LIST_W + GAP + 360.0;
                    let list_w = if wide {
                        LIST_W
                    } else {
                        (avail_w - 14.0).clamp(200.0, LIST_W)
                    };
                    let side_w = if wide {
                        (avail_w - LIST_W - GAP - 14.0).min(460.0)
                    } else {
                        avail_w.min(460.0)
                    };
                    let mut body = |ui: &mut egui::Ui| {
                        Frame::new()
                            .fill(pal.sidebar)
                            .stroke(Stroke::new(1.0, pal.separator))
                            .corner_radius(8)
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(list_w);
                                    list(d, ui, &pal, &rows, &sel, unit, out);
                                    if rows.is_empty() {
                                        ui.add_space(6.0);
                                        ui.weak(if is_menu {
                                            "No items yet: Add Item makes the first one."
                                        } else {
                                            "No actions yet: Add Action makes the first one."
                                        });
                                    }
                                    // Empty space below the rows selects the menu itself.
                                    let h = (240.0 - ROW_H * rows.len() as f32).max(ROW_H);
                                    let blank = ui.allocate_response(
                                        vec2(ui.available_width(), h),
                                        Sense::click(),
                                    );
                                    if blank.clicked() {
                                        d.select(owner.clone());
                                    }
                                });
                            });
                        ui.add_space(GAP);
                        ui.vertical(|ui| {
                            ui.set_max_width(side_w);
                            match &cur {
                                Some(c) => properties(d, state, ui, &pal, &sel, c, unit, out),
                                None => {
                                    section(ui, &pal, "ITEM");
                                    ui.weak(
                                        "Select an item to edit its caption, name and shortcut.",
                                    );
                                }
                            }
                            if is_menu {
                                ui.add_space(18.0);
                                section(ui, &pal, "PREVIEW");
                                ui.add_space(4.0);
                                preview(d, ui, &pal, &owner, &sel, main_menu);
                            }
                        });
                    };
                    if wide {
                        ui.horizontal_top(&mut body);
                    } else {
                        ui.vertical(&mut body);
                    }
                });
        });

    // Keys: the editor's own first; the designer's (undo, copy/paste) after.
    let ctx = ui.ctx().clone();
    if !ctx.egui_wants_keyboard_input() {
        use egui::{Key, Modifiers};
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Modifiers::NONE, Key::Escape) {
            close = true;
        }
        if pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace) {
            act = Some(Act::Delete);
        }
        if pressed(Modifiers::COMMAND, Key::ArrowUp) {
            act = Some(Act::Move(-1));
        } else if pressed(Modifiers::COMMAND, Key::ArrowDown) {
            act = Some(Act::Move(1));
        } else if is_menu && pressed(Modifiers::COMMAND, Key::ArrowRight) {
            act = Some(Act::Indent);
        } else if is_menu && pressed(Modifiers::COMMAND, Key::ArrowLeft) {
            act = Some(Act::Outdent);
        }
        let at = rows.iter().position(|(p, _)| *p == sel);
        if pressed(Modifiers::NONE, Key::ArrowDown) {
            let next = at.map_or(0, |i| (i + 1).min(rows.len().saturating_sub(1)));
            if let Some((p, _)) = rows.get(next) {
                d.select(p.clone());
            }
        } else if pressed(Modifiers::NONE, Key::ArrowUp) {
            match at {
                Some(0) | None => d.select(owner.clone()),
                Some(i) => d.select(rows[i - 1].0.clone()),
            }
        }
    }
    if let Some(a) = act {
        let target = if at_item { sel.clone() } else { owner.clone() };
        let changed = match a {
            Act::Add(kind) => d.add_item(&target, kind).is_ok(),
            Act::Move(by) if at_item => d.move_item(&sel, by).is_ok(),
            Act::Indent if at_item => d.indent_item(&sel).is_ok(),
            Act::Outdent if at_item => d.outdent_item(&sel).is_ok(),
            Act::Delete if at_item => d.delete_item(&sel).is_ok(),
            _ => false,
        };
        if changed {
            d.sync_unit(unit);
            out.unit_changed = true;
        }
    }
    if close {
        state.item_edit = None;
        d.select(owner);
        return false;
    }
    true
}

fn toolbar(
    ui: &mut egui::Ui,
    pal: &Palette,
    is_menu: bool,
    cur: Option<&Component>,
    sel: &[usize],
    rows: &[(Vec<usize>, usize)],
    act: &mut Option<Act>,
) {
    let at_item = cur.is_some();
    let sep = cur.is_some_and(is_separator);
    let siblings_before = sel.last().is_some_and(|&i| i > 0);
    let depth = rows.iter().find(|(p, _)| p == sel).map_or(0, |r| r.1);
    let last_sibling = rows.iter().any(|(p, _)| {
        p.len() == sel.len() && p[..p.len() - 1] == sel[..sel.len() - 1] && p > &sel.to_vec()
    });
    let mut button = |ui: &mut egui::Ui, label: &str, tip: &str, on: bool, a: Act| {
        let r = ui
            .add_enabled_ui(on, |ui| segment(ui, pal, label, false))
            .inner
            .on_hover_text(tip);
        if r.clicked() {
            *act = Some(a);
        }
    };
    // Three groups of buttons. egui does not wrap a group once it has started on a row, so the
    // rows are chosen from the width: all in one, add + (move, delete), or one group per row.
    let mut group = |ui: &mut egui::Ui, which: usize| {
        pill(ui, pal, |ui| match which {
            0 if is_menu => {
                button(
                    ui,
                    "Add Item",
                    "A new item after the selected one",
                    true,
                    Act::Add(NewItem::Item),
                );
                button(
                    ui,
                    "Add Sub-item",
                    "A new item inside the selected one",
                    at_item && !sep,
                    Act::Add(NewItem::SubItem),
                );
                button(
                    ui,
                    "Add Separator",
                    "A dividing line",
                    true,
                    Act::Add(NewItem::Separator),
                );
            }
            0 => button(
                ui,
                "Add Action",
                "A new action after the selected one",
                true,
                Act::Add(NewItem::Item),
            ),
            1 => {
                button(
                    ui,
                    "Up",
                    "Move up (Cmd+Up)",
                    at_item && siblings_before,
                    Act::Move(-1),
                );
                button(
                    ui,
                    "Down",
                    "Move down (Cmd+Down)",
                    at_item && last_sibling,
                    Act::Move(1),
                );
                if is_menu {
                    button(
                        ui,
                        "Outdent",
                        "Out of its parent item (Cmd+Left)",
                        at_item && depth > 0,
                        Act::Outdent,
                    );
                    button(
                        ui,
                        "Indent",
                        "Into the item above (Cmd+Right)",
                        at_item && siblings_before,
                        Act::Indent,
                    );
                }
            }
            _ => button(
                ui,
                "Delete",
                "Delete with its sub-items (Delete)",
                at_item,
                Act::Delete,
            ),
        });
    };
    let width = ui.available_width();
    if width >= 600.0 {
        ui.horizontal(|ui| (0..3).for_each(|g| group(ui, g)));
    } else if width >= 300.0 {
        ui.horizontal(|ui| group(ui, 0));
        ui.horizontal(|ui| (1..3).for_each(|g| group(ui, g)));
    } else {
        (0..3).for_each(|g| {
            ui.horizontal(|ui| group(ui, g));
        });
    }
}

/// The item tree: one row per item, indented by depth.
fn list(
    d: &mut Designer,
    ui: &mut egui::Ui,
    pal: &Palette,
    rows: &[(Vec<usize>, usize)],
    sel: &[usize],
    unit: &mut String,
    out: &mut Outcome,
) {
    for (path, depth) in rows {
        let Some(c) = d.doc.component(path).cloned() else {
            continue;
        };
        let selected = path.as_slice() == sel;
        let sep = is_separator(&c);
        let label = if sep {
            "Separator".to_string()
        } else {
            caption(&c)
        };
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let p = ui.painter();
        if selected {
            p.rect_filled(rect, 5.0, pal.row_selected);
        } else if resp.hovered() {
            p.rect_filled(rect, 5.0, pal.row_hover);
        }
        let x0 = rect.left() + 8.0 + 16.0 * *depth as f32;
        let cy = rect.center().y;
        if !c.children.is_empty() {
            paint_disclosure(p, pos2(x0 + 4.0, cy), true, pal.text_secondary);
        }
        let tx = x0 + 14.0;
        let muted = pal.text_tertiary;
        let name = p.layout_no_wrap(c.name.clone(), FontId::proportional(11.0), muted);
        let name_x = rect.right() - 8.0 - name.size().x;
        if sep {
            p.line_segment(
                [pos2(tx, cy), pos2(name_x - 10.0, cy)],
                Stroke::new(1.0, pal.text_tertiary),
            );
        } else {
            let enabled = flag(&c, "Enabled", true);
            let color = if enabled { pal.text } else { pal.text_tertiary };
            if flag(&c, "Checked", false) {
                paint_check(p, pos2(tx + 4.0, cy), color);
            }
            let text = p.layout_no_wrap(label.clone(), FontId::proportional(13.0), color);
            let w = text.size().x;
            p.galley(pos2(tx + 14.0, cy - text.size().y / 2.0), text, color);
            let sc = shortcut_symbols(shortcut(&c));
            if !sc.is_empty() {
                p.text(
                    pos2(tx + 14.0 + w + 10.0, cy),
                    egui::Align2::LEFT_CENTER,
                    sc,
                    FontId::proportional(12.0),
                    pal.text_secondary,
                );
            }
        }
        p.galley(pos2(name_x, cy - name.size().y / 2.0), name, muted);
        if resp.clicked() {
            d.select(path.clone());
        }
        if resp.double_clicked() {
            d.select(path.clone());
            create_handler(d, unit, out);
        }
    }
}

/// A small check mark centred at `c`.
fn paint_check(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.6, color);
    p.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.5)], s);
    p.line_segment([c + vec2(-1.0, 3.5), c + vec2(4.5, -4.0)], s);
}

#[allow(clippy::too_many_arguments)]
fn properties(
    d: &mut Designer,
    state: &mut UiState,
    ui: &mut egui::Ui,
    pal: &Palette,
    path: &[usize],
    c: &Component,
    unit: &mut String,
    out: &mut Outcome,
) {
    section(
        ui,
        pal,
        if c.class.eq_ignore_ascii_case("TAction") {
            "ACTION"
        } else {
            "ITEM"
        },
    );
    ui.add_space(2.0);
    let sep = is_separator(c);
    egui::Grid::new("item-props")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            if !sep {
                let lbl = ui.label("Caption");
                let mut v = c
                    .prop("Caption")
                    .and_then(PropValue::as_str)
                    .unwrap_or_default()
                    .to_string();
                if ui
                    .add(egui::TextEdit::singleline(&mut v).desired_width(220.0))
                    .labelled_by(lbl.id)
                    .on_hover_text("& marks the access key: &Open")
                    .changed()
                {
                    let _ = d.set_prop(path, "Caption", PropValue::Str(v));
                }
                ui.end_row();
            }

            let lbl = ui.label("Name");
            if !state.item_name_focus || state.item_name.0 != path {
                state.item_name = (path.to_vec(), c.name.clone());
            }
            let r = ui
                .add(egui::TextEdit::singleline(&mut state.item_name.1).desired_width(220.0))
                .labelled_by(lbl.id);
            state.item_name_focus = r.has_focus();
            if r.lost_focus() && state.item_name.1 != c.name {
                let new = state.item_name.1.clone();
                d.select(path.to_vec());
                if let Ok(handlers) = d.rename_selected(&new) {
                    d.sync_rename(unit, &c.name, &new, &handlers);
                    out.unit_changed = true;
                }
            }
            ui.end_row();

            if !sep {
                let lbl = ui.label("ShortCut");
                if !state.shortcut_focus || state.shortcut.0 != path {
                    state.shortcut = (path.to_vec(), shortcut_text(shortcut(c)), true);
                }
                let r = ui
                    .add(
                        egui::TextEdit::singleline(&mut state.shortcut.1)
                            .desired_width(220.0)
                            .hint_text("Cmd+S")
                            .text_color(if state.shortcut.2 {
                                pal.text
                            } else {
                                pal.error
                            }),
                    )
                    .labelled_by(lbl.id)
                    .on_hover_text("Cmd, Ctrl, Alt, Shift + a key: Cmd+Shift+S, F5, Del");
                state.shortcut_focus = r.has_focus();
                if r.changed() {
                    state.shortcut.2 = d.set_shortcut(path, &state.shortcut.1).unwrap_or(false);
                }
                ui.end_row();

                for (name, default) in [("Checked", false), ("Enabled", true)] {
                    let lbl = ui.label(name);
                    let mut v = flag(c, name, default);
                    if ui.checkbox(&mut v, "").labelled_by(lbl.id).changed() {
                        let _ = d.set_flag(path, name, v, default);
                    }
                    ui.end_row();
                }
            }

            let event = default_event(&c.class);
            ui.label(event);
            match c.prop(event).and_then(PropValue::as_ident) {
                Some(h) => {
                    if ui
                        .link(RichText::new(h).monospace())
                        .on_hover_text("Show the handler")
                        .clicked()
                    {
                        out.open_handler = Some(h.to_string());
                    }
                }
                None => {
                    if ui
                        .button(format!("Create {event} handler"))
                        .on_hover_text("Or double-click the item")
                        .clicked()
                    {
                        d.select(path.to_vec());
                        create_handler(d, unit, out);
                    }
                }
            }
            ui.end_row();
        });
}

/// What the menu will look like on macOS: the menu bar (for a main menu) and the open menus
/// along the selected item's path. Clicking an entry selects it.
fn preview(
    d: &mut Designer,
    ui: &mut egui::Ui,
    pal: &Palette,
    owner: &[usize],
    sel: &[usize],
    main_menu: bool,
) {
    let Some(menu) = d.doc.component(owner).cloned() else {
        return;
    };
    // The containers whose items are shown as open menus, outermost first.
    let mut levels: Vec<Vec<usize>> = Vec::new();
    if !main_menu {
        levels.push(owner.to_vec());
    }
    for k in owner.len() + 1..=sel.len() {
        let p = &sel[..k];
        if d.doc.component(p).is_some_and(|c| !c.children.is_empty()) {
            levels.push(p.to_vec());
        } else {
            break;
        }
    }
    let width = ui.available_width().clamp(200.0, 460.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 250.0), Sense::hover());
    let p = ui.painter_at(rect);
    let desk = if pal.dark {
        Color32::from_rgb(0x1E, 0x1E, 0x20)
    } else {
        Color32::from_rgb(0xE6, 0xE8, 0xEC)
    };
    p.rect_filled(rect, 8.0, desk);
    let panel = if pal.dark {
        Color32::from_rgb(0x2C, 0x2C, 0x2E)
    } else {
        Color32::from_rgb(0xF6, 0xF6, 0xF6)
    };
    let mut select: Option<Vec<usize>> = None;
    let font = FontId::proportional(13.0);
    let mut top = rect.top() + 12.0;
    let mut left = rect.left() + 12.0;
    if main_menu {
        // The menu bar: application menu, then the top-level items.
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 24.0));
        p.rect_filled(
            bar,
            CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            },
            panel,
        );
        let mut x = bar.left() + 12.0;
        let g = p.layout_no_wrap("Project".into(), FontId::proportional(13.0), pal.text);
        let w = g.size().x;
        p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, pal.text);
        x += w + 18.0;
        for (i, item) in menu.children.iter().enumerate() {
            let path = [owner, &[i]].concat();
            let open = sel.len() > owner.len() && sel[owner.len()] == i;
            let g = p.layout_no_wrap(caption(item), font.clone(), pal.text);
            let r = Rect::from_min_size(
                pos2(x - 7.0, bar.top() + 2.0),
                vec2(g.size().x + 14.0, 20.0),
            );
            let color = if open {
                p.rect_filled(r, 4.0, pal.accent);
                Color32::WHITE
            } else {
                pal.text
            };
            if open {
                left = r.left();
            }
            p.galley(pos2(x, bar.center().y - g.size().y / 2.0), g, color);
            if ui
                .interact(r, ui.scope_id().with(("menu-bar", i)), Sense::click())
                .clicked()
            {
                select = Some(path);
            }
            x = r.right() + 7.0;
        }
        top = bar.bottom() + 2.0;
    }
    for level in &levels {
        let Some(c) = d.doc.component(level).cloned() else {
            break;
        };
        // Width from the widest caption plus its shortcut.
        let mut w: f32 = 160.0;
        for item in &c.children {
            let g = p.layout_no_wrap(caption(item), font.clone(), pal.text);
            let s = p.layout_no_wrap(shortcut_symbols(shortcut(item)), font.clone(), pal.text);
            w = w.max(g.size().x + s.size().x + 70.0);
        }
        let h: f32 = c
            .children
            .iter()
            .map(|i| if is_separator(i) { 11.0 } else { 22.0 })
            .sum::<f32>()
            + 10.0;
        let box_rect = Rect::from_min_size(pos2(left, top), vec2(w, h));
        p.add(
            egui::epaint::Shadow {
                offset: [0, 4],
                blur: 16,
                spread: 0,
                color: Color32::from_black_alpha(if pal.dark { 110 } else { 40 }),
            }
            .as_shape(box_rect, CornerRadius::same(7)),
        );
        p.rect(
            box_rect,
            7.0,
            panel,
            Stroke::new(1.0, pal.separator),
            StrokeKind::Inside,
        );
        let mut y = box_rect.top() + 5.0;
        let mut next_top = top;
        for (i, item) in c.children.iter().enumerate() {
            let path = [level.as_slice(), &[i]].concat();
            if is_separator(item) {
                p.line_segment(
                    [
                        pos2(box_rect.left() + 10.0, y + 5.5),
                        pos2(box_rect.right() - 10.0, y + 5.5),
                    ],
                    Stroke::new(1.0, pal.separator),
                );
                y += 11.0;
                continue;
            }
            let r = Rect::from_min_size(pos2(box_rect.left() + 5.0, y), vec2(w - 10.0, 22.0));
            let on_path = sel.starts_with(&path);
            let enabled = flag(item, "Enabled", true);
            let color = if on_path {
                p.rect_filled(r, 4.0, pal.accent);
                Color32::WHITE
            } else if enabled {
                pal.text
            } else {
                pal.text_tertiary
            };
            if on_path {
                next_top = r.top() - 5.0;
            }
            if flag(item, "Checked", false) {
                paint_check(&p, pos2(r.left() + 10.0, r.center().y), color);
            }
            p.text(
                pos2(r.left() + 20.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                caption(item),
                font.clone(),
                color,
            );
            let right = if item.children.is_empty() {
                shortcut_symbols(shortcut(item))
            } else {
                "\u{203A}".into()
            };
            p.text(
                pos2(r.right() - 8.0, r.center().y),
                egui::Align2::RIGHT_CENTER,
                right,
                font.clone(),
                if on_path {
                    Color32::WHITE
                } else {
                    pal.text_secondary
                },
            );
            if ui
                .interact(
                    r,
                    ui.scope_id().with(("menu-preview", path.clone())),
                    Sense::click(),
                )
                .clicked()
            {
                select = Some(path);
            }
            y += 22.0;
        }
        left = box_rect.right() - 4.0;
        top = next_top;
    }
    if levels.is_empty() && menu.children.is_empty() {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "The menu is empty",
            FontId::proportional(13.0),
            pal.text_tertiary,
        );
    }
    if let Some(s) = select {
        d.select(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_drop_access_key_markers() {
        assert_eq!(display_caption("&File"), "File");
        assert_eq!(display_caption("Save && Quit"), "Save & Quit");
        assert_eq!(display_caption("-"), "-");
    }
}
