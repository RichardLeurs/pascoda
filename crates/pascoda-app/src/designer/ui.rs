//! Drawing and input for the designer: palette, canvas with live native controls, inspector.

use super::geometry::{self, Align, Anchors, Bounds, Guide, Handle};
use super::menu::has_item_editor;
use super::native::{Marks, Native};
use super::viewport::{self, TITLE_BAR, View};
use super::{Designer, PALETTE, default_event, is_container, is_nonvisual, item_editor};
use crate::style::{Palette, hline, pill, segment};
use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, Vec2, vec2,
};
use pascoda_forms::PropValue;

/// The largest form the corner grip makes, per side.
const MAX_FORM: i64 = 8192;
/// How close (in points) the pointer must be to a handle to grab it.
const HANDLE_SLOP: f64 = 5.0;

enum Drag {
    /// Moves the selected controls together; bounds relative to their parent.
    /// `from` (in all drags) is where the drag began, in form coordinates.
    Move {
        starts: Vec<(Vec<usize>, Bounds)>,
        from: (f64, f64),
    },
    Resize {
        path: Vec<usize>,
        handle: Handle,
        start: Bounds,
        from: (f64, f64),
    },
    /// Resizes the union box of several siblings in `parent`, scaling each of `starts`.
    ResizeGroup {
        parent: Vec<usize>,
        starts: Vec<(Vec<usize>, Bounds)>,
        handle: Handle,
        start: Bounds,
        from: (f64, f64),
    },
    /// Rubber band in `container` (whose absolute origin is `origin`), from a point in form
    /// coordinates. `initial` is the selection to add to (empty unless Shift was held).
    Band {
        container: Vec<usize>,
        origin: (i64, i64),
        from: (i64, i64),
        initial: Vec<Vec<usize>>,
    },
}

#[derive(Default)]
pub struct UiState {
    /// The SQLdb connection editor is open for this connector.
    pub connection: Option<super::ConnectionEdit>,
    /// Why the connection could not be applied (shown in the editor).
    pub connection_error: String,
    /// The palette class that the next click on the form places.
    pub tool: Option<&'static str>,
    /// The palette page shown (index into the pages of `PALETTE`).
    page: usize,
    drag: Option<Drag>,
    name_buf: (Vec<usize>, String),
    name_focus: bool,
    /// Form size and pointer position (plus the scroll, i.e. in content coordinates) when the
    /// corner grip was grabbed.
    form_drag: Option<((i64, i64), Pos2)>,
    /// Scroll offset of each form's canvas, by unit path; kept while switching forms.
    pub scroll: std::collections::HashMap<std::path::PathBuf, Vec2>,
    /// The canvas as last laid out (area, form size, scroll), for tests and the self-test.
    pub view: Option<View>,
    pub native_error: Option<String>,
    /// Alignment guides and rubber band of the drag in progress, in form coordinates.
    guides: Vec<Guide>,
    band: Option<Bounds>,
    /// The menu or action list (by name) whose items are being edited; the item editor then
    /// takes the canvas's place.
    pub item_edit: Option<String>,
    /// The item editor's Name field while it is being typed in, as in the inspector.
    pub(super) item_name: (Vec<usize>, String),
    pub(super) item_name_focus: bool,
    /// The item editor's ShortCut field: path, text, and whether the text is a valid shortcut.
    pub(super) shortcut: (Vec<usize>, String, bool),
    pub(super) shortcut_focus: bool,
    /// The tray chip (index into `Designer::tray`) being dragged to a new place.
    tray_drag: Option<usize>,
}

/// What the designer asks the rest of the IDE to do.
#[derive(Default)]
pub struct Outcome {
    /// Show this handler's body in the code editor.
    pub open_handler: Option<String>,
    /// The unit text was changed by a sync.
    pub unit_changed: bool,
}

/// The inspector may not be narrower than this ...
const INSPECTOR_MIN: f32 = 200.0;
/// ... and the palette and canvas keep at least this much.
const CENTRE_MIN: f32 = 360.0;

/// The widest the inspector may grow when `avail` is the width of the designer's whole area.
fn inspector_max(avail: f32) -> f32 {
    (avail - CENTRE_MIN).max(INSPECTOR_MIN)
}

pub fn show(
    d: &mut Designer,
    state: &mut UiState,
    native: &mut Option<Native>,
    ui: &mut egui::Ui,
    frame: Option<&eframe::Frame>,
    unit: &mut String,
) -> Outcome {
    let mut out = Outcome::default();
    connection_dialog(d, state, ui.ctx());
    let pal = Palette::of(ui.ctx());
    let max_w = inspector_max(ui.available_width());
    egui::Panel::right("designer-inspector")
        .resizable(true)
        .default_size(260.0)
        .min_size(INSPECTOR_MIN)
        .max_size(max_w)
        .show_separator_line(true)
        .frame(
            Frame::new()
                .fill(pal.sidebar)
                .inner_margin(Margin::symmetric(12, 10)),
        )
        .show(ui, |ui| inspector(d, state, ui, unit, &mut out));

    // The palette and the canvas never paint outside the area the inspector left to them.
    let rest = ui.available_rect_before_wrap();
    ui.set_clip_rect(ui.clip_rect().intersect(rest));
    Frame::new().fill(pal.chrome).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // Pages as real tabs: the chosen one is joined to the icon row below it.
        // Built-in pages first, then the pages of imported packages (see `imports`).
        let imported = super::imported_entries();
        // A data module offers only the pages that have non-visual components.
        let datamodule = d.is_datamodule();
        let mut pages: Vec<&str> = Vec::new();
        for en in PALETTE.iter().chain(&imported).filter(|en| !datamodule || en.nonvisual) {
            if !pages.contains(&en.page) {
                pages.push(en.page);
            }
        }
        // The project's own components, when there are any, on a page of their own.
        let mut custom: Vec<&'static str> = d.custom_units.keys().copied().collect();
        custom.sort_unstable_by_key(|c| c.to_ascii_lowercase());
        if !custom.is_empty() {
            pages.push("Project");
        }
        let page_index = state.page.min(pages.len() - 1);
        let page = pages[page_index];
        Frame::new()
            .fill(pal.sidebar)
            .inner_margin(Margin {
                left: 10,
                right: 10,
                top: 6,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // The row scrolls sideways rather than growing: a row wider than the free area
                // (many pages, e.g. imported packages, or a long sync error) would widen this Ui,
                // and the canvas laid out after it would then reach under the inspector (the form
                // centred too far right, and the native controls covering the inspector).
                egui::ScrollArea::horizontal()
                    .id_salt("designer-pages")
                    .show(ui, |ui| ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if let (Some(i), _) =
                        crate::style::page_tabs(ui, &pal, &pages, page_index, pal.chrome)
                    {
                        state.page = i;
                    }
                    if let Some(e) = &d.sync_error {
                        ui.colored_label(pal.error, format!("unit not updated: {e}"));
                    } else if let Some(e) = &state.native_error {
                        ui.colored_label(pal.error, e);
                    }
                    if let Some(a) = &d.ancestor {
                        ui.colored_label(pal.text_tertiary, a.note());
                    }
                }));
            });
        Frame::new()
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let classes: Vec<&'static str> = if page == "Project" {
                    custom.iter().copied().filter(|c| !datamodule || is_nonvisual(c)).collect()
                } else {
                    PALETTE
                        .iter()
                        .chain(&imported)
                        .filter(|en| en.page == page && (!datamodule || en.nonvisual))
                        .map(|en| en.class)
                        .collect()
                };
                ui.horizontal_wrapped(|ui| {
                    for class in classes {
                        if palette_button(ui, &pal, class, state.tool == Some(class)).clicked() {
                            state.tool = if state.tool == Some(class) {
                                None
                            } else {
                                Some(class)
                            };
                        }
                    }
                });
            });
    });
    hline(ui, &pal);
    egui::Panel::bottom("designer-tray")
        .resizable(false)
        .show_separator_line(true)
        .frame(
            Frame::new()
                .fill(pal.sidebar)
                .inner_margin(Margin::symmetric(10, 6)),
        )
        .show(ui, |ui| tray(d, state, ui, unit, &mut out));
    if state.item_edit.is_some() {
        // The editor takes the canvas's place; the native controls would cover it.
        if let Some(n) = native {
            n.hide();
        }
        if item_editor::show(d, state, ui, unit, &mut out) {
            let _ = keyboard(d, state, ui.ctx(), unit, &mut out);
            return out;
        }
    }
    canvas(d, state, native, ui, frame, unit, &mut out);
    out
}

