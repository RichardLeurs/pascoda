mod about;
mod assistant;
mod bottom;
mod editor_lines;
mod prefs;
mod unit_tests;
mod vendor_ui;
#[cfg(test)]
mod demo_tests;
mod recent;
mod system_menus;
mod session;
mod project_options;
mod chrome;
mod debugger;
mod designer;
mod export_dialog;
mod find;
mod files;
mod external;
mod highlight;
mod hints;
mod icons;
mod livecheck;
mod lsp;
mod migrate;
mod shell;
mod style;
mod terminal;
mod terminal_view;
mod theme;
mod watch;

use eframe::egui;
use egui::text::{CCursor, CCursorRange};
use pascoda_fpc::{BuildResult, Diagnostic, Severity};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};

const SOURCE_EXTENSIONS: &[&str] = &["pas", "pp", "lpr", "dpr", "inc"];

struct Doc {
    path: PathBuf,
    text: String,
    saved: String,
    editor_id: egui::Id,
    /// 1-based lines with a breakpoint.
    breakpoints: BTreeSet<u32>,
    /// The condition and hit condition of breakpoints that have them, by line.
    stops: std::collections::BTreeMap<u32, debugger::Stop>,
    /// Breakpoint lines that are kept but not sent to the debugger.
    disabled: BTreeSet<u32>,
}

impl Doc {
    fn dirty(&self) -> bool {
        self.text != self.saved
    }
}

struct App {
    docs: Vec<Doc>,
    active: usize,
    next_editor_id: u32,
    root: PathBuf,
    diagnostics: Vec<Diagnostic>,
    build_dir: PathBuf,
    console: String,
    building: Option<Receiver<(BuildResult, String)>>,
    /// Progress messages of the build in flight, the latest one, and how many units each
    /// program compiled last time (sizes the next bar).
    build_progress_rx: Option<Receiver<pascoda_fpc::Progress>>,
    /// An export (release build + `.app` + `.dmg`) in progress.
    exporting: Option<Exporting>,
    /// The program of the export in progress (keys its unit count).
    export_source: PathBuf,
    /// A question about unsaved changes that is waiting for an answer.
    confirm: Option<Confirm>,
    /// A change of project waiting for the answer to `confirm`: the new folder, and what to do in
    /// it once the old project's tabs are closed.
    project_switch: Option<(PathBuf, Box<dyn FnOnce(&mut App)>)>,
    /// The user chose to quit despite (or after saving) unsaved changes.
    quit_confirmed: bool,
    export_dialog: Option<export_dialog::ExportDialog>,
    /// The Component Packages dialog (File > Import Component Package...) is open.
    import_packages_open: bool,
    build_progress: Option<pascoda_fpc::Progress>,
    unit_counts: std::collections::HashMap<PathBuf, usize>,
    build_source: PathBuf,
    debug_after_build: bool,
    session: Option<debugger::Handle>,
    paused: Option<debugger::Paused>,
    /// Variable paths (see `debugger::PATH_SEP`) shown open in the Variables pane.
    var_open: std::collections::HashSet<String>,
    /// Initial height of the debug area (the user can drag it).
    debug_height: f32,
    /// The tabbed pane under the editor: selected tab and the terminal.
    bottom: bottom::BottomPane,
    lsp: Option<lsp::Handle>,
    /// The folder the language server was last started for (it follows the project folder).
    lsp_root: Option<PathBuf>,
    lsp_ready: bool,
    menu: Option<NativeMenu>,
    /// Waiting for the server: (file, text at request time, word start, cursor), in chars.
    pending_completion: Option<(PathBuf, String, usize, usize)>,
    completion: Option<Completion>,
    /// The pending completion was started by typing, so an empty answer is not worth a message.
    completion_auto: bool,
    /// Counts typed characters; `typing` waits for a pause after the latest one.
    typed: u64,
    typing: hints::Dwell<u64>,
    hover: hints::Hover,
    /// Diagnostics of the last build placed in the open files; dropped when a file changes.
    marks: Vec<hints::FileMarks>,
    live: livecheck::LiveCheck,
    /// The project's language mode (`pascoda.toml` in `mode_root`).
    mode: pascoda_fpc::Mode,
    mode_root: PathBuf,
    /// Open files' text as the last build started, to place `marks`.
    build_texts: Vec<(PathBuf, String)>,
    designers: Vec<designer::Designer>,
    /// Show the form designer instead of the code for the active unit.
    form_view: bool,
    /// The `.lfm` file whose form is showing (selected in the tree): Code shows its text.
    form_lfm: Option<PathBuf>,
    /// Tools that are missing, shown in the "Some tools are missing" window (see `vendor_ui`).
    vendor_dialog: Option<Vec<pascoda_fpc::vendor::Part>>,
    /// The missing language server was already pointed out this session.
    vendor_asked_pasls: bool,
    designer_ui: designer::ui::UiState,
    native: Option<designer::native::Native>,
    selftest: Option<SelfTest>,
    show_navigator: bool,
    /// The path bar (folder > file) above the editor; hidden by default to leave room for code.
    show_jump_bar: bool,
    nav_tab: shell::NavTab,
    expanded: std::collections::HashSet<PathBuf>,
    file_filter: String,
    /// 1-based line and column of the caret in the code editor.
    cursor_pos: (usize, usize),
    /// Where a hover popup for the current target goes (screen position).
    hover_pos: egui::Pos2,
    assistant: assistant::Assistant,
    /// An assistant build is running; its result goes back to the waiting agent.
    assistant_build_reply: Option<std::sync::mpsc::Sender<String>>,
    /// Run the program after a successful build (false for builds the assistant asked for).
    run_after_build: bool,
    /// Changes made to open files by other programs, and the watcher that notices them.
    external: external::External,
    unit_tests: unit_tests::UnitTests,
    /// Project tree: the selected file or folder, a name being typed for a new item, and an
    /// item waiting for the user to confirm its deletion.
    tree_sel: Option<PathBuf>,
    tree_new: Option<shell::TreeNew>,
    tree_delete: Option<PathBuf>,
    tree_rename: Option<shell::TreeRename>,
    outline: shell::Outline,
    /// File > Open Recent: the lists, and the file they are kept in (`None` in tests, so they are
    /// not written to the user's folder).
    recent: recent::Recent,
    recent_file: Option<PathBuf>,
    /// A character range the editor scrolls into view on its next frame (set by go-to).
    reveal: Option<std::ops::Range<usize>>,
    /// The session file the app restores at launch and writes at exit (`None` in tests).
    session_file: Option<PathBuf>,
    /// The breakpoint box open in the gutter's secondary click: the file and line, and the fields.
    bp_edit: Option<shell::BpEdit>,
    /// The Change Value box, while a variable's new value is being assigned.
    value_edit: Option<shell::ValueEdit>,
    /// Stop where a Pascal exception is raised (kept across debug sessions, set when one starts).
    exception_stops: bool,
    /// The signature of the call at the caret (parameter hints while a call is typed).
    signature: Option<shell::SigHint>,
    /// The watch expressions (kept across debug sessions), the text in their box, and the last
    /// Evaluate result.
    watch_list: Vec<String>,
    watch_input: String,
    watch_eval: Option<(String, Result<debugger::Var, String>)>,
    prefs: prefs::Prefs,
    project_options: project_options::Editor,
    preferences_open: bool,
    find: find::Bar,
    project_bar: find::ProjectBar,
    go_to: find::GoTo,
    rename: find::Rename,
    /// The project's custom components, and the project (and file-tree revision) they were read for.
    custom_units: std::sync::Arc<designer::ClassUnits>,
    custom_scan: Option<(PathBuf, u64)>,
}

/// `PASCODA_SELFTEST=<dir>`: open the form, exercise the designer, write snapshots, quit.
struct SelfTest {
    dir: PathBuf,
    frame: u32,
    /// `PASCODA_SELFTEST_SCROLL=1`: instead of the designer sequence, show a form larger than
    /// the canvas at the default window size and snapshot it at the top and scrolled to the end.
    scroll: bool,
}

struct Completion {
    /// Text the request was made against; the popup closes when it changes.
    text: String,
    start: usize,
    cursor: usize,
    items: Vec<pascoda_lsp::CompletionItem>,
    selected: usize,
    /// Enter/Tab accept only an item the user picked (arrows or click), or any item when the
    /// popup was asked for with Ctrl+Space. Auto popups leave Enter and Tab alone.
    chosen: bool,
}

impl App {
    /// The app without a window, menu bar or language server: all state, no side effects.
    /// `new` builds on it; tests use it to draw the UI offscreen.
    fn bare(path: Option<PathBuf>) -> Self {
        let root = path
            .as_ref()
            .and_then(|p| p.parent())
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        // Absolute, so tree rows compare equal to the (absolute) paths of open files.
        let root = root.canonicalize().unwrap_or(root);
        let mut app = Self {
            docs: Vec::new(),
            active: 0,
            next_editor_id: 0,
            build_dir: root.clone(),
            root,
            diagnostics: Vec::new(),
            console: String::new(),
            building: None,
            build_progress_rx: None,
            exporting: None,
            export_source: PathBuf::new(),
            confirm: None,
            project_switch: None,
            quit_confirmed: false,
            export_dialog: None,
            build_progress: None,
            unit_counts: Default::default(),
            build_source: PathBuf::new(),
            debug_after_build: false,
            session: None,
            paused: None,
            var_open: Default::default(),
            debug_height: 190.0,
            bottom: Default::default(),
            lsp: None,
            lsp_root: None,
            lsp_ready: false,
            menu: None,
            pending_completion: None,
            completion: None,
            completion_auto: false,
            typed: 0,
            typing: Default::default(),
            hover: Default::default(),
            marks: Vec::new(),
            live: Default::default(),
            mode: Default::default(),
            mode_root: PathBuf::new(),
            build_texts: Vec::new(),
            designers: Vec::new(),
            form_view: false,
            form_lfm: None,
            vendor_dialog: None,
            vendor_asked_pasls: false,
            designer_ui: designer::ui::UiState::default(),
            native: None,
            selftest: None,
            show_navigator: true,
            show_jump_bar: false,
            nav_tab: shell::NavTab::default(),
            expanded: Default::default(),
            file_filter: String::new(),
            cursor_pos: (1, 1),
            hover_pos: egui::Pos2::ZERO,
            assistant: Default::default(),
            assistant_build_reply: None,
            run_after_build: true,
            external: Default::default(),
            unit_tests: Default::default(),
            tree_sel: None,
            tree_new: None,
            tree_delete: None,
            tree_rename: None,
            outline: Default::default(),
            recent: Default::default(),
            recent_file: None,
            reveal: None,
            session_file: None,
            bp_edit: None,
            value_edit: None,
            exception_stops: false,
            signature: None,
            watch_list: Vec::new(),
            watch_input: String::new(),
            watch_eval: None,
            prefs: Default::default(),
            project_options: Default::default(),
            preferences_open: false,
            find: Default::default(),
            project_bar: Default::default(),
            go_to: Default::default(),
            rename: Default::default(),
            import_packages_open: false,
            custom_units: Default::default(),
            custom_scan: None,
        };
        if let Some(p) = path {
            app.open_path(p);
        }
        app
    }

    fn new(path: Option<PathBuf>, ctx: &egui::Context) -> Self {
        let given = path.is_some();
        let mut app = Self::bare(path);
        let file = session::Session::file();
        app.session_file = file.clone();
        if let Some(saved) = file.as_deref().and_then(session::Session::load) {
            app.restore(saved, given);
        }
        app.prefs = prefs::Prefs::load();
        app.prefs.apply();
        designer::imports::refresh();
        app.menu = NativeMenu::install(ctx);
        let file = recent::Recent::file();
        app.recent = file.as_deref().map(recent::Recent::load).unwrap_or_default();
        app.recent_file = file;
        app.refresh_recent_menu();
        app.start_watching(ctx);
        app.live.enabled = true;
        if pascoda_fpc::locate_fpc().is_none() {
            app.console = format!("{}\n", pascoda_fpc::NOT_FOUND_HINT);
            app.auto_tab(ctx, bottom::BottomTab::Output);
        }
        app.selftest = std::env::var_os("PASCODA_SELFTEST").map(|d| SelfTest {
            dir: d.into(),
            frame: 0,
            scroll: std::env::var_os("PASCODA_SELFTEST_SCROLL").is_some(),
        });
        app
    }

    /// What the session file keeps: the project, the open tabs and the active one.
    fn snapshot(&self) -> session::Session {
        session::Session {
            root: Some(self.root.clone()),
            tabs: self.docs.iter().map(|d| d.path.clone()).collect(),
            active: self.docs.get(self.active).map(|d| d.path.clone()),
            form_view: self.form_view,
        }
    }

    /// A project was opened or created: the problems and build errors of the previous project
    /// (the Problems list, the squiggles and the live check) do not belong to it any more.
    fn clear_build_results(&mut self) {
        self.diagnostics.clear();
        self.marks.clear();
        self.build_texts.clear();
        self.live.forget();
        if !self.unit_tests.busy() {
            // The tests listed belong to the previous project.
            self.unit_tests = Default::default();
        }
    }

    /// Reopens the saved session: its project folder (unless a file was given on the command line,
    /// which keeps the folder it is in), its tabs that still exist, and the active tab. A file given on
    /// the command line is the active tab.
    fn restore(&mut self, saved: session::Session, given: bool) {
        if !given && let Some(root) = saved.root.filter(|r| r.is_dir()) {
            self.root = root.clone();
            self.build_dir = root;
            self.clear_build_results();
        }
        // The file given on the command line is open already, as the first tab. Opening a tab makes
        // it active, so the first tab is made active again below.
        let given_tab = self.active;
        for tab in saved.tabs.iter().filter(|t| t.is_file()) {
            if !self.docs.iter().any(|d| &d.path == tab) {
                self.open_path(tab.clone());
            }
        }
        if given {
            self.active = given_tab;
        }
        if !given
            && let Some(i) = saved
                .active
                .as_ref()
                .and_then(|a| self.docs.iter().position(|d| &d.path == a))
        {
            self.active = i;
        }
        if saved.form_view {
            self.show_form();
        }
    }

    /// Opens `path` in a tab, or switches to it if it is already open.
    fn open_path(&mut self, path: PathBuf) {
        // Absolute, so it matches the paths the debugger reports.
        let path = path.canonicalize().unwrap_or(path);
        if let Some(i) = self.docs.iter().position(|d| d.path == path) {
            self.active = i;
            return;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            self.console = format!("could not read {}\n", path.display());
            return;
        };
        if pascoda_forms::lfm::is_binary(&bytes) {
            self.console = format!(
                "{} is a binary Delphi form, which Pascoda does not read (only text .dfm files)\n",
                path.display()
            );
            return;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            self.console = format!("could not read {} as text\n", path.display());
            return;
        };
        self.next_editor_id += 1;
        self.docs.push(Doc {
            path,
            saved: text.clone(),
            text,
            editor_id: egui::Id::unique(("pascoda-editor", self.next_editor_id)),
            breakpoints: BTreeSet::new(),
            stops: Default::default(),
            disabled: BTreeSet::new(),
        });
        self.active = self.docs.len() - 1;
    }

