//! Draws a `Terminal` as a grid of monospace cells and turns egui input into bytes for it.

use crate::style::Palette;
use crate::terminal::{self, Cell, Terminal};
use eframe::egui::{self, Color32, FontId, Rect, Sense, Stroke, vec2};

/// Row height of the grid; the same as the code editor's lines.
const CELL_H: f32 = crate::shell::LINE_H;
const PAD: egui::Vec2 = vec2(10.0, 4.0);
/// Width of the scroll bar on the right edge.
const BAR_W: f32 = 10.0;

#[derive(Default)]
pub(crate) struct ViewState {
    /// Selected cells, in the rows and columns currently on screen.
    selection: Option<(Cell, Cell)>,
    anchor: Option<Cell>,
    wheel_carry: f32,
    /// Output generation and scroll offset the selection belongs to.
    sel_gen: u64,
    sel_scroll: usize,
    /// The "[process exited]" line has been written into the screen.
    exit_noted: bool,
}

impl ViewState {
    pub fn selected_text(&self, term: &Terminal) -> Option<String> {
        let (a, b) = self.selection?;
        let text = terminal::selection_text(term.parser().screen(), a, b);
        (!text.is_empty()).then_some(text)
    }
}

/// The 16 ANSI colours: standard then bright.
fn ansi16(i: u8, dark: bool) -> Color32 {
    // Xcode-console-like tones, readable on white and on the dark editor.
    const LIGHT: [[u8; 3]; 16] = [
        [0x26, 0x26, 0x26],
        [0xC4, 0x1A, 0x16],
        [0x1E, 0x7A, 0x1E],
        [0x8A, 0x6A, 0x00],
        [0x1C, 0x4F, 0xC6],
        [0xA9, 0x20, 0x8F],
        [0x0E, 0x7C, 0x86],
        [0xBF, 0xBF, 0xC4],
        [0x6E, 0x6E, 0x73],
        [0xE0, 0x3A, 0x36],
        [0x2E, 0xA0, 0x43],
        [0xB0, 0x88, 0x00],
        [0x3A, 0x6E, 0xE8],
        [0xC8, 0x3F, 0xB0],
        [0x1F, 0xA3, 0xAE],
        [0x8E, 0x8E, 0x93],
    ];
    const DARK: [[u8; 3]; 16] = [
        [0x3F, 0x3F, 0x46],
        [0xFF, 0x5A, 0x52],
        [0x5C, 0xD8, 0x6B],
        [0xE5, 0xC0, 0x4A],
        [0x5A, 0x9B, 0xFF],
        [0xE0, 0x6C, 0xD8],
        [0x4F, 0xD2, 0xDE],
        [0xC9, 0xC9, 0xCE],
        [0x7F, 0x7F, 0x86],
        [0xFF, 0x80, 0x7A],
        [0x7E, 0xEB, 0x8B],
        [0xF5, 0xD8, 0x6E],
        [0x82, 0xB4, 0xFF],
        [0xF0, 0x93, 0xEA],
        [0x7C, 0xE6, 0xF0],
        [0xFF, 0xFF, 0xFF],
    ];
    let [r, g, b] = if dark { DARK } else { LIGHT }[usize::from(i & 15)];
    Color32::from_rgb(r, g, b)
}

/// An xterm 256-colour index: 0..16 ANSI, 16..232 the 6x6x6 cube, 232.. grey ramp.
pub(crate) fn indexed(i: u8, dark: bool) -> Color32 {
    match i {
        0..=15 => ansi16(i, dark),
        16..=231 => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            Color32::from_rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            Color32::from_rgb(v, v, v)
        }
    }
}

/// The colour a cell's foreground or background has, `None` for the terminal's default.
fn resolve(c: vt100::Color, dark: bool) -> Option<Color32> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(indexed(i, dark)),
        vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
    }
}

/// Foreground and background of one cell after bold, dim and inverse. `None` background is the
/// pane's own colour.
fn cell_colors(cell: &vt100::Cell, pal: &Palette) -> (Color32, Option<Color32>) {
    let dark = pal.dark;
    let mut fg = resolve(cell.fgcolor(), dark);
    let bg = resolve(cell.bgcolor(), dark);
    if cell.bold()
        && let vt100::Color::Idx(i @ 0..=7) = cell.fgcolor()
    {
        // Bold text in a standard colour is drawn in its bright variant (no bold face here).
        fg = Some(ansi16(i + 8, dark));
    }
    let (mut fg, mut bg) = if cell.inverse() {
        (bg.unwrap_or(pal.editor), Some(fg.unwrap_or(pal.text)))
    } else {
        (fg.unwrap_or(pal.text), bg)
    };
    if cell.dim() {
        fg = fg.gamma_multiply(0.6);
    }
    if bg == Some(pal.editor) {
        bg = None;
    }
    (fg, bg)
}