/// The component tray under the form: non-visual components (timers, dialogs, menus) as chips.
/// Click selects, double-click creates the default handler; with a non-visual palette tool, a
/// click anywhere on the strip places the component.
/// A data module's surface: there is no window to draw. The non-visual components are in the tray
/// below; clicking the empty surface clears the selection.
fn datamodule_surface(d: &mut Designer, native: &mut Option<Native>, ui: &mut egui::Ui) {
    if let Some(n) = native {
        // The native controls of a form would cover the surface.
        n.hide();
    }
    let pal = Palette::of(ui.ctx());
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click());
    ui.painter().rect_filled(rect, 0.0, pal.editor);
    let name = d.doc.root().name.clone();
    ui.painter().text(
        rect.center() - vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        format!("{name} (data module)"),
        egui::FontId::proportional(14.0),
        pal.text,
    );
    ui.painter().text(
        rect.center() + vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        "Non-visual components: they are listed in the tray below.",
        egui::FontId::proportional(12.0),
        pal.text_tertiary,
    );
    if resp.clicked() {
        d.select_paths(Vec::new());
    }
}

/// The SQLdb connection editor: the driver, host, database, user and password of a connector.
/// OK writes them to the connector (one undo step); Cancel discards them.
fn connection_dialog(d: &mut Designer, state: &mut UiState, ctx: &egui::Context) {
    let Some(mut edit) = state.connection.take() else { return };
    let (mut ok, mut cancel) = (false, false);
    let error = state.connection_error.clone();
    crate::style::dialog(ctx, "connection-editor", Some("SQLdb Connection"), 420.0, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        egui::Grid::new("connection-fields").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Driver");
            egui::ComboBox::from_id_salt("connection-driver")
                .selected_text(edit.connector.clone())
                .show_ui(ui, |ui| {
                    for (name, _) in super::CONNECTORS {
                        ui.selectable_value(&mut edit.connector, name.to_string(), *name);
                    }
                });
            ui.end_row();
            ui.label("Host name");
            ui.add(egui::TextEdit::singleline(&mut edit.host).desired_width(260.0));
            ui.end_row();
            ui.label("Database");
            ui.add(egui::TextEdit::singleline(&mut edit.database).desired_width(260.0).hint_text("a file path for SQLite"));
            ui.end_row();
            ui.label("User name");
            ui.add(egui::TextEdit::singleline(&mut edit.user).desired_width(260.0));
            ui.end_row();
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut edit.password).password(true).desired_width(260.0));
            ui.end_row();
        });
        if !error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal(|ui| {
            ok = ui.button("OK").clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if cancel {
        state.connection_error.clear();
        return;
    }
    if ok {
        match d.apply_connection(&edit) {
            Ok(()) => state.connection_error.clear(),
            Err(e) => state.connection_error = e.to_string(),
        }
        if state.connection_error.is_empty() {
            return;
        }
    }
    state.connection = Some(edit);
}

fn tray(
    d: &mut Designer,
    state: &mut UiState,
    ui: &mut egui::Ui,
    unit: &mut String,
    out: &mut Outcome,
) {
    let pal = Palette::of(ui.ctx());
    // Registered before the chips, so the chips get their own clicks.
    let strip = ui.interact(ui.max_rect(), ui.scope_id().with("tray"), Sense::click());
    let paths = d.tray();
    let mut rects: Vec<Rect> = Vec::new();
    let mut drop: Option<(usize, egui::Pos2)> = None;
    ui.horizontal_wrapped(|ui| {
        ui.set_min_height(24.0);
        ui.label(
            RichText::new("COMPONENTS")
                .size(10.5)
                .strong()
                .color(pal.text_tertiary),
        );
        if paths.is_empty() {
            ui.weak("Timers, dialogs and menus from the System page appear here");
        }
        for (k, path) in paths.iter().enumerate() {
            let Some(c) = d.doc.component(path) else {
                continue;
            };
            let (name, class) = (c.name.clone(), c.class.clone());
            let editing = state
                .item_edit
                .as_ref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&name));
            let r = chip(ui, &pal, &class, &name, d.is_selected(path) || editing);
            rects.push(r.rect);
            if r.clicked() {
                if ui.input(|i| i.modifiers.shift || i.modifiers.command) {
                    d.toggle(path.clone());
                } else {
                    d.select(path.clone());
                }
                // The item editor follows the chip clicked: another menu, or back to the form.
                if state.item_edit.is_some() && !editing {
                    state.item_edit = has_item_editor(&class).then(|| name.clone());
                }
            }
            if r.double_clicked() {
                d.select(path.clone());
                if has_item_editor(&class) {
                    // Like Lazarus: a double-click on a menu opens the menu editor.
                    state.item_edit = Some(name.clone());
                } else {
                    create_handler(d, unit, out);
                }
            }
            if r.drag_started() {
                state.tray_drag = Some(k);
            }
            if r.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            if r.drag_stopped()
                && let Some(p) = r.interact_pointer_pos()
            {
                drop = Some((k, p));
            }
        }
    });
    // Dragging a chip: a line shows where it will go; dropping rewrites the tray slots.
    if let (Some(from), Some(p)) = (state.tray_drag, ui.ctx().pointer_interact_pos()) {
        let to = tray_index(&rects, from, p);
        let x = if to < rects.len() && to != from {
            let r = rects[if to > from {
                to.min(rects.len() - 1)
            } else {
                to
            }];
            if to > from {
                r.right() + 3.0
            } else {
                r.left() - 3.0
            }
        } else {
            f32::NAN
        };
        if x.is_finite() {
            let y = rects[from].center().y;
            ui.painter().line_segment(
                [egui::pos2(x, y - 12.0), egui::pos2(x, y + 12.0)],
                Stroke::new(2.0, pal.accent),
            );
        }
    }
    if let Some((from, p)) = drop {
        state.tray_drag = None;
        let to = tray_index(&rects, from, p);
        if to != from {
            let _ = d.move_in_tray(from, to);
        }
    }
    if strip.clicked() {
        match state.tool {
            Some(class) if is_nonvisual(class) => {
                state.tool = None;
                if d.add_component(class, &[], 0, 0).is_ok() {
                    d.sync_unit(unit);
                    out.unit_changed = true;
                }
            }
            Some(_) => {}
            None => d.select(Vec::new()),
        }
    }
}