    fn pick_file(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Pascal", SOURCE_EXTENSIONS)
            // Everything the project tree lists: forms, project files, packages and text files too.
            .add_filter("Pascal, form, project and package files", files::TREE_EXTENSIONS)
            .pick_file()
        {
            match p.parent().map(Path::to_path_buf) {
                Some(dir) => self.switch_project(dir.clone(), move |app| {
                    app.remember(|r| r.note_project(&dir));
                    app.remember(|r| r.note_file(&p));
                    app.open_path(p);
                }),
                None => {
                    self.remember(|r| r.note_file(&p));
                    self.open_path(p);
                }
            }
        }
    }

    /// Changes the recent lists with `change`, saves them, and rebuilds the Open Recent menu.
    fn remember(&mut self, change: impl FnOnce(&mut recent::Recent)) {
        change(&mut self.recent);
        if let Some(file) = &self.recent_file {
            self.recent.save(file);
        }
        self.refresh_recent_menu();
    }

    fn refresh_recent_menu(&mut self) {
        let entries = self.recent.menu_entries();
        if let Some(m) = self.menu.as_mut() {
            m.set_recent(entries);
        }
    }

    /// Opens an item of Open Recent: a project's main program (its `.lpr`, else its first unit) or a
    /// file, with its folder as the project.
    fn open_recent(&mut self, index: usize) {
        let Some(entry) = self.menu.as_ref().and_then(|m| m.recent_entries.get(index).cloned()) else {
            return;
        };
        self.open_recent_entry(entry);
    }

    fn open_recent_entry(&mut self, entry: recent::Entry) {
        if entry.project {
            let folder = entry.path.clone();
            self.switch_project(folder.clone(), move |app| {
                let main = pascoda_project::program::program_for(&folder, &folder).or_else(|| {
                    let mut units: Vec<PathBuf> = std::fs::read_dir(&folder)
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pas")))
                        .collect();
                    units.sort();
                    units.into_iter().next()
                });
                if let Some(main) = main {
                    app.open_path(main);
                }
                app.remember(|r| r.note_project(&folder));
            });
        } else {
            let dir = entry.path.parent().map(Path::to_path_buf).unwrap_or_default();
            let file = entry.path.clone();
            self.switch_project(dir.clone(), move |app| {
                app.open_path(file.clone());
                app.remember(|r| {
                    r.note_file(&file);
                    r.note_project(&dir);
                });
            });
        }
    }

