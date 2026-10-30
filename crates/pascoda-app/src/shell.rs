//! The window layout, modelled on Xcode: a toolbar of grouped buttons on top, the navigator on
//! the left, the editor in the middle with a tab strip and a jump bar, the debug area and a
//! status bar below.

use super::*;
use crate::bottom::BottomTab;
use crate::chrome;
use crate::editor_lines::LineMap;
use crate::style::{self, Icon, Palette, hline, icon_button, pill};
use crate::terminal::Terminal;
use crate::terminal_view;
use egui::{Align, Frame, Layout, Margin, Rect, RichText, Sense, Vec2, vec2};
use std::collections::HashSet;

/// Xcode spaces code lines out to about 1.5 times the font size.
const GUTTER_W: f32 = 46.0;
const TOOLBAR_H: f32 = 44.0;
const STATUS_H: f32 = 22.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NavTab {
    #[default]
    Files,
    Debug,
    Outline,
    /// Project-wide find and replace.
    Search,
    /// Every breakpoint of the open files, enabled or not.
    Breakpoints,
}

/// The declarations of the active file, as the language server last read them. `shown` is the file
/// and the text (by key) they were read from; `pending` is the request on its way.
#[derive(Default)]
pub(crate) struct Outline {
    pub shown: Option<(PathBuf, u64)>,
    pub pending: Option<(PathBuf, u64)>,
    pub symbols: Vec<pascoda_lsp::Symbol>,
    /// When the last request was sent (egui time), to wait for the typing to stop.
    pub asked_at: f64,
}

/// The signature of the call at the caret, as the server gave it for the text with `key`.
#[derive(Clone)]
pub(crate) struct BpEdit {
    pub path: PathBuf,
    pub line: u32,
    pub condition: String,
    pub hits: String,
    pub error: String,
}

/// A variable whose value is being changed: its lldb name (`POINT.X`), its Pascal name, its kind, and the
/// text typed for the new value, with the reason the last try was refused.
#[derive(Clone)]
pub(crate) struct ValueEdit {
    pub target: String,
    pub name: String,
    pub kind: debugger::Kind,
    pub text: String,
    pub error: String,
}

/// The assignment text lldb needs for a value typed for a variable of `kind`, or why it is not one.
pub(crate) fn assignment_text(kind: debugger::Kind, typed: &str) -> Result<String, String> {
    let typed = typed.trim();
    match kind {
        debugger::Kind::Integer => typed
            .parse::<i64>()
            .map(|n| n.to_string())
            .map_err(|_| "A whole number, such as 42".to_string()),
        debugger::Kind::Float => typed
            .parse::<f64>()
            .map(|x| format!("{x:?}"))
            .map_err(|_| "A number, such as 2.5".to_string()),
        debugger::Kind::Boolean => match typed.to_ascii_lowercase().as_str() {
            "true" => Ok("1".to_string()),
            "false" => Ok("0".to_string()),
            _ => Err("true or false".to_string()),
        },
        debugger::Kind::Char => {
            let mut chars = typed.chars();
            match (chars.next(), chars.next()) {
                (Some('\''), _) => Ok("'\\''".to_string()),
                (Some(c), None) => Ok(format!("'{c}'")),
                _ => Err("One character".to_string()),
            }
        }
        _ => Err("Only numbers, booleans and characters can be changed here".to_string()),
    }
}

pub(crate) struct SigHint {
    pub key: u64,
    pub at: usize,
    pub sig: pascoda_lsp::Signature,
}

/// How long the text must stay unchanged before the outline asks the server again.
const OUTLINE_QUIET: f64 = 0.6;

/// A key for a text: two equal texts have equal keys.
pub(crate) fn text_key(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// What a declaration is, from its LSP symbol kind.
pub(crate) fn kind_word(kind: u32) -> &'static str {
    match kind {
        2 => "unit",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        22 => "member",
        23 => "record",
        _ => "symbol",
    }
}

/// Changes asked for while drawing, applied once drawing is done (drawing borrows the app).
#[derive(Default)]
struct Pending {
    goto: Option<(PathBuf, u32, u32)>,
    open: Option<PathBuf>,
    close: Option<usize>,
    designer_outcome: Option<(PathBuf, designer::ui::Outcome)>,
    breakpoints_changed: Option<(PathBuf, Vec<pascoda_dap::Breakpoint>)>,
    /// A secondary click in the gutter: the file and line whose breakpoint is edited.
    edit_breakpoint: Option<(PathBuf, u32)>,
    /// A variable's Change Value menu item was chosen.
    value_edit_request: Option<ValueEdit>,
    close_completion: bool,
    complete_clicked: bool,
    accept: Option<usize>,
    /// Typing paused: offer completion if the text before the caret asks for it.
    auto_complete: bool,
    /// File, text and 0-based position to ask the server about.
    hover_request: Option<(PathBuf, String, u32, u32)>,
    /// Cmd-click on an identifier: char index to look up the definition of.
    definition_at: Option<usize>,
    /// A `(` or `,` was typed: file, text, 0-based line and column, caret char index, and the text's key.
    signature_request: Option<(PathBuf, String, u32, u32, usize, u64)>,
    /// Variables rows were opened or closed; children not fetched yet are `var_fetch`.
    var_changed: bool,
    var_fetch: Vec<(String, i64)>,
    /// The user's answer to an external-change banner.
    external: Option<(external::Key, external::Resolution)>,
    /// A problem's "Add a folder" button was pressed for this unit (the folder is asked for next).
    search_path_unit: Option<String>,
}