/// Where a tray chip dragged from `from` and dropped at `p` goes: the chips (other than itself)
/// before the pointer in reading order.
fn tray_index(rects: &[Rect], from: usize, p: egui::Pos2) -> usize {
    rects
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != from && (r.bottom() < p.y || (r.top() <= p.y && r.center().x < p.x)))
        .count()
}

/// One tray entry: a rounded chip with a small glyph for the kind of component and its name.
fn chip(
    ui: &mut egui::Ui,
    pal: &Palette,
    class: &str,
    name: &str,
    selected: bool,
) -> egui::Response {
    let color = if selected {
        pal.text
    } else {
        pal.text_secondary
    };
    let galley =
        ui.painter()
            .layout_no_wrap(name.to_string(), egui::FontId::proportional(12.0), color);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(galley.size().x + 36.0, 24.0), Sense::click_and_drag());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    let fill = if selected {
        pal.row_selected
    } else if resp.hovered() {
        pal.row_hover
    } else {
        pal.chrome
    };
    let stroke = Stroke::new(1.0, if selected { pal.accent } else { pal.separator });
    ui.painter()
        .rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
    let icon = Rect::from_center_size(rect.left_center() + vec2(15.0, 0.0), vec2(16.0, 16.0));
    crate::icons::paint(ui.painter(), icon, class);
    ui.painter().galley(
        egui::pos2(icon.right() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    resp
}

/// A palette entry: the class's Lazarus icon on a rounded square; the name is its tooltip.
fn palette_button(ui: &mut egui::Ui, pal: &Palette, class: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 30.0), Sense::click());
    let label = class.strip_prefix('T').unwrap_or(class);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let fill = if selected {
        pal.row_selected
    } else if resp.hovered() {
        pal.row_hover
    } else {
        pal.editor
    };
    let stroke = if selected {
        Stroke::new(1.0, pal.accent)
    } else {
        Stroke::new(1.0, pal.separator)
    };
    ui.painter()
        .rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
    crate::icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(22.0, 22.0)),
        class,
    );
    resp.on_hover_text(label)
}