    fn save_doc(&mut self, i: usize) {
        let Some(d) = self.docs.get_mut(i) else {
            return;
        };
        let wrote = std::fs::write(&d.path, &d.text).is_ok();
        if wrote {
            d.saved = d.text.clone();
        }
        let path = d.path.clone();
        if let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == path) {
            let _ = form.save();
        }
        if wrote {
            // An answer to "changed on disk" is settled by overwriting it.
            self.forget_external(&path);
        }
    }

    fn save_all(&mut self) {
        for i in 0..self.docs.len() {
            self.save_doc(i);
        }
    }

    /// Has tab `i` (or the form of its unit) unsaved changes?
    fn is_dirty(&self, i: usize) -> bool {
        self.docs.get(i).is_some_and(|d| {
            d.dirty()
                || self
                    .designers
                    .iter()
                    .any(|f| designer::form_unit(&d.path).is_some_and(|u| u == f.pas_path) && f.dirty())
        })
    }

    /// The close button of a tab: closes at once, or asks first when there are unsaved changes.
    fn request_close(&mut self, i: usize) {
        match self.docs.get(i).map(|d| d.path.clone()) {
            Some(path) if self.is_dirty(i) => self.confirm = Some(Confirm::Tab(path)),
            Some(_) => self.close_doc(i),
            None => {}
        }
    }

    /// Makes `dir` the project, then runs `then` (what to open or record there). The tabs of the old
    /// project are closed first; when some of them have unsaved changes the user is asked, and the
    /// switch waits for the answer. Staying in the same folder keeps the tabs.
    fn switch_project(&mut self, dir: PathBuf, then: impl FnOnce(&mut App) + 'static) {
        if dir == self.root {
            then(self);
            return;
        }
        let dirty = (0..self.docs.len())
            .any(|i| !self.docs[i].path.starts_with(&dir) && self.is_dirty(i));
        if dirty {
            self.confirm = Some(Confirm::Project(dir.clone()));
            self.project_switch = Some((dir, Box::new(then)));
            return;
        }
        self.close_tabs_outside(&dir, false);
        self.enter_project(dir);
        then(self);
    }

    /// The project folder is now `dir`; the build results belong to the old one.
    fn enter_project(&mut self, dir: PathBuf) {
        self.root = dir;
        self.clear_build_results();
    }

    /// Closes the tabs whose files are not in `dir`. With `save`, the unsaved ones are saved first.
    fn close_tabs_outside(&mut self, dir: &Path, save: bool) {
        let mut i = 0;
        while i < self.docs.len() {
            if self.docs[i].path.starts_with(dir) {
                i += 1;
            } else if save {
                if self.is_dirty(i) {
                    self.save_doc(i);
                }
                self.close_doc(i);
            } else {
                self.discard_doc(i);
            }
        }
    }

    /// Closes tab `i` without saving it: its form edits are dropped too.
    fn discard_doc(&mut self, i: usize) {
        if let Some(path) = self.docs.get(i).map(|d| d.path.clone()) {
            self.forget_designer_if_unused(&path);
        }
        self.remove_doc(i);
    }

    fn close_doc(&mut self, i: usize) {
        self.save_doc_form(i);
        self.remove_doc(i);
    }

    fn remove_doc(&mut self, i: usize) {
        self.docs.remove(i);
        self.active = self.active.min(self.docs.len().saturating_sub(1));
        if !self.active_is_unit_with_form() {
            self.form_view = false;
        }
    }

    /// Writes the form of tab `i`'s unit (closing a tab must not lose form edits). A `.lfm` tab
    /// shows the form of its unit, so both tabs of the pair reach the same designer.
    fn save_doc_form(&mut self, i: usize) {
        let Some(path) = self.docs.get(i).map(|d| d.path.clone()) else {
            return;
        };
        let Some(unit) = designer::form_unit(&path) else {
            return;
        };
        if let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == unit) {
            let _ = form.save();
        }
        self.forget_designer_if_unused(&path);
    }

    /// Drops the designer of the unit of `path` once neither tab of the pair (unit, `.lfm`) is
    /// open; while one of them is open, the form stays available to it.
    fn forget_designer_if_unused(&mut self, path: &Path) {
        let Some(unit) = designer::form_unit(path) else {
            return;
        };
        let open_elsewhere = self.docs.iter().any(|d| {
            d.path != path && designer::form_unit(&d.path).as_ref() == Some(&unit)
        });
        if !open_elsewhere {
            self.designers.retain(|f| f.pas_path != unit);
        }
    }

    /// The active tab is half of a unit/form pair: the `.pas` or its `.lfm`.
    fn has_form(&self) -> bool {
        self.docs
            .get(self.active)
            .is_some_and(|d| designer::form_unit(&d.path).is_some())
    }

    /// The active tab is half of a unit/form pair, so the designer can show its form (on the `.pas`
    /// or on the `.lfm`).
    fn active_is_unit_with_form(&self) -> bool {
        self.has_form()
    }

    /// A file was selected in the project tree: a form file (`.lfm` or `.dfm`) shows its form, any
    /// other file shows its code (never the form that was showing before).
    pub(crate) fn select_in_tree(&mut self, path: PathBuf) {
        let is_form_file = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lfm") || e.eq_ignore_ascii_case("dfm"));
        self.form_lfm = is_form_file.then(|| path.clone());
        self.open_path(path.clone());
        if is_form_file {
            if designer::form_unit(&path).is_some() {
                self.show_form();
            }
        } else {
            self.form_view = false;
        }
    }

    /// The code view. A form opened from its `.lfm` file shows that file's text, not the unit's code.
    pub(crate) fn show_code(&mut self) {
        self.form_view = false;
        if let Some(lfm) = self.form_lfm.clone() {
            self.open_path(lfm);
        }
    }

    /// The designer is what the window is showing right now.
    fn form_showing(&self) -> bool {
        self.form_view
            && self.docs.get(self.active).is_some_and(|d| {
                designer::form_unit(&d.path).is_some_and(|u| self.designers.iter().any(|f| f.pas_path == u))
            })
    }

    /// While the `.lfm` is open as text, keep it in step with the designer: changes made in
    /// the designer are written to disk and the text tab reloads them (unless the text itself
    /// has unsaved edits).
    fn sync_lfm_tab(&mut self) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let Some(unit) = designer::sibling_pas(&doc.path) else {
            return;
        };
        let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == unit) else {
            return;
        };
        if !form.dirty() || form.save().is_err() {
            return;
        }
        let path = doc.path.clone();
        if let (false, Ok(text)) = (doc.dirty(), std::fs::read_to_string(&path))
            && let Some(doc) = self.docs.get_mut(self.active)
        {
            doc.saved = text.clone();
            doc.text = text;
        }
    }

    /// Shows the form designer for the active unit, or for the unit that the active `.lfm`
    /// belongs to. Coming from the `.lfm` text, that text is what the user means: it is saved
    /// and the designer reloads from it.
    fn show_form(&mut self) {
        let Some(active) = self.docs.get(self.active).map(|d| d.path.clone()) else {
            return;
        };
        let Some(unit) = designer::form_unit(&active) else {
            return;
        };
        if unit != active {
            // The form is shown on the `.lfm`'s own tab. The unit is opened for its text (the
            // designer needs it), and the `.lfm` tab stays the one shown.
            let form_tab = self.active;
            self.save_doc(form_tab);
            self.designers.retain(|f| f.pas_path != unit);
            self.open_path(unit.clone());
            self.active = form_tab;
        }
        if !self.designers.iter().any(|f| f.pas_path == unit) {
            match designer::Designer::open(&unit) {
                Ok(f) => self.designers.push(f),
                Err(e) => {
                    self.console.push_str(&format!("form designer: {e}\n"));
                    return;
                }
            }
        }
        self.form_view = true;
    }

    /// File > New Form, and New Form in a folder's menu: a new unit and its form in `dir`, opened
    /// with the form shown in the designer. The program is not changed, so the form is not built
    /// until a program uses it.
    fn new_form(&mut self, dir: &Path) {
        self.new_unit(dir, false);
    }

    /// Creates a form (or, with `data_module`, a data module) in `dir`, opens it in the designer and
    /// adds it to the project's program.
    fn new_unit(&mut self, dir: &Path, data_module: bool) {
        let created = if data_module {
            pascoda_project::new_data_module_unit(dir, &self.root)
        } else {
            pascoda_project::new_form_unit(dir, &self.root)
        };
        let what = if data_module { "data module" } else { "form" };
        match created {
            Ok(pas) => {
                let shown = pas.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let unit = shell::unit_name(&pas);
                let form = std::fs::read_to_string(pas.with_extension("lfm"))
                    .ok()
                    .and_then(|t| pascoda_project::program::form_of_lfm(&t));
                self.expanded.insert(dir.to_path_buf());
                self.tree_sel = Some(pas.clone());
                self.open_path(pas);
                self.show_form();
                let mut message = format!("Created {shown} and its {what}.");
                if let Some((class, var)) = form {
                    message.push_str(&match self.add_to_program(dir, &unit, &class, &var) {
                        Ok(Some(lpr)) => format!(" Added it to {lpr}."),
                        Ok(None) => " No program (.lpr) in this project, so it is not built yet.".to_string(),
                        Err(e) => format!(" The program was not changed: {e}."),
                    });
                }
                self.console = format!("{message}\n");
            }
            Err(e) => self.console = format!("could not create the {what}: {e}\n"),
        }
    }

    /// Adds a form's unit to the program of `dir` (see `program::program_for`). Returns the program's
    /// file name, or `None` when the project has no program.
    fn add_to_program(&mut self, dir: &Path, unit: &str, class: &str, var: &str) -> Result<Option<String>, String> {
        let Some(lpr) = pascoda_project::program::program_for(dir, &self.root) else {
            return Ok(None);
        };
        self.edit_program(&lpr, |src| pascoda_project::program::add_form(src, unit, class, var))?;
        Ok(lpr.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    /// Applies `edit` to a program file and writes it when the text changes. Returns whether it did.
    pub(crate) fn edit_program(
        &mut self,
        lpr: &Path,
        edit: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<bool, String> {
        let src = std::fs::read_to_string(lpr).map_err(|e| format!("could not read {}: {e}", lpr.display()))?;
        let out = edit(&src)?;
        if out == src {
            return Ok(false);
        }
        std::fs::write(lpr, out).map_err(|e| format!("could not write {}: {e}", lpr.display()))?;
        Ok(true)
    }

    /// File > New GUI Application: a project folder with a program and one form.
    fn new_gui_app(&mut self) {
        self.new_project(pascoda_project::Template::Gui);
    }

    /// File > New ... Application or Library: a project folder in a chosen place, opened with its main
    /// program (and, for the GUI and the library, the unit it starts from).
    fn new_project(&mut self, template: pascoda_project::Template) {
        let Some(parent) = rfd::FileDialog::new()
            .set_title("Choose where to create the project")
            .pick_folder()
        else {
            return;
        };
        let name = pascoda_project::free_project_name(&parent);
        match pascoda_project::create_project(&parent, &name, template) {
            Ok(program) => {
                let dir = program.parent().map(Path::to_path_buf).unwrap_or_default();
                let opened = dir.clone();
                self.switch_project(dir, move |app| {
                    app.remember(|r| r.note_project(&opened));
                    app.open_path(program);
                    match template {
                        pascoda_project::Template::Gui => {
                            app.open_path(opened.join("unit1.pas"));
                            app.show_form();
                        }
                        pascoda_project::Template::Library => {
                            app.open_path(opened.join(format!("{name}.pas")));
                        }
                        pascoda_project::Template::Package => {
                            app.open_path(opened.join(format!("{name}.pas")));
                        }
                        pascoda_project::Template::Console => {}
                    }
                });
            }
            Err(e) => self.console = format!("could not create the project: {e}\n"),
        }
    }

    /// Saves everything, then compiles and runs the active file on a background thread
    /// so the UI never blocks.
    fn build_and_run(&mut self, ctx: &egui::Context, debug: bool) {
        self.start_build(ctx, debug, true);
    }

    /// `run`: start the program after a successful build (not when debugging).
    fn start_build(&mut self, ctx: &egui::Context, debug: bool, run_after: bool) {
        let Some(path) = self.docs.get(self.active).map(|d| program_for(&d.path)) else {
            return;
        };
        self.sync_mode();
        let mode = self.mode;
        self.save_all();
        self.build_texts = self
            .docs
            .iter()
            .map(|d| (d.path.clone(), d.text.clone()))
            .collect();
        self.debug_after_build = debug;
        self.run_after_build = run_after;
        self.build_source = path.clone();
        self.build_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.console = "Building...\n".into();
        // A Lazarus or Delphi project in the program's folder adds its search paths and defines.
        let (mut project_args, project_note) = project_args(path.parent());
        self.console.push_str(&project_note);
        // Debugging always builds the debug configuration; Run builds the project's configuration.
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let configuration = if debug {
            pascoda_project::Configuration::Debug
        } else {
            pascoda_project::load_configuration(&folder)
        };
        let release = configuration == pascoda_project::Configuration::Release;
        if release {
            // The release executable goes to its own folder (not the debug one, and not the
            // project file's `-o`).
            let exe = pascoda_project::executable_in(&path, configuration);
            project_args.retain(|a| !a.to_string_lossy().starts_with("-o"));
            project_args.push(format!("-o{}", exe.display()).into());
            if let Some(dir) = exe.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        self.auto_tab(ctx, bottom::BottomTab::Output);
        let (tx, rx) = channel();
        let (ptx, prx) = channel();
        let total = self.unit_counts.get(&path).copied();
        self.build_progress = None;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let gui = is_gui_program(&path);
            let repaint = ctx.clone();
            let mut on = move |p: pascoda_fpc::Progress| {
                let _ = ptx.send(p);
                repaint.request_repaint();
            };
            let built = if gui {
                match pascoda_fpc::lcl::Lcl::find(&path) {
                    Some(lcl) => {
                        pascoda_fpc::lcl::build_gui_progress_with(
                            &path,
                            mode,
                            &lcl,
                            &project_args,
                            total,
                            &mut on,
                        )
                    }
                    None => Err(std::io::Error::other(
                        "vendor/lazarus not found (needed for GUI programs)",
                    )),
                }
            } else {
                pascoda_fpc::build_progress_configured(&path, mode, &project_args, total, release, &mut on)
            };
            let result = built.unwrap_or_else(|e| BuildResult {
                success: false,
                diagnostics: Vec::new(),
                raw_output: format!("build failed: {e}"),
            });
            let run_output = if result.success && !debug && run_after {
                if gui {
                    launch_app(&path)
                } else {
                    let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
                    let exe = pascoda_project::executable_in(&path, configuration);
                    let settings = pascoda_project::load_run(&folder);
                    run(&exe, &settings.args, &pascoda_project::run_folder(&folder, &exe))
                }
            } else {
                String::new()
            };
            let _ = tx.send((result, run_output));
            ctx.request_repaint();
        });
        self.building = Some(rx);
        self.build_progress_rx = Some(prx);
    }

    fn poll_build(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.building else { return };
        if let Some(prx) = &self.build_progress_rx {
            while let Ok(p) = prx.try_recv() {
                self.build_progress = Some(p);
            }
        }
        if let Ok((result, run_output)) = rx.try_recv() {
            if result.success
                && let Some(p) = self.build_progress.as_ref().filter(|p| p.done > 0)
            {
                self.unit_counts.insert(self.build_source.clone(), p.done);
            }
            self.build_progress = None;
            self.build_progress_rx = None;
            self.console = result.raw_output.clone();
            if !result.success {
                self.offer_vendor_for_build(&result.raw_output, &result.diagnostics);
            }
            if result.success && !self.debug_after_build && self.run_after_build {
                self.console.push_str("\n--- program output ---\n");
                self.console.push_str(&run_output);
            }
            if !result.success {
                self.auto_tab(ctx, bottom::BottomTab::Problems);
            } else if self.run_after_build || self.debug_after_build {
                self.auto_tab(ctx, bottom::BottomTab::Output);
            }
            if result.success && self.debug_after_build {
                self.start_debug(ctx);
            }
            self.marks = self.place_marks(&result.diagnostics);
            self.diagnostics = result.diagnostics.clone();
            self.building = None;
            self.assistant_build_done(&result);
        }
    }

    /// Live check of the active file: after a pause in typing, `fpc` checks its current text and
    /// the result becomes squiggles (the same marks a build makes).
    /// Opens the export dialog for the active program; its "Export..." button asks for the
    /// folder and starts the export.
    fn start_export(&mut self) {
        if self.exporting.is_some() || self.building.is_some() || self.session.is_some() {
            return;
        }
        let Some(path) = self.docs.get(self.active).map(|d| program_for(&d.path)) else {
            return;
        };
        self.sync_mode();
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Program".into());
        self.export_dialog = Some(export_dialog::ExportDialog::new(
            pascoda_project::load_export(&self.root),
            &name,
            is_gui_program(&path),
        ));
    }

    /// The window close button and Quit: with unsaved changes the user is asked first.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quit_confirmed
            && (0..self.docs.len()).any(|i| self.is_dirty(i))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(Confirm::Quit);
        }
    }

    /// The "save changes?" question: Save, Don't Save or Cancel.
    fn show_confirm(&mut self, ctx: &egui::Context) {
        let Some(what) = self.confirm.clone() else {
            return;
        };
        let (title, detail, save_label, discard_label) = match &what {
            Confirm::Tab(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    format!("Save changes to \u{201C}{name}\u{201D}?"),
                    "Your changes will be lost if you don't save them.".to_string(),
                    "Save",
                    "Don't Save",
                )
            }
            Confirm::Project(dir) => {
                let n = (0..self.docs.len())
                    .filter(|&i| !self.docs[i].path.starts_with(dir) && self.is_dirty(i))
                    .count();
                (
                    format!(
                        "Opening another project closes its tabs. {n} file{} with unsaved changes. Do you want to save {}?",
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "it" } else { "them" }
                    ),
                    "Your changes will be lost if you don't save them.".to_string(),
                    "Save",
                    "Don't Save",
                )
            }
            Confirm::Quit => {
                let n = (0..self.docs.len()).filter(|&i| self.is_dirty(i)).count();
                (
                    format!(
                        "You have {n} file{} with unsaved changes. Do you want to save {}?",
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "it" } else { "them" }
                    ),
                    "Your changes will be lost if you don't save them.".to_string(),
                    "Save All",
                    "Don't Save",
                )
            }
        };
        enum Answer {
            Save,
            Discard,
            Cancel,
        }
        let mut answer = None;
        style::dialog(ctx, "confirm-dialog", None, 380.0, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(title).strong().size(14.0));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(detail).weak());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(discard_label).clicked() {
                    answer = Some(Answer::Discard);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(save_label).clicked() {
                        answer = Some(Answer::Save);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(Answer::Cancel);
        } else if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer = Some(Answer::Save);
        }
        let Some(answer) = answer else {
            return;
        };
        self.confirm = None;
        match (what, answer) {
            (_, Answer::Cancel) => self.project_switch = None,
            (Confirm::Project(dir), a) => {
                if let Some((_, then)) = self.project_switch.take() {
                    self.close_tabs_outside(&dir, matches!(a, Answer::Save));
                    self.enter_project(dir);
                    then(self);
                }
            }
            (Confirm::Tab(path), a) => {
                if let Some(i) = self.docs.iter().position(|d| d.path == path) {
                    match a {
                        Answer::Save => {
                            self.save_doc(i);
                            self.close_doc(i);
                        }
                        _ => self.discard_doc(i),
                    }
                }
            }
            (Confirm::Quit, a) => {
                if matches!(a, Answer::Save) {
                    self.save_all();
                }
                self.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Draws the dialog while it is open and acts on its answer.
    fn show_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.export_dialog else {
            return;
        };
        match dialog.show(ctx, &self.root) {
            None => {}
            Some(export_dialog::Answer::Cancel) => self.export_dialog = None,
            Some(export_dialog::Answer::Export(name, info)) => {
                let start = self.docs.get(self.active).map(|d| program_for(&d.path));
                // A Save panel: the name is the file name of the disk image (the app beside it
                // gets the same name), the folder is where both are written.
                let dir = start
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new("."));
                let suggested = format!("{}.dmg", pascoda_fpc::export::clean_name(&name));
                let Some(chosen) = rfd::FileDialog::new()
                    .set_title("Export Application")
                    .set_directory(dir)
                    .set_file_name(suggested)
                    .add_filter("Disk image", &["dmg"])
                    .save_file()
                else {
                    return; // the dialog stays open
                };
                let Some((out, name)) = export_target(&chosen, &name) else {
                    return;
                };
                if let Some(d) = self.export_dialog.take() {
                    if let Err(e) = pascoda_project::save_export(&self.root, &d.settings) {
                        self.console = format!("Could not save the export settings: {e}\n");
                    }
                }
                self.export_to(ctx, out, name, info);
            }
        }
    }

    /// The export itself, into `out`.
    fn export_to(
        &mut self,
        ctx: &egui::Context,
        out: PathBuf,
        name: String,
        info: pascoda_fpc::lcl::BundleInfo,
    ) {
        let Some(path) = self.docs.get(self.active).map(|d| program_for(&d.path)) else {
            return;
        };
        self.save_all();
        self.sync_mode();
        let mode = self.mode;
        // how many units the last export of this program compiled: sizes the bar
        let total = self.unit_counts.get(&export_key(&path)).copied();
        self.export_source = path.clone();
        self.console = format!("Exporting {name}...\n");
        self.auto_tab(ctx, bottom::BottomTab::Output);
        let (tx, rx) = channel();
        let (ptx, prx) = channel();
        self.build_progress = None;
        self.build_progress_rx = Some(prx);
        let repaint = ctx.clone();
        let out_dir = out.clone();
        std::thread::spawn(move || {
            let mut on = move |p: pascoda_fpc::Progress| {
                let _ = ptx.send(p);
                repaint.request_repaint();
            };
            let lcl = if is_gui_program(&path) {
                pascoda_fpc::lcl::Lcl::find(&path)
            } else {
                None
            };
            let r = if is_gui_program(&path) && lcl.is_none() {
                Err(std::io::Error::other(
                    "vendor/lazarus not found (needed for GUI programs)",
                ))
            } else {
                pascoda_fpc::export::export(
                    &path,
                    mode,
                    lcl.as_ref(),
                    &out_dir,
                    &name,
                    &info,
                    true,
                    total,
                    &mut on,
                )
            };
            let _ = tx.send(r);
        });
        self.exporting = Some(Exporting { rx, out });
    }

    fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.exporting else { return };
        if let Some(prx) = &self.build_progress_rx {
            while let Ok(p) = prx.try_recv() {
                self.build_progress = Some(p);
            }
        }
        let Ok(result) = job.rx.try_recv() else {
            return;
        };
        let out = job.out.clone();
        self.exporting = None;
        if matches!(&result, Ok(d) if d.build.success)
            && let Some(p) = self.build_progress.as_ref().filter(|p| p.done > 0)
        {
            self.unit_counts
                .insert(export_key(&self.export_source), p.done);
        }
        self.build_progress = None;
        self.build_progress_rx = None;
        match result {
            Ok(done) if done.build.success => {
                self.console = done.build.raw_output.clone();
                for p in done.product.iter().chain(done.dmg.iter()) {
                    self.console
                        .push_str(&format!("\nExported {}", p.display()));
                }
                self.console.push('\n');
                self.diagnostics = done.build.diagnostics;
                // Show the result in Finder (not from tests).
                #[cfg(not(test))]
                {
                    let shown = done.dmg.or(done.product);
                    let mut open = std::process::Command::new("open");
                    match shown {
                        Some(p) => open.arg("-R").arg(p),
                        None => open.arg(&out),
                    };
                    let _ = open.spawn();
                }
                #[cfg(test)]
                let _ = &out;
            }
            Ok(done) => {
                self.console = format!("{}\nExport failed.\n", done.build.raw_output);
                self.diagnostics = done.build.diagnostics;
                self.auto_tab(ctx, bottom::BottomTab::Problems);
            }
            Err(e) => {
                self.console = format!("Export failed: {e}\n");
            }
        }
    }

    /// The Problems list shows the live result for `path` in place of what an earlier build or
    /// check said about that file; other files' entries stay.
    fn merge_live_problems(&mut self, path: &Path, live: &[Diagnostic]) {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let this = canon(path);
        let dir = self.build_dir.clone();
        self.diagnostics
            .retain(|d| canon(&dir.join(&d.file)) != this);
        self.diagnostics.extend(live.iter().cloned());
    }

    /// Re-reads the language mode when the project folder changed.
    fn sync_mode(&mut self) {
        if self.mode_root != self.root {
            self.mode_root = self.root.clone();
            self.mode = pascoda_project::mode_for(&self.root);
        }
    }

    /// Sets the project's language mode, saves it and re-checks the open file.
    fn set_mode(&mut self, mode: pascoda_fpc::Mode) {
        self.sync_mode();
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        if let Err(e) = pascoda_project::save_mode(&self.root, mode) {
            self.console = format!("Could not save the language mode: {e}\n");
        }
        self.live.forget();
    }

    fn poll_live_check(&mut self, ctx: &egui::Context) {
        self.sync_mode();
        if !self.live.enabled {
            return;
        }
        if let Some(done) = self.live.poll() {
            // Errors (no fpc, timeout) are silent: the build reports them properly.
            if let Ok(diags) = done.result {
                self.merge_live_problems(&done.path, &diags);
                self.marks.retain(|m| m.path != done.path);
                self.marks
                    .extend(livecheck::marks(&done.path, &done.text, &diags));
            }
        }
        if self.live.busy() || self.building.is_some() {
            return;
        }
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        if !pascoda_fpc::check::checkable(&doc.path) {
            return;
        }
        if self
            .live
            .due(&doc.path, &doc.text, std::time::Instant::now())
        {
            let (path, text) = (doc.path.clone(), doc.text.clone());
            self.live.start(ctx, path, text, self.mode);
        } else {
            // Come back when the pause is over, even if nothing else repaints.
            ctx.request_repaint_after(livecheck::QUIET);
        }
    }

    /// Puts build diagnostics into the open files they belong to (fpc lines and columns are
    /// 1-based).
    fn place_marks(&self, diagnostics: &[Diagnostic]) -> Vec<hints::FileMarks> {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        self.build_texts
            .iter()
            .filter_map(|(path, text)| {
                let items: Vec<_> = diagnostics
                    .iter()
                    .filter(|d| {
                        matches!(
                            d.severity,
                            Severity::Fatal | Severity::Error | Severity::Warning
                        ) && canon(&self.build_dir.join(&d.file)) == canon(path)
                    })
                    .map(|d| hints::Mark {
                        line: d.line.saturating_sub(1),
                        col: d.col.saturating_sub(1),
                        severity: d.severity,
                        message: d.message.clone(),
                    })
                    .collect();
                (!items.is_empty()).then(|| hints::FileMarks {
                    path: path.clone(),
                    text: text.clone(),
                    items,
                })
            })
            .collect()
    }

    fn start_debug(&mut self, ctx: &egui::Context) {
        let exe = pascoda_project::executable_for(&self.build_source);
        let folder = self.build_dir.clone();
        let cwd = pascoda_project::run_folder(&folder, &exe);
        let args = pascoda_project::load_run(&folder).args;
        let breakpoints = self
            .docs
            .iter()
            .filter(|d| !d.breakpoints.is_empty())
            .map(|d| (d.path.clone(), debugger::breakpoint_specs(&d.breakpoints, &d.stops, &d.disabled)))
            .collect();
        self.console.push_str("\n--- debugging ---\n");
        self.auto_tab(ctx, bottom::BottomTab::Output);
        self.var_open.clear();
        self.session = Some(debugger::spawn(exe, cwd, args, breakpoints, ctx.clone()));
        // The watches of earlier sessions are watched again, from the first stop.
        self.send(debugger::Cmd::Watches(self.watch_list.clone()));
        self.send(debugger::Cmd::ExceptionStops(self.exception_stops));
    }

    /// Replaces the watch expressions and, while the program is stopped, asks for their values now.
    fn set_watches(&mut self, list: Vec<String>) {
        self.watch_list = list.clone();
        self.send(debugger::Cmd::Watches(list));
    }

    fn send(&self, cmd: debugger::Cmd) {
        if let Some(s) = &self.session {
            let _ = s.cmds.send(cmd);
        }
    }

    fn poll_debug(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else { return };
        let events: Vec<_> = session.events.try_iter().collect();
        for event in events {
            match event {
                debugger::Event::Output(text) => self.console.push_str(&text),
                debugger::Event::Children(path, kids) => {
                    if let Some(v) = self.paused.as_mut().and_then(|p| p.var_mut(&path)) {
                        v.children = Some(kids);
                    }
                }
                debugger::Event::Watched(values) => {
                    if let Some(p) = self.paused.as_mut() {
                        p.set_scope(debugger::WATCH_SCOPE, values);
                    }
                }
                debugger::Event::Assigned(result) => match result {
                    Ok(()) => self.value_edit = None,
                    Err(e) => {
                        if let Some(v) = self.value_edit.as_mut() {
                            v.error = e;
                        }
                    }
                },
                debugger::Event::Refreshed(vars) => {
                    if let Some(p) = self.paused.as_mut() {
                        p.vars = vars;
                    }
                }
                debugger::Event::Evaluated(expr, result) => {
                    self.watch_eval = Some((expr, result));
                }
                debugger::Event::Stopped(p) => {
                    self.auto_tab(ctx, bottom::BottomTab::Debug);
                    if let Some((file, line)) = p.location() {
                        let file = file.to_path_buf();
                        self.paused = Some(p);
                        self.goto(ctx, &file, line, 1);
                    } else {
                        self.paused = Some(p);
                    }
                }
                debugger::Event::Ended(reason) => {
                    self.console.push_str(&format!("\n--- {reason} ---\n"));
                    self.session = None;
                    self.paused = None;
                    break;
                }
            }
        }
    }

    /// Shows `tab` in the bottom pane unless the user is using it (see `BottomPane::auto`).
    fn auto_tab(&mut self, ctx: &egui::Context, tab: bottom::BottomTab) {
        let now = ctx.input(|i| i.time);
        self.bottom.auto(tab, now);
    }

    fn cursor_index(&self, ctx: &egui::Context) -> Option<usize> {
        let doc = self.docs.get(self.active)?;
        let state = egui::TextEdit::load_state(ctx, doc.editor_id)?;
        state.cursor.char_range().map(|r| r.primary.index.0)
    }

    /// `auto` is completion started by typing: it stays silent when it cannot help.
    fn request_completion(&mut self, ctx: &egui::Context, auto: bool) {
        let Some(lsp) = self.lsp.as_ref().filter(|_| !auto || self.lsp_ready) else {
            if !auto {
                self.console
                    .push_str("completion: language server is off\n");
                // Off because the server is not installed: say so once, with the way to install it.
                if !self.vendor_asked_pasls {
                    self.vendor_asked_pasls = true;
                    self.offer_vendor_install(&[pascoda_fpc::vendor::Part::Pasls, pascoda_fpc::vendor::Part::Fpc]);
                }
            }
            return;
        };
        let (Some(doc), Some(cursor)) = (self.docs.get(self.active), self.cursor_index(ctx)) else {
            if !auto {
                self.console
                    .push_str("completion: click inside the editor first\n");
            }
            return;
        };
        if auto && !hints::should_complete(&doc.text, cursor) {
            return;
        }
        self.completion_auto = auto;
        let (line, col) = line_col(&doc.text, cursor);
        let start = word_start(&doc.text, cursor);
        let _ = lsp.requests.send(lsp::Req::Complete {
            path: doc.path.clone(),
            text: doc.text.clone(),
            line,
            col,
        });
        self.pending_completion = Some((doc.path.clone(), doc.text.clone(), start, cursor));
    }

    fn request_hover(&mut self, path: PathBuf, text: String, line: u32, col: u32) {
        let Some(lsp) = self.lsp.as_ref().filter(|_| self.lsp_ready) else {
            return;
        };
        self.hover.requested = Some((line, col));
        let _ = lsp.requests.send(lsp::Req::Hover {
            path,
            text,
            line,
            col,
        });
    }

    /// Asks the server for the signature of the call at the caret (after `(` or `,`).
    fn request_signature(&mut self, path: PathBuf, text: String, line: u32, col: u32, at: usize, key: u64) {
        let Some(lsp) = self.lsp.as_ref().filter(|_| self.lsp_ready) else {
            return;
        };
        let _ = lsp.requests.send(lsp::Req::Signature { path, text, line, col, at, key });
    }

    fn request_definition(&mut self, ctx: &egui::Context) {
        if let Some(cursor) = self.cursor_index(ctx) {
            self.request_definition_at(cursor);
        }
    }

    /// Asks the server for every use of the symbol under the caret (Shift+F12).
    fn request_references(&mut self, ctx: &egui::Context) {
        let Some(index) = self.cursor_index(ctx) else { return };
        let (Some(lsp), Some(doc)) = (&self.lsp, self.docs.get(self.active)) else {
            self.console.push_str("language server is not running\n");
            return;
        };
        let Some((_, name)) = find::word_at(&doc.text, index) else {
            self.console.push_str("put the caret on a name first\n");
            return;
        };
        let (line, col) = line_col(&doc.text, index);
        self.project_bar.title = format!("References to {name}");
        let _ = lsp.requests.send(lsp::Req::References {
            path: doc.path.clone(),
            text: doc.text.clone(),
            line,
            col,
        });
    }

    /// Asks the server to rename the symbol in the rename box to `self.rename.name`.
    fn send_rename(&mut self) {
        let (Some(lsp), Some(path), Some(doc)) = (
            &self.lsp,
            self.rename.path.clone(),
            self.docs.iter().find(|d| Some(&d.path) == self.rename.path.as_ref()),
        ) else {
            self.rename.error = "The language server is not running".into();
            return;
        };
        // The open files as they are now: the server's answer is checked against them.
        let open: Vec<(PathBuf, String)> = self.docs.iter().map(|d| (d.path.clone(), d.text.clone())).collect();
        let new_name = self.rename.name.clone();
        let _ = lsp.requests.send(lsp::Req::Rename {
            path,
            text: doc.text.clone(),
            line: self.rename.line,
            col: self.rename.col,
            new_name: new_name.clone(),
        });
        self.rename.sent = Some(find::Sent {
            old: self.rename.old.clone(),
            new: new_name,
            open,
        });
        self.rename.open = false;
        self.rename.error.clear();
    }

    /// Asks the server where the symbol at char `index` of the active tab is defined.
    fn request_definition_at(&mut self, index: usize) {
        let (Some(lsp), Some(doc)) = (&self.lsp, self.docs.get(self.active)) else {
            return;
        };
        let (line, col) = line_col(&doc.text, index);
        let _ = lsp.requests.send(lsp::Req::Definition {
            path: doc.path.clone(),
            text: doc.text.clone(),
            line,
            col,
        });
    }

    /// Starts the language server for the project folder, and again when the folder changes.
    /// Not for `/` or the home folder (a Finder launch has no project; pasls would index it all).
    fn follow_project_with_lsp(&mut self, ctx: &egui::Context) {
        if self.lsp_root.as_deref() == Some(self.root.as_path()) || !lsp_worthy(&self.root) {
            return;
        }
        self.lsp_root = Some(self.root.clone());
        self.lsp_ready = false;
        self.lsp = root_abs(&self.root).and_then(|r| lsp::spawn(&r, ctx.clone()));
        if self.lsp.is_none() {
            self.console
                .push_str("language server off: vendor/pasls not found\n");
        }
    }

    fn poll_lsp(&mut self, ctx: &egui::Context) {
        self.follow_project_with_lsp(ctx);
        let Some(lsp) = &self.lsp else { return };
        let responses: Vec<_> = lsp.responses.try_iter().collect();
        for resp in responses {
            match resp {
                lsp::Resp::Ready => self.lsp_ready = true,
                lsp::Resp::Completion(items) => {
                    let pending = self.pending_completion.take();
                    let auto = self.completion_auto;
                    // An answer for text that has since changed is stale.
                    let current = pending.filter(|(path, text, ..)| {
                        self.docs
                            .get(self.active)
                            .is_some_and(|d| d.path == *path && d.text == *text)
                    });
                    match current {
                        Some((_, text, start, cursor)) if !items.is_empty() => {
                            // Typing `foo` with only `foo` on offer needs no popup.
                            let word: String =
                                text.chars().skip(start).take(cursor - start).collect();
                            if !(auto && items.iter().all(|i| i.label.eq_ignore_ascii_case(&word)))
                            {
                                self.completion = Some(Completion {
                                    text,
                                    start,
                                    cursor,
                                    items,
                                    selected: 0,
                                    chosen: !auto,
                                });
                            }
                        }
                        Some(_) if !auto => self
                            .console
                            .push_str("completion: nothing to suggest here\n"),
                        _ => {}
                    }
                }
                lsp::Resp::Definition(Some(loc)) => {
                    self.goto(ctx, &loc.path, loc.line + 1, loc.col + 1);
                }
                lsp::Resp::Definition(None) => {}
                lsp::Resp::Signature { at, key, sig } => {
                    let current = self
                        .docs
                        .get(self.active)
                        .is_some_and(|d| shell::text_key(&d.text) == key);
                    self.signature = if current {
                        sig.map(|sig| shell::SigHint { key, at, sig })
                    } else {
                        None
                    };
                }
                lsp::Resp::Symbols { path, key, symbols } => {
                    if self.outline.pending.as_ref() == Some(&(path.clone(), key)) {
                        self.outline.pending = None;
                    }
                    self.outline.shown = Some((path, key));
                    self.outline.symbols = symbols;
                }
                lsp::Resp::References(locs) => {
                    self.show_references(locs);
                    self.nav_tab = shell::NavTab::Search;
                    self.show_navigator = true;
                }
                lsp::Resp::Rename(files) => {
                    if let Some(sent) = self.rename.sent.take() {
                        self.apply_rename(files, &sent);
                    }
                }
                lsp::Resp::Hover(line, col, text) => {
                    let pos = self.hover_pos;
                    self.hover.accept(line, col, text, pos);
                }
                lsp::Resp::Error(e) => {
                    self.console.push_str(&format!("language server: {e}\n"));
                    let empty = self.empty_sources();
                    if !empty.is_empty() {
                        self.console.push_str(&format!(
                            "pasls does not start while these files are empty: {}. Add some code to them and reopen the project.\n",
                            empty.join(", ")
                        ));
                    }
                    self.lsp = None;
                    self.lsp_ready = false;
                    return;
                }
            }
        }
    }

    /// Replaces the word being typed with completion `index`.
    fn accept_completion(&mut self, ctx: &egui::Context, index: usize) {
        let Some(c) = self.completion.take() else {
            return;
        };
        let (Some(item), Some(doc)) = (c.items.get(index), self.docs.get_mut(self.active)) else {
            return;
        };
        let chars: Vec<char> = doc.text.chars().collect();
        let (start, cursor) = (c.start.min(chars.len()), c.cursor.min(chars.len()));
        let mut text: String = chars[..start].iter().collect();
        text.push_str(&item.label);
        text.extend(&chars[cursor..]);
        doc.text = text;
        let id = doc.editor_id;
        let at = start + item.label.chars().count();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(at))));
        state.store(ctx, id);
    }

    fn handle_editor_keys(&mut self, ctx: &egui::Context, accept: &mut Option<usize>) {
        use egui::{Key, Modifiers};
        if self.bottom.tab == bottom::BottomTab::Terminal && self.bottom.terminal_focused {
            // The terminal owns the keyboard (Ctrl+Space, arrows, Esc are the shell's).
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Space)) {
            self.request_completion(ctx, false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F12)) {
            self.request_definition(ctx);
        }
        // Find (Cmd+F), replace (Cmd+Opt+F), next and previous match (Cmd+G, Shift+Cmd+G).
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::ALT, Key::F)) {
            self.find.show(true);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.find.show(false);
        }
        // References (Shift+F12) and Rename Symbol (F2).
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F12)) {
            self.request_references(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2)) {
            self.open_rename(ctx);
        }
        // Go to Line (Cmd+L) opens its box with the field focused.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::L)) {
            self.go_to.open = true;
            self.go_to.focus = true;
            self.go_to.error.clear();
        }
        // Project search (Cmd+Shift+F) opens its tab with the query focused.
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::F)) {
            self.nav_tab = shell::NavTab::Search;
            self.show_navigator = true;
            self.project_bar.focus = true;
        }
        if self.find.open {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::G)) {
                self.find.pending = Some(false);
            } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::G)) {
                self.find.pending = Some(true);
            }
        }
        let Some(c) = &mut self.completion else {
            return;
        };
        let (down, up, enter, tab, esc) = ctx.input_mut(|i| {
            // Enter and Tab belong to the editor until an item has been picked.
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                c.chosen && i.consume_key(Modifiers::NONE, Key::Enter),
                c.chosen && i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let last = c.items.len().saturating_sub(1);
        if down {
            // The first press picks the first item rather than skipping it.
            c.selected = if c.chosen {
                (c.selected + 1).min(last)
            } else {
                0
            };
            c.chosen = true;
        }
        if up {
            c.selected = c.selected.saturating_sub(1);
            c.chosen = true;
        }
        if enter || tab {
            *accept = Some(c.selected);
        }
        if esc {
            self.completion = None;
            self.pending_completion = None;
        }
    }

    /// Opens the unit and puts the cursor in the body of `handler`.
    fn show_handler(&mut self, ctx: &egui::Context, path: &Path, handler: &str) {
        let Some(doc) = self.docs.iter().find(|d| d.path == path) else {
            return;
        };
        let lower = doc.text.to_ascii_lowercase();
        // The implementation header is `procedure TForm1.Name(`; the declaration has no dot.
        let Some(header) = lower.find(&format!(".{}(", handler.to_ascii_lowercase())) else {
            return;
        };
        let Some(begin) = lower[header..].find("begin").map(|i| header + i) else {
            return;
        };
        let line = doc.text[..begin].matches('\n').count() as u32 + 2; // the line after `begin`
        self.goto(ctx, path, line, 1);
    }

    /// Drives the designer through a fixed sequence and writes what it saw to `PASCODA_SELFTEST`.
    fn selftest_step(&mut self, ctx: &egui::Context) {
        let Some(st) = &mut self.selftest else { return };
        st.frame += 1;
        let (frame, dir) = (st.frame, st.dir.clone());
        ctx.request_repaint();
        let out = |name: &str| dir.join(name);
        if st.scroll {
            self.selftest_scroll(ctx, frame, &dir);
            return;
        }
        match frame {
            1 => {
                if self.has_form() {
                    self.show_form();
                } else {
                    eprintln!("selftest: the opened file has no form");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            3 => {
                if let Some(n) = &self.native {
                    for (class, text) in [
                        ("TButton", "OK"),
                        ("TButton", "Hello world"),
                        ("TButton", "A much longer caption here"),
                        ("TLabel", "OK"),
                        ("TLabel", "Some label text"),
                        ("TLabel", "A much longer caption here"),
                        ("TCheckBox", "OK"),
                        ("TCheckBox", "Check me"),
                        ("TCheckBox", "A much longer caption here"),
                        ("TRadioButton", "OK"),
                        ("TRadioButton", "Pick me"),
                        ("TRadioButton", "A much longer caption here"),
                        ("TEdit", "abc"),
                    ] {
                        let got = n.preferred_size(class, text);
                        // Sizes the real LCL gives (autosize_truth test).
                        let want = match (class, text) {
                            ("TButton", "OK") => (41, 26),
                            ("TButton", "Hello world") => (90, 26),
                            ("TButton", _) => (189, 26),
                            ("TLabel", "OK") => (18, 16),
                            ("TLabel", "Some label text") => (93, 16),
                            ("TLabel", _) => (167, 16),
                            ("TCheckBox", "OK") | ("TRadioButton", "OK") => (44, 18),
                            ("TCheckBox", "Check me") => (83, 18),
                            ("TRadioButton", "Pick me") => (71, 18),
                            ("TCheckBox", _) | ("TRadioButton", _) => (182, 18),
                            _ => (0, 20), // TEdit: only the height is auto-sized
                        };
                        let ok = got.is_some_and(|(w, h)| {
                            h == want.1 && (want.0 == 0 || (w - want.0).abs() <= 1)
                        });
                        eprintln!(
                            "selftest: preferred {class} {text:?} = {got:?} (LCL {want:?}) {}",
                            if ok { "ok" } else { "MISMATCH" }
                        );
                    }
                }
            }
            8 => {
                if let Some(n) = &self.native {
                    let r = n.snapshot(&out("designer-1.png"));
                    eprintln!(
                        "selftest: {} native controls, snapshot 1: {r:?}",
                        n.control_count()
                    );
                } else {
                    eprintln!(
                        "selftest: no native layer ({:?})",
                        self.designer_ui.native_error
                    );
                }
            }
            9 => {
                let Some(doc) = self.docs.get_mut(self.active) else {
                    return;
                };
                let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == doc.path) else {
                    return;
                };
                let _ = form.add_component("TButton", &[], 16, 16);
                let _ = form.ensure_handler("OnClick");
                let _ = form.add_component("TEdit", &[], 16, 64);
                let _ = form.add_component("TCheckBox", &[], 16, 104);
                // A multi-selection: handles on Button1, outlines on Edit1 and CheckBox1.
                form.select_paths(vec![vec![0], vec![4], vec![5]]);
                form.sync_unit(&mut doc.text);
                eprintln!("selftest: sync error: {:?}", form.sync_error);
            }
            10 => {
                let Some(doc) = self.docs.get(self.active) else {
                    return;
                };
                let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == doc.path) else {
                    return;
                };
                // Label1 is auto-sized by default: a longer text must widen it.
                let _ = form.set_prop(
                    &[1],
                    "Caption",
                    pascoda_forms::PropValue::Str("A longer label text".into()),
                );
            }
            11 => {
                if let Some(n) = &self.native {
                    let r = n.snapshot(&out("designer-multi.png"));
                    eprintln!("selftest: multi-selection overlay: {r:?}");
                }
            }
            12 => {
                let Some(doc) = self.docs.get(self.active) else {
                    return;
                };
                let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == doc.path) else {
                    return;
                };
                let b = |f: &designer::Designer, p: &[usize]| {
                    f.doc.component(p).map(designer::geometry::bounds)
                };
                eprintln!(
                    "selftest: before resize: label {:?} edit {:?}",
                    b(form, &[1]),
                    b(form, &[3])
                );
                // Edit1 stretches with the form; the check box follows the bottom edge.
                let _ = form.set_anchors(
                    &[3],
                    designer::geometry::Anchors {
                        left: true,
                        top: true,
                        right: true,
                        bottom: false,
                    },
                );
                let _ = form.set_anchors(
                    &[4],
                    designer::geometry::Anchors {
                        left: true,
                        top: false,
                        right: false,
                        bottom: true,
                    },
                );
                let _ = form.resize_container(&[], 520, 260);
                eprintln!(
                    "selftest: after resize 320x160 -> 520x260: edit {:?} checkbox {:?}",
                    b(form, &[3]),
                    b(form, &[4])
                );
            }
            13 => {
                let Some(doc) = self.docs.get(self.active) else {
                    return;
                };
                let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == doc.path) else {
                    return;
                };
                // One of every palette class, for the look of the native previews.
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1500.0, 1000.0)));
                let _ = form.resize_container(&[], 540, 400);
                for (i, en) in designer::PALETTE.iter().enumerate() {
                    let _ = form.add_component(
                        en.class,
                        &[],
                        8 + (i as i64 % 6) * 88,
                        100 + (i as i64 / 6) * 48,
                    );
                }
            }
            22 => {
                if let Some(n) = &self.native {
                    let r = n.snapshot(&out("designer-3.png"));
                    eprintln!(
                        "selftest: {} native controls, all classes: {r:?}",
                        n.control_count()
                    );
                }
            }
            16 => {
                if let Some(n) = &self.native {
                    let r = n.snapshot(&out("designer-2.png"));
                    eprintln!(
                        "selftest: {} native controls, snapshot 2: {r:?}",
                        n.control_count()
                    );
                }
                if let Some(doc) = self.docs.get(self.active) {
                    let _ = std::fs::write(out("unit1.pas.txt"), &doc.text);
                    if let Some(f) = self.designers.iter().find(|f| f.pas_path == doc.path) {
                        let _ = std::fs::write(out("unit1.lfm.txt"), f.doc.source());
                    }
                }
            }
            20 => {
                // Where the toolchain comes from, and whether it can build a form from there.
                let lpr = self.docs.get(self.active).and_then(|d| {
                    std::fs::read_dir(d.path.parent()?)
                        .ok()?
                        .flatten()
                        .map(|e| e.path())
                        .find(|p| p.extension().is_some_and(|x| x == "lpr"))
                });
                let start = lpr.clone().unwrap_or_default();
                let lcl = pascoda_fpc::lcl::Lcl::find(&start);
                eprintln!(
                    "selftest: LCL from {:?}",
                    lcl.as_ref().map(|l| &l.lazarus_dir)
                );
                eprintln!(
                    "selftest: pasls from {:?}",
                    pascoda_lsp::Config::find(&start).map(|c| c.pasls)
                );
                if let (Some(lcl), Some(lpr)) = (lcl, lpr) {
                    let r = pascoda_fpc::lcl::build_gui(&lpr, pascoda_fpc::Mode::default(), &lcl);
                    eprintln!(
                        "selftest: GUI build: {:?}",
                        r.map(|b| (b.success, b.diagnostics.len()))
                    );
                }
            }
            5 => {
                // The window buttons sit on the toolbar's centre line; a second call has
                // nothing left to move (so the move is not oscillating).
                let first = chrome::align_window_buttons(44.0);
                let second = chrome::align_window_buttons(44.0);
                eprintln!(
                    "selftest: window buttons moved {first} pt, then {second} pt; close button centre {:?} pt from the top (toolbar centre 22)",
                    chrome::close_button_center_from_top()
                );
            }
            18 => eprintln!(
                "selftest: About menu item found and chosen: {}",
                about::open_via_menu()
            ),
            24 => eprintln!(
                "selftest: about panel: {:?}",
                about::snapshot_key_window(&out("about.png"))
            ),
            28 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    /// The scrolling self-test (see `SelfTest::scroll`).
    fn selftest_scroll(&mut self, ctx: &egui::Context, frame: u32, dir: &Path) {
        let out = |name: &str| dir.join(name);
        let report = |app: &Self, label: &str| {
            let v = app.designer_ui.view;
            eprintln!(
                "selftest: {label}: view {:?} max scroll {:?} clip {:?}",
                v,
                v.map(|v| v.max_scroll()),
                app.native.as_ref().map(|n| n.clip_frame())
            );
        };
        let snap = |app: &Self, name: &str| {
            if let Some(n) = &app.native {
                eprintln!(
                    "selftest: {name}: {} native controls, canvas {:?}, window {:?}",
                    n.control_count(),
                    n.snapshot(&out(&format!("{name}.png"))),
                    n.snapshot_window(&out(&format!("{name}-window.png")))
                );
            } else {
                eprintln!(
                    "selftest: no native layer ({:?})",
                    app.designer_ui.native_error
                );
            }
        };
        match frame {
            1 => {
                if self.has_form() {
                    self.show_form();
                } else {
                    eprintln!("selftest: the opened file has no form");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            8 => {
                report(self, "top");
                snap(self, "scroll-top");
            }
            9 => {
                // Select the bottom-most control and scroll to the end of both axes.
                let Some(path) = self.docs.get(self.active).map(|d| d.path.clone()) else {
                    return;
                };
                if let Some(form) = self.designers.iter_mut().find(|f| f.pas_path == path) {
                    let root = form.doc.root();
                    let bottom = (0..root.children.len())
                        .filter_map(|i| {
                            designer::geometry::absolute(root, &[i]).map(|b| (i, b.top + b.height))
                        })
                        .max_by_key(|(_, bottom)| *bottom)
                        .map(|(i, _)| i);
                    if let Some(i) = bottom {
                        eprintln!("selftest: selecting {}", root.children[i].name);
                        form.select(vec![i]);
                    }
                }
                self.designer_ui.scroll.insert(path, egui::vec2(1e6, 1e6));
            }
            14 => {
                report(self, "end");
                snap(self, "scroll-end");
            }
            16 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    /// The Variables pane opened or closed rows: tell the debugger which paths to keep fetching
    /// and ask for the children it has not sent yet.
    fn sync_var_open(&mut self, fetch: Vec<(String, i64)>) {
        self.send(debugger::Cmd::Expanded(self.var_open.clone()));
        for (path, reference) in fetch {
            self.send(debugger::Cmd::Fetch(path, reference));
        }
    }

    /// The package the Build Package command builds: the active file when it is a package, else the
    /// first package in the active file's folder (the unit's own package when it has one).
    fn active_package(&self) -> Option<PathBuf> {
        let active = self.docs.get(self.active)?.path.clone();
        if pascoda_project::packages::read(&active).is_some() {
            return Some(active);
        }
        let folder = active.parent()?;
        let packages: Vec<(PathBuf, pascoda_project::lpk::Package)> = pascoda_project::packages::in_folder(folder)
            .into_iter()
            .filter_map(|f| pascoda_project::packages::read(&f).map(|p| (f, p)))
            .collect();
        packages
            .iter()
            .find(|(_, p)| p.units.iter().any(|u| u == &active))
            .or(packages.first())
            .map(|(f, _)| f.clone())
    }

    /// Builds every unit of the package `file` on a background thread, with the folder's arguments
    /// (the resolved packages included). The result shows like a build's.
    fn build_package(&mut self, ctx: &egui::Context, file: PathBuf) {
        let Some(pkg) = pascoda_project::packages::read(&file) else {
            self.console = "Not a package\n".into();
            return;
        };
        self.sync_mode();
        let mode = self.mode;
        self.save_all();
        self.build_texts = self.docs.iter().map(|d| (d.path.clone(), d.text.clone())).collect();
        self.debug_after_build = false;
        self.run_after_build = false;
        self.build_source = file.clone();
        let folder = file.parent().map(Path::to_path_buf).unwrap_or_default();
        self.build_dir = folder.clone();
        self.console = format!("Building package {}...\n", pkg.name);
        let (project_args, project_note) = project_args(Some(&folder));
        self.console.push_str(&project_note);
        self.auto_tab(ctx, bottom::BottomTab::Output);
        let (tx, rx) = channel();
        self.build_progress = None;
        self.build_progress_rx = None;
        self.building = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = pascoda_project::packages::build(&pkg, &project_args, mode);
            let _ = tx.send((result, String::new()));
            ctx.request_repaint();
        });
    }

    /// Removes the build products of the program (Build > Clean): the executables, compiled units and
    /// scratch files. The console says what was removed.
    fn clean_project(&mut self) {
        let Some(program) = self.build_source_or_active() else {
            self.console.push_str("Nothing to clean: open a program first\n");
            return;
        };
        match pascoda_project::clean_build(&program) {
            Ok(removed) if removed.is_empty() => self.console = "Nothing to clean\n".into(),
            Ok(removed) => {
                let names: Vec<String> = removed
                    .iter()
                    .map(|p| p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()))
                    .collect();
                self.console = format!("Cleaned {}\n", names.join(", "));
            }
            Err(e) => self.console = format!("Could not clean: {e}\n"),
        }
    }

    /// The program the build works on: the last build's, else the active tab's.
    fn build_source_or_active(&self) -> Option<PathBuf> {
        if !self.build_source.as_os_str().is_empty() {
            return Some(self.build_source.clone());
        }
        self.docs.get(self.active).map(|d| program_for(&d.path))
    }

    /// Runs the program until the caret's line (Run to Cursor, F4). Needs a stopped program.
    fn run_to_cursor(&mut self, ctx: &egui::Context) {
        if self.session.is_none() {
            self.console.push_str("Run to Cursor needs a debug session (Build > Debug)\n");
            return;
        }
        let (Some(index), Some(doc)) = (self.cursor_index(ctx), self.docs.get(self.active)) else {
            return;
        };
        let (line, _) = line_col(&doc.text, index);
        let cmd = debugger::Cmd::RunTo(doc.path.clone(), line + 1);
        if self.paused.is_some() {
            self.resume_with(cmd);
        } else {
            self.send(cmd);
        }
    }

    fn resume_with(&mut self, cmd: debugger::Cmd) {
        self.paused = None;
        self.watch_eval = None;
        self.send(cmd);
    }

    /// Opens the diagnostic's file and moves the cursor to its 1-based line/column.
    fn goto(&mut self, ctx: &egui::Context, file: &Path, line: u32, col: u32) {
        self.open_path(self.build_dir.join(file));
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        let mut offset = 0;
        for (i, l) in doc.text.split_inclusive('\n').enumerate() {
            if i + 1 == line as usize {
                offset += l.chars().take(col.saturating_sub(1) as usize).count();
                break;
            }
            offset += l.chars().count();
        }
        let id = doc.editor_id;
        // One character is the range the editor scrolls to, so the line is in view.
        let end = (offset + 1).min(doc.text.chars().count());
        self.reveal = Some(offset..end);
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(offset))));
        state.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