fn pointer_cell(pos: egui::Pos2, origin: egui::Pos2, cell_w: f32, rows: u16, cols: u16) -> Cell {
    let col = ((pos.x - origin.x) / cell_w).floor().max(0.0) as u32;
    let row = ((pos.y - origin.y) / CELL_H).floor().max(0.0) as u32;
    (
        row.min(u32::from(rows.saturating_sub(1))) as u16,
        col.min(u32::from(cols.saturating_sub(1))) as u16,
    )
}

/// Cell width of the terminal font.
fn cell_width(ui: &egui::Ui) -> f32 {
    ui.painter()
        .layout_no_wrap(
            "M".into(),
            FontId::monospace(crate::EDITOR_FONT_SIZE),
            Color32::WHITE,
        )
        .size()
        .x
}

/// Rows and columns that fit in the space `ui` has left.
pub(crate) fn fit(ui: &egui::Ui) -> (u16, u16) {
    let grid = ui.available_size() - PAD * 2.0 - vec2(BAR_W, 0.0);
    terminal::grid_for(grid.x, grid.y, cell_width(ui), CELL_H)
}

/// Draws the terminal in the free space of `ui` and handles its input. `focus` asks for the
/// keyboard focus (when the tab was just chosen). Returns whether the terminal has the focus.
pub(crate) fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    term: &mut Terminal,
    st: &mut ViewState,
    focus: bool,
) -> bool {
    let font = FontId::monospace(crate::EDITOR_FONT_SIZE);
    let cell_w = cell_width(ui);
    let avail = ui.available_size();
    let (area, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Terminal, true, "Terminal"));
    ui.painter().rect_filled(area, 0.0, pal.editor);
    let grid = egui::Rect::from_min_max(area.min + PAD, area.max - vec2(PAD.x + BAR_W, PAD.y));
    let origin = grid.min;

    // The grid follows the pane.
    let (rows, cols) = terminal::grid_for(grid.width(), grid.height(), cell_w, CELL_H);
    term.resize(rows, cols);

    if focus || resp.clicked() || resp.drag_started() {
        resp.request_focus();
    }
    let focused = resp.has_focus();
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
    }

    note_exit(term, st);
    handle_mouse(ui, &resp, term, st, origin, cell_w, rows, cols);
    if focused {
        handle_keys(ui, term, st);
    }

    // Anything that changed the screen invalidates a selection.
    let (generation, scroll) = (term.generation(), term.parser().screen().scrollback());
    if generation != st.sel_gen || scroll != st.sel_scroll {
        st.selection = None;
        st.sel_gen = generation;
        st.sel_scroll = scroll;
    }

    paint(ui, pal, term, st, &font, origin, cell_w, focused, area);
    scroll_bar(ui, pal, term, area, rows, resp.id);
    focused
}

/// The history's scroll bar on the right edge: a thumb that shows where the view is and can be
/// dragged or clicked. Not drawn while there is no history.
fn scroll_bar(
    ui: &mut egui::Ui,
    pal: &Palette,
    term: &Terminal,
    area: egui::Rect,
    rows: u16,
    id: egui::Id,
) {
    let (max, offset) = {
        let mut p = term.parser();
        let now = p.screen().scrollback();
        p.screen_mut().set_scrollback(usize::MAX);
        let max = p.screen().scrollback();
        p.screen_mut().set_scrollback(now);
        (max, now)
    };
    if max == 0 || term.parser().screen().alternate_screen() {
        return;
    }
    let track = egui::Rect::from_min_max(
        egui::pos2(area.right() - BAR_W, area.top() + 2.0),
        egui::pos2(area.right() - 2.0, area.bottom() - 2.0),
    );
    let resp = ui.interact(track, id.with("scrollbar"), Sense::click_and_drag());
    if let Some(pos) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        let next = terminal::offset_for(pos.y - track.top(), max, rows, track.height());
        term.parser().screen_mut().set_scrollback(next);
    }
    let (top, height) = terminal::thumb(max, offset, rows, track.height());
    let thumb = egui::Rect::from_min_size(
        egui::pos2(track.left() + 1.0, track.top() + top),
        vec2(track.width() - 2.0, height),
    );
    let strong = resp.hovered() || resp.dragged();
    ui.painter().rect_filled(
        track,
        4.0,
        pal.separator
            .gamma_multiply(if strong { 0.6 } else { 0.25 }),
    );
    ui.painter().rect_filled(
        thumb,
        4.0,
        if strong {
            pal.text_secondary
        } else {
            pal.text_tertiary
        },
    );
}