fn canvas(
    d: &mut Designer,
    state: &mut UiState,
    native: &mut Option<Native>,
    ui: &mut egui::Ui,
    frame: Option<&eframe::Frame>,
    unit: &mut String,
    out: &mut Outcome,
) {
    if d.is_datamodule() {
        datamodule_surface(d, native, ui);
        return;
    }
    let ctx = ui.ctx().clone();
    // Never more than `show` left beside the inspector (it set the clip to that area): content
    // that grew the Ui past it must not move the canvas, or the native views, under the inspector.
    let avail = ui.available_rect_before_wrap().intersect(ui.clip_rect());
    let (w, h) = geometry::form_size(d.doc.root());
    // The visible canvas, minus scroll bars where the form does not fit.
    let (area, hbar, vbar) = viewport::split(avail, (w, h));
    let key = d.pas_path.clone();
    let mut view = View::new(
        area,
        (w, h),
        state.scroll.get(&key).copied().unwrap_or_default(),
    );

    if native.is_none()
        && state.native_error.is_none()
        && let Some(frame) = frame
    {
        match Native::attach(frame) {
            Ok(n) => *native = Some(n),
            Err(e) => state.native_error = Some(format!("native controls unavailable: {e}")),
        }
    }

    // Scrolling: wheel and trackpad over the canvas, the scroll bars, and dragging near an edge.
    if ui.rect_contains_pointer(area) {
        let mut delta = ui.input(|i| i.smooth_scroll_delta);
        if ui.input(|i| i.modifiers.shift) && delta.x == 0.0 {
            delta = vec2(delta.y, 0.0); // a mouse wheel with Shift scrolls sideways
        }
        if delta != Vec2::ZERO {
            view.scroll_by(-delta);
        }
    }
    let pal = Palette::of(&ctx);
    ui.painter().rect_filled(avail, 0.0, pal.editor);
    for (strip, horizontal) in [(hbar, true), (vbar, false)] {
        if let Some(strip) = strip {
            scroll_bar(ui, &pal, &mut view, strip, horizontal);
        }
    }
    let dragging = state.drag.is_some() || state.form_drag.is_some();
    if dragging && let Some(p) = ctx.pointer_latest_pos() {
        let d = view.auto_scroll(p);
        if d != Vec2::ZERO {
            view.scroll_by(d);
            ctx.request_repaint();
        }
    }
    state.scroll.insert(key, view.scroll);
    state.view = Some(view);

    let origin = view.origin();
    let client = view.client();
    let window = view.window();

    // Xcode's canvas: a plain surface with a faint dot grid (scrolling with the form), the window
    // floating on a shadow. Everything is clipped to the visible canvas.
    let painter = ui.painter_at(area);
    let step = 14.0;
    let first = |lo: f32, s: f32| lo + step / 2.0 - s.rem_euclid(step);
    let mut y = first(area.top(), view.scroll.y);
    while y < area.bottom() {
        let mut x = first(area.left(), view.scroll.x);
        while x < area.right() {
            painter.circle_filled(egui::pos2(x, y), 0.8, pal.separator);
            x += step;
        }
        y += step;
    }
    painter.add(
        egui::epaint::Shadow {
            offset: [0, 6],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 120 } else { 46 }),
        }
        .as_shape(window, CornerRadius::same(10)),
    );
    // What AppKit's window background looks like; the real controls draw over this.
    let client_bg = if pal.dark {
        Color32::from_rgb(0x2A, 0x2A, 0x2C)
    } else {
        Color32::from_rgb(0xF2, 0xF2, 0xF2)
    };
    painter.rect_filled(
        client,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 10,
            se: 10,
        },
        client_bg,
    );
    let bar = Rect::from_min_size(window.min, vec2(window.width(), TITLE_BAR));
    painter.rect_filled(
        bar,
        CornerRadius {
            nw: 10,
            ne: 10,
            sw: 0,
            se: 0,
        },
        pal.chrome,
    );
    for (i, c) in [
        Color32::from_rgb(255, 95, 87),
        Color32::from_rgb(254, 188, 46),
        Color32::from_rgb(40, 200, 64),
    ]
    .into_iter()
    .enumerate()
    {
        painter.circle_filled(
            bar.min + vec2(16.0 + 20.0 * i as f32, TITLE_BAR / 2.0),
            6.0,
            c,
        );
    }
    let caption = d
        .doc
        .root()
        .prop("Caption")
        .and_then(PropValue::as_str)
        .unwrap_or_default();
    painter.text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        caption,
        egui::FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    painter.rect_stroke(
        window,
        CornerRadius::same(10),
        Stroke::new(1.0, ui.visuals().window_stroke.color),
        StrokeKind::Inside,
    );

    // Input, on the visible part of the form only. The native canvas is click-through, so egui
    // sees every click and drag here.
    let response = ui.interact(
        client.intersect(area),
        ui.scope_id().with("form-surface"),
        Sense::click_and_drag(),
    );
    // The grip at the form's bottom-right corner resizes the form; anchored and docked
    // controls follow live, as they will in the running program.
    let grip = ui.interact(
        Rect::from_center_size(client.max, vec2(18.0, 18.0)).intersect(area),
        ui.scope_id().with("form-grip"),
        Sense::drag(),
    );
    if grip.hovered() || grip.dragged() {
        ctx.set_cursor_icon(CursorIcon::ResizeNwSe);
    }
    for k in 0..3 {
        let o = 4.0 + 4.0 * k as f32;
        painter.line_segment(
            [client.max + vec2(-o, -1.0), client.max + vec2(-1.0, -o)],
            Stroke::new(1.0, ui.visuals().weak_text_color()),
        );
    }
    if grip.drag_started() {
        d.checkpoint();
        if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
            state.form_drag = Some(((w, h), p + view.scroll));
        }
    }
    if let (Some(((sw, sh), from)), Some(p)) = (state.form_drag, grip.interact_pointer_pos()) {
        // In content coordinates (pointer + scroll), so auto-scrolling keeps the grip under the
        // pointer. The form may grow past the visible canvas: it scrolls.
        let p = p + view.scroll;
        let nw = (sw + (p.x - from.x).round() as i64).clamp(120, MAX_FORM);
        let nh = (sh + (p.y - from.y).round() as i64).clamp(80, MAX_FORM);
        let _ = d.resize_container(&[], nw, nh);
    }
    if grip.drag_stopped() {
        state.form_drag = None;
    }

    let to_form = |p: Pos2| view.to_form(p);
    // The resize handles: on the union box of the selected siblings, or on the one control.
    let group = d.resize_group();
    let handles = handle_box(d);

    if let Some(p) = response.hover_pos() {
        let (x, y) = to_form(p);
        let icon = match handles.and_then(|b| geometry::handle_at(b, x, y, HANDLE_SLOP)) {
            Some(Handle::TopLeft | Handle::BottomRight) => CursorIcon::ResizeNwSe,
            Some(Handle::TopRight | Handle::BottomLeft) => CursorIcon::ResizeNeSw,
            Some(Handle::Top | Handle::Bottom) => CursorIcon::ResizeVertical,
            Some(Handle::Left | Handle::Right) => CursorIcon::ResizeHorizontal,
            None if state.tool.is_some() => CursorIcon::Crosshair,
            None => CursorIcon::Default,
        };
        ctx.set_cursor_icon(icon);
    }

    let mods = ui.input(|i| i.modifiers);
    // Shift or Cmd adds to the selection (or takes out); Cmd-drag starts a rubber band even
    // over a control, inside the container under the pointer (Delphi uses Ctrl for that).
    let adding = mods.shift || mods.command;
    if response.drag_started()
        && let Some(p) = ui.input(|i| i.pointer.press_origin())
    {
        // Drags remember where they began in form coordinates, so scrolling during the drag
        // (auto-scroll, trackpad) does not move the pointer's place in the form.
        let (x, y) = to_form(p);
        let from = (x, y);
        let hit = geometry::hit_test(d.doc.root(), x, y);
        if let Some(handle) = handles.and_then(|b| geometry::handle_at(b, x, y, HANDLE_SLOP)) {
            d.checkpoint();
            state.drag = Some(match group {
                Some((parent, starts, union)) => Drag::ResizeGroup {
                    parent,
                    starts,
                    handle,
                    start: union,
                    from,
                },
                None => Drag::Resize {
                    path: d.selected.clone(),
                    handle,
                    start: d
                        .selected_component()
                        .map(geometry::bounds)
                        .or(handles)
                        .expect("a handle belongs to a control"),
                    from,
                },
            });
        } else if state.tool.is_none() {
            match hit {
                Some(path) if !mods.command => {
                    // Dragging a selected control moves the whole selection.
                    if !d.is_selected(&path) {
                        if mods.shift {
                            d.toggle(path);
                        } else {
                            d.select(path);
                        }
                    }
                    let starts = d.movable_selection();
                    if !starts.is_empty() {
                        d.checkpoint();
                        state.drag = Some(Drag::Move { starts, from });
                    }
                }
                _ => {
                    let (container, ox, oy) = container_at(d, x, y);
                    let initial = if mods.shift {
                        d.selection()
                    } else {
                        Vec::new()
                    };
                    state.drag = Some(Drag::Band {
                        container,
                        origin: (ox, oy),
                        from: (x as i64, y as i64),
                        initial,
                    });
                }
            }
        }
    }
    if let (Some(drag), Some(p)) = (&state.drag, response.interact_pointer_pos()) {
        let (fx, fy) = to_form(p);
        let delta =
            |from: &(f64, f64)| ((fx - from.0).round() as i64, (fy - from.1).round() as i64);
        let parent_origin = |path: &[usize]| {
            geometry::absolute(d.doc.root(), &path[..path.len() - 1])
                .map_or((0, 0), |b| (b.left, b.top))
        };
        match drag {
            Drag::Move { starts, from } => {
                // In the primary's container's coordinates, also when the selection spans
                // containers: the grid and guides are those of the primary's container.
                if let Some((union, near, o)) = d.move_context(starts) {
                    let (dx, dy) = delta(from);
                    let (b, guides) =
                        geometry::guided_move(union, dx, dy, geometry::GRID, &near, geometry::SNAP);
                    let starts = starts.clone();
                    let _ = d.offset_all(&starts, b.left - union.left, b.top - union.top);
                    state.guides = guides.into_iter().map(|g| g.offset(o.0, o.1)).collect();
                }
            }
            Drag::ResizeGroup {
                parent,
                starts,
                handle,
                start,
                from,
            } => {
                let paths: Vec<Vec<usize>> = starts.iter().map(|(p, _)| p.clone()).collect();
                let (b, guides) = geometry::guided_resize(
                    *start,
                    *handle,
                    delta(from),
                    geometry::GRID,
                    &d.neighbours(parent, &paths),
                    geometry::SNAP,
                );
                let o = d.origin_of(parent);
                let (starts, start) = (starts.clone(), *start);
                let _ = d.scale_all(&starts, start, b);
                state.guides = guides.into_iter().map(|g| g.offset(o.0, o.1)).collect();
            }
            Drag::Resize {
                path,
                handle,
                start,
                from,
            } => {
                let parent = path[..path.len() - 1].to_vec();
                let (b, guides) = geometry::guided_resize(
                    *start,
                    *handle,
                    delta(from),
                    geometry::GRID,
                    &d.neighbours(&parent, std::slice::from_ref(path)),
                    geometry::SNAP,
                );
                let o = parent_origin(path);
                let path = path.clone();
                let _ = d.place(&path, b);
                state.guides = guides.into_iter().map(|g| g.offset(o.0, o.1)).collect();
            }
            Drag::Band {
                container,
                origin: o,
                from,
                initial,
            } => {
                let band = Bounds::spanning(*from, (fx as i64, fy as i64));
                // Live: start again from the selection the band began with.
                let (container, initial, o) = (container.clone(), initial.clone(), *o);
                d.select_paths(initial);
                d.select_in_rect(&container, band.offset(-o.0, -o.1), true);
                state.band = Some(band);
            }
        }
    }
    if response.drag_stopped() {
        state.drag = None;
        state.guides.clear();
        state.band = None;
    }

    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let (x, y) = to_form(p);
        if let Some(class) = state.tool.take() {
            // Place into the container under the click, positioned relative to it.
            let (parent, ox, oy) = container_at(d, x, y);
            if d.add_component(
                class,
                &parent,
                (x - ox as f64) as i64,
                (y - oy as f64) as i64,
            )
            .is_ok()
            {
                d.sync_unit(unit);
                out.unit_changed = true;
            }
        } else {
            match geometry::hit_test(d.doc.root(), x, y) {
                Some(path) if adding => d.toggle(path),
                None if adding => {}
                hit => d.select(hit.unwrap_or_default()),
            }
        }
    }
    if response.double_clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let (x, y) = to_form(p);
        d.select(geometry::hit_test(d.doc.root(), x, y).unwrap_or_default());
        create_handler(d, unit, out);
    }

    let nudged = keyboard(d, state, &ctx, unit, out);
    if nudged && let Some(b) = handle_box(d) {
        // Arrow keys move the selection out of sight: follow it, like Interface Builder.
        let s = view.reveal((b.left as f64, b.top as f64, b.width as f64, b.height as f64));
        if s != view.scroll {
            state.scroll.insert(d.pas_path.clone(), s);
            ctx.request_repaint();
        }
    }

    // Components whose text or AutoSize changed get the size LCL would give them.
    if let Some(n) = native.as_ref()
        && d.apply_autosize(|class, text| n.preferred_size(class, text))
    {
        ctx.request_repaint();
    }
    let (w, h) = geometry::form_size(d.doc.root());
    if (w, h) != view.form {
        ctx.request_repaint(); // the scroll range changed; lay out again next frame
    }

    let marks = marks(d, state);
    // Drawn by egui as well: hidden under the native canvas when there is one, and what offscreen
    // renders show.
    paint_marks(&painter, origin, &marks, pal.accent);
    if let Some(n) = native {
        n.update(&ctx, area, origin, (w, h), &d.specs(), &marks);
    }
}