/// When `path` is a unit and its folder holds exactly one program file, that program is what
/// Build & Run should compile.
/// Where and under which name an export goes, from the path chosen in the Save panel: its folder,
/// and its file name without `.dmg` (`fallback` when that is empty).
fn export_target(chosen: &Path, fallback: &str) -> Option<(PathBuf, String)> {
    let dir = chosen.parent().filter(|d| !d.as_os_str().is_empty())?;
    let stem = chosen
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    Some((dir.to_path_buf(), stem))
}

/// Key of an export's unit count: a release build compiles every unit from scratch, so its
/// count differs from the (cached) debug builds of the same program.
fn export_key(program: &Path) -> PathBuf {
    program.with_extension("export")
}

/// The compiler arguments of the Lazarus or Delphi project file in `folder`, and a console note
/// when that file cannot be read.
fn project_args(folder: Option<&Path>) -> (Vec<std::ffi::OsString>, String) {
    let Some(folder) = folder else {
        return (Vec::new(), String::new());
    };
    let note = match pascoda_project::foreign::find(folder).map(|f| pascoda_project::foreign::read(&f)) {
        Some(Err(e)) => format!("project file not used: {e}\n"),
        _ => String::new(),
    };
    let mut note = note;
    for missing in pascoda_project::packages::resolve(folder).missing {
        note.push_str(&format!("A required package, {missing}, was not found: its units are not on the search path\n"));
    }
    (pascoda_project::compiler_args(folder), note)
}

