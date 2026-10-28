//! The window layout, modelled on Xcode: a toolbar of grouped buttons on top, the navigator on
//! the left, the editor in the middle with a tab strip and a jump bar, the debug area and a
//! status bar below.

use super::*;
use crate::bottom::BottomTab;
use crate::chrome;
use crate::style::{self, Icon, Palette, hline, icon_button, pill};
use crate::terminal::Terminal;
use crate::terminal_view;
use egui::{Align, Frame, Layout, Margin, Rect, RichText, Sense, Vec2, vec2};
use std::collections::HashSet;

/// Xcode spaces code lines out to about 1.5 times the font size.
pub(crate) const LINE_H: f32 = 18.0;
const GUTTER_W: f32 = 46.0;
const TOOLBAR_H: f32 = 44.0;
const STATUS_H: f32 = 22.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NavTab {
    #[default]
    Files,
    Debug,
}

/// Changes asked for while drawing, applied once drawing is done (drawing borrows the app).
#[derive(Default)]
struct Pending {
    goto: Option<(PathBuf, u32, u32)>,
    open: Option<PathBuf>,
    close: Option<usize>,
    designer_outcome: Option<(PathBuf, designer::ui::Outcome)>,
    breakpoints_changed: Option<(PathBuf, Vec<u32>)>,
    close_completion: bool,
    complete_clicked: bool,
    accept: Option<usize>,
    /// Typing paused: offer completion if the text before the caret asks for it.
    auto_complete: bool,
    /// File, text and 0-based position to ask the server about.
    hover_request: Option<(PathBuf, String, u32, u32)>,
    /// Variables rows were opened or closed; children not fetched yet are `var_fetch`.
    var_changed: bool,
    var_fetch: Vec<(String, i64)>,
    /// The user's answer to an external-change banner.
    external: Option<(external::Key, external::Resolution)>,
}