/// One scroll bar in `strip`: a track with a draggable thumb; a click on the track pages.
fn scroll_bar(ui: &mut egui::Ui, pal: &Palette, view: &mut View, strip: Rect, horizontal: bool) {
    let track = strip.shrink(2.0);
    let len = if horizontal {
        track.width()
    } else {
        track.height()
    };
    let id = ui
        .scope_id()
        .with(if horizontal { "hscroll" } else { "vscroll" });
    let (start, thumb_len) = view.thumb(len, horizontal);
    let thumb = if horizontal {
        Rect::from_min_size(
            track.min + vec2(start, 0.0),
            vec2(thumb_len, track.height()),
        )
    } else {
        Rect::from_min_size(track.min + vec2(0.0, start), vec2(track.width(), thumb_len))
    };
    let r = ui.interact(strip, id, Sense::click_and_drag());
    r.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::ScrollBar,
            true,
            if horizontal {
                "Horizontal scroll bar"
            } else {
                "Vertical scroll bar"
            },
        )
    });
    if r.dragged() {
        let d = r.drag_delta();
        let along = if horizontal { d.x } else { d.y };
        let s = view.thumb_drag(len, horizontal, along);
        view.scroll_by(if horizontal {
            vec2(s, 0.0)
        } else {
            vec2(0.0, s)
        });
    } else if r.clicked()
        && let Some(p) = r.interact_pointer_pos()
        && !thumb.contains(p)
    {
        // Page towards the click, like AppKit's "jump to the next page".
        let page = if horizontal {
            view.area.width()
        } else {
            view.area.height()
        } * 0.9;
        let before = if horizontal {
            p.x < thumb.left()
        } else {
            p.y < thumb.top()
        };
        let by = if before { -page } else { page };
        view.scroll_by(if horizontal {
            vec2(by, 0.0)
        } else {
            vec2(0.0, by)
        });
    }
    let painter = ui.painter();
    painter.rect_filled(strip, 0.0, pal.editor);
    let fill = if r.hovered() || r.dragged() {
        pal.text_secondary
    } else {
        pal.text_tertiary
    };
    painter.rect_filled(thumb, 4.0, fill.gamma_multiply(0.7));
}

/// Where the resize handles go, in form coordinates: the union box of the selected siblings
/// when there are two or more, else the primary control.
fn handle_box(d: &Designer) -> Option<Bounds> {
    match d.resize_group() {
        Some((parent, _, union)) => {
            let o = d.origin_of(&parent);
            Some(union.offset(o.0, o.1))
        }
        None if !d.selected.is_empty() => geometry::absolute(d.doc.root(), &d.selected),
        None => None,
    }
}

/// The selection, guides and rubber band to draw, in form coordinates.
fn marks(d: &Designer, state: &UiState) -> Marks {
    let abs = |p: &Vec<usize>| geometry::absolute(d.doc.root(), p);
    let sel = d.selection();
    Marks {
        primary: sel.first().and_then(abs),
        others: sel.iter().skip(1).filter_map(abs).collect(),
        handles: handle_box(d),
        guides: state.guides.clone(),
        band: state.band,
    }
}

fn paint_marks(painter: &egui::Painter, origin: Pos2, m: &Marks, accent: Color32) {
    let rect = |b: &Bounds| {
        Rect::from_min_size(
            origin + vec2(b.left as f32, b.top as f32),
            vec2(b.width as f32, b.height as f32),
        )
    };
    let stroke = Stroke::new(1.0, accent);
    if let Some(b) = &m.band {
        painter.rect(
            rect(b),
            0.0,
            accent.gamma_multiply(0.12),
            stroke,
            StrokeKind::Inside,
        );
    }
    for b in m.others.iter().chain(&m.primary) {
        painter.rect_stroke(rect(b), 0.0, stroke, StrokeKind::Inside);
    }
    if let Some(b) = &m.handles {
        let r = rect(b);
        if m.primary.as_ref() != Some(b) {
            // A group's union box: a faint frame that the handles belong to.
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, accent.gamma_multiply(0.45)),
                StrokeKind::Inside,
            );
        }
        let h = super::native::HANDLE as f32;
        for p in [
            r.left_top(),
            r.center_top(),
            r.right_top(),
            r.left_center(),
            r.right_center(),
            r.left_bottom(),
            r.center_bottom(),
            r.right_bottom(),
        ] {
            let hr = Rect::from_center_size(p, vec2(h, h));
            painter.rect(hr, 0.0, Color32::WHITE, stroke, StrokeKind::Inside);
        }
    }
    for g in &m.guides {
        let (at, from, to) = (g.at as f32 + 0.5, g.from as f32, g.to as f32);
        let (a, b) = if g.vertical {
            (egui::pos2(at, from), egui::pos2(at, to))
        } else {
            (egui::pos2(from, at), egui::pos2(to, at))
        };
        painter.line_segment([origin + a.to_vec2(), origin + b.to_vec2()], stroke);
    }
}

