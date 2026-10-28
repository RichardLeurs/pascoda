//! The look of the IDE shell: colours modelled on Xcode's default light and dark themes, and
//! the toolbar icons, drawn with the painter because egui has no icon font.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, vec2};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Toolbar, tab strip, jump bar, status bar.
    pub chrome: Color32,
    pub sidebar: Color32,
    pub editor: Color32,
    pub separator: Color32,
    pub row_selected: Color32,
    pub row_hover: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub text_tertiary: Color32,
    pub line_number: Color32,
    pub line_number_current: Color32,
    pub current_line: Color32,
    pub exec_line: Color32,
    pub exec_label_bg: Color32,
    pub exec_label_text: Color32,
    pub accent: Color32,
    pub breakpoint: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub ok: Color32,
    // Code
    pub code_text: Color32,
    pub keyword: Color32,
    pub user_type: Color32,
    pub builtin_type: Color32,
    pub call: Color32,
    pub comment: Color32,
    pub string: Color32,
    pub number: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Palette {
    pub const LIGHT: Palette = Palette {
        dark: false,
        chrome: rgb(0xFC, 0xFC, 0xFD),
        sidebar: rgb(0xEC, 0xEA, 0xE9),
        editor: rgb(0xFF, 0xFF, 0xFF),
        separator: rgb(0xDD, 0xDD, 0xDF),
        row_selected: rgb(0xD7, 0xD6, 0xD6),
        row_hover: rgb(0xE2, 0xE1, 0xE0),
        text: rgb(0x26, 0x26, 0x26),
        text_secondary: rgb(0x6E, 0x6E, 0x73),
        text_tertiary: rgb(0xA3, 0xA3, 0xA8),
        line_number: rgb(0xA3, 0xA3, 0xA8),
        line_number_current: rgb(0x5E, 0x5E, 0x63),
        current_line: rgb(0xEF, 0xEF, 0xF1),
        exec_line: rgb(0xDD, 0xF3, 0xD6),
        exec_label_bg: rgb(0xBE, 0xDF, 0xB9),
        exec_label_text: rgb(0x3E, 0x8E, 0x3B),
        accent: rgb(0x0A, 0x7A, 0xFF),
        breakpoint: rgb(0x5B, 0x9B, 0xF8),
        error: rgb(0xD1, 0x2F, 0x1B),
        warning: rgb(0xB8, 0x7B, 0x00),
        ok: rgb(0x3E, 0x8E, 0x3B),
        code_text: rgb(0x26, 0x26, 0x26),
        keyword: rgb(0xB5, 0x37, 0x6E),
        user_type: rgb(0x3B, 0x80, 0x9E),
        builtin_type: rgb(0x9A, 0x49, 0xCC),
        call: rgb(0x3F, 0x78, 0x6E),
        comment: rgb(0x6B, 0x70, 0x83),
        string: rgb(0xC4, 0x38, 0x2A),
        number: rgb(0x27, 0x2A, 0xD8),
    };

    pub const DARK: Palette = Palette {
        dark: true,
        chrome: rgb(0x1E, 0x1E, 0x22),
        sidebar: rgb(0x26, 0x26, 0x2B),
        editor: rgb(0x1F, 0x1F, 0x24),
        separator: rgb(0x38, 0x38, 0x3D),
        row_selected: rgb(0x3A, 0x3A, 0x40),
        row_hover: rgb(0x30, 0x30, 0x35),
        text: rgb(0xE5, 0xE5, 0xE7),
        text_secondary: rgb(0x98, 0x98, 0x9F),
        text_tertiary: rgb(0x6B, 0x6B, 0x72),
        line_number: rgb(0x6B, 0x6B, 0x72),
        line_number_current: rgb(0xB0, 0xB0, 0xB6),
        current_line: rgb(0x2A, 0x2B, 0x30),
        exec_line: rgb(0x23, 0x3B, 0x28),
        exec_label_bg: rgb(0x2F, 0x5A, 0x37),
        exec_label_text: rgb(0xB6, 0xE8, 0xB0),
        accent: rgb(0x0A, 0x84, 0xFF),
        breakpoint: rgb(0x4F, 0x8D, 0xF5),
        error: rgb(0xFF, 0x6B, 0x5E),
        warning: rgb(0xFF, 0xCC, 0x4D),
        ok: rgb(0x6F, 0xD2, 0x6B),
        code_text: rgb(0xE5, 0xE5, 0xE7),
        keyword: rgb(0xFC, 0x5F, 0xA3),
        user_type: rgb(0x5D, 0xD8, 0xFF),
        builtin_type: rgb(0xD0, 0xA8, 0xFF),
        call: rgb(0x67, 0xB7, 0xA4),
        comment: rgb(0x7F, 0x8C, 0x98),
        string: rgb(0xFC, 0x6A, 0x5D),
        number: rgb(0xD0, 0xBF, 0x69),
    };

    pub fn of(ctx: &egui::Context) -> Palette {
        if ctx.global_style().visuals.dark_mode {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Play,
    Stop,
    Bug,
    Plus,
    Folder,
    Save,
    Sidebar,
    Continue,
    StepOver,
    StepInto,
    StepOut,
    Files,
    Stack,
    Filter,
    Close,
    Sparkle,
    Gear,
    Send,
    Trash,
    NewFile,
    NewFolder,
}

/// Draws `icon` inside `rect` (about 14 points square) with `color`.
pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    let s = Stroke::new(1.4, color);
    let p = |dx: f32, dy: f32| Pos2::new(c.x + dx * r, c.y + dy * r);
    let line = |a: Pos2, b: Pos2| painter.line_segment([a, b], s);
    match icon {
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![p(-0.55, -0.8), p(-0.55, 0.8), p(0.8, 0.0)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Stop => {
            painter.rect_filled(
                Rect::from_center_size(c, vec2(r * 1.2, r * 1.2)),
                2.0,
                color,
            );
        }
        Icon::Continue => {
            line(p(-0.7, -0.7), p(-0.7, 0.7));
            painter.add(Shape::convex_polygon(
                vec![p(-0.2, -0.75), p(-0.2, 0.75), p(0.85, 0.0)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Bug => {
            painter.circle_stroke(c + vec2(0.0, r * 0.15), r * 0.55, s);
            line(p(0.0, -0.4), p(0.0, 0.85));
            for dy in [-0.1, 0.3, 0.7] {
                line(p(-0.55, dy), p(-0.95, dy - 0.15));
                line(p(0.55, dy), p(0.95, dy - 0.15));
            }
            line(p(-0.2, -0.45), p(-0.45, -0.85));
            line(p(0.2, -0.45), p(0.45, -0.85));
        }
        Icon::Plus => {
            line(p(-0.8, 0.0), p(0.8, 0.0));
            line(p(0.0, -0.8), p(0.0, 0.8));
        }
        Icon::Folder => {
            let body = Rect::from_min_max(p(-0.9, -0.45), p(0.9, 0.7));
            painter.rect_stroke(body, 2.0, s, egui::StrokeKind::Inside);
            line(p(-0.9, -0.45), p(-0.9, -0.75));
            line(p(-0.9, -0.75), p(-0.2, -0.75));
            line(p(-0.2, -0.75), p(0.0, -0.45));
        }
        Icon::Save => {
            line(p(0.0, -0.8), p(0.0, 0.3));
            line(p(-0.4, -0.1), p(0.0, 0.3));
            line(p(0.4, -0.1), p(0.0, 0.3));
            line(p(-0.8, 0.3), p(-0.8, 0.8));
            line(p(-0.8, 0.8), p(0.8, 0.8));
            line(p(0.8, 0.8), p(0.8, 0.3));
        }
        Icon::Sidebar => {
            painter.rect_stroke(
                Rect::from_center_size(c, vec2(r * 2.0, r * 1.5)),
                2.5,
                s,
                egui::StrokeKind::Inside,
            );
            line(p(-0.35, -0.75), p(-0.35, 0.75));
        }
        Icon::StepOver => {
            painter.circle_filled(p(0.0, 0.55), r * 0.14, color);
            line(p(-0.8, 0.1), p(-0.45, -0.5));
            line(p(-0.45, -0.5), p(0.45, -0.5));
            line(p(0.45, -0.5), p(0.8, 0.1));
            line(p(0.8, 0.1), p(0.5, -0.05));
            line(p(0.8, 0.1), p(0.95, -0.25));
        }
        Icon::StepInto => {
            painter.circle_filled(p(0.0, 0.7), r * 0.14, color);
            line(p(0.0, -0.8), p(0.0, 0.3));
            line(p(-0.4, -0.1), p(0.0, 0.3));
            line(p(0.4, -0.1), p(0.0, 0.3));
        }
        Icon::StepOut => {
            painter.circle_filled(p(0.0, 0.7), r * 0.14, color);
            line(p(0.0, 0.3), p(0.0, -0.8));
            line(p(-0.4, -0.4), p(0.0, -0.8));
            line(p(0.4, -0.4), p(0.0, -0.8));
        }
        Icon::Files => {
            let page = Rect::from_min_max(p(-0.6, -0.85), p(0.6, 0.85));
            painter.rect_stroke(page, 2.0, s, egui::StrokeKind::Inside);
            line(p(-0.3, -0.2), p(0.3, -0.2));
            line(p(-0.3, 0.2), p(0.3, 0.2));
        }
        Icon::Stack => {
            for dy in [-0.55, 0.0, 0.55] {
                painter.rect_stroke(
                    Rect::from_center_size(p(0.0, dy), vec2(r * 1.7, r * 0.34)),
                    1.5,
                    s,
                    egui::StrokeKind::Inside,
                );
            }
        }
        Icon::Filter => {
            painter.circle_stroke(p(-0.15, -0.15), r * 0.5, s);
            line(p(0.2, 0.2), p(0.8, 0.8));
        }
        Icon::Close => {
            line(p(-0.5, -0.5), p(0.5, 0.5));
            line(p(-0.5, 0.5), p(0.5, -0.5));
        }
        Icon::Sparkle => {
            // A big four-pointed star and a small one, as in assistant buttons everywhere.
            let star = |centre: Pos2, size: f32| {
                let q = |dx: f32, dy: f32| Pos2::new(centre.x + dx * size, centre.y + dy * size);
                painter.add(Shape::convex_polygon(
                    vec![
                        q(0.0, -1.0),
                        q(0.28, -0.28),
                        q(1.0, 0.0),
                        q(0.28, 0.28),
                        q(0.0, 1.0),
                        q(-0.28, 0.28),
                        q(-1.0, 0.0),
                        q(-0.28, -0.28),
                    ],
                    color,
                    Stroke::NONE,
                ));
            };
            star(p(-0.15, 0.1), r * 0.8);
            star(p(0.68, -0.62), r * 0.32);
        }
        Icon::Gear => {
            painter.circle_stroke(c, r * 0.32, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment(
                    [c + d * r * 0.58, c + d * r * 0.88],
                    Stroke::new(2.0, color),
                );
            }
            painter.circle_stroke(c, r * 0.62, Stroke::new(1.2, color));
        }
        Icon::Send => {
            line(p(0.0, 0.7), p(0.0, -0.7));
            line(p(-0.5, -0.2), p(0.0, -0.7));
            line(p(0.5, -0.2), p(0.0, -0.7));
        }
        Icon::NewFile => {
            let page = Rect::from_min_max(p(-0.75, -0.9), p(0.35, 0.9));
            painter.rect_stroke(page, 1.5, s, egui::StrokeKind::Inside);
            line(p(0.35, 0.9), p(0.35, 0.0));
            line(p(0.85, 0.5), p(0.85, 0.9));
            line(p(0.6, 0.7), p(1.0, 0.7));
        }
        Icon::NewFolder => {
            let body = Rect::from_min_max(p(-0.9, -0.45), p(0.4, 0.8));
            painter.rect_stroke(body, 2.0, s, egui::StrokeKind::Inside);
            line(p(-0.9, -0.45), p(-0.9, -0.75));
            line(p(-0.9, -0.75), p(-0.2, -0.75));
            line(p(-0.2, -0.75), p(0.0, -0.45));
            line(p(0.7, 0.3), p(0.7, 0.9));
            line(p(0.4, 0.6), p(1.0, 0.6));
        }
        Icon::Trash => {
            line(p(-0.8, -0.5), p(0.8, -0.5));
            line(p(-0.25, -0.5), p(-0.25, -0.8));
            line(p(-0.25, -0.8), p(0.25, -0.8));
            line(p(0.25, -0.8), p(0.25, -0.5));
            line(p(-0.55, -0.5), p(-0.45, 0.85));
            line(p(0.55, -0.5), p(0.45, 0.85));
            line(p(-0.45, 0.85), p(0.45, 0.85));
        }
    }
}

/// A flat icon button (about 26x24 points). Returns the response; `selected` draws the
/// rounded highlight Xcode uses for the active mode.
pub fn icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    enabled: bool,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), egui::Sense::click());
    let resp = if enabled {
        resp
    } else {
        resp.on_hover_cursor(egui::CursorIcon::NotAllowed)
    };
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            pal.row_selected
        } else if resp.hovered() && enabled {
            pal.row_hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 6.0, fill);
        let color = if !enabled {
            pal.text_tertiary
        } else if selected && icon != Icon::Play {
            pal.text
        } else {
            pal.text_secondary
        };
        paint_icon(
            ui.painter(),
            Rect::from_center_size(rect.center(), vec2(13.0, 13.0)),
            icon,
            color,
        );
    }
    resp
}

/// The rounded group Xcode puts around related toolbar buttons.
/// A modal-looking dialog in the app's own colours (no system-grey title bar): rounded, with a
/// soft shadow. `title`, when given, is the bold first line. Centred over the window.
pub fn dialog<R>(
    ctx: &egui::Context,
    id: &str,
    title: Option<&str>,
    width: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) {
    let pal = Palette::of(ctx);
    let frame = egui::Frame::new()
        .fill(pal.chrome)
        .stroke(Stroke::new(1.0, pal.separator))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(18))
        .shadow(egui::epaint::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 140 } else { 55 }),
        });
    egui::Window::new(title.unwrap_or(id))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            if let Some(t) = title {
                ui.label(egui::RichText::new(t).size(15.0).strong().color(pal.text));
                ui.add_space(10.0);
            }
            add(ui)
        });
}