/// Writes "[process exited]" into the screen once the shell is gone.
fn note_exit(term: &Terminal, st: &mut ViewState) {
    if term.exited() && !st.exit_noted {
        st.exit_noted = true;
        term.parser()
            .process(b"\r\n\x1b[2m[process exited]\x1b[0m\r\n");
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_mouse(
    ui: &egui::Ui,
    resp: &egui::Response,
    term: &Terminal,
    st: &mut ViewState,
    origin: egui::Pos2,
    cell_w: f32,
    rows: u16,
    cols: u16,
) {
    if let Some(pos) = resp.interact_pointer_pos() {
        let cell = pointer_cell(pos, origin, cell_w, rows, cols);
        if resp.drag_started() {
            st.anchor = Some(cell);
            st.selection = None;
        } else if resp.dragged()
            && let Some(a) = st.anchor
        {
            st.selection = Some((a, cell));
        }
        if resp.clicked() {
            st.selection = None;
        }
    }
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        let lines = terminal::wheel_lines(&mut st.wheel_carry, dy, CELL_H);
        if lines != 0 {
            let mut p = term.parser();
            if p.screen().alternate_screen() {
                // Full-screen programs (less, man) have no history here: wheel = arrow keys.
                let app = p.screen().application_cursor();
                drop(p);
                let key = if lines > 0 {
                    egui::Key::ArrowUp
                } else {
                    egui::Key::ArrowDown
                };
                if let Some(bytes) = terminal::encode_key(key, egui::Modifiers::NONE, app) {
                    for _ in 0..lines.unsigned_abs().min(10) {
                        term.write(&bytes);
                    }
                }
            } else {
                let now = p.screen().scrollback() as i64;
                let next = (now + i64::from(lines)).max(0) as usize;
                p.screen_mut().set_scrollback(next);
            }
        }
    } else {
        st.wheel_carry = 0.0;
    }
}

/// Sends the keyboard events to the shell and removes them from the frame's input, so nothing
/// else (the editor, the designer) reacts to them. Cmd shortcuts are left alone.
fn handle_keys(ui: &egui::Ui, term: &Terminal, st: &mut ViewState) {
    let events = ui.input(|i| i.events.clone());
    let (app_cursor, bracketed) = {
        let p = term.parser();
        (
            p.screen().application_cursor(),
            p.screen().bracketed_paste(),
        )
    };
    let mut typed = false;
    for event in &events {
        match event {
            egui::Event::Text(text) => {
                term.write(text.as_bytes());
                typed = true;
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if modifiers.command && *key == egui::Key::K {
                    clear(term);
                } else if let Some(bytes) = terminal::encode_key(*key, *modifiers, app_cursor) {
                    term.write(&bytes);
                    typed = true;
                }
            }
            egui::Event::Paste(text) => {
                term.write(&terminal::paste_bytes(text, bracketed));
                typed = true;
            }
            egui::Event::Copy | egui::Event::Cut => {
                if let Some(text) = st.selected_text(term) {
                    ui.ctx().copy_text(text);
                }
            }
            _ => {}
        }
    }
    if typed {
        // Typing returns to the live screen.
        term.parser().screen_mut().set_scrollback(0);
    }
    ui.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut => {
                false
            }
            egui::Event::Key { modifiers, .. } => modifiers.command || modifiers.mac_cmd,
            _ => true,
        });
    });
}