/// The main program of the Lazarus or Delphi project file in `dir`, if it exists.
fn project_main(dir: &Path) -> Option<PathBuf> {
    let file = pascoda_project::foreign::find(dir)?;
    pascoda_project::foreign::read(&file)
        .ok()?
        .main
        .filter(|m| m.is_file())
}

fn program_for(path: &Path) -> PathBuf {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    // A program file builds itself.
    if ext == "lpr" || ext == "dpr" {
        return path.to_path_buf();
    }
    // Any other selected file (a unit, a package, a form file, a text file) builds the folder's program.
    let Some(dir) = path.parent() else {
        return path.to_path_buf();
    };
    // A Lazarus or Delphi project in the folder names its main program.
    if let Some(main) = project_main(dir) {
        return main;
    }
    let programs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lpr")))
        .collect();
    match programs.as_slice() {
        [only] => only.clone(),
        _ => path.to_path_buf(),
    }
}

/// A program that uses the LCL (`Interfaces` unit) needs the GUI build.
fn is_gui_program(source: &Path) -> bool {
    std::fs::read_to_string(source)
        .map(|t| t.to_ascii_lowercase().contains("interfaces"))
        .unwrap_or(false)
}

/// Wraps the built program in a `.app` and starts it; GUI apps run on their own, so this
/// does not wait for them to quit.
fn launch_app(source: &Path) -> String {
    let exe = pascoda_project::executable_for(source);
    let dir = exe.parent().unwrap_or(Path::new("."));
    let stem = exe
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    match pascoda_fpc::lcl::make_bundle(&dir.join(&stem), &stem) {
        Ok(bundle) => match Command::new(bundle.join("Contents/MacOS").join(&stem)).spawn() {
            Ok(_) => format!("started {}\n", bundle.display()),
            Err(e) => format!("could not start {}: {e}\n", bundle.display()),
        },
        Err(e) => format!("could not create the app bundle: {e}\n"),
    }
}