impl App {
    pub(crate) fn draw(&mut self, ui: &mut egui::Ui, frame: Option<&eframe::Frame>) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let pal = Palette::of(ctx);
        self.poll_build(ctx);
        self.poll_debug(ctx);
        self.poll_lsp(ctx);
        self.poll_live_check(ctx);
        self.poll_export(ctx);
        self.handle_close_request(ctx);
        self.show_export_dialog(ctx);
        self.show_confirm(ctx);
        self.poll_assistant(ctx);
        self.poll_external(ctx);
        self.sync_lfm_tab();
        self.run_menu_actions(ctx);
        if frame.is_some() {
            chrome::align_window_buttons(TOOLBAR_H.into());
        }
        let mut p = Pending::default();
        self.handle_editor_keys(ctx, &mut p.accept);
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::S,
            ))
        }) {
            self.save_doc(self.active);
        }

        self.toolbar(ui, ctx, &pal);
        self.status_bar(ui, &pal);
        // The assistant is laid out before the bottom pane so it runs the full height, from the
        // toolbar to the status bar, and the bottom pane only spans the space left of it.
        if self.assistant.open {
            self.assistant_panel(ui, &pal);
        }
        self.debug_area(ui, &pal, &mut p);
        if self.show_navigator {
            self.navigator(ui, &pal, &mut p);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal.editor))
            .show(ui, |ui| self.center(ui, &pal, frame, &mut p));

        if p.var_changed {
            self.sync_var_open(std::mem::take(&mut p.var_fetch));
        }
        if p.complete_clicked {
            self.request_completion(ctx, false);
        }
        if p.auto_complete {
            self.request_completion(ctx, true);
        }
        if let Some((path, text, line, col)) = p.hover_request {
            self.request_hover(path, text, line, col);
        }
        if let Some((path, outcome)) = p.designer_outcome
            && let Some(handler) = outcome.open_handler
        {
            self.form_view = false;
            self.show_handler(ctx, &path, &handler);
        }
        self.selftest_step(ctx);
        if p.close_completion {
            self.completion = None;
        }
        if let Some(i) = p.accept {
            self.accept_completion(ctx, i);
        }
        if let Some((file, lines)) = p.breakpoints_changed {
            self.send(debugger::Cmd::Breakpoints(file, lines));
        }
        if let Some((key, what)) = p.external.take() {
            self.resolve_external(Some(ctx), key, what);
        }
        if let Some(path) = p.open {
            self.open_path(path);
        }
        if let Some(i) = p.close {
            self.request_close(i);
        }
        if let Some((file, line, col)) = p.goto {
            self.goto(ctx, &file, line, col);
        }
    }

    fn run_menu_actions(&mut self, ctx: &egui::Context) {
        let actions = self.menu.as_ref().map(NativeMenu::poll).unwrap_or_default();
        let can_build = !self.docs.is_empty()
            && self.building.is_none()
            && self.exporting.is_none()
            && self.session.is_none();
        for action in actions {
            match action {
                MenuAction::NewGui => self.new_gui_app(),
                MenuAction::ToggleView => {
                    if self.form_view {
                        self.form_view = false;
                    } else if self.has_form() {
                        self.show_form();
                    }
                }
                MenuAction::Appearance(a) => {
                    a.set(ctx);
                    if let Some(m) = &self.menu {
                        m.show_appearance(a);
                    }
                }
                MenuAction::Edit(cmd) => {
                    let events = cmd.events(crate::clipboard_text);
                    ctx.input_mut(|i| i.events.extend(events));
                }
                MenuAction::ToggleJumpBar => {
                    self.show_jump_bar = !self.show_jump_bar;
                    if let Some(m) = &self.menu {
                        m.jump_bar.set_checked(self.show_jump_bar);
                    }
                }
                MenuAction::ToggleAssistant => self.toggle_assistant(),
                MenuAction::Open => self.pick_file(),
                MenuAction::Save => self.save_doc(self.active),
                MenuAction::Run if can_build => self.build_and_run(ctx, false),
                MenuAction::Debug if can_build => self.build_and_run(ctx, true),
                MenuAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                MenuAction::Export if can_build && self.exporting.is_none() => self.start_export(),
                _ => {}
            }
        }
    }

    // ---- toolbar ----------------------------------------------------------------------

    /// One line saying what the IDE is doing, and its colour.
    /// A build or an export is running: the toolbar shows its progress.
    pub(crate) fn working(&self) -> bool {
        self.building.is_some() || self.exporting.is_some()
    }

    pub(crate) fn activity(&self, pal: &Palette) -> (String, egui::Color32) {
        if self.working() {
            let idle = if self.exporting.is_some() {
                "Exporting..."
            } else {
                "Building..."
            };
            let label = self
                .build_progress
                .as_ref()
                .map_or(idle, |p| p.label.as_str());
            (label.into(), pal.accent)
        } else if self.session.is_some() {
            match &self.paused {
                Some(p) => match p.location() {
                    Some((f, l)) => (
                        format!(
                            "Paused at {}:{l}",
                            f.file_name()
                                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                        ),
                        pal.ok,
                    ),
                    None => ("Paused".into(), pal.ok),
                },
                None => ("Running".into(), pal.ok),
            }
        } else if let Some(n) = Some(self.errors()).filter(|n| *n > 0) {
            (
                format!("Build failed: {n} error{}", if n == 1 { "" } else { "s" }),
                pal.error,
            )
        } else {
            ("Ready".into(), pal.text_secondary)
        }
    }

    fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| matches!(d.severity, Severity::Fatal | Severity::Error))
            .count()
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, pal: &Palette) {
        let can_build = !self.docs.is_empty()
            && self.building.is_none()
            && self.exporting.is_none()
            && self.session.is_none();
        let has_form = self.has_form();
        let form_showing = self.form_showing();
        let (status, status_color) = self.activity(pal);
        let project = self.root.file_name().map_or_else(
            || "Pascoda".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        egui::Panel::top("toolbar")
            .exact_size(TOOLBAR_H)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.chrome))
            .show(ui, |ui| {
                let bar = ui.max_rect();
                // The title bar is hidden, so empty space here drags the window. Registered
                // first, so the buttons added after it keep their clicks.
                let drag = ui.interact(bar, egui::Id::unique("titlebar"), Sense::drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal_centered(|ui| {
                    ui.add_space(78.0); // room for the traffic-light buttons
                    let hover = |r: egui::Response, tip: &str| r.on_hover_text(tip);
                    if hover(
                        icon_button(ui, pal, Icon::Sidebar, true, self.show_navigator),
                        "Show or hide the navigator",
                    )
                    .clicked()
                    {
                        self.show_navigator = !self.show_navigator;
                    }
                    ui.add_space(8.0);
                    pill(ui, pal, |ui| {
                        if hover(
                            icon_button(ui, pal, Icon::Plus, true, false),
                            "New GUI Application (Cmd+N)",
                        )
                        .clicked()
                        {
                            self.new_gui_app();
                        }
                        if hover(
                            icon_button(ui, pal, Icon::Folder, true, false),
                            "Open (Cmd+O)",
                        )
                        .clicked()
                        {
                            self.pick_file();
                        }
                        if hover(
                            icon_button(ui, pal, Icon::Save, !self.docs.is_empty(), false),
                            "Save (Cmd+S)",
                        )
                        .clicked()
                        {
                            self.save_doc(self.active);
                        }
                    });
                    ui.add_space(8.0);
                    pill(ui, pal, |ui| {
                        if hover(
                            icon_button(ui, pal, Icon::Play, can_build, false),
                            "Build and Run (Cmd+R)",
                        )
                        .clicked()
                        {
                            self.build_and_run(ctx, false);
                        }
                        if hover(
                            icon_button(ui, pal, Icon::Bug, can_build, false),
                            "Debug (Shift+Cmd+R)",
                        )
                        .clicked()
                        {
                            self.build_and_run(ctx, true);
                        }
                        if hover(
                            icon_button(ui, pal, Icon::Stop, self.session.is_some(), false),
                            "Stop",
                        )
                        .clicked()
                        {
                            self.send(debugger::Cmd::Stop);
                        }
                    });

                    // The activity viewer: project on the left, what is happening on the right.
                    let width = 380.0_f32;
                    let gap = (bar.center().x - width / 2.0 - ui.cursor().min.x).max(14.0);
                    ui.add_space(gap);
                    let (rect, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
                    let fill = if pal.dark {
                        egui::Color32::from_gray(0x2C)
                    } else {
                        egui::Color32::from_rgb(0xF1, 0xF1, 0xF3)
                    };
                    let painter = ui.painter();
                    painter.rect(
                        rect,
                        14.0,
                        fill,
                        egui::Stroke::new(1.0, pal.separator),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        rect.left_center() + vec2(14.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &project,
                        egui::FontId::proportional(13.0),
                        pal.text,
                    );
                    if self.working() {
                        // A thin bar along the bottom of the pill; it slides when the amount of
                        // work is not known.
                        let track = Rect::from_min_max(
                            egui::pos2(rect.left() + 14.0, rect.bottom() - 5.0),
                            egui::pos2(rect.right() - 14.0, rect.bottom() - 2.5),
                        );
                        painter.rect_filled(track, 1.5, pal.separator);
                        let (a, b) = match self.build_progress.as_ref().and_then(|p| p.fraction) {
                            Some(f) => (0.0, f.clamp(0.02, 1.0)),
                            None => {
                                let t = ui.input(|i| i.time) as f32;
                                let x = (t * 0.8).fract();
                                (x * 1.3 - 0.3, x * 1.3)
                            }
                        };
                        let (a, b) = (a.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
                        let fill_bar = Rect::from_min_max(
                            egui::pos2(track.left() + track.width() * a, track.top()),
                            egui::pos2(track.left() + track.width() * b, track.bottom()),
                        );
                        painter.rect_filled(fill_bar, 1.5, pal.accent);
                        ui.ctx().request_repaint();
                    }
                    painter.text(
                        rect.right_center() - vec2(14.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        &status,
                        egui::FontId::proportional(12.0),
                        status_color,
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(8.0);
                        if hover(
                            icon_button(ui, pal, Icon::Sparkle, true, self.assistant.open),
                            "Show or hide the assistant (Cmd+Opt+A)",
                        )
                        .clicked()
                        {
                            self.toggle_assistant();
                        }
                        ui.add_space(4.0);
                        if has_form {
                            pill(ui, pal, |ui| {
                                // Added right to left: Form first, so Code ends up on its left.
                                if style::segment(ui, pal, "Form", form_showing).clicked() {
                                    self.show_form();
                                }
                                if style::segment(ui, pal, "Code", !form_showing).clicked() {
                                    self.form_view = false;
                                }
                            });
                        }
                    });
                });
            });
    }

    // ---- status bar -------------------------------------------------------------------

    /// The project's language mode: shows it and offers the choice.
    fn mode_menu(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        self.sync_mode();
        let mut chosen = None;
        let current = self.mode;
        let text = RichText::new(current.label())
            .size(11.5)
            .color(pal.text_secondary);
        ui.menu_button(text, |ui| {
            for m in [pascoda_fpc::Mode::ObjFpc, pascoda_fpc::Mode::Delphi] {
                if ui.selectable_label(m == current, m.label()).clicked() {
                    chosen = Some(m);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Language mode of this project (saved in pascoda.toml)");
        if let Some(m) = chosen {
            self.set_mode(m);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let (dot, text) = match (&self.lsp, self.lsp_ready) {
            (None, _) => (pal.text_tertiary, "Language server off"),
            (Some(_), false) => (pal.warning, "Language server starting..."),
            (Some(_), true) => (pal.ok, "Language server ready"),
        };
        egui::Panel::bottom("status")
            .exact_size(STATUS_H)
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(pal.chrome)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, dot);
                    ui.label(RichText::new(text).size(11.5).color(pal.text_secondary));
                    if let Some((note, opacity)) = self.notice() {
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new(note)
                                .size(11.5)
                                .color(pal.text.gamma_multiply(opacity)),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (line, col) = self.cursor_pos;
                        ui.label(
                            RichText::new(format!("Line: {line}  Col: {col}"))
                                .size(11.5)
                                .color(pal.text_secondary),
                        );
                        ui.add_space(14.0);
                        self.mode_menu(ui, pal);
                    });
                });
            });
    }

    // ---- navigator --------------------------------------------------------------------

    fn navigator(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        egui::Panel::left("navigator")
            .resizable(true)
            .default_size(250.0)
            .min_size(170.0)
            .frame(Frame::new().fill(pal.sidebar))
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    for (tab, icon, tip) in [
                        (NavTab::Files, Icon::Files, "Project"),
                        (NavTab::Debug, Icon::Stack, "Debug"),
                    ] {
                        if icon_button(ui, pal, icon, true, self.nav_tab == tab)
                            .on_hover_text(tip)
                            .clicked()
                        {
                            self.nav_tab = tab;
                        }
                    }
                });
                ui.add_space(4.0);
                hline(ui, pal);

                egui::Panel::bottom("nav-filter")
                    .exact_size(32.0)
                    .show_separator_line(false)
                    .frame(
                        Frame::new()
                            .fill(pal.sidebar)
                            .inner_margin(Margin::symmetric(10, 4)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            style::paint_icon(ui.painter(), r, Icon::Filter, pal.text_tertiary);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.file_filter)
                                    .hint_text("Filter")
                                    .frame(Frame::NONE)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });

                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        match self.nav_tab {
                            NavTab::Files => self.file_rows(ui, pal, p),
                            NavTab::Debug => self.stack_rows(ui, pal, p),
                        }
                    });
            });
    }

    fn file_rows(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        if self.expanded.is_empty() {
            self.expanded.insert(self.root.clone());
        }
        let active = self.docs.get(self.active).map(|d| d.path.clone());
        let mut toggles = Vec::new();
        if self.file_filter.trim().is_empty() {
            let root = self.root.clone();
            tree_rows(
                ui,
                pal,
                &root,
                0,
                true,
                active.as_deref(),
                &self.expanded,
                &mut toggles,
                &mut p.open,
            );
        } else {
            let needle = self.file_filter.trim().to_lowercase();
            let mut hits = Vec::new();
            collect_files(&self.root, 0, &mut hits);
            for path in hits.into_iter().filter(|h| {
                h.file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&needle))
            }) {
                if row(
                    ui,
                    pal,
                    0,
                    false,
                    false,
                    &path,
                    active.as_deref() == Some(path.as_path()),
                )
                .clicked()
                {
                    p.open = Some(path);
                }
            }
        }
        for t in toggles {
            if !self.expanded.remove(&t) {
                self.expanded.insert(t);
            }
        }
    }

    fn stack_rows(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        let Some(paused) = &self.paused else {
            ui.add_space(14.0);
            let text = if self.session.is_some() {
                "Running..."
            } else {
                "No debug session"
            };
            ui.vertical_centered(|ui| ui.label(RichText::new(text).color(pal.text_tertiary)));
            return;
        };
        for (i, f) in paused.source_frames().enumerate() {
            let Some(file) = &f.file else { continue };
            let name = file
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
            let painter = ui.painter();
            let inner = rect.shrink2(vec2(6.0, 0.0));
            if i == 0 {
                painter.rect_filled(inner, 5.0, pal.row_selected);
            } else if resp.hovered() {
                painter.rect_filled(inner, 5.0, pal.row_hover);
            }
            painter.circle_filled(rect.left_center() + vec2(18.0, 0.0), 5.0, pal.accent);
            painter.text(
                rect.left_center() + vec2(32.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("{i} {}", f.name),
                egui::FontId::proportional(13.0),
                pal.text,
            );
            painter.text(
                rect.right_center() - vec2(12.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                format!("{name}:{}", f.line),
                egui::FontId::proportional(11.0),
                pal.text_secondary,
            );
            if resp.clicked() {
                p.goto = Some((file.clone(), f.line, 1));
            }
        }
    }

    // ---- centre: tabs, jump bar, editor or designer ----------------------------------

    fn center(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        frame: Option<&eframe::Frame>,
        p: &mut Pending,
    ) {
        self.tab_strip(ui, pal, p);
        if self.show_jump_bar {
            self.jump_bar(ui, pal);
        }
        for (key, banner, name) in self.active_banners() {
            if let Some(answer) = external::banner_row(ui, pal, &key, &name, &banner) {
                p.external = Some((key, answer));
            }
        }
        if self.form_view
            && let Some(doc) = self.docs.get_mut(self.active)
            && let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == doc.path)
        {
            let outcome = designer::ui::show(
                form,
                &mut self.designer_ui,
                &mut self.native,
                ui,
                frame,
                &mut doc.text,
            );
            p.designer_outcome = Some((doc.path.clone(), outcome));
            if (self.export_dialog.is_some() || self.confirm.is_some())
                && let Some(n) = &self.native
            {
                // egui windows draw under the native controls.
                n.hide();
            }
            return;
        }
        if let Some(n) = &self.native {
            n.hide();
        }
        self.code_editor(ui, pal, p);
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        // Real tabs: the active one has the editor's colour and rounded top corners and is
        // joined to the editor below; the others lie flat on the darker strip.
        let mut active_rect: Option<Rect> = None;
        let strip = Frame::new()
            .fill(pal.sidebar)
            .inner_margin(Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut switched = None;
                    for (i, d) in self.docs.iter().enumerate() {
                        let active = i == self.active;
                        let dirty = d.dirty()
                            || self
                                .designers
                                .iter()
                                .any(|f| f.pas_path == d.path && f.dirty());
                        let name = d
                            .path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        let galley = ui.painter().layout_no_wrap(
                            name,
                            egui::FontId::proportional(12.5),
                            if active { pal.text } else { pal.text_secondary },
                        );
                        let width = galley.size().x + 38.0;
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
                        let hovered = resp.hovered();
                        if active {
                            active_rect = Some(rect);
                            let top = egui::CornerRadius {
                                nw: 8,
                                ne: 8,
                                sw: 0,
                                se: 0,
                            };
                            ui.painter().rect_filled(rect, top, pal.editor);
                            ui.painter().rect_stroke(
                                rect,
                                top,
                                egui::Stroke::new(1.0, pal.separator),
                                egui::StrokeKind::Inside,
                            );
                            // Hide the stroke's bottom edge so the tab runs into the editor.
                            let seam = Rect::from_min_max(
                                egui::pos2(rect.left() + 1.0, rect.bottom() - 1.0),
                                egui::pos2(rect.right() - 1.0, rect.bottom() + 1.0),
                            );
                            ui.painter().rect_filled(seam, 0.0, pal.editor);
                        } else if hovered {
                            ui.painter().rect_filled(
                                rect.shrink2(vec2(0.0, 3.0)),
                                6.0,
                                pal.row_hover,
                            );
                        }
                        let text_pos = rect.left_center() + vec2(10.0, -galley.size().y / 2.0);
                        ui.painter().galley(text_pos, galley.clone(), pal.text);
                        let close = egui::Rect::from_center_size(
                            rect.right_center() - vec2(13.0, 0.0),
                            vec2(16.0, 16.0),
                        );
                        let over_close = hovered
                            && ui
                                .input(|i| i.pointer.hover_pos())
                                .is_some_and(|h| close.contains(h));
                        if dirty {
                            // Unsaved changes: a red dot, which becomes a red x under the pointer.
                            if over_close {
                                ui.painter().rect_filled(close, 4.0, pal.row_hover);
                                style::paint_icon(
                                    ui.painter(),
                                    close.shrink(4.5),
                                    Icon::Close,
                                    pal.error,
                                );
                            } else {
                                ui.painter().circle_filled(close.center(), 4.0, pal.error);
                            }
                        } else if hovered || active {
                            if over_close {
                                ui.painter().rect_filled(close, 4.0, pal.row_hover);
                            }
                            style::paint_icon(
                                ui.painter(),
                                close.shrink(4.5),
                                Icon::Close,
                                pal.text_secondary,
                            );
                        }
                        if resp.clicked() {
                            let clicked_close = ui
                                .input(|i| i.pointer.interact_pos())
                                .is_some_and(|h| close.contains(h));
                            if clicked_close {
                                p.close = Some(i);
                            } else {
                                switched = Some(i);
                            }
                        }
                    }
                    if let Some(i) = switched {
                        self.active = i;
                        // The designer only shows for a unit; a plain tab or the .lfm text is code.
                        if !self.active_is_unit_with_form() {
                            self.form_view = false;
                        }
                    }
                });
            });
        // The separator under the strip, interrupted where the active tab joins the editor.
        let y = strip.response.rect.bottom() - 0.5;
        let (l, r) = (strip.response.rect.left(), strip.response.rect.right());
        let line = egui::Stroke::new(1.0, pal.separator);
        match active_rect {
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
    }

    fn jump_bar(&self, ui: &mut egui::Ui, pal: &Palette) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let mut crumbs: Vec<String> = vec![
            self.root
                .file_name()
                .map_or_else(|| "Project".into(), |n| n.to_string_lossy().into_owned()),
        ];
        if let Ok(rel) = doc.path.strip_prefix(&self.root) {
            crumbs.extend(
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned()),
            );
        } else if let Some(n) = doc.path.file_name() {
            crumbs.push(n.to_string_lossy().into_owned());
        }
        Frame::new()
            .fill(pal.editor)
            .inner_margin(Margin::symmetric(14, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 7.0;
                    let last = crumbs.len() - 1;
                    for (i, c) in crumbs.iter().enumerate() {
                        if i > 0 {
                            ui.label(
                                RichText::new("\u{203a}")
                                    .size(12.0)
                                    .color(pal.text_tertiary),
                            );
                        }
                        let color = if i == last {
                            pal.text
                        } else {
                            pal.text_secondary
                        };
                        ui.label(RichText::new(c).size(12.0).color(color));
                    }
                });
            });
        hline(ui, pal);
    }

    fn code_editor(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        let current = self
            .paused
            .as_ref()
            .and_then(|s| s.location())
            .map(|(f, l)| (f.to_path_buf(), l));
        let ctx = ui.ctx().clone();
        let Some(doc) = self.docs.get_mut(self.active) else {
            ui.add_space(ui.available_height() / 3.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No Editor")
                        .size(22.0)
                        .color(pal.text_tertiary),
                );
                ui.label(
                    RichText::new("Open a file, or choose File > New GUI Application")
                        .color(pal.text_tertiary),
                );
            });
            return;
        };
        let font = editor_font();
        self.marks
            .retain(|m| m.path != doc.path || m.text == doc.text);
        let marks = self
            .marks
            .iter()
            .find(|m| m.path == doc.path)
            .map_or(&[][..], |m| m.items.as_slice());
        let mut flagged = std::collections::BTreeMap::new();
        for m in marks {
            *flagged.entry(m.line + 1).or_insert(false) |= m.is_error();
        }
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let lines = doc.text.split('\n').count();
                let exec_line = current
                    .as_ref()
                    .filter(|(f, _)| *f == doc.path)
                    .map(|(_, l)| *l);
                let caret = egui::TextEdit::load_state(&ctx, doc.editor_id)
                    .and_then(|s| s.cursor.char_range())
                    .map(|r| r.primary.index.0);
                let caret_line =
                    caret.map(|i| doc.text.chars().take(i).filter(|c| *c == '\n').count() + 1);

                // Row backgrounds go down first, then the gutter and the text on top.
                let top = ui.cursor().min.y;
                let left = ui.cursor().min.x;
                let right = ui.clip_rect().right().max(left + 400.0);
                let band = |line: u32| {
                    egui::Rect::from_min_max(
                        egui::pos2(left, top + (line - 1) as f32 * LINE_H),
                        egui::pos2(right, top + line as f32 * LINE_H),
                    )
                };
                if let Some(l) = caret_line {
                    ui.painter()
                        .rect_filled(band(l as u32), 0.0, pal.current_line);
                }
                if let Some(l) = exec_line {
                    ui.painter().rect_filled(band(l), 0.0, pal.exec_line);
                    let label = egui::Rect::from_center_size(
                        egui::pos2(right - 90.0, band(l).center().y),
                        vec2(130.0, 15.0),
                    );
                    ui.painter().rect_filled(label, 4.0, pal.exec_label_bg);
                    ui.painter().text(
                        label.center(),
                        egui::Align2::CENTER_CENTER,
                        "Thread 1: paused",
                        egui::FontId::proportional(10.5),
                        pal.exec_label_text,
                    );
                }

                if gutter(
                    ui,
                    pal,
                    lines,
                    &mut doc.breakpoints,
                    exec_line,
                    caret_line.map(|l| l as u32),
                    &flagged,
                ) {
                    p.breakpoints_changed =
                        Some((doc.path.clone(), doc.breakpoints.iter().copied().collect()));
                }
                let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                    let pal = Palette::of(ui.ctx());
                    let mut job = highlight::layout(text.as_str(), &pal, editor_font(), LINE_H);
                    job.wrap.max_width = wrap_width;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let rows = (ui.available_height() / LINE_H).floor().max(1.0) as usize;
                let len_before = doc.text.len();
                let output = egui::TextEdit::multiline(&mut doc.text)
                    .id(doc.editor_id)
                    .font(font.clone())
                    .frame(Frame::NONE)
                    .margin(Margin::ZERO)
                    .desired_width(f32::INFINITY)
                    .desired_rows(rows)
                    .layouter(&mut layouter)
                    .show(ui);
                // VoiceOver reads this name; without it the editor is announced as unnamed.
                output.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Code editor")
                });
                if let Some(r) = output.cursor_range {
                    let idx = r.primary.index.0;
                    let before: String = doc.text.chars().take(idx).collect();
                    let line = before.matches('\n').count() + 1;
                    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
                    self.cursor_pos = (line, col);
                }
                // Language features: squiggles, hover, completion while typing.
                let origin = output.galley_pos.to_vec2();
                for m in marks {
                    let Some(r) = hints::range_rect(&output.galley, &m.range(&doc.text)) else {
                        continue;
                    };
                    let color = if m.is_error() { pal.error } else { pal.warning };
                    let y = r.max.y + origin.y - 1.0;
                    let points = hints::squiggle_points(r.min.x + origin.x, r.max.x + origin.x, y);
                    ui.painter()
                        .add(egui::Shape::line(points, egui::Stroke::new(1.0, color)));
                }
                if output.response.changed() {
                    self.hover.clear();
                    if doc.text.len() > len_before {
                        self.typed += 1;
                    }
                }
                let now = ui.input(|i| i.time);
                let typed = (self.typed > 0).then_some(self.typed);
                match self.typing.update(typed, now, hints::COMPLETE_DELAY) {
                    hints::Poll::Ready => p.auto_complete = true,
                    hints::Poll::Wait(s) => ctx.request_repaint_after_secs(s as f32),
                    hints::Poll::Idle => {}
                }
                let resting = output.response.hovered()
                    && self.completion.is_none()
                    && ctx.input(|i| !i.pointer.any_down() && i.smooth_scroll_delta == Vec2::ZERO);
                let target = ctx.pointer_hover_pos().filter(|_| resting).and_then(|pt| {
                    hints::target_at(&output.galley, &doc.text, pt - output.galley_pos, marks)
                });
                self.hover.set_target(target.clone());
                let key = target
                    .as_ref()
                    .map(|t| (t.range.start, t.range.end, t.mark));
                match self.hover.dwell.update(key, now, hints::HOVER_DELAY) {
                    hints::Poll::Ready => {
                        let t = target.expect("ready implies a target");
                        let rect =
                            hints::range_rect(&output.galley, &t.range).unwrap_or(Rect::ZERO);
                        let pos = output.galley_pos + rect.left_bottom().to_vec2() + vec2(0.0, 3.0);
                        if let Some(m) = t.mark.map(|i| &marks[i]) {
                            let tag = if m.is_error() { "Error" } else { "Warning" };
                            self.hover.popup = Some(hints::Popup {
                                text: format!("{tag}: {}", m.message),
                                severity: Some(m.severity),
                                pos,
                            });
                        } else {
                            let (line, col) = line_col(&doc.text, t.range.start);
                            self.hover_pos = pos;
                            p.hover_request = Some((doc.path.clone(), doc.text.clone(), line, col));
                        }
                    }
                    hints::Poll::Wait(s) => ctx.request_repaint_after_secs(s as f32),
                    hints::Poll::Idle => {}
                }
                if let Some(popup) = &self.hover.popup {
                    hints::show_popup(&ctx, popup, pal.error, pal.warning);
                }
                if let Some(c) = &self.completion {
                    if c.text != doc.text {
                        p.close_completion = true;
                    } else {
                        let at = output.galley.pos_from_cursor(CCursor::new(c.cursor));
                        let pos = output.galley_pos + at.left_bottom().to_vec2();
                        if let Some(i) = completion_popup(ui.ctx(), pos, c) {
                            p.accept = Some(i);
                        }
                    }
                }
            });
        });
    }

    // ---- bottom pane: Problems, Output, Debug, Terminal -------------------------------

    fn debug_area(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        egui::Panel::bottom("debug-area")
            .resizable(true)
            .default_size(self.debug_height)
            .min_size(70.0)
            .frame(Frame::new().fill(pal.editor))
            .show(ui, |ui| {
                self.bottom_tabs(ui, pal);
                match self.bottom.tab {
                    BottomTab::Problems => {
                        self.bottom.terminal_focused = false;
                        self.bottom.view_shown = false;
                        self.problems_tab(ui, pal, p);
                    }
                    BottomTab::Debug => {
                        self.bottom.terminal_focused = false;
                        self.bottom.view_shown = false;
                        self.debug_tab(ui, pal, p);
                    }
                    BottomTab::Output => {
                        self.bottom.terminal_focused = false;
                        self.bottom.view_shown = false;
                        self.output_tab(ui, pal);
                    }
                    BottomTab::Terminal => self.terminal_tab(ui, pal),
                }
            });
    }

    fn bottom_tabs(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let problems = if self.diagnostics.is_empty() {
            "Problems".to_string()
        } else {
            format!("Problems ({})", self.diagnostics.len())
        };
        let names = [problems.as_str(), "Output", "Debug", "Terminal"];
        let tab = self.bottom.tab;
        let exited = self.bottom.terminal.as_ref().is_some_and(|t| t.exited());
        let (mut clear, mut restart, mut clear_terminal) = (false, false, false);
        let clicked = style::tab_bar(ui, pal, &names, tab.index(), pal.editor, |ui| match tab {
            BottomTab::Output => {
                clear = text_button(ui, pal, "Clear").clicked();
            }
            BottomTab::Terminal => {
                if exited {
                    restart = text_button(ui, pal, "Restart").clicked();
                } else {
                    clear_terminal = text_button(ui, pal, "Clear terminal").clicked();
                }
            }
            _ => {}
        });
        if let Some(i) = clicked {
            let now = ui.input(|i| i.time);
            self.bottom.choose(BottomTab::ALL[i], now);
        }
        if clear {
            self.console.clear();
        }
        if clear_terminal && let Some(t) = self.bottom.terminal.as_ref() {
            terminal_view::clear(t);
        }
        if restart {
            // Dropping the old terminal hangs up its shell.
            self.bottom.terminal = None;
            self.bottom.terminal_error = None;
            self.bottom.view_shown = false;
        }
    }

    fn problems_tab(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        Frame::new()
            .fill(pal.editor)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| problem_rows(ui, pal, &self.diagnostics, p));
            });
    }

    fn debug_tab(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        if self.session.is_none() {
            Frame::new()
                .fill(pal.editor)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.label(
                        RichText::new("Not debugging. Press Debug to start.")
                            .size(12.5)
                            .color(pal.text_tertiary),
                    );
                });
            return;
        }
        self.debug_bar(ui, pal);
        hline(ui, pal);
        Frame::new()
            .fill(pal.editor)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| match &self.paused {
                        Some(paused) => {
                            let source = self.docs.get(self.active).map_or("", |d| d.text.as_str());
                            variable_rows(ui, pal, paused, source, &mut self.var_open, p);
                        }
                        None => {
                            ui.label(
                                RichText::new("The program is running.")
                                    .size(12.5)
                                    .color(pal.text_tertiary),
                            );
                        }
                    });
            });
    }

    fn output_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        Frame::new()
            .fill(pal.editor)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&self.console)
                                    .monospace()
                                    .size(12.0)
                                    .color(pal.text),
                            )
                            .selectable(true),
                        );
                    });
            });
    }

    fn terminal_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let just_shown = !self.bottom.view_shown;
        self.bottom.view_shown = true;
        // The project folder changed (another file or project was opened): a shell nobody has
        // typed in yet simply starts again in the new folder; one in use is left alone and
        // offers a button instead (see below), so a running program is never disturbed.
        if self
            .bottom
            .terminal_root
            .as_deref()
            .is_some_and(|r| r != self.root)
            && self
                .bottom
                .terminal
                .as_ref()
                .is_some_and(|t| t.exited() || !t.typed())
        {
            self.bottom.terminal = None;
            self.bottom.terminal_error = None;
        }
        if self.bottom.terminal.is_none() && self.bottom.terminal_error.is_none() {
            // Started the first time the tab is shown, at the size the pane has now.
            let (rows, cols) = terminal_view::fit(ui);
            let ctx = ui.ctx().clone();
            match Terminal::spawn_shell(&self.root, rows, cols, move || ctx.request_repaint()) {
                Ok(t) => {
                    self.bottom.terminal = Some(t);
                    self.bottom.terminal_root = Some(self.root.clone());
                }
                Err(e) => {
                    self.bottom.terminal_error = Some(format!("could not start a shell: {e}"))
                }
            }
        }
        // A shell in use, started in another folder: say so, and let the user move it.
        if let (Some(started_in), Some(term)) = (
            self.bottom.terminal_root.clone(),
            self.bottom.terminal.as_ref(),
        ) && started_in != self.root
            && !term.exited()
        {
            let mut go = false;
            Frame::new()
                .fill(pal.chrome)
                .inner_margin(Margin::symmetric(10, 3))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "The project folder is now {}.",
                                self.root.display()
                            ))
                            .size(12.0)
                            .color(pal.text_secondary),
                        );
                        go = text_button(ui, pal, "cd to project folder").clicked();
                    });
                });
            if go {
                // Only on this click: the shell is idle at a prompt as far as the user is
                // concerned, and the command is visible in the terminal.
                let quoted = self.root.display().to_string().replace('\'', "'\\''");
                term.write(format!("cd '{quoted}'\n").as_bytes());
                self.bottom.terminal_root = Some(self.root.clone());
            }
        }
        if let Some(err) = &self.bottom.terminal_error {
            ui.label(RichText::new(err).size(12.5).color(pal.error));
            self.bottom.terminal_focused = false;
            return;
        }
        let Some(term) = self.bottom.terminal.as_mut() else {
            return;
        };
        self.bottom.terminal_focused =
            terminal_view::show(ui, pal, term, &mut self.bottom.view, just_shown);
    }

    fn debug_bar(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        Frame::new()
            .fill(pal.chrome)
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let paused = self.paused.is_some();
                    if icon_button(ui, pal, Icon::Continue, paused, false)
                        .on_hover_text("Continue")
                        .clicked()
                    {
                        self.resume_with(debugger::Cmd::Resume);
                    }
                    for (icon, tip, kind) in [
                        (Icon::StepOver, "Step Over", pascoda_dap::Step::Over),
                        (Icon::StepInto, "Step Into", pascoda_dap::Step::Into),
                        (Icon::StepOut, "Step Out", pascoda_dap::Step::Out),
                    ] {
                        if icon_button(ui, pal, icon, paused, false)
                            .on_hover_text(tip)
                            .clicked()
                        {
                            self.resume_with(debugger::Cmd::Step(kind));
                        }
                    }
                    ui.add_space(8.0);
                    let text = match self.paused.as_ref().and_then(|s| s.source_frames().next()) {
                        Some(f) => format!("Thread 1 \u{203a} {}", f.name),
                        None => "Running...".into(),
                    };
                    ui.label(RichText::new(text).size(12.0).color(pal.text_secondary));
                });
            });
    }
}