impl App {
    pub(crate) fn draw(&mut self, ui: &mut egui::Ui, frame: Option<&eframe::Frame>) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let pal = Palette::of(ctx);
        self.poll_build(ctx);
        self.poll_debug(ctx);
        self.poll_lsp(ctx);
        self.refresh_custom_classes();
        self.poll_live_check(ctx);
        self.poll_export(ctx);
        self.handle_close_request(ctx);
        self.show_export_dialog(ctx);
        self.show_confirm(ctx);
        self.show_vendor_dialog(ctx);
        self.show_go_to_line(ctx);
        self.show_rename(ctx);
        self.show_preferences(ctx);
        self.show_project_options(ctx);
        self.show_import_packages(ctx);
        self.show_breakpoint_edit(ctx);
        self.show_value_edit(ctx);
        self.poll_assistant(ctx);
        self.poll_external(ctx);
        self.poll_unit_tests();
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
        if let Some(edit) = p.value_edit_request.take() {
            self.value_edit = Some(edit);
        }
        if let Some((path, line)) = p.edit_breakpoint.take() {
            let (condition, hits) = self
                .docs
                .iter()
                .find(|d| d.path == path)
                .and_then(|d| d.stops.get(&line))
                .map(|s| (s.condition.clone(), s.hits.clone()))
                .unwrap_or_default();
            self.bp_edit = Some(BpEdit { path, line, condition, hits, error: String::new() });
        }
        if let Some((path, text, line, col, at, key)) = p.signature_request {
            self.request_signature(path, text, line, col, at, key);
        }
        if let Some((path, text, line, col)) = p.hover_request {
            self.request_hover(path, text, line, col);
        }
        if let Some(index) = p.definition_at {
            self.request_definition_at(index);
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
            self.remember(|r| r.note_file(&path));
            self.select_in_tree(path);
            // The switch to the form happens after this frame's panels were drawn; without a
            // repaint the window keeps showing the old tab until the next input event.
            ctx.request_repaint();
        }
        if let Some(i) = p.close {
            self.request_close(i);
        }
        if let Some(unit) = p.search_path_unit.take() {
            self.choose_search_path(&unit);
        }
        if let Some((file, line, col)) = p.goto {
            self.goto(ctx, &file, line, col);
        }
    }

    fn run_menu_actions(&mut self, ctx: &egui::Context) {
        crate::system_menus::hide_edit_items();
        let actions = self.menu.as_ref().map(NativeMenu::poll).unwrap_or_default();
        self.run_menu_list(ctx, actions);
    }

    /// Runs the menu actions the user picked (or a test's).
    pub(crate) fn run_menu_list(&mut self, ctx: &egui::Context, actions: Vec<MenuAction>) {
        let can_build = !self.docs.is_empty()
            && self.building.is_none()
            && self.exporting.is_none()
            && self.session.is_none();
        for action in actions {
            match action {
                MenuAction::NewGui => self.new_gui_app(),
                MenuAction::NewConsole => self.new_project(pascoda_project::Template::Console),
                MenuAction::NewLibrary => self.new_project(pascoda_project::Template::Library),
                MenuAction::NewPackage => self.new_project(pascoda_project::Template::Package),
                MenuAction::Preferences => self.preferences_open = true,
                MenuAction::RunToCursor => self.run_to_cursor(ctx),
                MenuAction::OpenRecent(i) => self.open_recent(i),
                MenuAction::Clean if self.building.is_none() => self.clean_project(),
                MenuAction::BuildPackage if can_build => match self.active_package() {
                    Some(package) => self.build_package(ctx, package),
                    None => self.console = "No package here: open a .lpk or .dpk, or a unit of one\n".into(),
                },
                MenuAction::Rebuild if can_build => {
                    self.clean_project();
                    self.start_build(ctx, false, false);
                }
                MenuAction::ClearRecent => self.remember(|r| r.clear()),
                MenuAction::ProjectOptions => {
                    // The default is what the project file says, so the window can show it under the
                    // output field (the key itself is shown there).
                    let target = if self.build_source.as_os_str().is_empty() {
                        "Not built yet: the program is built to the folder of its source".to_string()
                    } else {
                        pascoda_project::foreign::executable_of(&self.build_source)
                            .display()
                            .to_string()
                    };
                    self.project_options = project_options::Editor::load(&self.root, target);
                }
                MenuAction::NewForm => {
                    let dir = self.root.clone();
                    self.new_form(&dir);
                }
                MenuAction::NewDataModule => {
                    let dir = self.root.clone();
                    self.new_unit(&dir, true);
                }
                MenuAction::NewFile | MenuAction::NewFolder => {
                    // The same as the file tree's buttons: the new name is typed in the tree.
                    self.nav_tab = NavTab::Files;
                    let dir = self.tree_target_dir();
                    let is_dir = matches!(action, MenuAction::NewFolder);
                    self.apply_tree_actions(vec![TreeAction::NewIn { dir, is_dir }], &mut Pending::default());
                }
                MenuAction::RenameItem => {
                    self.nav_tab = NavTab::Files;
                    match self.tree_sel.clone().filter(|s| *s != self.root) {
                        Some(path) => {
                            self.apply_tree_actions(vec![TreeAction::AskRename(path)], &mut Pending::default());
                        }
                        None => self.console.push_str("Select a file or folder in the file tree first\n"),
                    }
                }
                MenuAction::ToggleView => {
                    if self.form_view {
                        self.show_code();
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
                MenuAction::RunTests => self.start_unit_tests(ctx, None),
                MenuAction::ImportPackages => self.import_packages_open = true,
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
        self.building.is_some() || self.exporting.is_some() || self.unit_tests.busy()
    }

    pub(crate) fn activity(&self, pal: &Palette) -> (String, egui::Color32) {
        if self.working() {
            if self.building.is_none() && self.exporting.is_none() {
                // Only the tests run: their progress is shown where a build's is.
                return (self.tests_progress(pal).0, pal.accent);
            }
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
        } else if let Some(summary) = self.tests_summary(pal) {
            summary
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
                        let fraction = if self.building.is_none() && self.exporting.is_none() {
                            self.tests_progress(pal).1
                        } else {
                            self.build_progress.as_ref().and_then(|p| p.fraction)
                        };
                        let (a, b) = match fraction {
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
                                    self.show_code();
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
        let mut actions = Vec::new();
        if self.nav_tab == NavTab::Outline {
            self.refresh_outline(ui.ctx());
        }
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
                        (NavTab::Outline, Icon::Outline, "Outline"),
                        (NavTab::Breakpoints, Icon::Breakpoint, "Breakpoints"),
                        (NavTab::Search, Icon::Search, "Search the project"),
                    ] {
                        if icon_button(ui, pal, icon, true, self.nav_tab == tab)
                            .on_hover_text(tip)
                            .clicked()
                        {
                            self.nav_tab = tab;
                        }
                    }
                });
                if self.nav_tab == NavTab::Files {
                    // The file actions get a row of their own: the tabs and the actions together are
                    // wider than the sidebar. Right to left: the first button added is the rightmost.
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let sel = self.tree_sel.clone();
                            let deletable = sel.clone().filter(|s| *s != self.root);
                            if icon_button(ui, pal, Icon::Trash, deletable.is_some(), false)
                                .on_hover_text("Move to Trash")
                                .clicked()
                                && let Some(s) = deletable
                            {
                                actions.push(TreeAction::AskDelete(s));
                            }
                            let target = self.tree_target_dir();
                            if icon_button(ui, pal, Icon::NewFolder, true, false)
                                .on_hover_text("New Folder")
                                .clicked()
                            {
                                actions.push(TreeAction::NewIn { dir: target.clone(), is_dir: true });
                            }
                            if icon_button(ui, pal, Icon::NewFile, true, false)
                                .on_hover_text("New File")
                                .clicked()
                            {
                                actions.push(TreeAction::NewIn { dir: target.clone(), is_dir: false });
                            }
                            if icon_button(ui, pal, Icon::NewForm, true, false)
                                .on_hover_text("New Form")
                                .clicked()
                            {
                                actions.push(TreeAction::NewForm { dir: target });
                            }
                        });
                    });
                }
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

                if let Some(path) = self.tree_delete.clone() {
                    self.delete_prompt(ui, pal, &path, &mut actions);
                }

                if self.nav_tab == NavTab::Search {
                    // The search has its own scrolling results list.
                    self.search_tab(ui, pal);
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            match self.nav_tab {
                                NavTab::Files => actions.extend(self.file_rows(ui, pal, p)),
                                NavTab::Debug => self.stack_rows(ui, pal, p),
                                NavTab::Outline => self.outline_rows(ui, pal, p),
                                NavTab::Search => {}
                                NavTab::Breakpoints => self.breakpoint_rows(ui, pal, p),
                            }
                        });
                }
            });
        self.apply_tree_actions(actions, p);
    }

    /// Asks the server for the declarations of the active file once its text has been still for a
    /// moment. Nothing is asked again while the answer for this text is shown or on its way.
    pub(crate) fn refresh_outline(&mut self, ctx: &egui::Context) {
        let Some(doc) = self.docs.get(self.active) else { return };
        let want = (doc.path.clone(), text_key(&doc.text));
        if self.outline.shown.as_ref() == Some(&want) || self.outline.pending.as_ref() == Some(&want) {
            return;
        }
        let Some(lsp) = &self.lsp else { return };
        let now = ctx.input(|i| i.time);
        let wait = OUTLINE_QUIET - (now - self.outline.asked_at);
        if wait > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            return;
        }
        let _ = lsp.requests.send(lsp::Req::Symbols {
            path: want.0.clone(),
            text: doc.text.clone(),
            key: want.1,
        });
        self.outline.pending = Some(want);
        self.outline.asked_at = now;
    }

    /// The declarations of the active file, indented by depth. A click goes to the declaration.
    fn outline_rows(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        let Some(doc) = self.docs.get(self.active) else {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Open a file").color(pal.text_tertiary)));
            return;
        };
        if self.lsp.is_none() {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("The language server is not running").color(pal.text_tertiary))
            });
            return;
        }
        if self.outline.shown.as_ref().map(|(path, _)| path) != Some(&doc.path) {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Reading the declarations...").color(pal.text_tertiary)));
            return;
        }
        if self.outline.symbols.is_empty() {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("No declarations").color(pal.text_tertiary)));
            return;
        }
        for s in &self.outline.symbols {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
            let painter = ui.painter();
            let inner = rect.shrink2(vec2(6.0, 0.0));
            if resp.hovered() {
                painter.rect_filled(inner, 5.0, pal.row_hover);
            }
            let x = rect.left() + 12.0 + s.depth as f32 * 14.0;
            painter.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &s.name,
                egui::FontId::proportional(13.0),
                pal.text,
            );
            painter.text(
                egui::pos2(rect.right() - 12.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                kind_word(s.kind),
                egui::FontId::proportional(11.0),
                pal.text_tertiary,
            );
            if resp.clicked() {
                p.goto = Some((doc.path.clone(), s.line + 1, s.col + 1));
            }
        }
    }

    /// The "Delete ...?" strip above the file list. Unsaved edits in the item are named.
    fn delete_prompt(&mut self, ui: &mut egui::Ui, pal: &Palette, path: &Path, actions: &mut Vec<TreeAction>) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let inside = files::count_items(path).saturating_sub(1);
        let dirty = self.docs.iter().any(|d| d.path.starts_with(path) && d.dirty());
        let mut text = format!("Move “{name}” to the Trash?");
        if inside > 0 {
            text += &format!(" It holds {inside} more {}.", if inside == 1 { "item" } else { "items" });
        }
        if dirty {
            text += " Unsaved changes will be lost.";
        }
        Frame::new()
            .fill(pal.row_selected)
            .inner_margin(Margin::symmetric(8, 6))
            .show(ui, |ui| {
                // As wide as the navigator, not as wide as its text.
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(text).size(12.0).color(pal.text)).wrap());
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("Move to Trash").color(pal.error)))
                        .clicked()
                    {
                        actions.push(TreeAction::ConfirmDelete(path.to_path_buf()));
                    }
                    if ui.button("Cancel").clicked() {
                        self.tree_delete = None;
                    }
                });
            });
    }

    fn file_rows(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) -> Vec<TreeAction> {
        if self.expanded.is_empty() {
            self.expanded.insert(self.root.clone());
        }
        let active = self.docs.get(self.active).map(|d| d.path.clone());
        let mut actions = Vec::new();
        if self.file_filter.trim().is_empty() {
            let root = self.root.clone();
            let mut view = TreeView {
                active: active.as_deref(),
                sel: self.tree_sel.as_deref(),
                expanded: &self.expanded,
                new: &mut self.tree_new,
                rename: &mut self.tree_rename,
                toggles: Vec::new(),
                actions: Vec::new(),
                open: None,
            };
            tree_rows(ui, pal, &root, 0, true, &mut view);
            let TreeView { toggles, open, actions: drawn, .. } = view;
            actions.extend(drawn);
            if open.is_some() {
                p.open = open;
            }
            for t in toggles {
                if !self.expanded.remove(&t) {
                    self.expanded.insert(t);
                }
            }
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
                    actions.push(TreeAction::Select(path.clone()));
                    p.open = Some(path);
                }
            }
        }
        actions
    }

    /// The folder new items go into: the selected folder, the selected file's folder, or the
    /// project folder.
    fn tree_target_dir(&self) -> PathBuf {
        match &self.tree_sel {
            Some(sel) if sel.is_dir() => sel.clone(),
            Some(sel) => sel.parent().map_or_else(|| self.root.clone(), Path::to_path_buf),
            None => self.root.clone(),
        }
    }

    /// Reads the project's custom components again when the project or its files change, and
    /// gives every open designer the current list.
    fn refresh_custom_classes(&mut self) {
        let key = (self.root.clone(), self.external.tree_rev);
        if self.custom_scan.as_ref() != Some(&key) {
            self.custom_units = std::sync::Arc::new(designer::custom_classes(&self.root));
            self.custom_scan = Some(key);
        }
        for d in &mut self.designers {
            d.custom_units = self.custom_units.clone();
        }
    }

    /// Applies what the tree asked for this frame.
    fn apply_tree_actions(&mut self, actions: Vec<TreeAction>, p: &mut Pending) {
        for action in actions {
            match action {
                TreeAction::Select(path) => self.tree_sel = Some(path),
                TreeAction::NewIn { dir, is_dir } => {
                    self.expanded.insert(dir.clone());
                    self.tree_new = Some(TreeNew {
                        dir,
                        is_dir,
                        name: String::new(),
                        focus: true,
                    });
                }
                TreeAction::CancelNew => self.tree_new = None,
                TreeAction::Create { dir, is_dir, name } => {
                    self.tree_new = None;
                    let made = if is_dir {
                        files::new_folder(&dir, &name)
                    } else {
                        files::new_file(&dir, &name)
                    };
                    match made {
                        Ok(path) => {
                            self.expanded.insert(dir);
                            if !is_dir {
                                p.open = Some(path.clone());
                            }
                            self.tree_sel = Some(path);
                        }
                        Err(e) => self.note(e),
                    }
                }
                TreeAction::AskDelete(path) => self.tree_delete = Some(path),
                TreeAction::ConfirmDelete(path) => {
                    self.tree_delete = None;
                    self.delete_in_tree(&path);
                }
                TreeAction::AskRename(path) => {
                    self.tree_new = None;
                    self.tree_delete = None;
                    let name = path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    self.tree_rename = Some(TreeRename {
                        path,
                        name,
                        focus: true,
                    });
                }
                TreeAction::CancelRename => self.tree_rename = None,
                TreeAction::RenameTo { path, name } => {
                    self.tree_rename = None;
                    self.rename_in_tree(&path, &name);
                }
                TreeAction::MoveInto { path, dir } => self.move_in_tree(&path, &dir),
                TreeAction::NewForm { dir } => self.new_form(&dir),
                TreeAction::NewDataModule { dir } => self.new_unit(&dir, true),
            }
        }
    }

    /// Renames an item in the tree. Open tabs, forms and the selection follow it.
    fn rename_in_tree(&mut self, path: &Path, name: &str) {
        match files::rename(path, name, &self.root) {
            Ok(moves) => {
                let Some(to) = moves.first().map(|(_, to)| to.clone()) else {
                    return;
                };
                for (from, to) in &moves {
                    self.follow_move(from, to);
                }
                let updated = self.sync_program_for_moves(&moves);
                let shown = to
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let mut note = format!("Renamed to “{shown}”");
                if !updated.is_empty() {
                    note.push_str(&format!(", updated {}", updated.join(", ")));
                }
                self.tree_sel = Some(to);
                self.note(note);
            }
            Err(e) => self.note(format!("Could not rename “{name}”: {e}")),
        }
    }

    /// Moves an item into a folder of the tree (drag and drop). Open tabs, forms and the selection
    /// follow it, as they do for a rename.
    fn move_in_tree(&mut self, path: &Path, dir: &Path) {
        match files::move_into(path, dir, &self.root) {
            Ok(moves) => {
                let Some(to) = moves.first().map(|(_, to)| to.clone()) else {
                    return;
                };
                for (from, to) in &moves {
                    self.follow_move(from, to);
                }
                let updated = self.sync_program_for_moves(&moves);
                self.expanded.insert(dir.to_path_buf());
                let shown = to
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let folder = dir
                    .file_name()
                    .map_or_else(|| "the project".to_string(), |n| n.to_string_lossy().into_owned());
                self.tree_sel = Some(to);
                let mut note = format!("Moved “{shown}” into “{folder}”");
                if !updated.is_empty() {
                    note.push_str(&format!(", updated {}", updated.join(", ")));
                }
                self.note(note);
            }
            Err(e) => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                self.note(format!("Could not move “{name}”: {e}"));
            }
        }
    }

    /// The Pascal source files of the project that are empty. pasls does not start while one is
    /// (it crashes while it reads the project), so the console names them.
    pub(crate) fn empty_sources(&self) -> Vec<String> {
        let mut files = Vec::new();
        collect_files(&self.root, 0, &mut files);
        files
            .into_iter()
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    ["pas", "pp", "lpr", "dpr"].iter().any(|k| e.eq_ignore_ascii_case(k))
                })
            })
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() == 0))
            .map(|p| unit_name_or_file(&p))
            .collect()
    }

    /// The Pascal units of the project that still exist, except those under `gone`.
    fn project_units(&self, gone: &[PathBuf]) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_files(&self.root, 0, &mut files);
        files
            .into_iter()
            .filter(|p| is_unit_file(p) && !gone.iter().any(|g| p.starts_with(g)))
            .collect()
    }

    /// Points the `uses` entry of a unit renamed (or moved) to `new_pas` at its new name and path in
    /// every other unit of the project. Returns the files that changed.
    fn rename_in_other_units(&mut self, old: &str, new: &str, new_pas: &Path, gone: &[PathBuf]) -> Vec<String> {
        let mut changed = Vec::new();
        for file in self.project_units(gone) {
            if file == new_pas {
                continue;
            }
            let Some(dir) = file.parent() else { continue };
            let rel = pascoda_project::program::relative_path(dir, new_pas);
            let rel_in = rel.contains('/').then_some(rel);
            match self.edit_program(&file, |src| {
                Ok(pascoda_project::program::set_unit(src, old, new, rel_in.as_deref()))
            }) {
                Ok(true) => changed.push(unit_name_or_file(&file)),
                Ok(false) => {}
                Err(e) => self.note(format!("Could not update a uses clause: {e}")),
            }
        }
        changed
    }

    /// Takes a deleted unit out of the `uses` clauses of the other units. Returns the files changed.
    fn remove_from_other_units(&mut self, unit: &str, gone: &[PathBuf]) -> Vec<String> {
        let mut changed = Vec::new();
        for file in self.project_units(gone) {
            match self.edit_program(&file, |src| Ok(pascoda_project::program::remove_unit(src, unit, None))) {
                Ok(true) => changed.push(unit_name_or_file(&file)),
                Ok(false) => {}
                Err(e) => self.note(format!("Could not update a uses clause: {e}")),
            }
        }
        changed
    }

    /// After a rename or move the program follows its units: each unit that moved is re-pointed in
    /// the program's `uses` clause (its new name, and its `in` path when it is in a subfolder), and a
    /// renamed unit's `unit` line gets the new name. Returns the program files that changed.
    fn sync_program_for_moves(&mut self, moves: &[(PathBuf, PathBuf)]) -> Vec<String> {
        let mut updated: Vec<String> = Vec::new();
        for (from, to) in moves {
            let pairs: Vec<(PathBuf, PathBuf)> = if to.is_dir() {
                let mut files = Vec::new();
                collect_files(to, 0, &mut files);
                files
                    .into_iter()
                    .filter(|p| is_unit_file(p))
                    .filter_map(|new| {
                        let rest = new.strip_prefix(to).ok()?.to_path_buf();
                        Some((from.join(rest), new))
                    })
                    .collect()
            } else if is_unit_file(to) {
                vec![(from.clone(), to.clone())]
            } else {
                Vec::new()
            };
            for (old_pas, new_pas) in pairs {
                let (old_name, new_name) = (unit_name(&old_pas), unit_name(&new_pas));
                let Some(dir) = new_pas.parent() else { continue };
                if let Some(lpr) = pascoda_project::program::program_for(dir, &self.root) {
                    let folder = lpr.parent().unwrap_or(&self.root).to_path_buf();
                    let rel = new_pas.strip_prefix(&folder).ok().map(|r| r.to_string_lossy().replace('\\', "/"));
                    let rel_in = rel.filter(|r| r.contains('/'));
                    let lpr_name = lpr.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    match self.edit_program(&lpr, |src| {
                        Ok(pascoda_project::program::set_unit(src, &old_name, &new_name, rel_in.as_deref()))
                    }) {
                        Ok(true) => {
                            if !updated.contains(&lpr_name) {
                                updated.push(lpr_name);
                            }
                        }
                        Ok(false) => {}
                        Err(e) => self.note(format!("Could not update the program: {e}")),
                    }
                }
                for file in self.rename_in_other_units(&old_name, &new_name, &new_pas, &[]) {
                    if !updated.contains(&file) {
                        updated.push(file);
                    }
                }
                if old_name != new_name {
                    if let Ok(text) = std::fs::read_to_string(&new_pas) {
                        let out = pascoda_project::program::rename_unit_line(&text, &old_name, &new_name);
                        if out != text {
                            if let Err(e) = std::fs::write(&new_pas, out) {
                                self.note(format!("Could not rename the unit line: {e}"));
                            }
                        }
                    }
                }
            }
        }
        updated
    }

    /// Points everything that names `from` (or a path inside it) at `to` instead.
    fn follow_move(&mut self, from: &Path, to: &Path) {
        for d in &mut self.docs {
            if let Some(p) = files::moved(&d.path, from, to) {
                d.path = p;
            }
        }
        for f in &mut self.designers {
            if let Some(p) = files::moved(&f.pas_path, from, to) {
                f.pas_path = p;
            }
            if let Some(p) = files::moved(&f.lfm_path, from, to) {
                f.lfm_path = p;
            }
        }
        self.expanded = self
            .expanded
            .iter()
            .map(|d| files::moved(d, from, to).unwrap_or_else(|| d.clone()))
            .collect::<HashSet<PathBuf>>();
        if let Some(sel) = self.tree_sel.as_deref().and_then(|s| files::moved(s, from, to)) {
            self.tree_sel = Some(sel);
        }
    }

    /// Deletes an item from disk. Tabs and forms inside it go too; the caller has asked first.
    fn delete_in_tree(&mut self, path: &Path) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        // The units and their programs are found before the files are gone.
        let gone: Vec<PathBuf> = if path.is_dir() { vec![path.to_path_buf()] } else { files::removed_with(path) };
        let is_gone = |p: &Path| gone.iter().any(|g| p.starts_with(g));
        let units = form_units_under(path);
        let removals: Vec<(PathBuf, String, Option<(String, String)>)> = units
            .iter()
            .filter_map(|(pas, unit, form)| {
                let dir = pas.parent()?.to_path_buf();
                let lpr = pascoda_project::program::program_for(&dir, &self.root)?;
                Some((lpr, unit.clone(), form.clone()))
            })
            .collect();
        match files::delete(path, &self.root) {
            Ok(()) => {
                let mut from_program = Vec::new();
                let mut from_others: Vec<String> = Vec::new();
                for (_, unit, _) in &units {
                    for file in self.remove_from_other_units(unit, &gone) {
                        if !from_others.contains(&file) {
                            from_others.push(file);
                        }
                    }
                }
                for (lpr, unit, form) in &removals {
                    let form_ref = form.as_ref().map(|(c, v)| (c.as_str(), v.as_str()));
                    match self.edit_program(lpr, |src| {
                        Ok(pascoda_project::program::remove_unit(src, unit, form_ref))
                    }) {
                        Ok(true) => from_program.push(unit.clone()),
                        Ok(false) => {}
                        Err(e) => self.note(format!("Could not update the program: {e}")),
                    }
                }
                for i in (0..self.docs.len()).rev() {
                    if is_gone(&self.docs[i].path) {
                        self.discard_doc(i);
                    }
                }
                self.designers.retain(|f| !is_gone(&f.pas_path) && !is_gone(&f.lfm_path));
                self.expanded.retain(|d| !is_gone(d));
                if self.tree_sel.as_deref().is_some_and(is_gone) {
                    self.tree_sel = None;
                }
                let mut note = if gone.len() > 1 {
                    let others: Vec<String> = gone[1..].iter().map(|p| unit_name_or_file(p)).collect();
                    format!("Moved “{name}” and {} to the Trash", others.join(" and "))
                } else {
                    format!("Moved “{name}” to the Trash")
                };
                if !from_program.is_empty() {
                    note.push_str(&format!(" and removed {} from the program", from_program.join(", ")));
                }
                if !from_others.is_empty() {
                    note.push_str(&format!(", and from the uses clauses of {}", from_others.join(", ")));
                }
                self.note(note);
            }
            Err(e) => self.note(format!("Could not move “{name}” to the Trash: {e}")),
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
        // The form of the active tab's unit: on the `.pas` tab or on its `.lfm` tab. The designer
        // edits the unit's text, which is the unit's own tab.
        if self.form_view
            && let Some(active) = self.docs.get(self.active).map(|d| d.path.clone())
            && let Some(unit) = designer::form_unit(&active)
            && let Some(unit_tab) = self.docs.iter().position(|d| d.path == unit)
            && let Some(fi) = self.designers.iter().position(|f| f.pas_path == unit)
        {
            let doc = &mut self.docs[unit_tab];
            let outcome = designer::ui::show(
                &mut self.designers[fi],
                &mut self.designer_ui,
                &mut self.native,
                ui,
                frame,
                &mut doc.text,
            );
            p.designer_outcome = Some((doc.path.clone(), outcome));
            if self.dialog_open()
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
                        self.form_lfm = None;
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
        // Tab inserts spaces up to the next tab stop, and keeps the focus in the editor. A Tab that
        // completion took (a chosen item) was consumed before this, so what is left is ours.
        if ctx.memory(|m| m.has_focus(doc.editor_id))
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
        {
            let caret = egui::TextEdit::load_state(&ctx, doc.editor_id)
                .and_then(|s| s.cursor.char_range())
                .map(|r| (r.primary.index.0, r.secondary.index.0))
                .unwrap_or((0, 0));
            let (lo, hi) = (caret.0.min(caret.1), caret.0.max(caret.1));
            let (text, at) = prefs::insert_tab(&doc.text, lo, hi, prefs::tab_width());
            doc.text = text;
            let mut state = egui::TextEdit::load_state(&ctx, doc.editor_id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
            state.store(&ctx, doc.editor_id);
        }
        if self.find.from_selection {
            self.find.from_selection = false;
            let caret = egui::TextEdit::load_state(&ctx, doc.editor_id)
                .and_then(|s| s.cursor.char_range())
                .map(|r| (r.primary.index.0, r.secondary.index.0));
            if let Some((a, b)) = caret.filter(|(a, b)| a != b) {
                let (lo, hi) = (a.min(b), a.max(b));
                let picked: String = doc.text.chars().skip(lo).take(hi - lo).collect();
                if !picked.contains('\n') {
                    self.find.query = picked;
                    self.find.current = None;
                }
            }
        }
        let found = if self.find.open {
            find::matches(&doc.text, &self.find.query, self.find.opts)
        } else {
            Vec::new()
        };
        if self.find.open {
            let asked = find_bar(ui, pal, &mut self.find, found.len())
                .or_else(|| self.find.pending.take().map(FindAction::Go));
            if let Some(action) = asked {
                let caret = egui::TextEdit::load_state(&ctx, doc.editor_id)
                    .and_then(|s| s.cursor.char_range())
                    .map(|r| (r.primary.index.0, r.secondary.index.0));
                match action {
                    FindAction::Go(forward) => {
                        let (lo, hi) = caret.map_or((0, 0), |(a, b)| (a.min(b), a.max(b)));
                        if let Some(i) = find::step(&found, if forward { hi } else { lo }, forward) {
                            self.find.current = Some(i);
                            select_in_editor(&ctx, doc.editor_id, &found[i]);
                            self.find.scroll = Some(found[i].clone());
                        }
                    }
                    FindAction::ReplaceOne => {
                        let lo = caret.map_or(0, |(a, b)| a.min(b));
                        let done = find::replace_one(
                            &doc.text,
                            &self.find.query,
                            self.find.opts,
                            self.find.current,
                            lo,
                            &self.find.with,
                        );
                        if let Some(done) = done {
                            doc.text = done.text;
                            self.find.current = done.index;
                            self.find.note = if done.selected.is_none() {
                                "Replaced the last match".to_string()
                            } else {
                                String::new()
                            };
                            if let Some(r) = done.selected {
                                select_in_editor(&ctx, doc.editor_id, &r);
                                self.find.scroll = Some(r);
                            }
                        }
                    }
                    FindAction::ReplaceAll => {
                        let (text, n) =
                            find::replace_all(&doc.text, &self.find.query, &self.find.with, self.find.opts);
                        self.find.note = format!("Replaced {n}");
                        if n > 0 {
                            doc.text = text;
                        }
                        self.find.current = None;
                    }
                }
            }
        }
        // Find's match and go-to's line both scroll the editor to them.
        let scroll_to = self.find.scroll.take().or_else(|| self.reveal.take());
        let wrap = crate::prefs::word_wrap();
        // Wrapped text fits the width, so only the height scrolls.
        let scroll = if wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
        scroll.auto_shrink(false).show(ui, |ui| {
            if let Some([r, g, b]) = crate::prefs::editor_background(pal.dark) {
                ui.painter().rect_filled(ui.clip_rect(), 0.0, egui::Color32::from_rgb(r, g, b));
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let lines = doc.text.split('\n').count();
                // The width the text wraps to: the row past the gutter (no limit when not wrapping).
                let wrap_width = if wrap { (ui.available_width() - GUTTER_W).max(40.0) } else { f32::INFINITY };
                let galley = {
                    let mut job = highlight::layout(doc.text.as_str(), pal, editor_font(), crate::prefs::line_height());
                    job.wrap.max_width = wrap_width;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let map = LineMap::new(&galley, lines);
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
                        egui::pos2(left, top + map.top(line)),
                        egui::pos2(right, top + map.bottom(line)),
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

                let mut edit_line = None;
                if gutter(
                    ui,
                    pal,
                    &map,
                    &mut doc.breakpoints,
                    &doc.disabled,
                    exec_line,
                    caret_line.map(|l| l as u32),
                    &flagged,
                    &mut edit_line,
                ) {
                    // A condition stays only with its breakpoint.
                    let lines = doc.breakpoints.clone();
                    doc.stops.retain(|line, _| lines.contains(line));
                    p.breakpoints_changed =
                        Some((doc.path.clone(), debugger::breakpoint_specs(&doc.breakpoints, &doc.stops, &doc.disabled)));
                }
                if let Some(line) = edit_line {
                    p.edit_breakpoint = Some((doc.path.clone(), line));
                }
                // The same width as the map above, so the galley the text is drawn with is the one it was measured with.
                let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _: f32| {
                    let mut job = highlight::layout(text.as_str(), pal, editor_font(), crate::prefs::line_height());
                    job.wrap.max_width = wrap_width;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let rows = (ui.available_height() / crate::prefs::line_height()).floor().max(1.0) as usize;
                let len_before = doc.text.len();
                let output = egui::TextEdit::multiline(&mut doc.text)
                    .id(doc.editor_id)
                    // Tab stays in the editor (it inserts spaces, see above) instead of moving focus.
                    .lock_focus(true)
                    .font(font.clone())
                    .frame(Frame::NONE)
                    .margin(Margin::ZERO)
                    .desired_width(wrap_width)
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
                // The signature popup under the caret: the label, the active parameter in the accent colour.
                if output.response.has_focus() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.signature = None;
                }
                if let Some(h) = self.signature.as_ref().filter(|h| h.key == text_key(&doc.text)) {
                    let anchor = output.galley_pos
                        + output.galley.pos_from_cursor(CCursor::new(h.at)).left_bottom().to_vec2();
                    egui::Area::new(egui::Id::unique("signature-hint"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(anchor)
                        .show(&ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                let mut job = egui::text::LayoutJob::default();
                                for (text, marked) in
                                    hints::highlight_parameter(&h.sig.label, &h.sig.parameters, h.sig.active)
                                {
                                    job.append(
                                        &text,
                                        0.0,
                                        egui::TextFormat {
                                            font_id: egui::FontId::monospace(12.0),
                                            color: if marked { pal.accent } else { pal.text },
                                            ..Default::default()
                                        },
                                    );
                                }
                                ui.label(job);
                            });
                        });
                }
                // Every match of the find bar, the current one stronger; the editor scrolls to it.
                for (i, r) in found.iter().enumerate() {
                    let Some(rect) = hints::range_rect(&output.galley, r) else { continue };
                    let fill = if self.find.current == Some(i) {
                        pal.warning.gamma_multiply(0.55)
                    } else {
                        pal.warning.gamma_multiply(0.22)
                    };
                    ui.painter().rect_filled(rect.translate(origin), 2.0, fill);
                }
                if let Some(r) = scroll_to.as_ref()
                    && let Some(rect) = hints::range_rect(&output.galley, r)
                {
                    ui.scroll_to_rect(rect.translate(origin), Some(egui::Align::Center));
                }
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
                // Cmd-click on a name goes to its definition (marks are not symbols).
                if output.response.clicked() && ui.input(|i| i.modifiers.command) {
                    let pointer = ctx.pointer_interact_pos().unwrap_or_default();
                    let local = pointer - output.galley_pos;
                    if let Some(t) = hints::target_at(&output.galley, &doc.text, local, marks)
                        && t.mark.is_none()
                    {
                        p.definition_at = Some(t.range.start);
                    }
                }
                if output.response.changed() {
                    self.hover.clear();
                    // Any edit hides the signature; typing `(` or `,` asks for the one at the caret.
                    self.signature = None;
                    if let Some(c) = output.cursor_range.map(|r| r.primary.index.0)
                        && c > 0
                        && matches!(doc.text.chars().nth(c - 1), Some('(' | ','))
                    {
                        let (line, col) = line_col(&doc.text, c);
                        p.signature_request = Some((doc.path.clone(), doc.text.clone(), line, col, c, text_key(&doc.text)));
                    }
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
                    BottomTab::Tests => {
                        self.bottom.terminal_focused = false;
                        self.bottom.view_shown = false;
                        self.unit_tests_tab(ui, pal);
                    }
                    BottomTab::Terminal => self.terminal_tab(ui, pal),
                }
            });
    }

    /// Opens the Rename Symbol box (F2) on the name under the caret.
    pub(crate) fn open_rename(&mut self, ctx: &egui::Context) {
        let Some(index) = self.cursor_index(ctx) else { return };
        let Some(doc) = self.docs.get(self.active) else { return };
        let Some((_, name)) = find::word_at(&doc.text, index) else {
            self.console.push_str("put the caret on a name first\n");
            return;
        };
        let (line, col) = line_col(&doc.text, index);
        self.rename = find::Rename {
            open: true,
            old: name.clone(),
            path: Some(doc.path.clone()),
            line,
            col,
            name,
            focus: true,
            error: String::new(),
            sent: None,
        };
    }

    /// The Project Options window (Build > Project Options...): the mode, defines, search paths,
    /// switches and target. Save writes `pascoda.toml` and closes; the next build reads it.
    fn show_project_options(&mut self, ctx: &egui::Context) {
        if !self.project_options.open {
            return;
        }
        let pal = Palette::of(ctx);
        let root = self.root.clone();
        let warnings = self.project_options.warnings(&root);
        let mut save = false;
        let mut cancel = false;
        // The app's own dialog (no system-grey title bar), as the export dialog is drawn.
        crate::style::dialog(ctx, "project-options", Some("Project Options"), 560.0, |ui| {
                // Roomier rows than the default: the dialog is for a form of settings.
                ui.spacing_mut().item_spacing.y = 8.0;
                let e = &mut self.project_options;
                // The settings scroll; Save and Cancel stay in view below them.
                egui::ScrollArea::vertical().max_height(600.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Configuration");
                    ui.radio_value(&mut e.configuration, pascoda_project::Configuration::Debug, "Debug");
                    ui.radio_value(&mut e.configuration, pascoda_project::Configuration::Release, "Release");
                });
                ui.horizontal(|ui| {
                    ui.label("Language");
                    ui.radio_value(&mut e.mode, pascoda_fpc::Mode::ObjFpc, "ObjFPC");
                    ui.radio_value(&mut e.mode, pascoda_fpc::Mode::Delphi, "Delphi");
                });
                ui.label(
                    RichText::new("ObjFPC and Delphi make Integer 32 bits; fpc's own default is 16.")
                        .size(11.0)
                        .color(pal.text_tertiary),
                );
                ui.add_space(6.0);
                for (label, text, hint) in [
                    ("Defines (-d)", &mut e.defines, "One name per line"),
                    ("Unit search paths (-Fu)", &mut e.units, "Folders, relative to the project"),
                    ("Include search paths (-Fi)", &mut e.includes, "Folders, relative to the project"),
                    ("Compiler switches", &mut e.switches, "One per line, such as -O2 or -Cr"),
                ] {
                    let l = ui.label(label);
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .id(egui::Id::unique(label))
                            .hint_text(hint)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(l.id);
                }
                ui.add_space(4.0);
                for (label, text, hint) in [
                    ("Program arguments", &mut e.args, "One argument per line"),
                ] {
                    let l = ui.label(label);
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .id(egui::Id::unique(label))
                            .hint_text(hint)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(l.id);
                }
                ui.horizontal(|ui| {
                    let l = ui.label("Working folder");
                    ui.add(
                        egui::TextEdit::singleline(&mut e.workdir)
                            .id(egui::Id::unique("project-workdir"))
                            .hint_text("The program's folder")
                            .desired_width(300.0),
                    )
                    .labelled_by(l.id);
                });
                ui.horizontal(|ui| {
                    let l = ui.label("Output file");
                    ui.add(
                        egui::TextEdit::singleline(&mut e.output)
                            .id(egui::Id::unique("project-output"))
                            .hint_text("Default")
                            .desired_width(300.0),
                    )
                    .labelled_by(l.id);
                });
                ui.label(
                    RichText::new(format!("Default: {}", e.default_target))
                        .monospace()
                        .size(11.0)
                        .color(pal.text_tertiary),
                );
                if let Some(view) = &e.project_file {
                    egui::CollapsingHeader::new(format!("From {} (read-only)", view.file_name))
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("project-file-view")
                                .num_columns(2)
                                .spacing([12.0, 3.0])
                                .show(ui, |ui| {
                                    for (label, value) in &view.rows {
                                        ui.label(RichText::new(label).size(12.0).color(pal.text_tertiary));
                                        ui.label(RichText::new(value).monospace().size(12.0));
                                        ui.end_row();
                                    }
                                });
                            if !view.skipped.is_empty() {
                                ui.label(
                                    RichText::new(format!("Left out by the build: {}", view.skipped.join(", ")))
                                        .size(11.0)
                                        .color(pal.text_tertiary),
                                );
                            }
                            ui.label(
                                RichText::new("The build reads these; they are changed in the project file, not here.")
                                    .size(11.0)
                                    .color(pal.text_tertiary),
                            );
                        });
                }
                for w in &warnings {
                    ui.label(RichText::new(w).size(12.0).color(pal.warning));
                }
                if !e.error.is_empty() {
                    ui.label(RichText::new(&e.error).color(pal.error));
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("The language server reads these when it starts: reopen the project for it.")
                        .size(11.0)
                        .color(pal.text_tertiary),
                );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if save {
            match self.project_options.save(&root) {
                Ok(()) => {
                    self.project_options.open = false;
                    self.console.push_str("Project options saved to pascoda.toml\n");
                }
                Err(e) => self.project_options.error = format!("Could not save: {e}"),
            }
        } else if cancel {
            self.project_options.open = false;
        }
    }

    /// The Component Packages dialog (File > Import Component Package...): the imported packages,
    /// with Add and Remove. Changes take effect in the designer's palette at once.
    fn show_import_packages(&mut self, ctx: &egui::Context) {
        if !self.import_packages_open {
            return;
        }
        let files = pascoda_project::imports::list();
        let built_in = pascoda_project::imports::builtin();
        let mut add = false;
        let mut remove: Option<PathBuf> = None;
        let mut done = false;
        crate::style::dialog(ctx, "import-packages", Some("Component Packages"), 520.0, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("The components of these packages appear in the form designer's palette, on the pages the packages name.")
                    .size(12.0)
                    .color(egui::Color32::GRAY),
            );
            if files.is_empty() {
                ui.label("No packages are imported yet.");
            }
            for file in &files {
                ui.horizontal(|ui| {
                    let name = file.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                    ui.label(RichText::new(name).strong());
                    ui.label(RichText::new(file.display().to_string()).size(11.0).color(egui::Color32::GRAY));
                    if built_in.contains(file) {
                        ui.label(RichText::new("built in").size(11.0).color(egui::Color32::GRAY));
                    } else if ui.button("Remove").clicked() {
                        remove = Some(file.clone());
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Add Package...").clicked() {
                    add = true;
                }
                if ui.button("Done").clicked() {
                    done = true;
                }
            });
        });
        let Some(store) = pascoda_project::imports::default_store() else {
            self.console.push_str("Packages cannot be saved: no home folder\n");
            self.import_packages_open = false;
            return;
        };
        let mut changed = false;
        if let Some(file) = remove {
            match pascoda_project::imports::remove_from(&store, &file) {
                Ok(_) => changed = true,
                Err(e) => self.console.push_str(&format!("Could not remove the package: {e}\n")),
            }
        }
        if add
            && let Some(file) = rfd::FileDialog::new()
                .add_filter("Lazarus package", &["lpk"])
                .pick_file()
        {
            if pascoda_project::packages::read(&file).is_some() {
                match pascoda_project::imports::add_to(&store, &file) {
                    Ok(_) => {
                        changed = true;
                        self.console.push_str(&format!("Imported {}\n", file.display()));
                    }
                    Err(e) => self.console.push_str(&format!("Could not save the import: {e}\n")),
                }
            } else {
                self.console.push_str(&format!("{} is not a Lazarus package\n", file.display()));
            }
        }
        if changed {
            designer::imports::refresh();
        }
        if done {
            self.import_packages_open = false;
        }
    }

    /// Whether an egui dialog or box is open. Those draw under the native designer controls, so the
    /// canvas is hidden while one is open.
    pub(crate) fn dialog_open(&self) -> bool {
        self.export_dialog.is_some()
            || self.confirm.is_some()
            || self.vendor_dialog.is_some()
            || self.go_to.open
            || self.rename.open
            || self.preferences_open
            || self.project_options.open
            || self.bp_edit.is_some()
            || self.value_edit.is_some()
            || self.import_packages_open
            || self.designer_ui.connection.is_some()
    }

    /// The Preferences window (Cmd+,): the editor's font size and what the Tab key inserts. Each
    /// change is applied and saved at once.
    fn show_preferences(&mut self, ctx: &egui::Context) {
        if !self.preferences_open {
            return;
        }
        let before = self.prefs;
        let mut done = false;
        // The app's own dialog (no system-grey title bar), with a Done button to close it.
        crate::style::dialog(ctx, "preferences", Some("Preferences"), 460.0, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.heading("Editor");
                egui::Grid::new("prefs-editor").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Line numbers");
                    ui.checkbox(&mut self.prefs.line_numbers, "Show in the gutter");
                    ui.end_row();
                    ui.label("Long lines");
                    ui.checkbox(&mut self.prefs.word_wrap, "Wrap to the editor width");
                    ui.end_row();
                    let font_label = ui.label("Font");
                    egui::ComboBox::from_id_salt("prefs-font")
                        .selected_text(theme::EDITOR_FONTS[self.prefs.font].0)
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in theme::EDITOR_FONTS.iter().enumerate() {
                                ui.selectable_value(&mut self.prefs.font, i, *name);
                            }
                        })
                        .response
                        .labelled_by(font_label.id);
                    ui.end_row();
                    ui.label("Background");
                    ui.horizontal(|ui| {
                        for dark in [false, true] {
                            let pal = if dark { Palette::DARK } else { Palette::LIGHT };
                            let fallback = pal.editor;
                            let mut rgb = self.prefs.background[dark as usize]
                                .unwrap_or([fallback.r(), fallback.g(), fallback.b()]);
                            let label = if dark { "Dark" } else { "Light" };
                            ui.label(label);
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                self.prefs.set_background(dark, Some(rgb));
                            }
                            if self.prefs.background[dark as usize].is_some() && ui.small_button("Reset").clicked() {
                                self.prefs.set_background(dark, None);
                            }
                        }
                    });
                    ui.end_row();
                    let size_label = ui.label("Font size");
                    ui.add(
                        egui::DragValue::new(&mut self.prefs.font_size)
                            .range(*prefs::FONT_SIZES.start()..=*prefs::FONT_SIZES.end())
                            .speed(0.2)
                            .fixed_decimals(0)
                            .suffix(" pt"),
                    )
                    .labelled_by(size_label.id);
                    ui.end_row();
                    ui.label("Tab key inserts");
                    ui.horizontal(|ui| {
                        for width in prefs::TAB_WIDTHS {
                            ui.radio_value(&mut self.prefs.tab_width, width, format!("{width} spaces"));
                        }
                    });
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.heading("Code colours");
                egui::Grid::new("prefs-colors").num_columns(3).spacing([16.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Token").color(egui::Color32::GRAY));
                    ui.label(RichText::new("Light").color(egui::Color32::GRAY));
                    ui.label(RichText::new("Dark").color(egui::Color32::GRAY));
                    ui.end_row();
                    for (token, (kind, label)) in highlight::TOKENS.iter().enumerate() {
                        ui.label(*label);
                        for dark in [false, true] {
                            ui.horizontal(|ui| {
                                let pal = if dark { Palette::DARK } else { Palette::LIGHT };
                                let fallback = highlight::default_color(*kind, &pal);
                                let mut rgb = self.prefs.colors[dark as usize][token]
                                    .unwrap_or([fallback.r(), fallback.g(), fallback.b()]);
                                if ui.color_edit_button_srgb(&mut rgb).changed() {
                                    self.prefs.set_color(dark, token, Some(rgb));
                                }
                                if self.prefs.colors[dark as usize][token].is_some()
                                    && ui.small_button("Reset").clicked()
                                {
                                    self.prefs.set_color(dark, token, None);
                                }
                            });
                        }
                        ui.end_row();
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Each appearance (View > Appearance) has its own colours and background.")
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Reset to Defaults").clicked() {
                        self.prefs = prefs::Prefs::default();
                    }
                    if ui.button("Done").clicked() {
                        done = true;
                    }
                });
        });
        if done {
            self.preferences_open = false;
        }
        let after = self.prefs.checked();
        if after != before {
            self.prefs = after;
            after.apply();
            after.save();
        }
    }

    /// The Change Value box: the new value, Save to assign it while the program is stopped.
    fn show_value_edit(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.value_edit.as_mut() else { return };
        let pal = Palette::of(ctx);
        let mut save = false;
        let mut cancel = false;
        let mut open = true;
        egui::Window::new("Change Value")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0])
            .frame(egui::Frame::window(&ctx.global_style()).fill(pal.chrome))
            .show(ctx, |ui| {
                ui.label(RichText::new(&edit.name).monospace());
                let l = ui.label("New value");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut edit.text)
                        .id(egui::Id::unique("value-edit"))
                        .desired_width(f32::INFINITY),
                );
                let field = field.labelled_by(l.id);
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                if !edit.error.is_empty() {
                    ui.label(RichText::new(&edit.error).size(12.0).color(pal.error));
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if save {
            let edit = edit.clone();
            match assignment_text(edit.kind, &edit.text) {
                Ok(value) => {
                    if let Some(e) = self.value_edit.as_mut() {
                        e.error.clear();
                    }
                    self.send(debugger::Cmd::SetValue(edit.target.clone(), value));
                }
                Err(e) => {
                    if let Some(v) = self.value_edit.as_mut() {
                        v.error = e;
                    }
                }
            }
        } else if cancel || !open {
            self.value_edit = None;
        }
    }

    /// The breakpoint box (a secondary click in the gutter): the condition, and the hit count from
    /// which the breakpoint stops. Save applies them; Remove takes the breakpoint away.
    fn show_breakpoint_edit(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.bp_edit.as_mut() else { return };
        let pal = Palette::of(ctx);
        let mut save = false;
        let mut remove = false;
        let mut cancel = false;
        let mut open = true;
        let title = format!("Breakpoint at line {}", edit.line);
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0])
            .frame(egui::Frame::window(&ctx.global_style()).fill(pal.chrome))
            .show(ctx, |ui| {
                let l = ui.label("Condition");
                ui.add(
                    egui::TextEdit::singleline(&mut edit.condition)
                        .id(egui::Id::unique("bp-condition"))
                        .hint_text("Stops only when true, e.g. i = 2")
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(l.id);
                let l = ui.label("Hit count");
                ui.add(
                    egui::TextEdit::singleline(&mut edit.hits)
                        .id(egui::Id::unique("bp-hits"))
                        .hint_text("Stops from this hit on, e.g. 3")
                        .desired_width(120.0),
                )
                .labelled_by(l.id);
                ui.label(
                    RichText::new("Leave both empty for a plain breakpoint.")
                        .size(11.0)
                        .color(pal.text_tertiary),
                );
                if !edit.error.is_empty() {
                    ui.label(RichText::new(&edit.error).size(12.0).color(pal.error));
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Remove Breakpoint").clicked() {
                        remove = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        let Some(edit) = self.bp_edit.clone() else { return };
        if save {
            match self.save_breakpoint_edit(&edit.path, edit.line, &edit.condition, &edit.hits) {
                Ok(()) => self.bp_edit = None,
                Err(e) => {
                    if let Some(b) = self.bp_edit.as_mut() {
                        b.error = e;
                    }
                }
            }
        } else if remove {
            self.remove_breakpoint(&edit.path, edit.line);
            self.bp_edit = None;
        } else if cancel || !open {
            self.bp_edit = None;
        }
    }

    /// Sets the condition and hit count of the breakpoint at `line` of `path`. Blank fields make it
    /// a plain breakpoint. A hit count is a whole number from 1; the debugger stops from that hit on.
    pub(crate) fn save_breakpoint_edit(&mut self, path: &Path, line: u32, condition: &str, hits: &str) -> Result<(), String> {
        let hits = hits.trim();
        if !hits.is_empty() && !hits.parse::<u32>().is_ok_and(|n| n >= 1) {
            return Err("The hit count is a whole number from 1, such as 3".into());
        }
        let Some(doc) = self.docs.iter_mut().find(|d| d.path == path) else {
            return Err("The file is not open".into());
        };
        doc.breakpoints.insert(line);
        let stop = debugger::Stop { condition: condition.trim().to_string(), hits: hits.to_string() };
        if stop.condition.is_empty() && stop.hits.is_empty() {
            doc.stops.remove(&line);
        } else {
            doc.stops.insert(line, stop);
        }
        self.send_breakpoints(path);
        Ok(())
    }

    /// Takes the breakpoint at `line` of `path` away, with its condition.
    pub(crate) fn remove_breakpoint(&mut self, path: &Path, line: u32) {
        if let Some(doc) = self.docs.iter_mut().find(|d| d.path == path) {
            doc.breakpoints.remove(&line);
            doc.stops.remove(&line);
            doc.disabled.remove(&line);
        }
        self.send_breakpoints(path);
    }

    /// Turns the breakpoint at `line` of `path` on or off. An off breakpoint stays listed and in the
    /// gutter, but the debugger does not stop there.
    pub(crate) fn set_breakpoint_enabled(&mut self, path: &Path, line: u32, enabled: bool) {
        if let Some(doc) = self.docs.iter_mut().find(|d| d.path == path && d.breakpoints.contains(&line)) {
            if enabled {
                doc.disabled.remove(&line);
            } else {
                doc.disabled.insert(line);
            }
        }
        self.send_breakpoints(path);
    }

    /// The breakpoints of the open files: each file's name, then its lines with a checkbox to turn
    /// them on or off, what they stop on, a remove button, and a click that goes to the line.
    fn breakpoint_rows(&mut self, ui: &mut egui::Ui, pal: &Palette, p: &mut Pending) {
        let mut toggles: Vec<(PathBuf, u32, bool)> = Vec::new();
        let mut removals: Vec<(PathBuf, u32)> = Vec::new();
        let mut any = false;
        let mut exceptions = self.exception_stops;
        if ui
            .checkbox(&mut exceptions, "Stop on Pascal exceptions")
            .on_hover_text("Stops on the line that raises an exception, in the program or its units")
            .changed()
        {
            self.exception_stops = exceptions;
            self.send(debugger::Cmd::ExceptionStops(exceptions));
        }
        ui.add_space(4.0);
        for doc in self.docs.iter().filter(|d| !d.breakpoints.is_empty()) {
            any = true;
            let name = doc
                .path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            ui.label(RichText::new(name).strong());
            for &line in &doc.breakpoints {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let mut on = !doc.disabled.contains(&line);
                    if ui.checkbox(&mut on, format!("Line {line}")).changed() {
                        toggles.push((doc.path.clone(), line, on));
                    }
                    let summary = match doc.stops.get(&line) {
                        Some(s) if !s.condition.is_empty() && !s.hits.is_empty() => {
                            format!("if {} · from hit {}", s.condition, s.hits)
                        }
                        Some(s) if !s.condition.is_empty() => format!("if {}", s.condition),
                        Some(s) if !s.hits.is_empty() => format!("from hit {}", s.hits),
                        _ => "plain breakpoint".to_string(),
                    };
                    let row = ui.selectable_label(
                        false,
                        RichText::new(summary).size(12.0).color(pal.text_secondary),
                    );
                    if row.clicked() {
                        p.goto = Some((doc.path.clone(), line, 1));
                    }
                    if text_button(ui, pal, "×").on_hover_text("Remove this breakpoint").clicked() {
                        removals.push((doc.path.clone(), line));
                    }
                });
            }
            ui.add_space(4.0);
        }
        if !any {
            ui.add_space(14.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No breakpoints. Click the gutter to add one.").color(pal.text_tertiary))
            });
        }
        for (path, line, on) in toggles {
            self.set_breakpoint_enabled(&path, line, on);
        }
        for (path, line) in removals {
            self.remove_breakpoint(&path, line);
        }
    }

    /// Sends the file's breakpoints (with their conditions) to the debugger, if one is running.
    fn send_breakpoints(&self, path: &Path) {
        if let Some(doc) = self.docs.iter().find(|d| d.path == path) {
            let specs = debugger::breakpoint_specs(&doc.breakpoints, &doc.stops, &doc.disabled);
            self.send(debugger::Cmd::Breakpoints(doc.path.clone(), specs));
        }
    }

    /// The Rename Symbol box: the new name, and Rename to send it to the server.
    fn show_rename(&mut self, ctx: &egui::Context) {
        if !self.rename.open {
            return;
        }
        let mut go = false;
        let mut close = false;
        egui::Window::new(format!("Rename “{}”", self.rename.old))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.rename.name)
                        .id(egui::Id::unique("rename-symbol"))
                        .hint_text("New name")
                        .desired_width(220.0),
                );
                if self.rename.focus {
                    field.request_focus();
                    self.rename.focus = false;
                }
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Rename").clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                if !self.rename.error.is_empty() {
                    ui.label(RichText::new(&self.rename.error).color(egui::Color32::from_rgb(200, 60, 50)));
                }
            });
        if go {
            let name = self.rename.name.trim().to_string();
            if !pascoda_project::is_valid_name(&name) {
                self.rename.error = "A name is letters, digits and _, starting with a letter".into();
            } else {
                self.rename.name = name;
                self.send_rename();
            }
        } else if close {
            self.rename.open = false;
            self.rename.error.clear();
        }
    }

    /// Shows the places a symbol is used in the Search tab, grouped by file.
    pub(crate) fn show_references(&mut self, locs: Vec<pascoda_lsp::Location>) {
        let name = self.project_bar.title.strip_prefix("References to ").unwrap_or_default().to_string();
        let name_len = name.chars().count();
        let mut texts: std::collections::HashMap<PathBuf, String> = std::collections::HashMap::new();
        let mut files: Vec<find::FileHits> = Vec::new();
        for loc in locs {
            let text = texts.entry(loc.path.clone()).or_insert_with(|| {
                self.docs
                    .iter()
                    .find(|d| d.path == loc.path)
                    .map(|d| d.text.clone())
                    .or_else(|| std::fs::read_to_string(&loc.path).ok())
                    .unwrap_or_default()
            });
            let line_text = text.split('\n').nth(loc.line as usize).unwrap_or_default().to_string();
            let line_start: usize = text
                .split('\n')
                .take(loc.line as usize)
                .map(|l| l.chars().count() + 1)
                .sum();
            let start = line_start + loc.col as usize;
            let hit = find::Hit {
                line: loc.line + 1,
                col: loc.col + 1,
                text: line_text,
                range: start..start + name_len,
            };
            match files.iter_mut().find(|f| f.path == loc.path) {
                Some(f) => f.hits.push(hit),
                None => files.push(find::FileHits { path: loc.path.clone(), hits: vec![hit] }),
            }
        }
        self.project_bar.results = files;
        self.project_bar.truncated = false;
        self.project_bar.searched = true;
        self.project_bar.note.clear();
    }

    /// Writes the rename's replacements: into open tabs (unsaved, as typing is), and into the files
    /// on disk. Nothing changes when an open file has changed since the rename was asked.
    pub(crate) fn apply_rename(&mut self, files: Vec<pascoda_lsp::FileEdit>, sent: &find::Sent) {
        let changed_since = sent.open.iter().any(|(path, text)| {
            self.docs.iter().any(|d| &d.path == path && &d.text != text)
        });
        if changed_since {
            self.note("Nothing renamed: a file changed while the rename was being worked out");
            return;
        }
        let units: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).filter(|p| is_unit_file(p)).collect();
        let (mut places, mut count) = (0, 0);
        for file in files {
            let edits: Vec<((u32, u32), (u32, u32), String)> = file
                .edits
                .iter()
                .map(|e| (e.start, e.end, e.new_text.clone()))
                .collect();
            if let Some(d) = self.docs.iter_mut().find(|d| d.path == file.path) {
                d.text = find::apply_edits(&d.text, &edits);
            } else {
                let Ok(text) = std::fs::read_to_string(&file.path) else { continue };
                if let Err(e) = std::fs::write(&file.path, find::apply_edits(&text, &edits)) {
                    self.note(format!("Could not write {}: {e}", file.path.display()));
                    continue;
                }
            }
            places += file.edits.len();
            count += 1;
        }
        // The form of each unit that changed: its component and the properties that name it.
        let mut form_refs = 0;
        for unit in &units {
            form_refs += self.rename_in_form(unit, &sent.old, &sent.new);
        }
        let mut note = format!("Renamed to {} in {count} files ({places} places)", sent.new);
        if form_refs > 0 {
            note.push_str(&format!(", and {form_refs} in its form"));
        }
        self.note(note);
    }

    /// Renames `old` in the form of `unit` (its `.lfm` or `.dfm`): the component and every property
    /// that names it. The open designer takes it as one undo step, an open `.lfm` tab as unsaved
    /// text, and otherwise the file is written. Returns how many names and references changed.
    fn rename_in_form(&mut self, unit: &Path, old: &str, new: &str) -> usize {
        let Some(form_path) = ["lfm", "dfm"]
            .iter()
            .map(|e| unit.with_extension(e))
            .find(|p| p.is_file())
        else {
            return 0;
        };
        if let Some(d) = self.designers.iter_mut().find(|d| d.pas_path == unit) {
            let Ok(mut form) = pascoda_forms::FormDoc::parse(d.doc.source()) else { return 0 };
            let Ok(n) = form.rename_symbol(old, new) else { return 0 };
            if n > 0 && d.replace_source(form.source()).is_err() {
                return 0;
            }
            return n;
        }
        let text = match self.docs.iter().find(|d| d.path == form_path) {
            Some(d) => d.text.clone(),
            None => match std::fs::read_to_string(&form_path) {
                Ok(t) => t,
                Err(_) => return 0,
            },
        };
        let Ok(mut form) = pascoda_forms::FormDoc::parse(&text) else { return 0 };
        let Ok(n) = form.rename_symbol(old, new) else { return 0 };
        if n == 0 {
            return 0;
        }
        if let Some(d) = self.docs.iter_mut().find(|d| d.path == form_path) {
            d.text = form.source().to_string();
        } else if let Err(e) = std::fs::write(&form_path, form.source()) {
            self.note(format!("Could not write {}: {e}", form_path.display()));
            return 0;
        }
        n
    }

    /// The Go to Line box (Cmd+L): a line, or a line and column, in the active file.
    fn show_go_to_line(&mut self, ctx: &egui::Context) {
        if !self.go_to.open {
            return;
        }
        let mut go = false;
        let mut close = false;
        egui::Window::new("Go to Line")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 90.0])
            .show(ctx, |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.go_to.text)
                        .id(egui::Id::unique("go-to-line"))
                        .hint_text("Line or line:column")
                        .desired_width(220.0),
                );
                if self.go_to.focus {
                    field.request_focus();
                    self.go_to.focus = false;
                }
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Go").clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                if !self.go_to.error.is_empty() {
                    ui.label(RichText::new(&self.go_to.error).color(egui::Color32::from_rgb(200, 60, 50)));
                }
            });
        if go {
            let Some(doc) = self.docs.get(self.active) else {
                self.go_to.error = "Open a file first".into();
                return;
            };
            let lines = doc.text.split('\n').count() as u32;
            match find::parse_position(&self.go_to.text, lines) {
                Ok((line, col)) => {
                    let path = doc.path.clone();
                    self.go_to.open = false;
                    self.go_to.error.clear();
                    self.goto(ctx, &path, line, col);
                }
                Err(e) => self.go_to.error = e,
            }
        } else if close {
            self.go_to.open = false;
            self.go_to.error.clear();
        }
    }

    /// The project search (the navigator's Search tab): find across the project's files, and replace
    /// in them. Rows fill the sidebar's width; the results list takes the rest of its height.
    fn search_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let (mut run, mut replace) = (false, false);
        let mut open: Option<(PathBuf, u32, u32)> = None;
        ui.add_space(4.0);
        let bar = &mut self.project_bar;
        let field = ui.add(
            egui::TextEdit::singleline(&mut bar.query)
                .id(egui::Id::unique("project-query"))
                .hint_text("Search the project")
                .desired_width(f32::INFINITY),
        );
        if bar.focus {
            field.request_focus();
            bar.focus = false;
        }
        if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            run = true;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui.selectable_label(bar.opts.case, "Aa").on_hover_text("Match case").clicked() {
                bar.opts.case = !bar.opts.case;
            }
            if ui.selectable_label(bar.opts.word, "Whole word").clicked() {
                bar.opts.word = !bar.opts.word;
            }
            if ui.button("Search").clicked() {
                run = true;
            }
        });
        let status = if !bar.title.is_empty() {
            bar.title.clone()
        } else if !bar.searched {
            String::new()
        } else if bar.results.is_empty() {
            "No results".to_string()
        } else {
            let n: usize = bar.results.iter().map(|f| f.hits.len()).sum();
            format!("{n} results in {} files", bar.results.len())
        };
        ui.label(RichText::new(status).size(12.0).color(pal.text_secondary));
        ui.add(
            egui::TextEdit::singleline(&mut bar.with)
                .id(egui::Id::unique("project-replace"))
                .hint_text("Replace with")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let enabled = !bar.results.is_empty();
            if ui
                .add_enabled(enabled, egui::Button::new("Replace All in Project"))
                .on_hover_text("Changes the files on disk, and the open tabs (save them to keep the change)")
                .clicked()
            {
                replace = true;
            }
            if !bar.note.is_empty() {
                ui.label(RichText::new(&bar.note).size(12.0).color(pal.text_secondary));
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for file in &self.project_bar.results {
                let shown = file
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&file.path)
                    .to_string_lossy()
                    .into_owned();
                ui.label(RichText::new(format!("{shown}  ({})", file.hits.len())).strong());
                for hit in &file.hits {
                    // One line per result: the text is cut to the row's width, with "...".
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let full = format!("{:>5}:{:<4} {}", hit.line, hit.col, one_line(&hit.text));
                    let fitted = fit_one_line(ui, &full, &font, ui.available_width() - 12.0);
                    let row = ui.selectable_label(false, RichText::new(fitted).monospace());
                    if row.clicked() {
                        open = Some((file.path.clone(), hit.line, hit.col));
                    }
                }
            }
            if self.project_bar.truncated {
                ui.label(
                    RichText::new(format!("Only the first {} matches are listed", find::PROJECT_LIMIT))
                        .color(pal.text_tertiary),
                );
            }
        });
        if run {
            self.run_project_search();
        }
        if replace {
            self.replace_in_project();
        }
        if let Some((path, line, col)) = open {
            let ctx = ui.ctx().clone();
            self.goto(&ctx, &path, line, col);
        }
    }

    /// The text of every project file the tree lists, with the unsaved text of open tabs.
    fn project_texts(&self) -> Vec<(PathBuf, String)> {
        let mut files = Vec::new();
        collect_files(&self.root, 0, &mut files);
        files
            .into_iter()
            .filter_map(|path| {
                if let Some(d) = self.docs.iter().find(|d| d.path == path) {
                    return Some((path, d.text.clone()));
                }
                // Big or non-text files are not searched.
                if std::fs::metadata(&path).ok()?.len() > 2_000_000 {
                    return None;
                }
                let text = std::fs::read_to_string(&path).ok()?;
                Some((path, text))
            })
            .collect()
    }

    pub(crate) fn run_project_search(&mut self) {
        let texts = self.project_texts();
        let bar = &mut self.project_bar;
        let (results, truncated) = find::project_hits(
            texts.iter().map(|(p, t)| (p.clone(), t.as_str())),
            &bar.query,
            bar.opts,
            find::PROJECT_LIMIT,
        );
        bar.results = results;
        bar.title.clear();
        bar.truncated = truncated;
        bar.searched = true;
        bar.note.clear();
    }

    /// Replaces every match in the project. An open tab takes the change in its text (unsaved, as
    /// typing is); other files are written.
    fn replace_in_project(&mut self) {
        let texts = self.project_texts();
        let (query, with, opts) = (
            self.project_bar.query.clone(),
            self.project_bar.with.clone(),
            self.project_bar.opts,
        );
        let changed = find::replace_in_files(texts, &query, &with, opts);
        let (mut total, mut files) = (0, 0);
        for (path, text, n) in changed {
            if let Some(d) = self.docs.iter_mut().find(|d| d.path == path) {
                d.text = text;
            } else if let Err(e) = std::fs::write(&path, &text) {
                self.note(format!("Could not write {}: {e}", path.display()));
                continue;
            }
            total += n;
            files += 1;
        }
        self.run_project_search();
        self.project_bar.note = format!("Replaced {total} in {files} files");
    }

    fn bottom_tabs(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let problems = if self.diagnostics.is_empty() {
            "Problems".to_string()
        } else {
            format!("Problems ({})", self.diagnostics.len())
        };
        let tests = match self.unit_tests.failures + self.unit_tests.errors {
            0 => "Tests".to_string(),
            n => format!("Tests ({n})"),
        };
        let names = [problems.as_str(), "Output", "Debug", tests.as_str(), "Terminal"];
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

    /// Asks for the folder of a unit that the build could not find, and adds it to the search path.
    fn choose_search_path(&mut self, unit: &str) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title(format!("Choose the folder that has unit {unit}"))
            .pick_folder()
        else {
            return;
        };
        self.add_search_path(&folder);
    }

    /// Adds `folder` to the project's unit search path in `pascoda.toml` (relative to the project when it
    /// is inside it). The next build finds the units there.
    pub(crate) fn add_search_path(&mut self, folder: &Path) {
        let root = self.root.clone();
        let stored = match folder.strip_prefix(&root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
            _ => folder.to_string_lossy().into_owned(),
        };
        let mut settings = pascoda_project::load_compiler(&root);
        if settings.unit_paths.iter().any(|p| *p == stored) {
            self.console.push_str(&format!("{stored} is already on the unit search path\n"));
            return;
        }
        settings.unit_paths.push(stored.clone());
        match pascoda_project::save_compiler(&root, &settings) {
            Ok(()) => self.console.push_str(&format!(
                "Added {stored} to the unit search path (pascoda.toml). Build again.\n"
            )),
            Err(e) => self.console.push_str(&format!("could not save the search path: {e}\n")),
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
                self.watch_panel(ui, pal);
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

    /// The watch box: add an expression to watch (its value is listed under Watches at every stop),
    /// evaluate one once while stopped, and remove watches.
    fn watch_panel(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let stopped = self.paused.is_some();
        let mut add = false;
        let mut evaluate = false;
        let mut remove = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let label = ui.label(RichText::new("Watch").size(12.0).color(pal.text_secondary));
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.watch_input)
                        .id(egui::Id::unique("watch-input"))
                        .hint_text("Expression")
                        .desired_width(200.0),
                )
                .labelled_by(label.id);
            if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                add = true;
            }
            let typed = !self.watch_input.trim().is_empty();
            if ui.add_enabled(typed, egui::Button::new("Add")).clicked() {
                add = true;
            }
            if ui
                .add_enabled(typed && stopped, egui::Button::new("Evaluate"))
                .on_hover_text("Shows the value once, without adding a watch")
                .clicked()
            {
                evaluate = true;
            }
        });
        if !self.watch_list.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (i, expr) in self.watch_list.iter().enumerate() {
                    ui.label(RichText::new(expr).monospace().size(12.0));
                    if text_button(ui, pal, "×").on_hover_text(format!("Remove the watch {expr}")).clicked() {
                        remove = Some(i);
                    }
                }
            });
        }
        if let Some((expr, result)) = &self.watch_eval {
            let text = match result {
                Ok(v) => format!("{expr} = {}", v.value),
                Err(e) => format!("{expr}: {e}"),
            };
            ui.label(RichText::new(text).monospace().size(12.0).color(pal.text_secondary));
        }
        if add {
            let text = self.watch_input.trim().to_string();
            self.watch_input.clear();
            if !text.is_empty() && !self.watch_list.contains(&text) {
                let mut list = self.watch_list.clone();
                list.push(text);
                self.set_watches(list);
            }
        }
        if evaluate {
            let text = self.watch_input.trim().to_string();
            self.send(debugger::Cmd::Evaluate(text));
        }
        if let Some(i) = remove {
            let mut list = self.watch_list.clone();
            list.remove(i);
            self.set_watches(list);
        }
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
        // Something asked for a command at the prompt (see `type_in_terminal`): typed, not run.
        if let Some(text) = self.bottom.pending_input.take() {
            term.write(text.as_bytes());
        }
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
/// The editor's find bar: the query, the match count, previous/next, case and whole-word options,
/// and (when shown) the replacement row. Returns what the user asked for; `code_editor` acts on it.
fn find_bar(ui: &mut egui::Ui, pal: &Palette, bar: &mut find::Bar, found: usize) -> Option<FindAction> {
    let mut action = None;
    let mut close = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let field = ui.add(
            egui::TextEdit::singleline(&mut bar.query)
                .id(egui::Id::unique("find-query"))
                .hint_text("Find")
                .desired_width(220.0),
        );
        if bar.focus {
            field.request_focus();
            bar.focus = false;
        }
        if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            action = Some(FindAction::Go(!ui.input(|i| i.modifiers.shift)));
        }
        if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if field.changed() {
            bar.current = None;
        }
        let status = if bar.query.is_empty() {
            String::new()
        } else if found == 0 {
            "No results".to_string()
        } else {
            match bar.current {
                Some(i) if i < found => format!("{} of {found}", i + 1),
                _ => format!("{found} found"),
            }
        };
        ui.label(RichText::new(status).size(12.0).color(pal.text_secondary));
        if text_button(ui, pal, "‹").on_hover_text("Previous match (Shift+Cmd+G)").clicked() {
            action = Some(FindAction::Go(false));
        }
        if text_button(ui, pal, "›").on_hover_text("Next match (Cmd+G)").clicked() {
            action = Some(FindAction::Go(true));
        }
        let mut changed = false;
        if ui.selectable_label(bar.opts.case, "Aa").on_hover_text("Match case").clicked() {
            bar.opts.case = !bar.opts.case;
            changed = true;
        }
        if ui.selectable_label(bar.opts.word, "Whole word").clicked() {
            bar.opts.word = !bar.opts.word;
            changed = true;
        }
        if changed {
            bar.current = None;
        }
        if ui.selectable_label(bar.replace, "Replace").clicked() {
            bar.replace = !bar.replace;
        }
        if text_button(ui, pal, "×").on_hover_text("Close the find bar (Esc)").clicked() {
            close = true;
        }
    });
    if bar.replace {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add(
                egui::TextEdit::singleline(&mut bar.with)
                    .id(egui::Id::unique("find-replace"))
                    .hint_text("Replace with")
                    .desired_width(220.0),
            );
            if ui.button("Replace").clicked() {
                action = Some(FindAction::ReplaceOne);
            }
            if ui.button("Replace All").clicked() {
                action = Some(FindAction::ReplaceAll);
            }
            if !bar.note.is_empty() {
                ui.label(RichText::new(&bar.note).size(12.0).color(pal.text_secondary));
            }
        });
    }
    if close {
        bar.open = false;
        bar.replace = false;
        bar.current = None;
    }
    action
}