/// The container a click at `(x, y)` should add to, with that container's absolute origin.
fn container_at(d: &Designer, x: f64, y: f64) -> (Vec<usize>, i64, i64) {
    let mut path = geometry::hit_test(d.doc.root(), x, y).unwrap_or_default();
    while !path.is_empty()
        && !d
            .doc
            .component(&path)
            .is_some_and(|c| is_container(&c.class))
    {
        path.pop();
    }
    let origin = geometry::absolute(d.doc.root(), &path).map_or((0, 0), |b| (b.left, b.top));
    (path, origin.0, origin.1)
}

pub(super) fn create_handler(d: &mut Designer, unit: &mut String, out: &mut Outcome) {
    let Some(class) = d.selected_component().map(|c| c.class.clone()) else {
        return;
    };
    let event = if d.selected.is_empty() {
        "OnCreate"
    } else {
        default_event(&class)
    };
    if let Ok(Some(name)) = d.ensure_handler(event) {
        d.sync_unit(unit);
        out.unit_changed = true;
        out.open_handler = Some(name);
    }
}

/// Designer keys; returns whether the arrow keys moved the selection.
fn keyboard(
    d: &mut Designer,
    state: &mut UiState,
    ctx: &egui::Context,
    unit: &mut String,
    out: &mut Outcome,
) -> bool {
    use egui::{Key, Modifiers};
    if ctx.egui_wants_keyboard_input() {
        return false; // a text field has focus
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        if state.tool.is_some() {
            state.tool = None;
        } else {
            // Like Delphi: Esc selects the parent.
            d.select(d.current_container());
        }
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
        d.select_all();
    }
    let delete = ctx.input_mut(|i| {
        i.consume_key(Modifiers::NONE, Key::Delete)
            || i.consume_key(Modifiers::NONE, Key::Backspace)
    });
    if delete && d.delete_selected().is_ok() {
        d.sync_unit(unit);
        out.unit_changed = true;
    }
    // egui-winit turns Cmd+C/X/V into these events (Paste carries the system clipboard's text).
    let (mut copy, mut cut, mut paste) = (false, false, None);
    ctx.input(|i| {
        for e in &i.events {
            match e {
                egui::Event::Copy => copy = true,
                egui::Event::Cut => cut = true,
                egui::Event::Paste(text) => paste = Some(text.clone()),
                _ => {}
            }
        }
    });
    // The clipboard is the system one, as `.lfm` text, so it can be pasted into a text editor
    // (or from one, or from Lazarus) too. Text that is not a component is ignored.
    if (copy || cut)
        && let Some(text) = d.copy_selection()
    {
        ctx.copy_text(text);
        if cut && d.delete_selected().is_ok() {
            d.sync_unit(unit);
            out.unit_changed = true;
        }
    }
    let duplicate = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D));
    let pasted = match paste {
        Some(text) => d.paste(&text).is_ok_and(|p| !p.is_empty()),
        None if duplicate => d.duplicate().is_ok_and(|p| !p.is_empty()),
        None => false,
    };
    if pasted {
        d.sync_unit(unit);
        out.unit_changed = true;
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)) {
        d.redo();
        d.sync_unit(unit);
        out.unit_changed = true;
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
        d.undo();
        d.sync_unit(unit);
        out.unit_changed = true;
    }
    let mut nudged = false;
    for (key, dx, dy) in [
        (Key::ArrowLeft, -1, 0),
        (Key::ArrowRight, 1, 0),
        (Key::ArrowUp, 0, -1),
        (Key::ArrowDown, 0, 1),
    ] {
        for (mods, step) in [(Modifiers::NONE, 1), (Modifiers::SHIFT, geometry::GRID)] {
            if ctx.input_mut(|i| i.consume_key(mods, key)) {
                nudged |= d.nudge_selection(dx * step, dy * step).is_ok();
            }
        }
    }
    nudged
}