/// A flat text button for the tab strip's right end.
fn text_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(pal.text_secondary))
            .frame(false)
            .min_size(vec2(0.0, 22.0)),
    )
}

/// One navigator row: disclosure triangle (folders), icon and name. Returns the response.
fn row(
    ui: &mut egui::Ui,
    pal: &Palette,
    depth: usize,
    is_dir: bool,
    open: bool,
    path: &Path,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let painter = ui.painter();
    let inner = rect.shrink2(vec2(6.0, 0.0));
    if selected {
        painter.rect_filled(inner, 5.0, pal.row_selected);
    } else if resp.hovered() {
        painter.rect_filled(inner, 5.0, pal.row_hover);
    }
    let x = rect.left() + 12.0 + depth as f32 * 14.0;
    if is_dir {
        style::paint_disclosure(
            painter,
            egui::pos2(x + 4.0, rect.center().y),
            open,
            pal.text_tertiary,
        );
    }
    let icon_rect =
        egui::Rect::from_center_size(egui::pos2(x + 20.0, rect.center().y), vec2(16.0, 16.0));
    if is_dir {
        style::paint_folder(painter, icon_rect, pal);
    } else {
        style::paint_file(
            painter,
            icon_rect,
            path.extension().and_then(|e| e.to_str()).unwrap_or(""),
            pal,
        );
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    painter.text(
        egui::pos2(x + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        pal.text,
    );
    resp
}

const TREE_EXTENSIONS: &[&str] = &["pas", "pp", "lpr", "dpr", "inc", "lfm"];
const HIDDEN_DIRS: &[&str] = &["target", "vendor", "lib"];

pub(crate) fn listing(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            !name.starts_with('.')
                && if p.is_dir() {
                    !HIDDEN_DIRS.contains(&name.as_str()) && !name.ends_with(".app")
                } else {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| TREE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                }
        })
        .collect();
    entries.sort_by_key(|p| {
        (
            !p.is_dir(),
            p.file_name().map(|n| n.to_string_lossy().to_lowercase()),
        )
    });
    entries
}

fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for p in listing(dir) {
        if p.is_dir() {
            if depth < 6 {
                collect_files(&p, depth + 1, out);
            }
        } else {
            out.push(p);
        }
    }
}

/// The folder `dir` and, when expanded, everything below it.
#[allow(clippy::too_many_arguments)]
fn tree_rows(
    ui: &mut egui::Ui,
    pal: &Palette,
    dir: &Path,
    depth: usize,
    is_root: bool,
    active: Option<&Path>,
    expanded: &HashSet<PathBuf>,
    toggles: &mut Vec<PathBuf>,
    open: &mut Option<PathBuf>,
) {
    let is_open = expanded.contains(dir);
    let resp = row(ui, pal, depth, true, is_open, dir, false);
    if is_root {
        // Xcode shows the project itself at the top; its name is the folder's.
    }
    if resp.clicked() {
        toggles.push(dir.to_path_buf());
    }
    if !is_open || depth > 8 {
        return;
    }
    for p in listing(dir) {
        if p.is_dir() {
            tree_rows(
                ui,
                pal,
                &p,
                depth + 1,
                false,
                active,
                expanded,
                toggles,
                open,
            );
        } else if row(
            ui,
            pal,
            depth + 1,
            false,
            false,
            &p,
            active == Some(p.as_path()),
        )
        .clicked()
        {
            *open = Some(p);
        }
    }
}