pub fn pill<R>(ui: &mut egui::Ui, pal: &Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(pal.chrome)
        .stroke(Stroke::new(1.0, pal.separator))
        .corner_radius(15)
        .inner_margin(egui::Margin::symmetric(5, 3))
        .shadow(egui::epaint::Shadow {
            offset: [0, 1],
            blur: 4,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 90 } else { 22 }),
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(add).inner
        })
        .inner
}

/// A thin horizontal rule across the full width of `ui`.
pub fn hline(ui: &mut egui::Ui, pal: &Palette) {
    let r = ui.available_rect_before_wrap();
    let y = ui.cursor().min.y;
    ui.painter()
        .hline(r.x_range(), y, Stroke::new(1.0, pal.separator));
    ui.add_space(1.0);
}

/// The blue-grey folder Xcode shows in the navigator.
pub fn paint_folder(painter: &egui::Painter, rect: Rect, pal: &Palette) {
    let col = if pal.dark {
        Color32::from_rgb(0x6C, 0x9B, 0xD6)
    } else {
        Color32::from_rgb(0x5E, 0x93, 0xD6)
    };
    let c = rect.center();
    let (w, h) = (13.0, 10.0);
    let body = Rect::from_center_size(c + vec2(0.0, 1.0), vec2(w, h));
    painter.rect_filled(body, 2.0, col);
    painter.rect_filled(
        Rect::from_min_size(body.min + vec2(0.0, -2.0), vec2(5.5, 3.0)),
        1.5,
        col,
    );
}