/// What the find bar asked for.
enum FindAction {
    /// The next match (true) or the previous one (false).
    Go(bool),
    ReplaceOne,
    ReplaceAll,
}

/// Selects the characters `r` in the editor `id`.
fn select_in_editor(ctx: &egui::Context, id: egui::Id, r: &std::ops::Range<usize>) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::two(CCursor::new(r.start), CCursor::new(r.end))));
    state.store(ctx, id);
}

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
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
    // Every row can be dragged onto a folder; the payload is the item's path.
    resp.dnd_set_drag_payload(path.to_path_buf());
    let painter = ui.painter();
    let inner = rect.shrink2(vec2(6.0, 0.0));
    if selected {
        painter.rect_filled(inner, 5.0, pal.row_selected);
    } else if resp.hovered() || (is_dir && resp.dnd_hover_payload::<PathBuf>().is_some()) {
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

const TREE_EXTENSIONS: &[&str] = crate::files::TREE_EXTENSIONS;
const HIDDEN_DIRS: &[&str] = crate::files::HIDDEN_DIRS;

/// What the project tree asked for while it was drawn; applied once drawing is done.
pub(crate) enum TreeAction {
    Select(PathBuf),
    NewIn { dir: PathBuf, is_dir: bool },
    AskRename(PathBuf),
    RenameTo { path: PathBuf, name: String },
    CancelRename,
    Create { dir: PathBuf, is_dir: bool, name: String },
    CancelNew,
    AskDelete(PathBuf),
    ConfirmDelete(PathBuf),
    MoveInto { path: PathBuf, dir: PathBuf },
    NewForm { dir: PathBuf },
    NewDataModule { dir: PathBuf },
}

/// A name being typed for a new file or folder: an editable row at the top of `dir`.
pub(crate) struct TreeNew {
    dir: PathBuf,
    is_dir: bool,
    name: String,
    focus: bool,
}

/// An item being renamed: its name field replaces its row.
pub(crate) struct TreeRename {
    path: PathBuf,
    name: String,
    focus: bool,
}

#[cfg(test)]
impl TreeRename {
    /// An item being renamed, with its current name in the field.
    pub(crate) fn new(path: PathBuf, name: &str) -> Self {
        Self {
            path,
            name: name.to_string(),
            focus: false,
        }
    }
}

#[cfg(test)]
impl TreeNew {
    /// A new item being named in `dir`, with the field not yet focused.
    pub(crate) fn new(dir: PathBuf, is_dir: bool) -> Self {
        Self {
            dir,
            is_dir,
            name: String::new(),
            focus: false,
        }
    }
}

/// The state the project tree reads and writes while it is drawn.
struct TreeView<'a> {
    active: Option<&'a Path>,
    sel: Option<&'a Path>,
    expanded: &'a HashSet<PathBuf>,
    new: &'a mut Option<TreeNew>,
    rename: &'a mut Option<TreeRename>,
    toggles: Vec<PathBuf>,
    actions: Vec<TreeAction>,
    open: Option<PathBuf>,
}

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