/// Cmd+K: wipes the screen and the history, then asks the shell to redraw its prompt (Ctrl+L).
pub(crate) fn clear(term: &Terminal) {
    {
        let mut p = term.parser();
        p.process(b"\x1b[2J\x1b[3J\x1b[H");
        p.screen_mut().clear_scrollback();
    }
    term.write_quiet(&[0x0c]);
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &egui::Ui,
    pal: &Palette,
    term: &Terminal,
    st: &ViewState,
    font: &FontId,
    origin: egui::Pos2,
    cell_w: f32,
    focused: bool,
    area: Rect,
) {
    let painter = ui.painter_at(area);
    let parser = term.parser();
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    let selected = |row: u16, col: u16| {
        st.selection.is_some_and(|(a, b)| {
            let (s, e) = if a <= b { (a, b) } else { (b, a) };
            (row, col) >= s && (row, col) <= e
        })
    };

    for row in 0..rows {
        let y = origin.y + f32::from(row) * CELL_H;
        // Runs of cells with the same colours become one rectangle and one piece of text.
        let mut run = String::new();
        let mut run_start = 0u16;
        let mut run_style: Option<(Color32, Option<Color32>, bool)> = None;
        let flush = |run: &mut String,
                     start: u16,
                     end: u16,
                     style: Option<(Color32, Option<Color32>, bool)>| {
            let Some((fg, bg, underline)) = style else {
                run.clear();
                return;
            };
            let x = origin.x + f32::from(start) * cell_w;
            let w = f32::from(end - start) * cell_w;
            let r = Rect::from_min_size(egui::pos2(x, y), vec2(w, CELL_H));
            if let Some(bg) = bg {
                painter.rect_filled(r, 0.0, bg);
            }
            if !run.trim().is_empty() {
                let galley = painter.layout_no_wrap(std::mem::take(run), font.clone(), fg);
                let pos = egui::pos2(x, y + (CELL_H - galley.size().y) / 2.0);
                painter.galley(pos, galley, fg);
            }
            if underline {
                painter.hline(r.x_range(), y + CELL_H - 2.5, Stroke::new(1.0, fg));
            }
            run.clear();
        };
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let (fg, bg) = cell_colors(cell, pal);
            let style = Some((fg, bg, cell.underline()));
            if style != run_style {
                flush(&mut run, run_start, col, run_style);
                run_start = col;
                run_style = style;
            }
            let text = cell.contents();
            run.push_str(if text.is_empty() { " " } else { text });
            if cell.is_wide() {
                // A wide glyph spans two columns: keep the text aligned with a following space.
                run.push(' ');
            }
        }
        flush(&mut run, run_start, cols, run_style);

        // Selection highlight over the row.
        if st.selection.is_some() {
            let mut c = 0;
            while c < cols {
                if selected(row, c) {
                    let start = c;
                    while c < cols && selected(row, c) {
                        c += 1;
                    }
                    let r = Rect::from_min_max(
                        egui::pos2(origin.x + f32::from(start) * cell_w, y),
                        egui::pos2(origin.x + f32::from(c) * cell_w, y + CELL_H),
                    );
                    painter.rect_filled(r, 0.0, pal.accent.gamma_multiply(0.35));
                } else {
                    c += 1;
                }
            }
        }
    }

    if !screen.hide_cursor() && screen.scrollback() == 0 {
        let (row, col) = screen.cursor_position();
        let r = Rect::from_min_size(
            egui::pos2(
                origin.x + f32::from(col) * cell_w,
                origin.y + f32::from(row) * CELL_H,
            ),
            vec2(cell_w, CELL_H),
        );
        if focused {
            painter.rect_filled(r, 0.0, pal.text.gamma_multiply(0.55));
        } else {
            painter.rect_stroke(
                r.shrink(0.5),
                0.0,
                Stroke::new(1.0, pal.text_secondary),
                egui::StrokeKind::Inside,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_256_colour_cube_and_ramp() {
        assert_eq!(indexed(16, false), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(231, false), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(196, false), Color32::from_rgb(255, 0, 0));
        assert_eq!(indexed(21, false), Color32::from_rgb(0, 0, 255));
        assert_eq!(indexed(232, false), Color32::from_rgb(8, 8, 8));
        assert_eq!(indexed(255, false), Color32::from_rgb(238, 238, 238));
        // The ANSI part depends on the theme.
        assert_ne!(indexed(1, false), indexed(1, true));
    }

    #[test]
    fn pointer_maps_to_a_clamped_cell() {
        let o = egui::pos2(10.0, 4.0);
        assert_eq!(pointer_cell(egui::pos2(10.0, 4.0), o, 8.0, 10, 100), (0, 0));
        assert_eq!(
            pointer_cell(egui::pos2(27.0, 23.0), o, 8.0, 10, 100),
            (1, 2)
        );
        assert_eq!(
            pointer_cell(egui::pos2(-50.0, -50.0), o, 8.0, 10, 100),
            (0, 0)
        );
        assert_eq!(
            pointer_cell(egui::pos2(5000.0, 5000.0), o, 8.0, 10, 100),
            (9, 99)
        );
    }

    #[test]
    fn colours_follow_inverse_bold_and_truecolor() {
        let mut p = vt100::Parser::new(
            std::num::NonZeroU16::new(2).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        p.process(b"\x1b[1;31mA\x1b[0m\x1b[7mB\x1b[0m\x1b[38;2;1;2;3mC\x1b[0m\x1b[48;5;21mD");
        let pal = Palette::LIGHT;
        let (fg, bg) = cell_colors(p.screen().cell(0, 0).unwrap(), &pal);
        assert_eq!(fg, ansi16(9, false), "bold red is bright red");
        assert_eq!(bg, None);
        let (fg, bg) = cell_colors(p.screen().cell(0, 1).unwrap(), &pal);
        assert_eq!((fg, bg), (pal.editor, Some(pal.text)), "inverse swaps");
        let (fg, _) = cell_colors(p.screen().cell(0, 2).unwrap(), &pal);
        assert_eq!(fg, Color32::from_rgb(1, 2, 3));
        let (_, bg) = cell_colors(p.screen().cell(0, 3).unwrap(), &pal);
        assert_eq!(bg, Some(Color32::from_rgb(0, 0, 255)));
    }
}