/// Runs the executable with its arguments, in `dir`, and returns what it printed. Both paths are
/// made absolute first: the child starts in `dir`, and a relative path would resolve twice.
fn run(exe: &Path, args: &[String], dir: &Path) -> String {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    match Command::new(&exe).args(args).current_dir(dir).output() {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => format!("could not run {}: {e}", exe.display()),
    }
}

/// The macOS menu bar. Its key equivalents are handled by AppKit, so they never reach egui.
struct NativeMenu {
    _menu: muda::Menu,
    new_gui: muda::MenuId,
    new_console: muda::MenuId,
    new_library: muda::MenuId,
    new_package: muda::MenuId,
    new_form: muda::MenuId,
    new_data_module: muda::MenuId,
    import_packages: muda::MenuId,
    new_file: muda::MenuId,
    new_folder: muda::MenuId,
    rename_item: muda::MenuId,
    preferences: muda::MenuId,
    run_to: muda::MenuId,
    rebuild: muda::MenuId,
    clean: muda::MenuId,
    build_package: muda::MenuId,
    /// File > Open Recent and its items: the menu's list (the entries it shows, by index), the
    /// items' ids and the items themselves (kept alive while they are in the menu).
    open_recent: muda::Submenu,
    recent_entries: Vec<recent::Entry>,
    recent_ids: Vec<(muda::MenuId, usize)>,
    recent_items: Vec<muda::MenuItem>,
    recent_clear: muda::MenuId,
    project_options: muda::MenuId,
    toggle_view: muda::MenuId,
    jump_bar: muda::CheckMenuItem,
    assistant: muda::CheckMenuItem,
    open: muda::MenuId,
    save: muda::MenuId,
    run: muda::MenuId,
    run_tests: muda::MenuId,
    debug: muda::MenuId,
    export: muda::MenuId,
    quit: muda::MenuId,
    edit: Vec<(muda::MenuItem, EditCmd)>,
    appearance: Vec<(muda::CheckMenuItem, theme::Appearance)>,
    events: Receiver<muda::MenuId>,
}

/// What the unsaved-changes question is about.
#[derive(Clone, PartialEq)]
enum Confirm {
    /// Closing the tab of this file.
    Tab(PathBuf),
    /// Opening another project folder: the tabs of the old one are closed.
    Project(PathBuf),
    /// Quitting Pascoda.
    Quit,
}

/// An export running on a worker thread, and where it writes.
struct Exporting {
    rx: Receiver<std::io::Result<pascoda_fpc::export::Exported>>,
    out: PathBuf,
}

#[derive(Clone, Copy)]
enum MenuAction {
    NewGui,
    NewForm,
    NewDataModule,
    ImportPackages,
    NewFile,
    NewFolder,
    RenameItem,
    ToggleView,
    ToggleJumpBar,
    ToggleAssistant,
    Preferences,
    RunTests,
    RunToCursor,
    ProjectOptions,
    Rebuild,
    Clean,
    NewConsole,
    NewLibrary,
    NewPackage,
    BuildPackage,
    /// An item of File > Open Recent (its index in the menu's list).
    OpenRecent(usize),
    ClearRecent,
    Open,
    Save,
    Run,
    Debug,
    Export,
    Quit,
    Appearance(theme::Appearance),
    Edit(EditCmd),
}

/// Edit-menu commands. AppKit's own Cut/Copy/Paste items would send selectors winit's view does
/// not implement, and their key equivalents would never reach egui, so these are plain items
/// that `edit_events` turns back into the events egui's text fields and the designer expect.
#[derive(Clone, Copy, PartialEq, Debug)]
enum EditCmd {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl EditCmd {
    /// The input events a focused egui widget would have received for this command.
    fn events(self, clipboard: impl FnOnce() -> String) -> Vec<egui::Event> {
        let key = |key, shift| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                mac_cmd: true,
                shift,
                ..Default::default()
            },
        };
        match self {
            Self::Undo => vec![key(egui::Key::Z, false)],
            Self::Redo => vec![key(egui::Key::Z, true)],
            Self::SelectAll => vec![key(egui::Key::A, false)],
            Self::Cut => vec![egui::Event::Cut],
            Self::Copy => vec![egui::Event::Copy],
            Self::Paste => vec![egui::Event::Paste(clipboard())],
        }
    }
}

/// The text on the system clipboard (empty when there is none).
fn clipboard_text() -> String {
    std::process::Command::new("pbpaste")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

impl NativeMenu {
    fn install(ctx: &egui::Context) -> Option<Self> {
        use muda::accelerator::{Accelerator, Code, Modifiers};
        use muda::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
        let cmd = |code| Some(Accelerator::new(Modifiers::META, code));
        let new_gui = MenuItem::new("New GUI Application...", true, cmd(Code::KeyN));
        let new_console = MenuItem::new("New Console Application...", true, None);
        let new_library = MenuItem::new("New Library...", true, None);
        let new_package = MenuItem::new("New Package...", true, None);
        let new_file = MenuItem::new("New File", true, None);
        let new_folder = MenuItem::new("New Folder", true, None);
        let rename_item = MenuItem::new("Rename", true, None);
        let new_form = MenuItem::new(
            "New Form",
            true,
            Some(Accelerator::new(Modifiers::META | Modifiers::SHIFT, Code::KeyN)),
        );
        let new_data_module = MenuItem::new("New Data Module", true, None);
        let toggle_view = MenuItem::new(
            "Switch Between Code and Form",
            true,
            Some(Accelerator::new(
                Modifiers::META | Modifiers::SHIFT,
                Code::KeyF,
            )),
        );
        let jump_bar = CheckMenuItem::new("Show Path Bar", true, false, None);
        let assistant = CheckMenuItem::new(
            "Show Assistant",
            true,
            false,
            Some(Accelerator::new(
                Modifiers::META | Modifiers::ALT,
                Code::KeyA,
            )),
        );
        let open = MenuItem::new("Open...", true, cmd(Code::KeyO));
        let import_packages = MenuItem::new("Import Component Package...", true, None);
        let save = MenuItem::new("Save", true, cmd(Code::KeyS));
        let run = MenuItem::new("Build & Run", true, cmd(Code::KeyR));
        let run_tests = MenuItem::new("Run Unit Tests", true, cmd(Code::KeyU));
        let debug = MenuItem::new(
            "Debug",
            true,
            Some(Accelerator::new(
                Modifiers::META | Modifiers::SHIFT,
                Code::KeyR,
            )),
        );
        let edit: Vec<_> = [
            ("Undo", EditCmd::Undo, Code::KeyZ, Modifiers::META),
            (
                "Redo",
                EditCmd::Redo,
                Code::KeyZ,
                Modifiers::META | Modifiers::SHIFT,
            ),
            ("Cut", EditCmd::Cut, Code::KeyX, Modifiers::META),
            ("Copy", EditCmd::Copy, Code::KeyC, Modifiers::META),
            ("Paste", EditCmd::Paste, Code::KeyV, Modifiers::META),
            (
                "Select All",
                EditCmd::SelectAll,
                Code::KeyA,
                Modifiers::META,
            ),
        ]
        .into_iter()
        .map(|(label, c, code, m)| {
            (
                MenuItem::new(label, true, Some(Accelerator::new(m, code))),
                c,
            )
        })
        .collect();
        let edit_menu = Submenu::new("Edit", true);
        for (i, (item, _)) in edit.iter().enumerate() {
            if i == 2 || i == 5 {
                edit_menu.append(&PredefinedMenuItem::separator()).ok()?;
            }
            edit_menu.append(item).ok()?;
        }
        let menu = Menu::new();
        // Not the system's Quit: that ends the process without asking about unsaved changes.
        let quit = MenuItem::new("Quit Pascoda", true, cmd(Code::KeyQ));
        let preferences = MenuItem::new("Preferences...", true, cmd(Code::Comma));
        let app = Submenu::with_items(
            "Pascoda",
            true,
            &[
                &PredefinedMenuItem::about(Some("About Pascoda"), Some(about::metadata())),
                &PredefinedMenuItem::separator(),
                &preferences,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::separator(),
                &quit,
            ],
        )
        .ok()?;
        let open_recent = Submenu::new("Open Recent", true);
        let recent_clear = MenuItem::new("Clear Menu", true, None);
        let file = Submenu::with_items(
            "File",
            true,
            &[
                &new_gui,
                &new_console,
                &new_library,
                &new_package,
                &new_form,
                &new_data_module,
                &PredefinedMenuItem::separator(),
                &new_file,
                &new_folder,
                &rename_item,
                &PredefinedMenuItem::separator(),
                &open_recent,
                &open,
                &save,
                &PredefinedMenuItem::separator(),
                &import_packages,
            ],
        )
        .ok()?;
        let export = MenuItem::new(
            "Export Application...",
            true,
            Some(Accelerator::new(
                Modifiers::META | Modifiers::SHIFT,
                Code::KeyE,
            )),
        );
        let run_to = MenuItem::new("Run to Cursor", true, Some(Accelerator::new(Modifiers::empty(), Code::F4)));
        let project_options = MenuItem::new("Project Options...", true, None);
        let rebuild = MenuItem::new("Rebuild", true, None);
        let clean = MenuItem::new("Clean", true, None);
        let build_package = MenuItem::new("Build Package", true, None);
        let build = Submenu::with_items(
            "Build",
            true,
            &[
                &run,
                &run_tests,
                &debug,
                &run_to,
                &PredefinedMenuItem::separator(),
                &project_options,
                &PredefinedMenuItem::separator(),
                &rebuild,
                &clean,
                &build_package,
                &PredefinedMenuItem::separator(),
                &export,
            ],
        )
        .ok()?;
        let current = theme::Appearance::load();
        let appearance: Vec<_> = theme::Appearance::ALL
            .into_iter()
            .map(|a| (CheckMenuItem::new(a.label(), true, a == current, None), a))
            .collect();
        let appearance_menu = Submenu::new("Appearance", true);
        for (item, _) in &appearance {
            appearance_menu.append(item).ok()?;
        }
        let view = Submenu::with_items(
            "View",
            true,
            &[&toggle_view, &jump_bar, &assistant, &appearance_menu],
        )
        .ok()?;
        menu.append_items(&[&app, &file, &edit_menu, &view, &build])
            .ok()?;
        menu.init_for_nsapp();

        let (tx, events) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
            let _ = tx.send(e.id);
            ctx.request_repaint();
        }));
        Some(Self {
            _menu: menu,
            new_gui: new_gui.id().clone(),
            new_console: new_console.id().clone(),
            new_library: new_library.id().clone(),
            new_package: new_package.id().clone(),
            new_form: new_form.id().clone(),
            new_data_module: new_data_module.id().clone(),
            import_packages: import_packages.id().clone(),
            new_file: new_file.id().clone(),
            new_folder: new_folder.id().clone(),
            rename_item: rename_item.id().clone(),
            preferences: preferences.id().clone(),
            run_to: run_to.id().clone(),
            rebuild: rebuild.id().clone(),
            clean: clean.id().clone(),
            build_package: build_package.id().clone(),
            open_recent,
            recent_entries: Vec::new(),
            recent_ids: Vec::new(),
            recent_items: Vec::new(),
            recent_clear: recent_clear.id().clone(),
            project_options: project_options.id().clone(),
            toggle_view: toggle_view.id().clone(),
            jump_bar,
            assistant,
            open: open.id().clone(),
            save: save.id().clone(),
            run: run.id().clone(),
            run_tests: run_tests.id().clone(),
            debug: debug.id().clone(),
            export: export.id().clone(),
            quit: quit.id().clone(),
            edit,
            appearance,
            events,
        })
    }

    /// Ticks the chosen appearance, and only that one.
    fn show_appearance(&self, chosen: theme::Appearance) {
        for (item, a) in &self.appearance {
            item.set_checked(*a == chosen);
        }
    }

    /// Rebuilds File > Open Recent for `entries`: the projects, a separator, the files, and Clear
    /// Menu. Nothing recent shows a disabled note.
    fn set_recent(&mut self, entries: Vec<recent::Entry>) {
        while self.open_recent.remove_at(0).is_some() {}
        self.recent_ids.clear();
        self.recent_items.clear();
        if entries.is_empty() {
            let none = muda::MenuItem::new("No Recent Items", false, None);
            let _ = self.open_recent.append(&none);
            self.recent_items.push(none);
            return;
        }
        let title = |e: &recent::Entry| {
            let name = e.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            if e.project {
                format!("{name} (project)")
            } else {
                let folder = e.path.parent().and_then(|p| p.file_name()).map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                format!("{name} — {folder}")
            }
        };
        let mut previous_project = None;
        for (i, e) in entries.iter().enumerate() {
            if previous_project == Some(true) && !e.project {
                let sep = muda::PredefinedMenuItem::separator();
                let _ = self.open_recent.append(&sep);
            }
            previous_project = Some(e.project);
            let item = muda::MenuItem::new(title(e), true, None);
            let _ = self.open_recent.append(&item);
            self.recent_ids.push((item.id().clone(), i));
            self.recent_items.push(item);
        }
        let sep = muda::PredefinedMenuItem::separator();
        let _ = self.open_recent.append(&sep);
        let clear = muda::MenuItem::new("Clear Menu", true, None);
        let _ = self.open_recent.append(&clear);
        self.recent_clear = clear.id().clone();
        self.recent_items.push(clear);
        self.recent_entries = entries;
    }

    fn poll(&self) -> Vec<MenuAction> {
        self.events
            .try_iter()
            .filter_map(|id| {
                [
                    (&self.new_gui, MenuAction::NewGui),
                    (&self.new_console, MenuAction::NewConsole),
                    (&self.new_library, MenuAction::NewLibrary),
                    (&self.new_package, MenuAction::NewPackage),
                    (&self.new_form, MenuAction::NewForm),
                    (&self.new_data_module, MenuAction::NewDataModule),
                    (&self.import_packages, MenuAction::ImportPackages),
                    (&self.new_file, MenuAction::NewFile),
                    (&self.new_folder, MenuAction::NewFolder),
                    (&self.rename_item, MenuAction::RenameItem),
                    (&self.preferences, MenuAction::Preferences),
                    (&self.toggle_view, MenuAction::ToggleView),
                    (&self.open, MenuAction::Open),
                    (&self.save, MenuAction::Save),
                    (&self.run, MenuAction::Run),
                    (&self.run_tests, MenuAction::RunTests),
                    (&self.debug, MenuAction::Debug),
                    (&self.run_to, MenuAction::RunToCursor),
                    (&self.project_options, MenuAction::ProjectOptions),
                    (&self.rebuild, MenuAction::Rebuild),
                    (&self.clean, MenuAction::Clean),
                    (&self.build_package, MenuAction::BuildPackage),
                    (&self.recent_clear, MenuAction::ClearRecent),
                    (&self.export, MenuAction::Export),
                    (&self.quit, MenuAction::Quit),
                ]
                .into_iter()
                .find(|(i, _)| **i == id)
                .map(|(_, a)| a)
                .or_else(|| (*self.jump_bar.id() == id).then_some(MenuAction::ToggleJumpBar))
                .or_else(|| {
                    self.recent_ids
                        .iter()
                        .find(|(item, _)| *item == id)
                        .map(|(_, i)| MenuAction::OpenRecent(*i))
                })
                .or_else(|| (*self.assistant.id() == id).then_some(MenuAction::ToggleAssistant))
                .or_else(|| {
                    self.edit
                        .iter()
                        .find(|(item, _)| *item.id() == id)
                        .map(|(_, c)| MenuAction::Edit(*c))
                })
                .or_else(|| {
                    self.appearance
                        .iter()
                        .find(|(item, _)| *item.id() == id)
                        .map(|(_, a)| MenuAction::Appearance(*a))
                })
            })
            .collect()
    }
}