/// A line of source as one line of text: tabs and carriage returns become spaces, the ends are
/// trimmed.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\t' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// `text` cut to the longest prefix that, with "...", fits `max_width` points in `font` on one line.
/// Text that fits is returned unchanged.
pub(crate) fn fit_one_line(ui: &egui::Ui, text: &str, font: &egui::FontId, max_width: f32) -> String {
    let width = |s: &str| {
        ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), egui::Color32::WHITE).size().x)
    };
    if width(text) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "...";
        if width(&candidate) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "..."
}

/// A unit's name as its `unit` line declares it: `Unit2` for `unit2.pas`.
pub(crate) fn unit_name(pas: &Path) -> String {
    let stem = pas.file_stem().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    match stem.chars().next() {
        Some(c) => c.to_ascii_uppercase().to_string() + &stem[c.len_utf8()..],
        None => stem,
    }
}

/// Whether a path is a Pascal unit file (`.pas` or `.pp`).
fn is_unit_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pas") || e.eq_ignore_ascii_case("pp"))
}

/// A file's name, for a note.
fn unit_name_or_file(p: &Path) -> String {
    p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// The Pascal units (`.pas`, `.pp`) a deletion of `path` removes, each with its unit name and the
/// form it declares (read from its `.lfm` or `.dfm`, if any). A folder gives every unit in it; a
/// file gives its own unit or, for a form file, the unit it goes with.
fn form_units_under(path: &Path) -> Vec<(PathBuf, String, Option<(String, String)>)> {
    let files = if path.is_dir() {
        let mut all = Vec::new();
        collect_files(path, 0, &mut all);
        all
    } else {
        files::removed_with(path)
    };
    files
        .into_iter()
        .filter(|p| is_unit_file(p))
        .map(|pas| {
            let unit = unit_name(&pas);
            let form = ["lfm", "dfm"].iter().find_map(|ext| {
                std::fs::read_to_string(pas.with_extension(ext))
                    .ok()
                    .and_then(|t| pascoda_project::program::form_of_lfm(&t))
            });
            (pas, unit, form)
        })
        .collect()
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
fn tree_rows(ui: &mut egui::Ui, pal: &Palette, dir: &Path, depth: usize, is_root: bool, view: &mut TreeView) {
    let is_open = view.expanded.contains(dir);
    let resp = row(ui, pal, depth, true, is_open, dir, view.sel == Some(dir));
    let renaming = view.rename.as_ref().is_some_and(|r| r.path == dir);
    if renaming {
        rename_overlay(ui, pal, resp.rect, depth, view);
    } else if resp.clicked() {
        view.toggles.push(dir.to_path_buf());
        view.actions.push(TreeAction::Select(dir.to_path_buf()));
    }
    if let Some(from) = resp.dnd_release_payload::<PathBuf>() {
        view.actions.push(TreeAction::MoveInto { path: (*from).clone(), dir: dir.to_path_buf() });
    }
    context_menu(ui, &resp, |ui| {
        if ui.button("New File").clicked() {
            view.actions.push(TreeAction::NewIn { dir: dir.to_path_buf(), is_dir: false });
            ui.close();
        }
        if ui.button("New Folder").clicked() {
            view.actions.push(TreeAction::NewIn { dir: dir.to_path_buf(), is_dir: true });
            ui.close();
        }
        if ui.button("New Form").clicked() {
            view.actions.push(TreeAction::NewForm { dir: dir.to_path_buf() });
            ui.close();
        }
        if ui.button("New Data Module").clicked() {
            view.actions.push(TreeAction::NewDataModule { dir: dir.to_path_buf() });
            ui.close();
        }
        if !is_root {
            if ui.button("Rename").clicked() {
                view.actions.push(TreeAction::AskRename(dir.to_path_buf()));
                ui.close();
            }
            ui.separator();
            if ui.button("Move to Trash").clicked() {
                view.actions.push(TreeAction::AskDelete(dir.to_path_buf()));
                ui.close();
            }
        }
    });
    if !is_open || depth > 8 {
        return;
    }
    if view.new.as_ref().is_some_and(|n| n.dir == dir) {
        new_name_row(ui, pal, depth + 1, view);
    }
    for p in listing(dir) {
        if p.is_dir() {
            tree_rows(ui, pal, &p, depth + 1, false, view);
        } else {
            let resp = row(
                ui,
                pal,
                depth + 1,
                false,
                false,
                &p,
                view.active == Some(p.as_path()) || view.sel == Some(p.as_path()),
            );
            let renaming = view.rename.as_ref().is_some_and(|r| r.path == p);
            if renaming {
                rename_overlay(ui, pal, resp.rect, depth + 1, view);
            } else if resp.clicked() {
                view.open = Some(p.clone());
                view.actions.push(TreeAction::Select(p.clone()));
            }
            context_menu(ui, &resp, |ui| {
                if ui.button("Rename").clicked() {
                    view.actions.push(TreeAction::AskRename(p.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button("Move to Trash").clicked() {
                    view.actions.push(TreeAction::AskDelete(p.clone()));
                    ui.close();
                }
            });
        }
    }
}

/// What an editable name field did this frame.
enum NameEdit {
    Submit(String),
    Cancel,
    Editing,
}

/// The area of a row where its name goes, at `depth`.
fn name_rect(row: Rect, depth: usize) -> Rect {
    let x = row.left() + 12.0 + depth as f32 * 14.0 + 34.0;
    Rect::from_min_max(
        egui::pos2(x, row.top() + 2.0),
        egui::pos2(row.right() - 8.0, row.bottom() - 2.0),
    )
}

/// A name field in `rect`. Enter or clicking away submits it (an empty name cancels); Escape
/// cancels.
fn name_field(
    ui: &mut egui::Ui,
    pal: &Palette,
    rect: Rect,
    name: &mut String,
    focus: &mut bool,
    hint: &str,
) -> NameEdit {
    let edit = ui.put(
        rect,
        egui::TextEdit::singleline(name)
            .hint_text(hint)
            .text_color(pal.text)
            .desired_width(f32::INFINITY),
    );
    if std::mem::take(focus) {
        edit.request_focus();
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        NameEdit::Cancel
    } else if edit.lost_focus() {
        if name.trim().is_empty() {
            NameEdit::Cancel
        } else {
            NameEdit::Submit(name.clone())
        }
    } else {
        NameEdit::Editing
    }
}

/// The name field of a file or folder being created, as a row of its own at the top of `dir`.
fn new_name_row(ui: &mut egui::Ui, pal: &Palette, depth: usize, view: &mut TreeView) {
    let Some(new) = view.new.as_mut() else {
        return;
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let hint = if new.is_dir { "Folder name" } else { "File name" };
    match name_field(ui, pal, name_rect(rect, depth), &mut new.name, &mut new.focus, hint) {
        NameEdit::Submit(name) => view.actions.push(TreeAction::Create {
            dir: new.dir.clone(),
            is_dir: new.is_dir,
            name,
        }),
        NameEdit::Cancel => view.actions.push(TreeAction::CancelNew),
        NameEdit::Editing => {}
    }
}

/// The name field over the row of an item being renamed.
fn rename_overlay(ui: &mut egui::Ui, pal: &Palette, row: Rect, depth: usize, view: &mut TreeView) {
    let Some(rename) = view.rename.as_mut() else {
        return;
    };
    match name_field(
        ui,
        pal,
        name_rect(row, depth),
        &mut rename.name,
        &mut rename.focus,
        "Name",
    ) {
        NameEdit::Submit(name) => view.actions.push(TreeAction::RenameTo {
            path: rename.path.clone(),
            name,
        }),
        NameEdit::Cancel => view.actions.push(TreeAction::CancelRename),
        NameEdit::Editing => {}
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
    // A scalar value can be changed while the program is stopped (a watch is not a variable).
    let scalar = matches!(v.kind, debugger::Kind::Integer | debugger::Kind::Float | debugger::Kind::Boolean | debugger::Kind::Char);
    let in_watches = path.starts_with(&format!("{}{}", debugger::WATCH_SCOPE, debugger::PATH_SEP));
    if scalar && !expandable && !in_watches && !v.name.contains('*') {
        // lldb names the variable by its path below the scope: `POINT.X` for `Globals/POINT/X`.
        let target = path.split(debugger::PATH_SEP).skip(1).collect::<Vec<_>>().join(".");
        let text = if v.kind == debugger::Kind::Char {
            v.value.trim_matches('\'').to_string()
        } else {
            v.value.clone()
        };
        context_menu(ui, &resp, |ui| {
            if ui.button("Change Value...").clicked() {
                p.value_edit_request = Some(ValueEdit {
                    target: target.clone(),
                    name: name.clone(),
                    kind: v.kind,
                    text: text.clone(),
                    error: String::new(),
                });
                ui.close();
            }
        });
    }
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
        // One allocated row takes the click, as the navigator's rows do: the marker, the position and
        // the message are painted into it. The message wraps to the panel's width.
        let width = ui.available_width();
        let position = ui.painter().layout_no_wrap(
            format!("{name}:{}:{}", d.line, d.col),
            egui::FontId::proportional(12.5),
            pal.text_secondary,
        );
        let message = ui.painter().layout(
            format!("{tag}: {}", d.message),
            egui::FontId::proportional(12.5),
            pal.text,
            (width - 40.0 - position.size().x).max(80.0),
        );
        let height = message.size().y.max(position.size().y) + 4.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 4.0, pal.row_hover);
        }
        // The row is announced by its message (its marker and position are not text to read).
        let label = format!("{tag}: {}", d.message);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
        let mid = rect.top() + height / 2.0;
        ui.painter().circle_filled(egui::pos2(rect.left() + 10.0, mid), 5.0, color);
        let at = egui::pos2(rect.left() + 22.0, mid - position.size().y / 2.0);
        ui.painter().galley(at, position.clone(), pal.text_secondary);
        let at = egui::pos2(at.x + position.size().x + 8.0, mid - message.size().y / 2.0);
        ui.painter().galley(at, message, pal.text);
        if resp.clicked() {
            p.goto = Some((d.file.clone(), d.line, d.col));
        }
        // fpc cannot find a unit: the folder that has it can be added to the search path.
        if let Some(unit) = missing_unit(&d.message) {
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                if ui.small_button(format!("Add the folder of unit {unit}...")).clicked() {
                    p.search_path_unit = Some(unit.to_string());
                }
            });
        }
    }
}

/// The unit fpc could not find, from its message `Can't find unit helper used by app`.
pub(crate) fn missing_unit(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("Can't find unit ")?;
    rest.split_whitespace().next()
}

/// Line numbers and breakpoints. Breakpoints are Xcode's blue arrow-shaped chips; click a
/// number to toggle one. Returns true when a click changed them.
fn gutter(
    ui: &mut egui::Ui,
    pal: &Palette,
    map: &LineMap,
    breakpoints: &mut BTreeSet<u32>,
    disabled: &BTreeSet<u32>,
    exec_line: Option<u32>,
    caret_line: Option<u32>,
    flagged: &std::collections::BTreeMap<u32, bool>,
    edit: &mut Option<u32>,
) -> bool {
    let size = vec2(GUTTER_W, map.height());
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter_at(rect);
    let clip = ui.clip_rect();
    let lh = crate::prefs::line_height();
    for line in 1..=map.lines() as u32 {
        if rect.top() + map.bottom(line) < clip.top() {
            continue;
        }
        if rect.top() + map.top(line) > clip.bottom() {
            break;
        }
        let y = rect.top() + map.top(line);
        let row = egui::Rect::from_min_size(egui::pos2(rect.left(), y), vec2(rect.width(), crate::prefs::line_height()));
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
                // A disabled breakpoint stays in the gutter, faded.
                if disabled.contains(&line) { pal.breakpoint.gamma_multiply(0.35) } else { pal.breakpoint },
                egui::Stroke::NONE,
            ));
            if crate::prefs::line_numbers() {
                painter.text(
                    egui::pos2(r - 11.0, row.center().y),
                    egui::Align2::RIGHT_CENTER,
                    line.to_string(),
                    font,
                    egui::Color32::WHITE,
                );
            }
        } else if crate::prefs::line_numbers() {
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
        if line as usize <= map.lines() {
            let y = rect.top() + map.top(line) + lh / 2.0;
            let color = if error { pal.error } else { pal.warning };
            painter.circle_filled(egui::pos2(rect.right() - 5.0, y), 3.0, color);
        }
    }
    let Some(pos) = response
        .interact_pointer_pos()
        .filter(|_| response.clicked() || response.secondary_clicked())
    else {
        return false;
    };
    let line = map.line_at(pos.y - rect.top());
    // A secondary click sets the breakpoint (if it is not there) and opens its condition box.
    if response.secondary_clicked() {
        breakpoints.insert(line);
        *edit = Some(line);
        return true;
    }
    if !breakpoints.remove(&line) {
        breakpoints.insert(line);
    }
    true
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    /// A fresh project folder holding `debug.pas`.
    fn project(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-tree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("debug.pas"), "program debug;\nbegin\nend.\n").unwrap();
        d
    }

    #[test]
    fn a_new_file_is_named_then_created_and_opened() {
        let root = project("new");
        let mut app = App::bare(Some(root.join("debug.pas")));
        let mut p = Pending::default();
        app.apply_tree_actions(
            vec![TreeAction::NewIn { dir: root.clone(), is_dir: false }],
            &mut p,
        );
        assert!(app.tree_new.as_ref().is_some_and(|n| n.focus && n.dir == root));
        app.apply_tree_actions(
            vec![TreeAction::Create {
                dir: root.clone(),
                is_dir: false,
                name: "unit2".into(),
            }],
            &mut p,
        );
        assert!(app.tree_new.is_none());
        assert!(root.join("unit2.pas").is_file());
        assert_eq!(p.open.as_deref(), Some(root.join("unit2.pas").as_path()));
        assert_eq!(app.tree_sel.as_deref(), Some(root.join("unit2.pas").as_path()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bad_name_is_reported_and_nothing_is_created() {
        let root = project("bad");
        let mut app = App::bare(Some(root.join("debug.pas")));
        let mut p = Pending::default();
        app.apply_tree_actions(
            vec![TreeAction::Create {
                dir: root.clone(),
                is_dir: false,
                name: "notes.exe".into(),
            }],
            &mut p,
        );
        assert!(!root.join("notes.exe").exists());
        assert!(p.open.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn deleting_a_folder_closes_its_tabs_and_forgets_its_selection() {
        // The app's root is canonical (`/private/var` on macOS): build paths from it.
        let project_dir = project("delete");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("inner.pas"), "unit inner;\n").unwrap();
        app.open_path(sub.join("inner.pas"));
        assert_eq!(app.docs.len(), 2);
        app.tree_sel = Some(sub.join("inner.pas"));

        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::AskDelete(sub.clone())], &mut p);
        assert_eq!(app.tree_delete.as_deref(), Some(sub.as_path()));
        assert!(sub.exists(), "asking deletes nothing");

        app.apply_tree_actions(vec![TreeAction::ConfirmDelete(sub.clone())], &mut p);
        assert!(!sub.exists());
        assert!(app.tree_delete.is_none());
        assert_eq!(app.docs.len(), 1, "the tab of the deleted file closed");
        assert!(app.tree_sel.is_none());
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn the_project_folder_is_never_deleted() {
        let project_dir = project("root");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::ConfirmDelete(root.clone())], &mut p);
        assert!(root.join("debug.pas").exists());
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn renaming_an_open_file_keeps_its_tab_and_selection() {
        let project_dir = project("rename-open");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::AskRename(root.join("debug.pas"))], &mut p);
        assert!(app.tree_rename.as_ref().is_some_and(|r| r.name == "debug.pas" && r.focus));
        app.apply_tree_actions(
            vec![TreeAction::RenameTo {
                path: root.join("debug.pas"),
                name: "main".into(),
            }],
            &mut p,
        );
        assert!(app.tree_rename.is_none());
        assert!(root.join("main.pas").is_file() && !root.join("debug.pas").exists());
        assert_eq!(app.docs[0].path, root.join("main.pas"));
        assert_eq!(app.tree_sel.as_deref(), Some(root.join("main.pas").as_path()));
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn renaming_a_folder_moves_the_open_files_and_the_expansion_under_it() {
        let project_dir = project("rename-dir");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("inner.pas"), "unit inner;\n").unwrap();
        app.open_path(sub.join("inner.pas"));
        app.expanded.insert(sub.clone());
        let mut p = Pending::default();
        app.apply_tree_actions(
            vec![TreeAction::RenameTo {
                path: sub.clone(),
                name: "other".into(),
            }],
            &mut p,
        );
        assert!(root.join("other/inner.pas").is_file());
        assert_eq!(app.docs[1].path, root.join("other/inner.pas"));
        assert!(app.expanded.contains(&root.join("other")));
        assert!(!app.expanded.contains(&sub));
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn a_rename_changes_open_tabs_and_files_on_disk_and_is_refused_after_an_edit() {
        let root = project("rename-apply");
        std::fs::write(root.join("other.pas"), "total := 1;\n").unwrap();
        let mut app = App::bare(Some(root.join("debug.pas")));
        let root = app.root.clone();
        app.docs[0].text = "program debug;\nvar total: Integer;\nbegin\n    total := 2;\nend.\n".into();
        let sent = find::Sent {
            old: "total".into(),
            new: "sum".into(),
            open: vec![(root.join("debug.pas"), app.docs[0].text.clone())],
        };
        let edits = || {
            vec![
                pascoda_lsp::FileEdit {
                    path: root.join("debug.pas"),
                    edits: vec![pascoda_lsp::TextEdit { start: (1, 4), end: (1, 9), new_text: "sum".into() }, pascoda_lsp::TextEdit { start: (3, 4), end: (3, 9), new_text: "sum".into() }],
                },
                pascoda_lsp::FileEdit {
                    path: root.join("other.pas"),
                    edits: vec![pascoda_lsp::TextEdit { start: (0, 0), end: (0, 5), new_text: "sum".into() }],
                },
            ]
        };
        app.apply_rename(edits(), &sent);
        assert_eq!(app.docs[0].text, "program debug;\nvar sum: Integer;\nbegin\n    sum := 2;\nend.\n", "the open tab takes it, unsaved");
        assert_eq!(std::fs::read_to_string(root.join("debug.pas")).unwrap(), "program debug;\nbegin\nend.\n", "the disk is left for the save");
        assert_eq!(std::fs::read_to_string(root.join("other.pas")).unwrap(), "sum := 1;\n");
        assert_eq!(app.notice().map(|(m, _)| m.to_string()).as_deref(), Some("Renamed to sum in 2 files (3 places)"));

        // An open file edited after the rename was asked: nothing is renamed.
        app.docs[0].text.push_str("typed\n");
        let before = app.docs[0].text.clone();
        app.apply_rename(edits(), &find::Sent { new: "again".into(), ..sent });
        assert_eq!(app.docs[0].text, before);
        assert_eq!(std::fs::read_to_string(root.join("other.pas")).unwrap(), "sum := 1;\n", "not renamed twice");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A GUI project whose form has a button and a label anchored to it.
    fn form_project(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("pascoda-form-rename-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "GuiApp1").unwrap();
        let lfm = "object Form1: TForm1\n  object Button1: TButton\n    OnClick = Button1Click\n  end\n  object Label1: TLabel\n    AnchorSideLeft.Control = Button1\n  end\nend\n";
        std::fs::write(base.join("GuiApp1").join("unit1.lfm"), lfm).unwrap();
        base.join("GuiApp1")
    }

    fn rename_sent(old: &str, new: &str) -> find::Sent {
        find::Sent { old: old.into(), new: new.into(), open: Vec::new() }
    }

    #[test]
    fn a_rename_follows_into_the_form_file_on_disk() {
        let root = form_project("disk");
        let mut app = App::bare(Some(root.join("unit1.pas")));
        let root = app.root.clone();
        app.apply_rename(
            vec![pascoda_lsp::FileEdit { path: root.join("unit1.pas"), edits: Vec::new() }],
            &rename_sent("Button1", "OkButton"),
        );
        let lfm = std::fs::read_to_string(root.join("unit1.lfm")).unwrap();
        assert!(lfm.contains("object OkButton: TButton"), "{lfm}");
        assert!(lfm.contains("AnchorSideLeft.Control = OkButton"), "{lfm}");
        assert!(lfm.contains("OnClick = Button1Click"), "handlers keep their names");
        assert!(app.notice().is_some_and(|(m, _)| m.contains("2 in its form")), "{:?}", app.notice().map(|(m, _)| m.to_string()));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_rename_follows_into_an_open_designer_as_one_undo_step() {
        let root = form_project("designer");
        let mut app = App::bare(Some(root.join("unit1.pas")));
        let root = app.root.clone();
        app.show_form();
        assert_eq!(app.designers.len(), 1, "the form is open in the designer");
        app.apply_rename(
            vec![pascoda_lsp::FileEdit { path: root.join("unit1.pas"), edits: Vec::new() }],
            &rename_sent("Button1", "OkButton"),
        );
        let source = app.designers[0].doc.source().to_string();
        assert!(source.contains("object OkButton: TButton") && source.contains("AnchorSideLeft.Control = OkButton"), "{source}");
        assert!(app.designers[0].dirty(), "the change is unsaved, as the designer's edits are");
        assert_eq!(std::fs::read_to_string(root.join("unit1.lfm")).unwrap().contains("OkButton"), false, "the disk waits for the save");
        // One undo takes the whole rename back.
        assert!(app.designers[0].undo());
        assert!(!app.designers[0].doc.source().contains("OkButton"), "one undo step for the whole rename");
        assert!(app.designers[0].doc.source().contains("object Button1: TButton"));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn references_list_each_use_with_its_line_and_open_the_search_tab() {
        let root = project("references");
        std::fs::write(root.join("other.pas"), "unit other;\nuses debug;\n  total := total + 1;\n").unwrap();
        let mut app = App::bare(Some(root.join("debug.pas")));
        let root = app.root.clone();
        app.docs[0].text = "var total: Integer;\nbegin\n    total := 2;\nend.\n".into();
        app.project_bar.title = "References to total".into();
        let locs = vec![
            pascoda_lsp::Location { path: root.join("debug.pas"), line: 0, col: 4 },
            pascoda_lsp::Location { path: root.join("debug.pas"), line: 2, col: 4 },
            pascoda_lsp::Location { path: root.join("other.pas"), line: 2, col: 2 },
        ];
        app.show_references(locs);
        let files: Vec<(String, usize)> = app
            .project_bar
            .results
            .iter()
            .map(|f| (f.path.file_name().unwrap().to_string_lossy().into_owned(), f.hits.len()))
            .collect();
        assert_eq!(files, [("debug.pas".to_string(), 2), ("other.pas".to_string(), 1)]);
        let hit = &app.project_bar.results[0].hits[1];
        assert_eq!((hit.line, hit.col, hit.text.trim()), (3, 5, "total := 2;"));
        assert_eq!(app.project_bar.results[1].hits[0].text.trim(), "total := total + 1;");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_search_sees_unsaved_text_and_replace_all_changes_tabs_and_files() {
        let root = project("project-search");
        std::fs::write(root.join("a.pas"), "foo\nfoo\n").unwrap();
        std::fs::write(root.join("b.txt"), "foo here\n").unwrap();
        let mut app = App::bare(Some(root.join("a.pas")));
        let root = app.root.clone();
        // The open tab has unsaved text: the search must see it, not the disk.
        app.docs[app.active].text = "foo foo foo".to_string();
        app.project_bar.query = "foo".into();
        app.project_bar.with = "bar".into();
        app.run_project_search();
        let total: usize = app.project_bar.results.iter().map(|f| f.hits.len()).sum();
        assert_eq!(total, 4, "three in the tab's unsaved text, one in b.txt");
        app.replace_in_project();
        assert_eq!(app.docs[app.active].text, "bar bar bar", "the tab takes the change, unsaved");
        assert_eq!(std::fs::read_to_string(root.join("a.pas")).unwrap(), "foo\nfoo\n", "the disk is not written for an open tab");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "bar here\n");
        assert_eq!(app.project_bar.note, "Replaced 4 in 2 files");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The breakpoint box: a condition and a hit count are kept with their line, a bad hit count is
    /// refused, and removing the breakpoint takes its condition away too.
    /// Breakpoints can be turned off and on in one place: an off one stays listed and is not sent to
    /// the debugger, and removing one takes it out of the list and its condition with it.
    #[test]
    fn breakpoints_turn_off_and_on_and_leave_the_list_when_removed() {
        let root = project("breakpoint-list");
        std::fs::write(root.join("loop.pas"), "program loop;\nbegin\nend.\n").unwrap();
        let mut app = App::bare(Some(root.join("loop.pas")));
        let path = app.docs[0].path.clone();
        app.docs[0].breakpoints.extend([2, 3]);
        app.save_breakpoint_edit(&path, 3, "i = 2", "").unwrap();

        app.set_breakpoint_enabled(&path, 2, false);
        assert!(app.docs[0].breakpoints.contains(&2), "still listed");
        assert!(app.docs[0].disabled.contains(&2));
        let sent = debugger::breakpoint_specs(&app.docs[0].breakpoints, &app.docs[0].stops, &app.docs[0].disabled);
        assert_eq!(sent.iter().map(|b| b.line).collect::<Vec<_>>(), [3], "only the enabled one goes to the debugger");

        app.set_breakpoint_enabled(&path, 2, true);
        assert!(!app.docs[0].disabled.contains(&2));
        app.remove_breakpoint(&path, 3);
        assert!(!app.docs[0].breakpoints.contains(&3));
        assert!(!app.docs[0].stops.contains_key(&3), "the condition goes with it");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_breakpoint_box_keeps_conditions_with_their_line() {
        let root = project("breakpoint-box");
        std::fs::write(root.join("loop.pas"), "program loop;\nbegin\nend.\n").unwrap();
        let mut app = App::bare(Some(root.join("loop.pas")));
        let path = app.docs[0].path.clone();
        app.save_breakpoint_edit(&path, 2, "i = 2", "").unwrap();
        assert!(app.docs[0].breakpoints.contains(&2));
        assert_eq!(app.docs[0].stops[&2].condition, "i = 2");

        assert!(app.save_breakpoint_edit(&path, 2, "", "0").is_err(), "a hit count starts at 1");
        assert!(app.save_breakpoint_edit(&path, 2, "", "many").is_err());
        assert_eq!(app.docs[0].stops[&2].condition, "i = 2", "a refused edit changes nothing");

        app.save_breakpoint_edit(&path, 2, "", "3").unwrap();
        assert_eq!(app.docs[0].stops[&2].hits, "3");
        assert!(app.docs[0].stops[&2].condition.is_empty());

        app.remove_breakpoint(&path, 2);
        assert!(!app.docs[0].breakpoints.contains(&2));
        assert!(!app.docs[0].stops.contains_key(&2), "the condition goes with the breakpoint");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What a typed value becomes for lldb, by the variable's kind, and what is refused.
    #[test]
    fn typed_values_become_assignments_by_kind() {
        use debugger::Kind;
        assert_eq!(assignment_text(Kind::Integer, " 42 "), Ok("42".into()));
        assert!(assignment_text(Kind::Integer, "4.2").is_err());
        assert_eq!(assignment_text(Kind::Float, "2.5"), Ok("2.5".into()));
        assert_eq!(assignment_text(Kind::Float, "3"), Ok("3.0".into()));
        assert_eq!(assignment_text(Kind::Boolean, "True"), Ok("1".into()));
        assert_eq!(assignment_text(Kind::Boolean, "false"), Ok("0".into()));
        assert!(assignment_text(Kind::Boolean, "yes").is_err());
        assert_eq!(assignment_text(Kind::Char, "x"), Ok("'x'".into()));
        assert_eq!(assignment_text(Kind::Char, "'"), Ok("'\\''".into()));
        assert!(assignment_text(Kind::Char, "xy").is_err());
        assert!(assignment_text(Kind::String, "a").is_err(), "strings are not changed here");
    }

    #[test]
    fn the_empty_sources_are_named_for_the_language_server_message() {
        let root = project("empty-names");
        std::fs::write(root.join("blank.pas"), "").unwrap();
        std::fs::write(root.join("blank.txt"), "").unwrap();
        let app = App::bare(Some(root.join("debug.pas")));
        assert_eq!(app.empty_sources(), ["blank.pas"], "empty text files do not stop pasls");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_outline_keys_texts_and_names_declaration_kinds() {
        assert_eq!(text_key("a := 1;"), text_key("a := 1;"));
        assert_ne!(text_key("a := 1;"), text_key("a := 2;"));
        assert_eq!(kind_word(5), "class");
        assert_eq!(kind_word(8), "field");
        assert_eq!(kind_word(999), "symbol", "an unknown kind is still listed");
    }

    #[test]
    fn a_new_text_file_from_the_tree_is_created_and_opened() {
        let project_dir = project("new-text");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let mut p = Pending::default();
        app.apply_tree_actions(
            vec![TreeAction::Create { dir: root.clone(), is_dir: false, name: "notes.md".into() }],
            &mut p,
        );
        assert!(root.join("notes.md").is_file(), "created");
        assert_eq!(p.open.as_deref(), Some(root.join("notes.md").as_path()), "and opened");
        assert_eq!(app.tree_sel.as_deref(), Some(root.join("notes.md").as_path()));
        app.apply_tree_actions(
            vec![TreeAction::Create { dir: root.clone(), is_dir: false, name: "bin.o".into() }],
            &mut p,
        );
        assert!(!root.join("bin.o").exists(), "a kind the tree does not create is refused");
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn new_form_creates_the_pair_opens_the_unit_and_shows_the_form() {
        let project_dir = project("new-form");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::NewForm { dir: sub.clone() }], &mut p);
        assert!(sub.join("unit1.pas").is_file() && sub.join("unit1.lfm").is_file());
        assert_eq!(app.tree_sel.as_deref(), Some(sub.join("unit1.pas").as_path()));
        assert!(app.docs.iter().any(|d| d.path == sub.join("unit1.pas")));
        assert!(app.console.contains("unit1.pas"), "the console says what was created: {}", app.console);
        assert!(
            app.designers.iter().any(|f| f.pas_path == sub.join("unit1.pas")),
            "the designer opened the new form (console: {})",
            app.console
        );
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn a_form_is_added_to_the_program_and_removed_with_its_unit() {
        let base = std::env::temp_dir().join(format!("pascoda-program-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "GuiApp1").unwrap();
        // The app's own root: it is canonical, so paths built from `base` would not match it.
        let mut app = App::bare(Some(base.join("GuiApp1").join("unit1.pas")));
        let root = app.root.clone();
        let lpr = root.join("GuiApp1.lpr");
        let template = std::fs::read_to_string(&lpr).unwrap();
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::NewForm { dir: root.clone() }], &mut p);
        let added = std::fs::read_to_string(&lpr).unwrap();
        assert!(added.contains("Forms, Unit1, Unit2;"), "{added}");
        assert!(added.contains("Application.CreateForm(TForm2, Form2);\n  Application.Run;"), "{added}");
        assert!(app.console.contains("Added it to GuiApp1.lpr"), "{}", app.console);

        app.apply_tree_actions(vec![TreeAction::ConfirmDelete(root.join("unit2.pas"))], &mut p);
        assert_eq!(std::fs::read_to_string(&lpr).unwrap(), template, "back to the template");
        assert!(!root.join("unit2.pas").exists() && !root.join("unit2.lfm").exists(), "the form goes with its unit");
        assert!(
            app.notice().is_some_and(|(m, _)| m.contains("removed Unit2 from the program")),
            "{:?}",
            app.notice().map(|(m, _)| m.to_string())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn renaming_and_moving_a_form_unit_updates_the_program() {
        let base = std::env::temp_dir().join(format!("pascoda-program-moves-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "GuiApp1").unwrap();
        let mut app = App::bare(Some(base.join("GuiApp1").join("unit1.pas")));
        let root = app.root.clone();
        let lpr = root.join("GuiApp1.lpr");
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::NewForm { dir: root.clone() }], &mut p);
        assert!(std::fs::read_to_string(&lpr).unwrap().contains("Forms, Unit1, Unit2;"));

        // Rename: the uses entry, the unit line and the form file follow; the form's class stays.
        app.apply_tree_actions(
            vec![TreeAction::RenameTo { path: root.join("unit2.pas"), name: "dialog".into() }],
            &mut p,
        );
        let prog = std::fs::read_to_string(&lpr).unwrap();
        assert!(prog.contains("Forms, Unit1, Dialog;") && prog.contains("Application.CreateForm(TForm2, Form2);"), "{prog}");
        assert!(std::fs::read_to_string(root.join("dialog.pas")).unwrap().starts_with("unit Dialog;"));
        assert!(root.join("dialog.lfm").is_file() && !root.join("unit2.lfm").exists());

        // Move into a subfolder: the entry gets its `in` path; back to the folder: it goes.
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        app.apply_tree_actions(vec![TreeAction::MoveInto { path: root.join("dialog.pas"), dir: sub.clone() }], &mut p);
        assert!(std::fs::read_to_string(&lpr).unwrap().contains("Dialog in 'sub/dialog.pas'"));
        app.apply_tree_actions(vec![TreeAction::MoveInto { path: sub.join("dialog.pas"), dir: root.clone() }], &mut p);
        let back = std::fs::read_to_string(&lpr).unwrap();
        assert!(back.contains("Forms, Unit1, Dialog;") && !back.contains("in '"), "{back}");

        // A folder that holds a listed unit moves: the unit's path follows it.
        let inner = root.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        app.apply_tree_actions(vec![TreeAction::MoveInto { path: root.join("dialog.pas"), dir: inner.clone() }], &mut p);
        app.apply_tree_actions(vec![TreeAction::MoveInto { path: inner.clone(), dir: sub.clone() }], &mut p);
        assert!(sub.join("inner/dialog.pas").is_file());
        assert!(std::fs::read_to_string(&lpr).unwrap().contains("Dialog in 'sub/inner/dialog.pas'"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn other_units_uses_clauses_follow_rename_move_and_delete() {
        let base = std::env::temp_dir().join(format!("pascoda-others-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "GuiApp1").unwrap();
        let mut app = App::bare(Some(base.join("GuiApp1").join("unit1.pas")));
        let root = app.root.clone();
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::NewForm { dir: root.clone() }], &mut p);
        // unit1 uses unit2 in its interface, and the program lists both.
        let unit1 = root.join("unit1.pas");
        let text = std::fs::read_to_string(&unit1).unwrap().replace("StdCtrls;", "StdCtrls, Unit2;");
        std::fs::write(&unit1, &text).unwrap();

        app.apply_tree_actions(
            vec![TreeAction::RenameTo { path: root.join("unit2.pas"), name: "dialog".into() }],
            &mut p,
        );
        let renamed = std::fs::read_to_string(&unit1).unwrap();
        assert!(renamed.contains("StdCtrls, Dialog;"), "{renamed}");
        assert!(app.notice().is_some_and(|(m, _)| m.contains("unit1")), "{:?}", app.notice().map(|(m, _)| m.to_string()));

        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        app.apply_tree_actions(vec![TreeAction::MoveInto { path: root.join("dialog.pas"), dir: sub.clone() }], &mut p);
        let moved = std::fs::read_to_string(&unit1).unwrap();
        assert!(moved.contains("StdCtrls, Dialog in 'sub/dialog.pas';"), "{moved}");

        app.apply_tree_actions(vec![TreeAction::ConfirmDelete(sub.join("dialog.pas"))], &mut p);
        let deleted = std::fs::read_to_string(&unit1).unwrap();
        assert!(deleted.contains("StdCtrls;") && !deleted.contains(", Dialog;"), "{deleted}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn deleting_a_form_file_deletes_its_unit_and_its_program_entry() {
        let base = std::env::temp_dir().join(format!("pascoda-program-lfm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "GuiApp1").unwrap();
        let mut app = App::bare(Some(base.join("GuiApp1").join("unit1.pas")));
        let root = app.root.clone();
        let lpr = root.join("GuiApp1.lpr");
        let template = std::fs::read_to_string(&lpr).unwrap();
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::NewForm { dir: root.clone() }], &mut p);
        app.apply_tree_actions(vec![TreeAction::ConfirmDelete(root.join("unit2.lfm"))], &mut p);
        assert!(!root.join("unit2.pas").exists() && !root.join("unit2.lfm").exists());
        assert_eq!(std::fs::read_to_string(&lpr).unwrap(), template);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn deleting_a_folder_removes_its_units_from_the_program() {
        let base = std::env::temp_dir().join(format!("pascoda-program-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "GuiApp1").unwrap();
        let root = base.join("GuiApp1");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let lpr = root.join("GuiApp1.lpr");
        std::fs::write(sub.join("unit7.pas"), "unit Unit7;\ninterface\nimplementation\nend.\n").unwrap();
        let text = std::fs::read_to_string(&lpr).unwrap();
        let text = pascoda_project::program::add_form(&text, "Unit7", "TForm7", "Form7").unwrap();
        std::fs::write(&lpr, &text).unwrap();
        std::fs::write(sub.join("unit7.lfm"), "object Form7: TForm7\nend\n").unwrap();
        let mut app = App::bare(Some(root.join("unit1.pas")));
        let sub = app.root.join("sub");
        let mut p = Pending::default();
        app.apply_tree_actions(vec![TreeAction::ConfirmDelete(sub.clone())], &mut p);
        let after = std::fs::read_to_string(&lpr).unwrap();
        assert!(!after.contains("Unit7") && !after.contains("TForm7"), "{after}");
        assert!(after.contains("Forms, Unit1;"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dragging_an_open_file_into_a_folder_moves_its_tab_and_expands_the_folder() {
        let project_dir = project("move-open");
        let mut app = App::bare(Some(project_dir.join("debug.pas")));
        let root = app.root.clone();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut p = Pending::default();
        app.apply_tree_actions(
            vec![TreeAction::MoveInto { path: root.join("debug.pas"), dir: sub.clone() }],
            &mut p,
        );
        assert!(sub.join("debug.pas").is_file() && !root.join("debug.pas").exists());
        assert_eq!(app.docs[0].path, sub.join("debug.pas"));
        assert_eq!(app.tree_sel.as_deref(), Some(sub.join("debug.pas").as_path()));
        assert!(app.expanded.contains(&sub));
        // A refused move leaves the tab where it was and says so.
        app.apply_tree_actions(vec![TreeAction::MoveInto { path: sub.clone(), dir: sub.clone() }], &mut p);
        assert!(sub.is_dir());
        assert_eq!(app.docs[0].path, sub.join("debug.pas"));
        let _ = std::fs::remove_dir_all(&project_dir);
    }
}

/// A context menu that opens on a secondary click, and on Control-click too: a Mac without a right
/// button (or a trackpad set up that way) sends a Control-click, which egui reads as a plain click.
fn context_menu(ui: &egui::Ui, resp: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    let control_click = resp.clicked() && ui.input(|i| i.modifiers.ctrl);
    let open = if control_click || resp.secondary_clicked() {
        Some(egui::SetOpenCommand::Bool(true))
    } else if resp.clicked() {
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    };
    let _ = egui::Popup::context_menu(resp).open_memory(open).show(add);
}

#[cfg(test)]
mod menu_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn project_with_a_unit() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pascoda-file-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("debug.pas"), "program debug;\nbegin\nend.\n").unwrap();
        dir
    }

    #[test]
    fn the_file_menu_starts_new_files_and_folders_and_renames_the_selection() {
        let dir = project_with_a_unit();
        let mut app = App::bare(Some(dir.join("debug.pas")));
        let root = app.root.clone();
        let ctx = egui::Context::default();

        app.run_menu_list(&ctx, vec![MenuAction::NewFile]);
        assert!(app.tree_new.as_ref().is_some_and(|n| !n.is_dir && n.dir == root), "New File names a file in the target folder");
        app.tree_new = None;

        app.run_menu_list(&ctx, vec![MenuAction::NewFolder]);
        assert!(app.tree_new.as_ref().is_some_and(|n| n.is_dir && n.dir == root), "New Folder names a folder");
        app.tree_new = None;

        app.run_menu_list(&ctx, vec![MenuAction::RenameItem]);
        assert!(app.tree_rename.is_none(), "nothing is selected: Rename says so");
        assert!(app.console.contains("Select a file or folder"), "{}", app.console);

        app.tree_sel = Some(root.join("debug.pas"));
        app.run_menu_list(&ctx, vec![MenuAction::RenameItem]);
        assert!(app.tree_rename.as_ref().is_some_and(|r| r.path == root.join("debug.pas")), "Rename opens the box on the selection");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_data_module_creates_the_unit_opens_it_and_adds_it_to_the_program() {
        let dir = std::env::temp_dir().join(format!("pascoda-new-dm-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prog.lpr"), "program prog;\n\n{$mode objfpc}{$H+}\n\nuses\n  Forms;\n\nbegin\n  Application.Initialize;\n  Application.Run;\nend.\n").unwrap();
        let mut app = App::bare(Some(dir.join("prog.lpr")));
        let ctx = egui::Context::default();
        app.run_menu_list(&ctx, vec![MenuAction::NewDataModule]);
        let pas = dir.join("unit1.pas");
        assert!(pas.is_file(), "the unit is created: {}", app.console);
        let lfm = std::fs::read_to_string(dir.join("unit1.lfm")).unwrap();
        assert!(lfm.starts_with("object DataModule1: TDataModule1"), "{lfm}");
        assert!(app.console.contains("data module"), "{}", app.console);
        let canon = pas.canonicalize().unwrap();
        assert!(app.docs.iter().any(|d| d.path.canonicalize().ok().as_ref() == Some(&canon)), "the unit is open");
        let program = std::fs::read_to_string(dir.join("prog.lpr")).unwrap();
        let data = program.find("Application.CreateForm(TDataModule1, DataModule1);").expect("created");
        let run = program.find("Application.Run;").unwrap();
        assert!(data < run, "created before Run: {program}");
        assert!(program.to_lowercase().contains("unit1"), "{program}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_and_run_builds_the_program_whatever_file_is_selected() {
        let dir = std::env::temp_dir().join(format!("pascoda-build-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lpr = dir.join("prog.lpr");
        std::fs::write(&lpr, "program prog;\n").unwrap();
        for file in ["Pkg.lpk", "unit1.lfm", "unit1.pas", "notes.txt"] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        assert_eq!(crate::program_for(&dir.join("Pkg.lpk")), lpr, "a package builds the program");
        assert_eq!(crate::program_for(&dir.join("unit1.lfm")), lpr, "a form file builds the program");
        assert_eq!(crate::program_for(&dir.join("unit1.pas")), lpr, "a unit builds the program");
        assert_eq!(crate::program_for(&dir.join("notes.txt")), lpr, "a text file builds the program");
        assert_eq!(crate::program_for(&lpr), lpr, "a program builds itself");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_a_project_clears_the_problems_and_build_errors() {
        let old = project_with_a_unit();
        let new = std::env::temp_dir().join(format!("pascoda-open-clears-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&new);
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("other.pas"), "program other;\nbegin\nend.\n").unwrap();
        let mut app = App::bare(Some(old.join("debug.pas")));
        app.diagnostics = vec![pascoda_fpc::parse_line("debug.pas(2,1) Error: boom").unwrap()];
        app.restore(session::Session { root: Some(new.clone()), ..Default::default() }, false);
        assert_eq!(app.root, new);
        assert!(app.diagnostics.is_empty(), "the old project's problems are gone");
        assert!(app.marks.is_empty());
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&new);
    }

    #[test]
    fn a_control_click_opens_a_context_menu_and_a_plain_click_does_not() {
        let mut harness = Harness::new_ui(|ui| {
            let row = ui.button("Row");
            context_menu(ui, &row, |ui| {
                ui.label("Menu item");
            });
        });
        harness.run();
        harness.get_by_label("Row").click();
        harness.run();
        assert!(harness.query_by_label("Menu item").is_none(), "a plain click does not open the menu");

        harness.get_by_label("Row").click_modifiers(egui::Modifiers::CTRL);
        harness.run();
        assert!(harness.query_by_label("Menu item").is_some(), "Control-click opens the menu");
    }

    #[test]
    fn a_secondary_click_still_opens_the_context_menu() {
        let mut harness = Harness::new_ui(|ui| {
            let row = ui.button("Row");
            context_menu(ui, &row, |ui| {
                ui.label("Menu item");
            });
        });
        harness.run();
        harness.get_by_label("Row").click_secondary();
        harness.run();
        assert!(harness.query_by_label("Menu item").is_some());
    }
}

#[cfg(test)]
mod search_path_tests {
    use super::*;

    #[test]
    fn a_missing_unit_is_read_from_fpcs_message() {
        assert_eq!(missing_unit("Can't find unit helper used by app"), Some("helper"));
        assert_eq!(missing_unit("Identifier not found \"x\""), None);
    }

    /// The Problems pane offers the folder of a unit that fpc cannot find: adding it to `pascoda.toml`
    /// lets the next build find the unit.
    #[test]
    fn adding_the_folder_of_a_missing_unit_makes_the_build_find_it() {
        let root = std::env::temp_dir().join(format!("pascoda-search-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("libs")).unwrap();
        let program = root.join("app.pas");
        std::fs::write(&program, "program app;\n\nuses helper;\n\nbegin\n  WriteLn(Twice(2));\nend.\n").unwrap();
        std::fs::write(
            root.join("libs/helper.pas"),
            "unit helper;\n\n{$mode objfpc}\n\ninterface\n\nfunction Twice(x: Integer): Integer;\n\nimplementation\n\nfunction Twice(x: Integer): Integer;\nbegin\n  Result := 2 * x;\nend;\n\nend.\n",
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let program = root.join("app.pas");

        let before = pascoda_fpc::build_progress_with(
            &program,
            pascoda_fpc::Mode::ObjFpc,
            &pascoda_project::compiler_args(&root),
            None,
            &mut |_| {},
        )
        .unwrap();
        assert!(!before.success);
        assert!(before.raw_output.contains("Can't find unit helper"), "{}", before.raw_output);

        let mut app = App::bare(Some(program.clone()));
        app.add_search_path(&root.join("libs"));
        assert_eq!(pascoda_project::load_compiler(&root).unit_paths, ["libs"], "stored relative to the project");
        app.add_search_path(&root.join("libs"));
        assert_eq!(pascoda_project::load_compiler(&root).unit_paths, ["libs"], "not added twice");

        let after = pascoda_fpc::build_progress_with(
            &program,
            pascoda_fpc::Mode::ObjFpc,
            &pascoda_project::compiler_args(&root),
            None,
            &mut |_| {},
        )
        .unwrap();
        assert!(after.success, "{}", after.raw_output);
        let _ = std::fs::remove_dir_all(&root);
    }
}