/// Badge letters and colour for a kind of value.
fn kind_badge(kind: debugger::Kind, pal: &Palette) -> (&'static str, egui::Color32) {
    use debugger::Kind::*;
    match kind {
        Integer => ("I", pal.number),
        Float => ("F", pal.number),
        Boolean => ("B", pal.keyword),
        Char => ("Ch", pal.string),
        String => ("S", pal.string),
        Enum => ("E", pal.call),
        Set => ("Se", pal.call),
        Record => ("R", pal.user_type),
        Class => ("C", pal.user_type),
        Array => ("A", pal.builtin_type),
        Pointer => ("P", pal.text_secondary),
        Other => ("?", pal.text_tertiary),
    }
}

const VAR_ROW: f32 = 20.0;
const VAR_INDENT: f32 = 14.0;

fn variable_rows(
    ui: &mut egui::Ui,
    pal: &Palette,
    paused: &debugger::Paused,
    source: &str,
    open: &mut HashSet<String>,
    p: &mut Pending,
) {
    ui.spacing_mut().item_spacing.y = 0.0;
    for (scope, vars) in &paused.vars {
        if vars.is_empty() {
            continue;
        }
        ui.label(
            RichText::new(scope)
                .size(11.0)
                .strong()
                .color(pal.text_tertiary),
        );
        for v in vars {
            var_row(
                ui,
                pal,
                v,
                &format!("{scope}{}{}", debugger::PATH_SEP, v.name),
                0,
                source,
                open,
                p,
            );
        }
        ui.add_space(4.0);
    }
}