/// A small page with a colour that depends on the file type.
pub fn paint_file(painter: &egui::Painter, rect: Rect, ext: &str, pal: &Palette) {
    let col = match ext.to_ascii_lowercase().as_str() {
        "pas" | "pp" => Color32::from_rgb(0x3B, 0x82, 0xF6),
        "lpr" | "dpr" => Color32::from_rgb(0xF0, 0x8C, 0x1E),
        "lfm" | "dfm" => Color32::from_rgb(0x9B, 0x59, 0xD6),
        "inc" => Color32::from_rgb(0x8E, 0x8E, 0x93),
        _ => pal.text_secondary,
    };
    let page = Rect::from_center_size(rect.center(), vec2(11.0, 14.0));
    painter.rect_filled(page, 2.5, col);
    let lines = Color32::from_white_alpha(200);
    for dy in [-2.5, 0.0, 2.5] {
        painter.hline(
            page.min.x + 2.5..=page.max.x - 2.5,
            rect.center().y + dy,
            Stroke::new(1.0, lines),
        );
    }
}

/// The ▸ / ▾ in front of a folder row.
pub fn paint_disclosure(painter: &egui::Painter, center: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![
            center + vec2(-3.5, -2.0),
            center + vec2(3.5, -2.0),
            center + vec2(0.0, 2.5),
        ]
    } else {
        vec![
            center + vec2(-2.0, -3.5),
            center + vec2(-2.0, 3.5),
            center + vec2(2.5, 0.0),
        ]
    };
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A small rounded tag with one or two letters, tinted with `color` (the kind of a variable).
pub fn paint_badge(painter: &egui::Painter, rect: Rect, text: &str, color: Color32) {
    painter.rect_filled(rect, 4.0, color.gamma_multiply(0.2));
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(0.75, color.gamma_multiply(0.55)),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(9.5),
        color,
    );
}