fn root_abs(root: &Path) -> Option<PathBuf> {
    root.canonicalize().ok()
}

/// 0-based (line, column) of the char index `idx` in `text`.
fn line_col(text: &str, idx: usize) -> (u32, u32) {
    let before: Vec<char> = text.chars().take(idx).collect();
    let line = before.iter().filter(|c| **c == '\n').count() as u32;
    let col = before.iter().rev().take_while(|c| **c != '\n').count() as u32;
    (line, col)
}

/// Char index where the identifier ending at `idx` begins.
fn word_start(text: &str, idx: usize) -> usize {
    let chars: Vec<char> = text.chars().take(idx).collect();
    let word = chars
        .iter()
        .rev()
        .take_while(|c| c.is_alphanumeric() || **c == '_')
        .count();
    chars.len() - word
}

/// Draws the completion list at `pos`; returns the item the user clicked.
fn completion_popup(ctx: &egui::Context, pos: egui::Pos2, c: &Completion) -> Option<usize> {
    let mut clicked = None;
    egui::Area::new(egui::Id::unique("completion"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_height(220.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, item) in c.items.iter().enumerate() {
                        let text = if item.detail.is_empty() {
                            item.label.clone()
                        } else {
                            format!("{}  {}", item.label, item.detail)
                        };
                        let r = ui.selectable_label(
                            i == c.selected,
                            egui::RichText::new(text).monospace(),
                        );
                        if i == c.selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            });
        });
    clicked
}

fn editor_font() -> egui::FontId {
    let family = theme::editor_family(prefs::font_index());
    egui::FontId::new(prefs::font_size(), family)
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        // Hang up the shell (and its jobs) instead of leaving them behind.
        self.bottom.terminal = None;
        if let Some(file) = &self.session_file {
            self.snapshot().save(file);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.draw(ui, Some(frame));
    }
}

/// The self-test edits the form it opens, so it works on a copy of the project folder under the
/// output folder instead of the user's files.
fn selftest_copy(path: PathBuf, out: &Path) -> PathBuf {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return path;
    };
    let copy = out.join("project");
    let _ = std::fs::remove_dir_all(&copy);
    let copied = std::fs::create_dir_all(&copy).is_ok()
        && std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_file())
                .all(|e| std::fs::copy(e.path(), copy.join(e.file_name())).is_ok())
        });
    if copied { copy.join(name) } else { path }
}

/// A project folder worth indexing: not the file system root and not the home folder.
fn lsp_worthy(root: &Path) -> bool {
    root.parent().is_some() && std::env::var_os("HOME").is_none_or(|h| Path::new(&h) != root)
}

/// Apps started from Finder get a minimal `PATH`; Free Pascal normally lives in Homebrew's.
fn extend_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&current).collect();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        if !dirs.iter().any(|d| d == Path::new(extra)) {
            dirs.push(extra.into());
        }
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        // SAFETY: called first thing in `main`, before any thread is started.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

fn main() -> eframe::Result {
    migrate::run();
    extend_path();
    let mut path = std::env::args().nth(1).map(PathBuf::from);
    if let Some(out) = std::env::var_os("PASCODA_SELFTEST") {
        path = path.map(|p| selftest_copy(p, Path::new(&out)));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            // navigator 250 + inspector 200 + a usable centre; smaller squeezes the panels over each other
            .with_min_inner_size([860.0, 520.0])
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native(
        "Pascoda",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            theme::Appearance::restore(&cc.egui_ctx);
            Ok(Box::new(App::new(path, &cc.egui_ctx)))
        }),
    )
}

/// fpc writes scratch files (`link.res`, `ppas.sh`) into its working directory, so tests that
/// compile in the shared fixtures folder must not overlap.
#[cfg(test)]
static FPC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod render_tests;