/// One Xcode-style row: disclosure triangle, kind badge, name, muted type, value; then its
/// children, indented, while it is open.
#[allow(clippy::too_many_arguments)]
fn var_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    v: &debugger::Var,
    path: &str,
    depth: usize,
    source: &str,
    open: &mut HashSet<String>,
    p: &mut Pending,
) {
    let name = debugger::respell(&v.name, source);
    let type_name = debugger::respell(&v.type_name, source);
    let value = if v.kind == debugger::Kind::Enum {
        debugger::respell(&v.value, source)
    } else {
        v.value.clone()
    };
    let expandable = v.reference > 0;
    let is_open = expandable && open.contains(path);

    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), VAR_ROW),
        if expandable {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            true,
            format!("{name}: {type_name} = {value}"),
        )
    });
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 4.0, pal.row_hover);
    }
    let x = rect.left() + depth as f32 * VAR_INDENT;
    let cy = rect.center().y;
    if expandable {
        style::paint_disclosure(painter, egui::pos2(x + 7.0, cy), is_open, pal.text_tertiary);
    }
    let (letters, color) = kind_badge(v.kind, pal);
    let badge = Rect::from_center_size(egui::pos2(x + 28.0, cy), vec2(20.0, 14.0));
    style::paint_badge(painter, badge, letters, color);

    // Name, type and value share the width; each is clipped to its column.
    let name_x = x + 44.0;
    let type_x = (rect.left() + rect.width() * 0.40).max(name_x + 60.0);
    let value_x = (rect.left() + rect.width() * 0.62).max(type_x + 60.0);
    let cell = |from: f32, to: f32, text: &str, size: f32, color: egui::Color32| {
        let clip = Rect::from_min_max(
            egui::pos2(from, rect.top()),
            egui::pos2(to - 6.0, rect.bottom()),
        );
        painter.with_clip_rect(clip).text(
            egui::pos2(from, cy),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(size),
            color,
        );
    };
    cell(name_x, type_x, &name, 12.5, pal.text);
    cell(type_x, value_x, &type_name, 12.0, pal.text_secondary);
    cell(value_x, rect.right(), &value, 12.5, pal.text);

    if expandable && resp.clicked() {
        if is_open {
            open.remove(path);
        } else {
            open.insert(path.to_string());
            if v.children.is_none() {
                p.var_fetch.push((path.to_string(), v.reference));
            }
        }
        p.var_changed = true;
    }
    if is_open {
        for kid in v.children.iter().flatten() {
            let kid_path = format!("{path}{}{}", debugger::PATH_SEP, kid.name);
            var_row(ui, pal, kid, &kid_path, depth + 1, source, open, p);
        }
    }
}