/// A flat text segment, like the Code / Form switch in Xcode's toolbar.
pub fn segment(ui: &mut egui::Ui, pal: &Palette, label: &str, selected: bool) -> egui::Response {
    let color = if selected {
        pal.text
    } else {
        pal.text_secondary
    };
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), egui::FontId::proportional(12.5), color);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(galley.size().x + 18.0, 22.0), egui::Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let fill = if selected {
        pal.row_selected
    } else if resp.hovered() {
        pal.row_hover
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 11.0, fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// A row of real tabs: the chosen one has rounded top corners and the colour `fill` of the panel
/// under it, so the two read as one piece; the others lie flat on the (darker) strip behind them.
/// Draw it on a strip filled with `pal.sidebar`. Returns the tab that was clicked and the chosen
/// tab's rectangle.
pub fn page_tabs(
    ui: &mut egui::Ui,
    pal: &Palette,
    names: &[&str],
    selected: usize,
    fill: Color32,
) -> (Option<usize>, Option<egui::Rect>) {
    let mut clicked = None;
    let mut active_rect = None;
    for (i, name) in names.iter().enumerate() {
        let active = i == selected;
        let color = if active { pal.text } else { pal.text_secondary };
        let galley =
            ui.painter()
                .layout_no_wrap(name.to_string(), egui::FontId::proportional(12.5), color);
        let (rect, resp) =
            ui.allocate_exact_size(vec2(galley.size().x + 28.0, 28.0), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, *name));
        let top = egui::CornerRadius {
            nw: 8,
            ne: 8,
            sw: 0,
            se: 0,
        };
        if active {
            active_rect = Some(rect);
            // One pixel taller, so no seam shows between the tab and the panel below.
            ui.painter()
                .rect_filled(rect.expand2(vec2(0.0, 0.5)), top, fill);
            ui.painter().rect_stroke(
                rect,
                top,
                Stroke::new(1.0, pal.separator),
                egui::StrokeKind::Inside,
            );
            let seam = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.bottom() - 1.0),
                egui::pos2(rect.right() - 1.0, rect.bottom() + 1.0),
            );
            ui.painter().rect_filled(seam, 0.0, fill);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect.shrink2(vec2(0.0, 3.0)), 6.0, pal.row_hover);
        }
        ui.painter().galley(
            rect.center() - galley.size() / 2.0 + vec2(0.0, 0.5),
            galley,
            color,
        );
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    (clicked, active_rect)
}