/// `<target dir>/tmp`, where tests put scratch projects and renders. Found from the test
/// executable (`<target>/debug/deps/x`) because cargo's target dir is configurable.
#[cfg(test)]
pub(crate) fn target_tmp() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    let dir = target.join("tmp");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    #[test]
    fn edit_commands_become_the_events_egui_expects() {
        use super::EditCmd::*;
        let ev = |c: super::EditCmd| c.events(|| "clip".into());
        assert!(matches!(ev(Copy)[..], [egui::Event::Copy]));
        assert!(matches!(ev(Cut)[..], [egui::Event::Cut]));
        assert!(matches!(&ev(Paste)[..], [egui::Event::Paste(t)] if t == "clip"));
        assert!(matches!(
            &ev(Redo)[..],
            [egui::Event::Key { key: egui::Key::Z, modifiers, .. }] if modifiers.shift && modifiers.command
        ));
        assert!(matches!(
            &ev(SelectAll)[..],
            [egui::Event::Key {
                key: egui::Key::A,
                ..
            }]
        ));
    }

    use super::*;

    /// A copy of the GUI fixture that a test may write to.
    fn scratch_project(name: &str) -> PathBuf {
        let dir = target_tmp().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["hello.lpr", "unit1.pas", "unit1.lfm"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/gui")
                    .join(f),
                dir.join(f),
            )
            .unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn the_lfm_tab_offers_the_form_and_opens_it() {
        let dir = scratch_project("lfm-tab");
        let mut app = App::bare(Some(dir.join("unit1.lfm")));
        assert!(app.has_form(), "the .lfm of a unit is half of a form pair");
        assert!(!app.form_showing());

        app.show_form(); // what clicking "Form" does
        assert!(app.form_showing(), "the designer is showing");
        assert_eq!(
            app.docs[app.active].path,
            dir.join("unit1.lfm"),
            "the form shows on the .lfm tab, which stays active"
        );
        assert_eq!(app.designers.len(), 1);

        // The unit is open for its text, but is not the tab shown.
        let unit = app
            .docs
            .iter()
            .position(|d| d.path == dir.join("unit1.pas"))
            .unwrap();
        assert_ne!(unit, app.active);

        // Going back to the .lfm text shows code, not the designer.
        app.form_view = false;
        assert!(!app.form_showing());
    }

    #[test]
    fn text_edits_to_the_lfm_reach_the_designer_and_designer_edits_reach_the_text_tab() {
        let dir = scratch_project("lfm-sync");
        let mut app = App::bare(Some(dir.join("unit1.lfm")));
        // The user retitles the form in the text, then asks for the form.
        app.docs[app.active].text = app.docs[app.active]
            .text
            .replace("Caption = 'Form1'", "Caption = 'From text'");
        app.show_form();
        assert_eq!(
            app.designers[0]
                .doc
                .root()
                .prop("Caption")
                .and_then(pascoda_forms::PropValue::as_str),
            Some("From text")
        );

        // The designer changes something; the text tab must show it, not a stale copy.
        app.designers[0]
            .add_component("TButton", &[], 8, 8)
            .unwrap();
        let lfm = app
            .docs
            .iter()
            .position(|d| d.path == dir.join("unit1.lfm"))
            .unwrap();
        app.active = lfm;
        app.sync_lfm_tab();
        assert!(
            app.docs[lfm].text.contains("object Button2: TButton")
                || app.docs[lfm].text.contains("object Button")
        );
        assert!(
            !app.designers[0].dirty(),
            "the designer's change was written to disk"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("unit1.lfm")).unwrap(),
            app.docs[lfm].text
        );
    }

    #[test]
    fn tells_gui_programs_from_console_programs() {
        assert!(is_gui_program(Path::new(
            "../../tests/fixtures/gui/hello.lpr"
        )));
        assert!(!is_gui_program(Path::new("../../tests/fixtures/hello.pas")));
    }

    #[test]
    fn maps_char_index_to_line_and_word() {
        let text = "begin\n  total := tot";
        assert_eq!(line_col(text, 0), (0, 0));
        assert_eq!(line_col(text, 6), (1, 0));
        assert_eq!(line_col(text, text.chars().count()), (1, 14));
        assert_eq!(word_start(text, text.chars().count()), 17);
        assert_eq!(word_start("a b", 1), 0);
    }

    fn popup(chosen: bool) -> Completion {
        Completion {
            text: String::new(),
            start: 0,
            cursor: 0,
            items: vec![
                pascoda_lsp::CompletionItem {
                    label: "total".into(),
                    detail: String::new(),
                },
                pascoda_lsp::CompletionItem {
                    label: "tool".into(),
                    detail: String::new(),
                },
            ],
            selected: 0,
            chosen,
        }
    }

    /// Runs `handle_editor_keys` for one frame in which `key` is pressed.
    fn press(app: &mut App, key: egui::Key) -> Option<usize> {
        let ctx = egui::Context::default();
        let mut accept = None;
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        ctx.run_pass(input, |ctx| app.handle_editor_keys(ctx, &mut accept))
            .drop_without_applying_deltas();
        accept
    }

    #[test]
    fn an_auto_popup_leaves_enter_and_tab_alone_until_an_item_is_picked() {
        let mut app = App::bare(None);
        app.completion = Some(popup(false));
        assert_eq!(press(&mut app, egui::Key::Enter), None);
        assert_eq!(press(&mut app, egui::Key::Tab), None);
        assert!(app.completion.is_some());
        // The first Down picks the first item; Enter then takes it.
        assert_eq!(press(&mut app, egui::Key::ArrowDown), None);
        assert_eq!(app.completion.as_ref().unwrap().selected, 0);
        assert_eq!(press(&mut app, egui::Key::Enter), Some(0));
        assert_eq!(press(&mut app, egui::Key::ArrowDown), None);
        assert_eq!(app.completion.as_ref().unwrap().selected, 1);
        assert_eq!(press(&mut app, egui::Key::Tab), Some(1));
        // Esc dismisses.
        assert_eq!(press(&mut app, egui::Key::Escape), None);
        assert!(app.completion.is_none());
    }

    #[test]
    fn a_ctrl_space_popup_accepts_enter_straight_away() {
        let mut app = App::bare(None);
        app.completion = Some(popup(true));
        assert_eq!(press(&mut app, egui::Key::Enter), Some(0));
    }

    #[test]
    fn the_language_mode_is_saved_per_project_and_follows_the_folder() {
        let a = crate::target_tmp().join("mode-a");
        let b = crate::target_tmp().join("mode-b");
        for d in [&a, &b] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).unwrap();
        }
        let mut app = App::bare(None);
        app.root = a.clone();
        app.set_mode(pascoda_fpc::Mode::Delphi);
        assert_eq!(app.mode, pascoda_fpc::Mode::Delphi);
        assert!(
            std::fs::read_to_string(a.join("pascoda.toml"))
                .unwrap()
                .contains("delphi")
        );
        // another project starts in the default mode, and coming back finds the saved one
        app.root = b.clone();
        app.sync_mode();
        assert_eq!(app.mode, pascoda_fpc::Mode::ObjFpc);
        assert!(
            !b.join("pascoda.toml").exists(),
            "nothing is written until a change"
        );
        app.root = a;
        app.sync_mode();
        assert_eq!(app.mode, pascoda_fpc::Mode::Delphi);
    }

    #[test]
    fn exporting_writes_the_executable_and_dmg_and_reports_errors() {
        if pascoda_fpc::locate_fpc().is_none() {
            return;
        }
        let dir = crate::target_tmp().join("export-app");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("tool.pas");
        std::fs::write(&good, "program tool;\nbegin\n  WriteLn('hi');\nend.\n").unwrap();
        let out = dir.join("out");
        let ctx = egui::Context::default();
        let wait = |app: &mut App| {
            let t = std::time::Instant::now();
            while app.exporting.is_some() && t.elapsed() < std::time::Duration::from_secs(60) {
                std::thread::sleep(std::time::Duration::from_millis(20));
                app.poll_export(&ctx);
            }
            assert!(app.exporting.is_none(), "the export finished");
        };
        let mut app = App::bare(Some(good.clone()));
        app.export_to(&ctx, out.clone(), "tool".into(), Default::default());
        assert!(app.exporting.is_some());
        wait(&mut app);
        assert!(out.join("tool").is_file() && out.join("tool.dmg").is_file());
        // the next export of this program knows how many units to expect (a determinate bar)
        assert!(
            app.unit_counts
                .contains_key(&export_key(&program_for(&good))),
            "{:?}",
            app.unit_counts
        );
        assert!(app.console.contains("Exported"), "{}", app.console);
        assert!(
            app.diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error)
        );
        // a program that does not compile
        std::fs::write(&good, "program tool;\nbegin\n  nope;\nend.\n").unwrap();
        let mut app = App::bare(Some(good));
        let out2 = dir.join("out2");
        app.export_to(&ctx, out2.clone(), "tool".into(), Default::default());
        wait(&mut app);
        assert!(app.console.contains("Export failed"), "{}", app.console);
        assert!(!app.diagnostics.is_empty());
        assert!(!out2.join("tool.dmg").exists());
    }

    #[test]
    fn closing_a_changed_tab_asks_and_only_saves_when_told_to() {
        let dir = crate::target_tmp().join("close-tab");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("c.pas");
        std::fs::write(&file, "program c;\nbegin end.\n").unwrap();
        let ctx = egui::Context::default();
        let open = |text: &str| {
            let mut app = App::bare(Some(file.clone()));
            app.docs[0].text = text.to_string();
            app
        };
        // a clean tab closes at once
        let mut app = open("program c;\nbegin end.\n");
        app.request_close(0);
        assert!(app.docs.is_empty() && app.confirm.is_none());
        // a changed tab asks first and stays open
        let mut app = open("program c; // changed\nbegin end.\n");
        app.request_close(0);
        assert_eq!(app.docs.len(), 1);
        assert!(matches!(&app.confirm, Some(Confirm::Tab(p)) if *p == app.docs[0].path));
        // Cancel: nothing happens (drive the dialog's answer through the same code path)
        app.confirm = None;
        assert_eq!(app.docs.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "program c;\nbegin end.\n"
        );
        // Don't Save: the tab closes, the file stays as it was
        app.discard_doc(0);
        assert!(app.docs.is_empty());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "program c;\nbegin end.\n"
        );
        // Save: the file is written, then the tab closes
        let mut app = open("program c; // changed\nbegin end.\n");
        app.save_doc(0);
        app.close_doc(0);
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("// changed")
        );
        let _ = ctx;
    }

    #[test]
    fn quitting_with_unsaved_changes_asks_first() {
        let dir = crate::target_tmp().join("quit-ask");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("q.pas");
        std::fs::write(&file, "program q;\nbegin end.\n").unwrap();
        let mut app = App::bare(Some(file));
        assert!(!(0..app.docs.len()).any(|i| app.is_dirty(i)));
        app.docs[0].text.push_str("// typed\n");
        assert!((0..app.docs.len()).any(|i| app.is_dirty(i)));
    }

    #[test]
    fn the_save_panel_gives_the_folder_and_the_name_of_the_export() {
        let (dir, name) = export_target(Path::new("/Users/me/Out/My App.dmg"), "x").unwrap();
        assert_eq!(
            (dir, name.as_str()),
            (PathBuf::from("/Users/me/Out"), "My App")
        );
        // typing another name changes the name of both files
        let (_, name) = export_target(Path::new("/o/Other.dmg"), "x").unwrap();
        assert_eq!(name, "Other");
        // no extension typed: still the name
        let (_, name) = export_target(Path::new("/o/Plain"), "x").unwrap();
        assert_eq!(name, "Plain");
        assert!(export_target(Path::new("justname.dmg"), "x").is_none());
    }

    #[test]
    fn typing_an_error_gets_a_squiggle_after_a_pause_without_touching_the_file() {
        if pascoda_fpc::locate_fpc().is_none() {
            return;
        }
        let dir = crate::target_tmp().join("live");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("l.pas");
        let on_disk = "program l;\nbegin\nend.\n";
        std::fs::write(&file, on_disk).unwrap();
        let mut app = App::bare(Some(file.clone()));
        app.live.enabled = true;
        let ctx = egui::Context::default();
        app.docs[0].text = "program l;\nbegin\n  nope;\nend.\n".into();
        app.poll_live_check(&ctx); // starts the quiet period
        assert!(!app.live.busy(), "not before the pause");
        std::thread::sleep(livecheck::QUIET + std::time::Duration::from_millis(100));
        app.poll_live_check(&ctx);
        assert!(app.live.busy());
        let t = std::time::Instant::now();
        while app.marks.is_empty() && t.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.poll_live_check(&ctx);
        }
        let m = &app.marks[0].items[0];
        assert_eq!(m.line, 2);
        assert!(m.message.contains("nope"), "{}", m.message);
        // the same finding is in the Problems list, once, and fixing the text clears it
        assert_eq!(app.diagnostics.len(), 1, "{:?}", app.diagnostics);
        assert_eq!(app.diagnostics[0].line, 3);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), on_disk);
        // the same text is not checked again
        std::thread::sleep(livecheck::QUIET + std::time::Duration::from_millis(100));
        app.poll_live_check(&ctx);
        assert!(!app.live.busy());
        // fixing the text clears the squiggle and the Problems entry
        app.docs[0].text = on_disk.into();
        app.poll_live_check(&ctx);
        std::thread::sleep(livecheck::QUIET + std::time::Duration::from_millis(100));
        app.poll_live_check(&ctx);
        let t = std::time::Instant::now();
        while app.live.busy() && t.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.poll_live_check(&ctx);
        }
        assert!(app.diagnostics.is_empty() && app.marks.is_empty());
    }

    #[test]
    fn build_diagnostics_become_marks_in_the_matching_open_file() {
        let dir = crate::target_tmp().join("marks");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.pas");
        std::fs::write(&file, "program m;\nbegin\n  nope;\nend.\n").unwrap();
        let mut app = App::bare(Some(file.clone()));
        app.build_dir = dir.clone();
        app.build_texts = vec![(app.docs[0].path.clone(), app.docs[0].text.clone())];
        let d = |name: &str, severity| Diagnostic {
            file: PathBuf::from(name),
            line: 3,
            col: 3,
            severity,
            message: "Identifier not found".into(),
        };
        let marks = app.place_marks(&[
            d("m.pas", Severity::Error),
            d("other.pas", Severity::Error),
            d("m.pas", Severity::Note),
        ]);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].items.len(), 1);
        let m = &marks[0].items[0];
        assert_eq!((m.line, m.col), (2, 2));
        assert_eq!(m.range(&marks[0].text), 19..23);
    }

    /// Regression: a relative source path used to resolve twice and fail to launch.
    #[test]
    fn runs_program_from_relative_path() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = Path::new("../../tests/fixtures/hello.pas");
        let built =
            pascoda_fpc::build(src, pascoda_fpc::Mode::default()).expect("fpc should start");
        assert!(built.success, "{}", built.raw_output);
        // The app runs the executable the build writes, which is what `executable_of` names.
        let exe = pascoda_project::executable_for(src);
        let folder = src.parent().unwrap();
        let out = run(&exe, &[], &pascoda_project::run_folder(folder, &exe));
        assert!(out.contains("Hello from Pascora"), "{out}");
    }

    /// Run passes each argument as written (spaces kept) and starts in the run folder.
    #[test]
    fn run_passes_the_arguments_and_the_folder() {
        let dir = std::env::temp_dir().join(format!("pascoda-run-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("show");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s|' \"$@\"; pwd\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = run(&script, &["a b".to_string(), "c".to_string()], &dir);
        let here = dir.canonicalize().unwrap();
        assert!(out.contains(&format!("a b|c|{}", here.display())), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod lpk_open_tests {
    use super::*;

    /// A package in the tree opens as a tab, and is listed by the tree.
    #[test]
    fn a_package_opens_from_the_tree() {
        let base = std::env::temp_dir().join(format!("pascoda-lpk-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let lpk = pascoda_project::create_project(&base, "Shapes", pascoda_project::Template::Package).unwrap();
        let mut app = App::bare(Some(lpk.with_extension("pas")));
        let listed: Vec<String> = shell::listing(&app.root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(listed.contains(&"Shapes.lpk".to_string()), "the tree lists it: {listed:?}");
        app.select_in_tree(lpk.clone());
        assert!(app.docs.iter().any(|d| d.path == lpk.canonicalize().unwrap()), "it opens as a tab");
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod tree_select_tests {
    use super::*;

    /// Selecting a form file in the tree shows its form; selecting a unit shows its code, even when
    /// the form was showing before.
    #[test]
    fn selecting_a_form_file_shows_the_form_and_a_unit_shows_its_code() {
        let base = std::env::temp_dir().join(format!("pascoda-tree-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "SelectApp").unwrap();
        let folder = base.join("SelectApp");
        let mut app = App::bare(Some(folder.join("unit1.pas")));
        let root = app.root.clone();

        app.select_in_tree(root.join("unit1.lfm"));
        assert!(app.form_view, "a .lfm shows its form");
        assert!(app.designers.iter().any(|f| f.pas_path == root.join("unit1.pas")), "the designer is open");

        app.select_in_tree(root.join("unit1.pas"));
        assert!(!app.form_view, "a .pas shows its code, not the form");
        assert_eq!(app.docs[app.active].path, root.join("unit1.pas").canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Clicking Form on a `.lfm` tab shows the form on that tab; the unit is not made the active tab,
    /// even when it was not open yet.
    #[test]
    fn form_on_an_lfm_tab_stays_on_the_lfm() {
        let base = std::env::temp_dir().join(format!("pascoda-lfm-form-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "LfmFormApp").unwrap();
        let folder = base.join("LfmFormApp");
        let mut app = App::bare(Some(folder.join("unit1.lfm")));
        let lfm = folder.join("unit1.lfm").canonicalize().unwrap();
        assert_eq!(app.docs[app.active].path, lfm);

        app.show_form();
        assert!(app.form_view && app.form_showing());
        assert_eq!(app.docs[app.active].path, lfm, "the .lfm tab stays the one shown");
        assert!(app.docs.iter().any(|d| d.path.ends_with("unit1.pas")), "the unit is open for its text");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A form opened from its `.lfm` file: Code shows that file's text, not the unit's code.
    #[test]
    fn code_after_a_form_opened_from_its_lfm_shows_the_lfm() {
        let base = std::env::temp_dir().join(format!("pascoda-lfm-code-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        pascoda_project::create_gui_app(&base, "LfmCodeApp").unwrap();
        let folder = base.join("LfmCodeApp");
        let mut app = App::bare(Some(folder.join("unit1.pas")));
        let root = app.root.clone();

        app.select_in_tree(root.join("unit1.lfm"));
        app.show_code();
        assert!(!app.form_view);
        assert_eq!(app.docs[app.active].path, root.join("unit1.lfm").canonicalize().unwrap(),
            "Code shows the .lfm text");

        // A unit selected after that is code of the unit again.
        app.select_in_tree(root.join("unit1.pas"));
        app.show_code();
        assert_eq!(app.docs[app.active].path, root.join("unit1.pas").canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// The session of the open tabs is saved at exit and restored at launch: the project, the tabs in
    /// order, the active tab. Tabs whose files are gone are not reopened.
    #[test]
    fn the_tabs_and_the_project_come_back_after_a_restart() {
        let base = std::env::temp_dir().join(format!("pascoda-session-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.pas"), "unit a;\n").unwrap();
        std::fs::write(base.join("b.pas"), "unit b;\n").unwrap();
        let file = base.join("session");

        let mut before = App::bare(Some(base.join("a.pas")));
        before.open_path(base.join("b.pas"));
        before.open_path(base.join("gone.pas"));
        before.docs.retain(|d| d.path.file_name() != Some(std::ffi::OsStr::new("gone.pas")));
        before.active = 1;
        before.session_file = Some(file.clone());
        eframe::App::on_exit(&mut before);
        let saved = session::Session::load(&file).expect("saved at exit");
        assert_eq!(saved.tabs.len(), 2);
        assert_eq!(saved.active.as_deref(), Some(base.join("b.pas").canonicalize().unwrap().as_path()));

        let mut after = App::bare(None);
        after.restore(saved, false);
        let names: Vec<_> = after.docs.iter().map(|d| d.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["a.pas", "b.pas"], "the tabs in their order");
        assert_eq!(after.docs[after.active].path.file_name().unwrap(), "b.pas", "the active tab");
        assert_eq!(after.root, base.canonicalize().unwrap(), "the project folder");

        // A file given on the command line stays the active tab, and the project is its folder.
        let mut given = App::bare(Some(base.join("a.pas")));
        given.restore(session::Session::load(&file).unwrap(), true);
        assert_eq!(given.docs[given.active].path.file_name().unwrap(), "a.pas");
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod project_switch_tests {
    use super::*;

    /// A project folder with the GUI fixture's files, in the scratch area.
    fn project(name: &str) -> PathBuf {
        let dir = target_tmp().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["hello.lpr", "unit1.pas", "unit1.lfm"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui").join(f),
                dir.join(f),
            )
            .unwrap();
        }
        dir.canonicalize().unwrap()
    }

    /// Opening another project closes the tabs of the old one and opens the file in the new one.
    #[test]
    fn opening_another_project_closes_the_old_tabs() {
        let a = project("switch-clean-a");
        let b = project("switch-clean-b");
        let mut app = App::bare(Some(a.join("unit1.pas")));
        app.open_path(a.join("hello.lpr"));
        assert_eq!(app.docs.len(), 2);

        let file = b.join("unit1.pas");
        app.switch_project(b.clone(), move |app| app.open_path(file));
        assert_eq!(app.root, b);
        assert_eq!(app.docs.len(), 1, "only the new project's tab is open");
        assert!(app.docs[0].path.starts_with(&b));
        assert!(app.confirm.is_none());
    }

    /// A tab in the same folder stays open, so opening a file of the project keeps its tabs.
    #[test]
    fn a_file_of_the_same_project_keeps_the_tabs() {
        let a = project("switch-same-a");
        let mut app = App::bare(Some(a.join("unit1.pas")));
        app.switch_project(a.clone(), move |app| app.open_path(a.join("hello.lpr")));
        assert_eq!(app.docs.len(), 2);
    }

    /// Unsaved changes in the old project's tabs: the switch waits for the answer, and Save writes
    /// them before their tabs close.
    #[test]
    fn unsaved_tabs_of_the_old_project_are_asked_about_then_saved() {
        let a = project("switch-dirty-a");
        let b = project("switch-dirty-b");
        let mut app = App::bare(Some(a.join("unit1.pas")));
        app.docs[0].text.push_str("\n// edited\n");
        let file = b.join("unit1.pas");
        app.switch_project(b.clone(), move |app| app.open_path(file));
        assert!(matches!(&app.confirm, Some(Confirm::Project(d)) if *d == b));
        assert_eq!(app.root, a, "the project does not change until the question is answered");
        assert_eq!(app.docs.len(), 1);

        // The answer "Save": what show_confirm does with it.
        app.confirm = None;
        let (_, then) = app.project_switch.take().unwrap();
        app.close_tabs_outside(&b, true);
        app.enter_project(b.clone());
        then(&mut app);
        assert_eq!(app.root, b);
        assert_eq!(app.docs.len(), 1);
        assert!(app.docs[0].path.starts_with(&b));
        let saved = std::fs::read_to_string(a.join("unit1.pas")).unwrap();
        assert!(saved.ends_with("// edited\n"), "the edit was saved to the old project");
    }
}