fn problem_rows(ui: &mut egui::Ui, pal: &Palette, diagnostics: &[Diagnostic], p: &mut Pending) {
    if diagnostics.is_empty() {
        ui.label(
            RichText::new("No problems")
                .size(12.5)
                .color(pal.text_tertiary),
        );
        return;
    }
    for d in diagnostics {
        let (color, tag) = match d.severity {
            Severity::Fatal | Severity::Error => (pal.error, "Error"),
            Severity::Warning => (pal.warning, "Warning"),
            _ => (pal.text_tertiary, "Note"),
        };
        let name = d
            .file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let resp = ui
            .horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, color);
                ui.label(
                    RichText::new(format!("{name}:{}:{}", d.line, d.col))
                        .size(12.5)
                        .color(pal.text_secondary),
                );
                ui.label(
                    RichText::new(format!("{tag}: {}", d.message))
                        .size(12.5)
                        .color(pal.text),
                );
            })
            .response
            .interact(Sense::click());
        if resp.clicked() {
            p.goto = Some((d.file.clone(), d.line, d.col));
        }
    }
}

/// Line numbers and breakpoints. Breakpoints are Xcode's blue arrow-shaped chips; click a
/// number to toggle one. Returns true when a click changed them.
fn gutter(
    ui: &mut egui::Ui,
    pal: &Palette,
    lines: usize,
    breakpoints: &mut BTreeSet<u32>,
    exec_line: Option<u32>,
    caret_line: Option<u32>,
    flagged: &std::collections::BTreeMap<u32, bool>,
) -> bool {
    let size = vec2(GUTTER_W, LINE_H * lines as f32);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter_at(rect);
    let clip = ui.clip_rect();
    let first = ((clip.top() - rect.top()) / LINE_H).floor().max(0.0) as usize;
    let last = (((clip.bottom() - rect.top()) / LINE_H).ceil().max(0.0) as usize).min(lines);
    for i in first..last {
        let line = i as u32 + 1;
        let y = rect.top() + i as f32 * LINE_H;
        let row = egui::Rect::from_min_size(egui::pos2(rect.left(), y), vec2(rect.width(), LINE_H));
        let font = egui::FontId::monospace(11.0);
        if breakpoints.contains(&line) {
            let (l, r, t, b) = (
                row.left() + 5.0,
                row.right() - 3.0,
                row.top() + 2.0,
                row.bottom() - 2.0,
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(l, t),
                    egui::pos2(r - 7.0, t),
                    egui::pos2(r, row.center().y),
                    egui::pos2(r - 7.0, b),
                    egui::pos2(l, b),
                ],
                pal.breakpoint,
                egui::Stroke::NONE,
            ));
            painter.text(
                egui::pos2(r - 11.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                line.to_string(),
                font,
                egui::Color32::WHITE,
            );
        } else {
            let color = if caret_line == Some(line) || exec_line == Some(line) {
                pal.line_number_current
            } else {
                pal.line_number
            };
            painter.text(
                egui::pos2(row.right() - 12.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                line.to_string(),
                font,
                color,
            );
        }
    }
    for (&line, &error) in flagged {
        if line as usize <= lines {
            let y = rect.top() + (line - 1) as f32 * LINE_H + LINE_H / 2.0;
            let color = if error { pal.error } else { pal.warning };
            painter.circle_filled(egui::pos2(rect.right() - 5.0, y), 3.0, color);
        }
    }
    let Some(pos) = response
        .interact_pointer_pos()
        .filter(|_| response.clicked())
    else {
        return false;
    };
    let line = ((pos.y - rect.top()) / LINE_H) as u32 + 1;
    if line as usize > lines {
        return false;
    }
    if !breakpoints.remove(&line) {
        breakpoints.insert(line);
    }
    true
}