/// A strip of real tabs on the `pal.sidebar` colour, with the separator line under it broken
/// where the chosen tab joins the panel below (filled `fill`). `extra` draws controls at the
/// strip's right end. Returns the clicked tab.
pub fn tab_bar(
    ui: &mut egui::Ui,
    pal: &Palette,
    names: &[&str],
    selected: usize,
    fill: Color32,
    extra: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    let mut clicked = None;
    let mut active = None;
    let strip = egui::Frame::new()
        .fill(pal.sidebar)
        .inner_margin(egui::Margin {
            left: 8,
            right: 8,
            top: 6,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                (clicked, active) = page_tabs(ui, pal, names, selected, fill);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    extra(ui);
                });
            });
        });
    let y = strip.response.rect.bottom() - 0.5;
    let (l, r) = (strip.response.rect.left(), strip.response.rect.right());
    let line = Stroke::new(1.0, pal.separator);
    match active {
        Some(a) => {
            ui.painter()
                .line_segment([egui::pos2(l, y), egui::pos2(a.left(), y)], line);
            ui.painter()
                .line_segment([egui::pos2(a.right(), y), egui::pos2(r, y)], line);
        }
        None => {
            ui.painter()
                .line_segment([egui::pos2(l, y), egui::pos2(r, y)], line);
        }
    }
    clicked
}