/// `AnchorSideLeft/Top/Right/Bottom`: per edge, the control it follows (the parent or a
/// sibling) and which line of it. Collapsed unless one is set.
fn anchor_sides(d: &mut Designer, ui: &mut egui::Ui, path: &[usize], c: &pascoda_forms::Component) {
    use geometry::{AnchorSide, Edge, SideRef};
    let any = Edge::ALL
        .iter()
        .any(|e| geometry::anchor_side(c, *e).is_some());
    let targets = d.anchor_targets(path);
    egui::CollapsingHeader::new("Anchor sides")
        .id_salt(("anchor-sides", path.to_vec()))
        .default_open(any)
        .show(ui, |ui| {
            egui::Grid::new(("anchor-sides-grid", path.to_vec()))
                .num_columns(3)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    for edge in Edge::ALL {
                        let cur = geometry::anchor_side(c, edge);
                        let lbl = ui.label(edge.name());
                        let mut control = cur.as_ref().map(|s| s.control.clone());
                        let shown = control.clone().unwrap_or_else(|| "\u{2014}".into());
                        egui::ComboBox::from_id_salt(("anchor-ctl", edge.name(), path.to_vec()))
                            .width(110.0)
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut control, None, "\u{2014} (none)");
                                for t in &targets {
                                    ui.selectable_value(&mut control, Some(t.clone()), t);
                                }
                            })
                            .response
                            .labelled_by(lbl.id);
                        let mut side = cur.as_ref().map_or(SideRef::Start, |s| s.side);
                        ui.add_enabled_ui(control.is_some(), |ui| {
                            egui::ComboBox::from_id_salt((
                                "anchor-side",
                                edge.name(),
                                path.to_vec(),
                            ))
                            .width(70.0)
                            .selected_text(side.label(edge.horizontal()))
                            .show_ui(ui, |ui| {
                                for s in SideRef::ALL {
                                    ui.selectable_value(&mut side, s, s.label(edge.horizontal()));
                                }
                            })
                            .response
                            .on_hover_text("Which line of that control this edge follows");
                        });
                        let new = control.map(|control| AnchorSide { control, side });
                        if new != cur {
                            let _ = d.set_anchor_side(path, edge, new);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// `BorderSpacing.Left/Top/Right/Bottom/Around`: space kept to whatever an edge is anchored or
/// docked to. 0 is not written.
fn border_spacing(
    d: &mut Designer,
    ui: &mut egui::Ui,
    path: &[usize],
    c: &pascoda_forms::Component,
) {
    let sp = geometry::Spacing::of(c);
    egui::CollapsingHeader::new("Border spacing")
        .id_salt(("border-spacing", path.to_vec()))
        .default_open(sp != geometry::Spacing::default())
        .show(ui, |ui| {
            egui::Grid::new(("border-spacing-grid", path.to_vec()))
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    for field in geometry::Spacing::FIELDS {
                        let lbl = ui.label(field);
                        let mut v = sp.get(field);
                        if ui
                            .add(egui::DragValue::new(&mut v).range(0..=999))
                            .labelled_by(lbl.id)
                            .changed()
                        {
                            let _ = d.set_border_spacing(path, field, v);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// The focusable controls of the form or a container in tab order, with buttons to move one up
/// or down; each move renumbers `TabOrder` 0..n. Kept in the inspector rather than a popup, as
/// egui windows would be drawn under the native controls.
fn tab_order(d: &mut Designer, ui: &mut egui::Ui, container: &[usize]) {
    let order = d.tab_order(container);
    egui::CollapsingHeader::new(format!("Tab order ({})", order.len()))
        .id_salt(("tab-order", container.to_vec()))
        .show(ui, |ui| {
            if order.is_empty() {
                ui.weak("No controls here take the focus");
            }
            for (i, p) in order.iter().enumerate() {
                let name = d
                    .doc
                    .component(p)
                    .map_or_else(String::new, |c| c.name.clone());
                ui.horizontal(|ui| {
                    let up = ui
                        .add_enabled(i > 0, egui::Button::new("\u{2191}").small())
                        .on_hover_text(format!("Move {name} earlier"));
                    let down = ui
                        .add_enabled(i + 1 < order.len(), egui::Button::new("\u{2193}").small())
                        .on_hover_text(format!("Move {name} later"));
                    ui.label(format!("{i}. {name}"));
                    let by = if up.clicked() {
                        -1
                    } else if down.clicked() {
                        1
                    } else {
                        0
                    };
                    if by != 0 {
                        let _ = d.move_in_tab_order(container, i, by);
                    }
                });
            }
        });
    ui.add_space(4.0);
}

pub(super) fn section(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.label(
        RichText::new(title)
            .size(10.5)
            .strong()
            .color(pal.text_tertiary),
    );
}

/// Several components selected: the layout properties they all have, shown with the primary's
/// values and applied to all, plus the arrange commands.
fn multi_inspector(d: &mut Designer, ui: &mut egui::Ui, pal: &Palette) {
    let n = d.selection().len();
    ui.label(RichText::new(format!("{n} components selected")).color(pal.text));
    let movable = d.movable_selection();
    if movable.is_empty() {
        ui.weak("Non-visual components: Delete, Copy and Cut apply to all.");
        return;
    }
    if movable.len() < n {
        ui.weak("Non-visual components in the selection are left out of layout edits.");
    }
    ui.separator();
    // The primary's values, or the first control's when the primary is non-visual.
    let reference = d
        .selected_component()
        .filter(|c| !is_nonvisual(&c.class))
        .or_else(|| d.doc.component(&movable[0].0))
        .cloned();
    let Some(c) = reference else {
        return;
    };
    section(ui, pal, "LAYOUT");
    let b = geometry::bounds(&c);
    egui::Grid::new("multi-bounds")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for (field, value) in [
                ("Left", b.left),
                ("Top", b.top),
                ("Width", b.width),
                ("Height", b.height),
            ] {
                let lbl = ui.label(field);
                let mut v = value;
                if ui
                    .add(egui::DragValue::new(&mut v))
                    .labelled_by(lbl.id)
                    .changed()
                {
                    let _ = d.set_selection_bound(field, v);
                }
                ui.end_row();
            }
        });
    let a = geometry::anchors_of(&c);
    let (mut l, mut t, mut r, mut bo) = (a.left, a.top, a.right, a.bottom);
    ui.horizontal(|ui| {
        ui.label("Anchors");
        ui.checkbox(&mut l, "L").on_hover_text("akLeft");
        ui.checkbox(&mut t, "T").on_hover_text("akTop");
        ui.checkbox(&mut r, "R").on_hover_text("akRight");
        ui.checkbox(&mut bo, "B").on_hover_text("akBottom");
    });
    let new = Anchors {
        left: l,
        top: t,
        right: r,
        bottom: bo,
    };
    if new != a {
        let _ = d.set_selection_anchors(new);
    }
    let before = geometry::align_of(&c);
    let mut align = before;
    egui::ComboBox::from_label("Align")
        .selected_text(align.ident())
        .show_ui(ui, |ui| {
            for a in Align::ALL {
                ui.selectable_value(&mut align, a, a.ident());
            }
        });
    if align != before {
        let _ = d.set_selection_align(align);
    }
    let parent = d.current_container();
    let spans = movable.iter().any(|(p, _)| p[..p.len() - 1] != *parent);
    if movable.len() >= 2 {
        ui.add_space(8.0);
        section(ui, pal, "ARRANGE");
        ui.weak(if spans {
            "Only the controls in the first selected control's container take part"
        } else {
            "Relative to the first selected control"
        });
        for row in [
            &geometry::Arrange::ALIGN[..3],
            &geometry::Arrange::ALIGN[3..],
            &geometry::Arrange::SIZE_SPACE[..],
        ] {
            ui.horizontal(|ui| {
                pill(ui, pal, |ui| {
                    for op in row {
                        let (label, tip) = op.label();
                        // Spacing needs three controls.
                        let usable = movable.len() >= 3
                            || !matches!(op, geometry::Arrange::SpaceX | geometry::Arrange::SpaceY);
                        let r = ui
                            .add_enabled_ui(usable, |ui| segment(ui, pal, label, false))
                            .inner;
                        if r.on_hover_text(tip).clicked() {
                            let _ = d.arrange(*op);
                        }
                    }
                });
            });
        }
    }
}

fn inspector(
    d: &mut Designer,
    state: &mut UiState,
    ui: &mut egui::Ui,
    unit: &mut String,
    out: &mut Outcome,
) {
    let pal = Palette::of(ui.ctx());
    ui.label(
        RichText::new("Properties")
            .size(13.0)
            .strong()
            .color(pal.text),
    );
    ui.add_space(4.0);
    if d.selection().len() > 1 {
        multi_inspector(d, ui, &pal);
        return;
    }
    let Some(c) = d.selected_component() else {
        return;
    };
    let (name, class) = (c.name.clone(), c.class.clone());
    let props: Vec<(String, PropValue)> = c
        .props
        .iter()
        .map(|p| (p.name.clone(), p.value.clone()))
        .collect();
    let is_form = d.selected.is_empty();

    ui.horizontal(|ui| {
        let name_label = ui.label("Name");
        if !state.name_focus || state.name_buf.0 != d.selected {
            state.name_buf = (d.selected.clone(), name.clone());
        }
        let r = ui
            .add(egui::TextEdit::singleline(&mut state.name_buf.1).desired_width(120.0))
            .labelled_by(name_label.id);
        state.name_focus = r.has_focus();
        if r.lost_focus() && state.name_buf.1 != name && !is_form {
            let new = state.name_buf.1.clone();
            if let Ok(handlers) = d.rename_selected(&new) {
                d.sync_rename(unit, &name, &new, &handlers);
                out.unit_changed = true;
            }
        }
        ui.weak(&class);
    });
    ui.separator();

    let path = d.selected.clone();
    // A SQLdb connector opens the connection editor.
    if let Some(edit) = d.connection_of(&path)
        && ui.button("Edit Connection\u{2026}").clicked()
    {
        state.connection = Some(edit);
        state.connection_error.clear();
    }
    // Menus and action lists (and the items in them) open the item editor.
    if let Some(owner) = d.item_owner(&path).filter(|_| !is_form)
        && let Some(oc) = d.doc.component(&owner)
    {
        let label = if oc.class.eq_ignore_ascii_case("TActionList") {
            "Edit Actions\u{2026}"
        } else {
            "Edit Menu Items\u{2026}"
        };
        let editing = state
            .item_edit
            .as_ref()
            .is_some_and(|n| n.eq_ignore_ascii_case(&oc.name));
        if !editing && ui.button(label).clicked() {
            state.item_edit = Some(oc.name.clone());
        }
        ui.add_space(4.0);
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        if !is_form
            && let Some(c) = d
                .selected_component()
                .filter(|c| !is_nonvisual(&c.class))
                .cloned()
        {
            ui.label(
                RichText::new("LAYOUT")
                    .size(10.5)
                    .strong()
                    .color(pal.text_tertiary),
            );
            let a = geometry::anchors_of(&c);
            let (mut l, mut t, mut r, mut b) = (a.left, a.top, a.right, a.bottom);
            ui.horizontal(|ui| {
                ui.label("Anchors");
                ui.checkbox(&mut l, "L").on_hover_text("akLeft");
                ui.checkbox(&mut t, "T").on_hover_text("akTop");
                ui.checkbox(&mut r, "R").on_hover_text("akRight");
                ui.checkbox(&mut b, "B").on_hover_text("akBottom");
            });
            let new = Anchors {
                left: l,
                top: t,
                right: r,
                bottom: b,
            };
            if new != a {
                let _ = d.set_anchors(&path, new);
            }
            let before = geometry::align_of(&c);
            let mut align = before;
            egui::ComboBox::from_label("Align")
                .selected_text(align.ident())
                .show_ui(ui, |ui| {
                    for a in Align::ALL {
                        ui.selectable_value(&mut align, a, a.ident());
                    }
                });
            if align != before {
                let _ = d.set_align(&path, align);
            }
            let mut auto = geometry::auto_size_of(&c);
            if ui.checkbox(&mut auto, "AutoSize").changed() {
                let _ = d.set_auto_size(&path, auto);
            }
            ui.add_space(4.0);
            anchor_sides(d, ui, &path, &c);
            border_spacing(d, ui, &path, &c);
            ui.add_space(8.0);
        }
        if is_form || is_container(&class) {
            tab_order(d, ui, &path);
        }
        egui::Grid::new("props")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                let layout_props = ["Anchors", "Align", "AutoSize"];
                // Anchor sides and border spacing have their own sections.
                let own_section =
                    |n: &str| n.starts_with("AnchorSide") || n.starts_with("BorderSpacing.");
                for (pname, value) in props.iter().filter(|(n, _)| {
                    !n.starts_with("On") && !layout_props.contains(&n.as_str()) && !own_section(n)
                }) {
                    let lbl = ui.label(pname);
                    match value {
                        PropValue::Int(i) => {
                            let mut v = *i;
                            if ui
                                .add(egui::DragValue::new(&mut v))
                                .labelled_by(lbl.id)
                                .changed()
                            {
                                // A container's size reflows its children.
                                let is_width = matches!(pname.as_str(), "Width" | "ClientWidth");
                                let is_height = matches!(pname.as_str(), "Height" | "ClientHeight");
                                if (is_width || is_height) && (is_form || is_container(&class)) {
                                    let (cw, ch) = d.client_size(&path).unwrap_or((v, v));
                                    let size = if is_width { (v, ch) } else { (cw, v) };
                                    let _ = d.set_container_size(&path, size.0, size.1);
                                } else {
                                    let _ = d.set_prop(&path, pname, PropValue::Int(v));
                                }
                            }
                        }
                        PropValue::Str(s) => {
                            let mut v = s.clone();
                            if ui
                                .add(egui::TextEdit::singleline(&mut v).desired_width(140.0))
                                .labelled_by(lbl.id)
                                .changed()
                            {
                                let _ = d.set_prop(&path, pname, PropValue::Str(v));
                            }
                        }
                        PropValue::Ident(_) if value.as_bool().is_some() => {
                            let mut v = value.as_bool().unwrap_or(false);
                            if ui.checkbox(&mut v, "").labelled_by(lbl.id).changed() {
                                let _ = d.set_prop(&path, pname, PropValue::bool(v));
                            }
                        }
                        PropValue::Ident(s) => {
                            let mut v = s.clone();
                            if ui
                                .add(egui::TextEdit::singleline(&mut v).desired_width(140.0))
                                .labelled_by(lbl.id)
                                .changed()
                            {
                                let _ = d.set_prop(&path, pname, PropValue::Ident(v));
                            }
                        }
                        other => {
                            ui.weak(format!("{other:?}").chars().take(24).collect::<String>());
                        }
                    }
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        ui.label(
            RichText::new("EVENTS")
                .size(10.5)
                .strong()
                .color(pal.text_tertiary),
        );
        let event = if is_form {
            "OnCreate"
        } else {
            default_event(&class)
        };
        for (pname, value) in props.iter().filter(|(n, _)| n.starts_with("On")) {
            ui.horizontal(|ui| {
                ui.label(pname);
                if let Some(h) = value.as_ident() {
                    ui.monospace(h);
                }
            });
        }
        if !props.iter().any(|(n, _)| n == event)
            && ui.button(format!("Create {event} handler")).clicked()
        {
            create_handler(d, unit, out);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_tray_chip_goes_before_the_first_chip_after_the_pointer() {
        let r = |x: f32, y: f32| Rect::from_min_size(egui::pos2(x, y), vec2(80.0, 24.0));
        // Two rows: chips 0, 1, 2 on the first, 3 on the second.
        let rects = [r(0.0, 0.0), r(90.0, 0.0), r(180.0, 0.0), r(0.0, 30.0)];
        let at = |x, y| egui::pos2(x, y);
        assert_eq!(tray_index(&rects, 0, at(250.0, 10.0)), 2, "after chip 2");
        assert_eq!(tray_index(&rects, 2, at(5.0, 10.0)), 0, "to the front");
        assert_eq!(
            tray_index(&rects, 0, at(100.0, 40.0)),
            3,
            "the second row, last"
        );
        assert_eq!(
            tray_index(&rects, 1, at(130.0, 10.0)),
            1,
            "dropped on itself"
        );
    }
}

#[cfg(test)]
mod width_tests {
    use super::*;

    #[test]
    fn the_inspector_leaves_the_palette_and_canvas_room() {
        assert_eq!(inspector_max(1200.0), 1200.0 - CENTRE_MIN);
        // a very narrow window: never below the inspector's own minimum
        assert_eq!(inspector_max(500.0), INSPECTOR_MIN);
        assert_eq!(inspector_max(100.0), INSPECTOR_MIN);
    }
}
