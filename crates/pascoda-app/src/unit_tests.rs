//! Unit tests written for FPCUnit: finds the project's test units, builds and runs them through
//! `pascoda_fpc::unittest`, and keeps the results the Tests pane shows. The work runs on a thread;
//! `poll` takes its messages on the UI thread, as the build does.

use pascoda_fpc::unittest::{self, Event, TestUnit};
use pascoda_fpc::{BuildResult, Progress};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Where one test is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not run since the last run that selected it.
    Pending,
    Running,
    Passed,
    Failed,
    /// Raised an exception rather than failing an assertion.
    Errored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub error: bool,
    pub class: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// `TClass.Method`, as the runner names it.
    pub id: String,
    pub state: State,
    pub failures: Vec<Failure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// Building the runner; the label says what the compiler is doing.
    Building(String),
    Running,
    Finished,
    /// The project has no unit with a test case.
    NoUnits,
    /// The runner did not build, or FPCUnit is not in `vendor/`.
    Failed(String),
}

enum Msg {
    Progress(Progress),
    Built(BuildResult),
    Event(Event),
    Problem(String),
    Finished,
}

#[derive(Default)]
pub struct UnitTests {
    pub phase: Phase,
    pub entries: Vec<Entry>,
    /// The test the user picked in the list (its id).
    pub selected: Option<String>,
    /// Totals of the latest run (several runs add up when failures are rerun).
    pub runs: u32,
    pub failures: u32,
    pub errors: u32,
    /// The test units of the latest run.
    pub units: Vec<TestUnit>,
    /// How far the runner's build is, 0..1, when known.
    pub progress: Option<f32>,
    rx: Option<Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
}

impl UnitTests {
    /// Whether a run is building or running now.
    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// Builds and runs the project's test units. `selection` is `None` for every test, or the ids
    /// to run one by one. Returns `false` (with the phase set) when there is nothing to run.
    pub fn start(
        &mut self,
        ctx: &eframe::egui::Context,
        root: &Path,
        extra: Vec<OsString>,
        selection: Option<Vec<String>>,
    ) -> bool {
        self.launch(ctx, root, extra, selection, false)
    }

    /// Builds the project's test units and lists their tests, without running them, so the pane can
    /// show what there is to select.
    pub fn list(&mut self, ctx: &eframe::egui::Context, root: &Path, extra: Vec<OsString>) -> bool {
        self.launch(ctx, root, extra, None, true)
    }

    fn launch(
        &mut self,
        ctx: &eframe::egui::Context,
        root: &Path,
        extra: Vec<OsString>,
        selection: Option<Vec<String>>,
        list_only: bool,
    ) -> bool {
        if self.busy() {
            return false;
        }
        self.units = find_test_units(root);
        if self.units.is_empty() {
            self.phase = Phase::NoUnits;
            return false;
        }
        let Some(fpcunit) = unittest::fpcunit_src(root) else {
            self.phase = Phase::Failed("FPCUnit was not found: vendor/fpcsrc is missing".into());
            return false;
        };
        // A run replaces the results of the tests it runs; the others keep theirs.
        for entry in &mut self.entries {
            if selection.as_ref().is_none_or(|ids| ids.contains(&entry.id)) {
                entry.state = State::Pending;
                entry.failures.clear();
            }
        }
        self.runs = 0;
        self.failures = 0;
        self.errors = 0;
        self.progress = None;
        self.phase = Phase::Building("Building the tests".into());
        self.cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let units = self.units.clone();
        let root = root.to_path_buf();
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            work(&ctx, &root, &extra, &units, &fpcunit, selection, list_only, &cancel, &tx);
            let _ = tx.send(Msg::Finished);
            ctx.request_repaint();
        });
        true
    }

    /// Stops the run: the running test is killed and the rest is not run.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Takes what the worker sent since the last frame. Returns the lines for the Output pane.
    pub fn poll(&mut self) -> Vec<String> {
        let mut output = Vec::new();
        let Some(rx) = self.rx.take() else { return output };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Progress(p) => {
                    self.progress = p.fraction;
                    self.phase = Phase::Building(p.label);
                }
                Msg::Built(result) => {
                    if !result.success {
                        output.push(result.raw_output.clone());
                        self.phase = Phase::Failed("The tests did not build; see the Output pane".into());
                        finished = true;
                    } else {
                        self.phase = Phase::Running;
                        self.progress = None;
                    }
                }
                Msg::Event(e) => {
                    if let Some(line) = self.apply(e) {
                        output.push(line);
                    }
                }
                Msg::Problem(text) => {
                    self.phase = Phase::Failed(text);
                    finished = true;
                }
                Msg::Finished => finished = true,
            }
        }
        if finished {
            if matches!(self.phase, Phase::Running | Phase::Building(_)) {
                self.phase = Phase::Finished;
            }
        } else {
            self.rx = Some(rx);
        }
        output
    }

    /// Applies one runner event to the results. Returns a line for the Output pane when the event
    /// is not a result (the tests' own output).
    pub fn apply(&mut self, event: Event) -> Option<String> {
        match event {
            Event::Listed(id) => {
                if !self.entries.iter().any(|e| e.id == id) {
                    self.entries.push(Entry { id, state: State::Pending, failures: Vec::new() });
                }
            }
            Event::Started(id) => self.set(&id, |e| e.state = State::Running),
            Event::Ended(id) => self.set(&id, |e| {
                if e.state == State::Running {
                    e.state = State::Passed;
                }
            }),
            Event::Failed { id, error, class, message } => {
                self.set(&id, |e| {
                    e.state = if error || e.state == State::Errored { State::Errored } else { State::Failed };
                    e.failures.push(Failure { error, class, message });
                });
            }
            Event::Done { runs, failures, errors } => {
                self.runs += runs;
                self.failures += failures;
                self.errors += errors;
            }
            Event::Output(line) => return Some(line),
        }
        None
    }

    fn set(&mut self, id: &str, change: impl FnOnce(&mut Entry)) {
        if !self.entries.iter().any(|e| e.id == id) {
            self.entries.push(Entry { id: id.to_string(), state: State::Pending, failures: Vec::new() });
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            change(entry);
        }
    }

    /// The ids of the tests that failed or raised an exception in the latest runs.
    pub fn failed_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| matches!(e.state, State::Failed | State::Errored))
            .map(|e| e.id.clone())
            .collect()
    }
}

/// The worker: build the runner, list the tests, then run them.
#[allow(clippy::too_many_arguments)]
fn work(
    ctx: &eframe::egui::Context,
    root: &Path,
    extra: &[OsString],
    units: &[TestUnit],
    fpcunit: &Path,
    selection: Option<Vec<String>>,
    list_only: bool,
    cancel: &AtomicBool,
    tx: &Sender<Msg>,
) {
    let send = |msg: Msg| {
        let _ = tx.send(msg);
        ctx.request_repaint();
    };
    let built = unittest::build(root, units, extra, fpcunit, None, &mut |p| send(Msg::Progress(p)));
    let (result, binary) = match built {
        Ok(b) => b,
        Err(e) => {
            send(Msg::Problem(format!("The runner could not be built: {e}")));
            return;
        }
    };
    let ok = result.success;
    send(Msg::Built(result));
    if !ok {
        return;
    }
    let mut forward = |e: Event| send(Msg::Event(e));
    if let Err(e) = unittest::run(&binary, &["--list"], cancel, &mut forward) {
        send(Msg::Problem(format!("The tests did not start: {e}")));
        return;
    }
    if list_only {
        return;
    }
    match selection {
        None => {
            if let Err(e) = unittest::run(&binary, &[], cancel, &mut forward) {
                send(Msg::Problem(format!("The tests did not run: {e}")));
            }
        }
        Some(ids) => {
            for id in ids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = unittest::run(&binary, &["--test", &id], cancel, &mut forward) {
                    send(Msg::Problem(format!("The test {id} did not run: {e}")));
                    break;
                }
            }
        }
    }
}

/// The units of the project that have test cases: Pascal units that mention `TTestCase` (the
/// class FPCUnit's tests derive from). Hidden folders, build output, `vendor` and bundles are left
/// out, as the file tree leaves them out.
pub fn find_test_units(root: &Path) -> Vec<TestUnit> {
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<TestUnit>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            let skip = name.starts_with('.')
                || matches!(name.as_str(), "lib" | "target" | "vendor" | "node_modules" | "backup")
                || name.ends_with(".app")
                || name.ends_with(".dSYM");
            if !skip {
                walk(&path, depth + 1, out);
            }
            continue;
        }
        let is_pascal = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pas") || e.eq_ignore_ascii_case("pp"));
        if !is_pascal {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if !text.to_ascii_lowercase().contains("ttestcase") {
            continue;
        }
        if let Some(unit) = unit_name(&text) {
            out.push(TestUnit { name: unit, dir: dir.to_path_buf() });
        }
    }
}

/// The name on the `unit` line of a Pascal source, if it has one.
fn unit_name(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| {
        l.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("unit ")) && !l.starts_with("unit_")
    })?;
    let name = line[5..].trim().trim_end_matches(';').trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Where a test method is implemented: the file and the 1-based line of its `procedure` header.
/// `texts` are the candidate units' paths and sources; `id` is `TClass.Method`.
pub fn method_location(texts: &[(PathBuf, String)], id: &str) -> Option<(PathBuf, u32)> {
    let wanted = id.to_ascii_lowercase();
    for (path, text) in texts {
        for (i, line) in text.lines().enumerate() {
            let l = line.trim_start().to_ascii_lowercase();
            let Some(rest) = l.strip_prefix("procedure ").or_else(|| l.strip_prefix("function ")) else {
                continue;
            };
            let head: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            if head == wanted {
                return Some((path.clone(), i as u32 + 1));
            }
        }
    }
    None
}

impl crate::App {
    /// Each frame: takes the worker's messages. The tests' own output goes to the Output pane.
    pub(crate) fn poll_unit_tests(&mut self) {
        for line in self.unit_tests.poll() {
            self.console.push_str(&line);
            self.console.push('\n');
        }
    }

    /// Builds and runs the project's tests (`selection` `None`), or the given ones. Shows the Tests pane.
    pub(crate) fn start_unit_tests(&mut self, ctx: &eframe::egui::Context, selection: Option<Vec<String>>) {
        let root = self.root.clone();
        let (args, note) = crate::project_args(Some(&root));
        self.console.push_str(&note);
        self.unit_tests.start(ctx, &root, args, selection);
        self.auto_tab(ctx, crate::bottom::BottomTab::Tests);
        // FPCUnit comes with the FPC sources: when they are missing, offer to install them.
        if matches!(&self.unit_tests.phase, Phase::Failed(m) if m.contains("FPCUnit was not found")) {
            self.offer_vendor_install(&[pascoda_fpc::vendor::Part::FpcSources]);
        }
    }

    /// Lists the project's tests in the Tests pane (no run).
    pub(crate) fn list_unit_tests(&mut self, ctx: &eframe::egui::Context) {
        let root = self.root.clone();
        let (args, _) = crate::project_args(Some(&root));
        self.unit_tests.list(ctx, &root, args);
    }

    /// Opens the editor at the method of a test (its `procedure` header in the test's unit).
    fn goto_test(&mut self, ctx: &eframe::egui::Context, id: &str) {
        let mut texts = Vec::new();
        for unit in &self.unit_tests.units {
            for ext in ["pas", "pp"] {
                let path = unit.dir.join(format!("{}.{ext}", unit.name));
                if let Ok(text) = std::fs::read_to_string(&path) {
                    texts.push((path, text));
                }
            }
        }
        match method_location(&texts, id) {
            Some((path, line)) => self.goto(ctx, &path, line, 1),
            None => self.console.push_str(&format!("{id}: the method was not found in the test units\n")),
        }
    }

    /// The Tests pane: the run buttons, the status and progress, and the results of each test.
    pub(crate) fn unit_tests_tab(&mut self, ui: &mut eframe::egui::Ui, pal: &crate::style::Palette) {
        use eframe::egui::{Button, RichText};
        let ctx = ui.ctx().clone();
        // Nothing listed yet: list the tests (build and list, no run) so there is something to select.
        if !self.unit_tests.busy() && self.unit_tests.phase == Phase::Idle && self.unit_tests.entries.is_empty() {
            self.list_unit_tests(&ctx);
        }
        let busy = self.unit_tests.busy();
        let failed = self.unit_tests.failed_ids();
        let selected = self.unit_tests.selected.clone();
        let (mut run_all, mut run_selected, mut run_failed, mut stop) = (false, false, false, false);
        let mut run_one = None;
        let mut go = None;
        ui.horizontal(|ui| {
            run_all = ui.add_enabled(!busy, Button::new("Run All")).on_hover_text("Cmd+U").clicked();
            run_selected = ui
                .add_enabled(!busy && selected.is_some(), Button::new("Run Selected"))
                .clicked();
            run_failed = ui
                .add_enabled(!busy && !failed.is_empty(), Button::new("Rerun Failed"))
                .clicked();
            if busy {
                stop = ui.button("Stop").clicked();
            }
            let (text, color) = self.unit_tests_status(pal);
            ui.label(RichText::new(text).color(color));
        });
        let entries = self.unit_tests.entries.clone();
        eframe::egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            // Two columns: the test (dot and name) and, beside it, what failed.
            eframe::egui::Grid::new("unit-test-rows")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for entry in &entries {
                        // A filled dot for a result, a hollow one for a test not run yet or one that raised an
                        // exception; drawn, as the UI font has no such glyphs.
                        let (color, filled) = match entry.state {
                            State::Passed => (pal.ok, true),
                            State::Failed => (pal.error, true),
                            State::Errored => (pal.error, false),
                            State::Running => (pal.warning, true),
                            State::Pending => (pal.text_tertiary, false),
                        };
                        let is_selected = selected.as_deref() == Some(entry.id.as_str());
                        let row = ui
                            .horizontal(|ui| {
                                ui.add_space(10.0); // the dots keep off the pane's left edge
                                let (rect, _) = ui.allocate_exact_size(eframe::egui::vec2(12.0, 14.0), eframe::egui::Sense::hover());
                                if filled {
                                    ui.painter().circle_filled(rect.center(), 4.0, color);
                                } else {
                                    ui.painter().circle_stroke(rect.center(), 3.5, eframe::egui::Stroke::new(1.5, color));
                                }
                                ui.selectable_label(
                                    is_selected,
                                    RichText::new(&entry.id).color(if is_selected { pal.text } else { pal.text_secondary }),
                                )
                            })
                            .inner;
                        // A click selects the test and shows its line; a double-click runs it.
                        if row.clicked() {
                            self.unit_tests.selected = Some(entry.id.clone());
                            go = Some(entry.id.clone());
                        }
                        if row.double_clicked() {
                            run_one = Some(entry.id.clone());
                        }
                        ui.vertical(|ui| {
                            for failure in &entry.failures {
                                let kind = if failure.error { "Error" } else { "Failure" };
                                ui.label(
                                    RichText::new(format!("{kind} {}: {}", failure.class, failure.message))
                                        .color(pal.error)
                                        .size(12.5),
                                );
                            }
                        });
                        ui.end_row();
                    }
                });
        });
        if run_all {
            self.start_unit_tests(&ctx, None);
        }
        if run_selected && let Some(id) = selected {
            self.start_unit_tests(&ctx, Some(vec![id]));
        }
        if run_failed {
            self.start_unit_tests(&ctx, Some(failed));
        }
        if let Some(id) = run_one {
            self.start_unit_tests(&ctx, Some(vec![id]));
        }
        if stop {
            self.unit_tests.stop();
        }
        if let Some(id) = go {
            self.goto_test(&ctx, &id);
        }
    }

    /// The status line of the Tests pane: the text and its colour.
    fn unit_tests_status(&self, pal: &crate::style::Palette) -> (String, eframe::egui::Color32) {
        let t = &self.unit_tests;
        let bad = t.failures + t.errors;
        match &t.phase {
            Phase::Idle => ("Press Cmd+U to build and run the project's unit tests.".into(), pal.text_tertiary),
            Phase::Building(label) => (format!("Building: {label}"), pal.text_secondary),
            Phase::Running => {
                let done = t.entries.iter().filter(|e| matches!(e.state, State::Passed | State::Failed | State::Errored)).count();
                (format!("Running: {done} of {} done", t.entries.len()), pal.text_secondary)
            }
            Phase::Finished if t.runs == 0 && !t.entries.is_empty() => (
                format!("{} tests. Select one and press Run Selected, or Run All.", t.entries.len()),
                pal.text_secondary,
            ),
            Phase::Finished if bad == 0 => (format!("{} passed", t.runs), pal.ok),
            Phase::Finished => (
                format!("{} run, {} failed, {} raised errors", t.runs, t.failures, t.errors),
                pal.error,
            ),
            Phase::NoUnits => (
                "No test units: the project has no unit with a TTestCase class (uses fpcunit).".into(),
                pal.text_tertiary,
            ),
            Phase::Failed(msg) => (msg.clone(), pal.error),
        }
    }

    /// The toolbar's line while a test run goes on: its status, and the share done when it is known
    /// (of the tests, else of the runner's build).
    pub(crate) fn tests_progress(&self, pal: &crate::style::Palette) -> (String, Option<f32>) {
        let (text, _) = self.unit_tests_status(pal);
        let total = self.unit_tests.entries.len();
        let finished = self
            .unit_tests
            .entries
            .iter()
            .filter(|e| matches!(e.state, State::Passed | State::Failed | State::Errored))
            .count();
        let fraction = if total > 0 {
            Some(finished as f32 / total as f32)
        } else {
            self.unit_tests.progress
        };
        (text, fraction)
    }

    /// The toolbar's line after a test run is over: green when every test passed, red with the
    /// counts or with what stopped the run. None while nothing runs or no test ran.
    pub(crate) fn tests_summary(&self, pal: &crate::style::Palette) -> Option<(String, eframe::egui::Color32)> {
        let t = &self.unit_tests;
        if t.busy() {
            return None;
        }
        match &t.phase {
            Phase::Finished if t.runs > 0 && t.failures + t.errors == 0 => {
                Some((format!("{} tests passed", t.runs), pal.ok))
            }
            Phase::Finished if t.runs > 0 => Some((
                format!("{} tests: {} failed, {} raised errors", t.runs, t.failures, t.errors),
                pal.error,
            )),
            Phase::Failed(msg) => Some((msg.clone(), pal.error)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, state: State) -> Entry {
        Entry { id: id.into(), state, failures: Vec::new() }
    }

    #[test]
    fn finds_units_with_test_cases_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!("pascoda-unit-tests-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("tests_a.pas"), "unit tests_a;\ninterface\nuses fpcunit;\ntype TA = class(TTestCase) end;\nimplementation\nend.\n").unwrap();
        std::fs::write(dir.join("sub/tests_b.pp"), "UNIT tests_b ;\ninterface\ntype TB = class(ttestcase) end;\nend.\n").unwrap();
        std::fs::write(dir.join("plain.pas"), "unit plain;\ninterface\nend.\n").unwrap();
        std::fs::write(dir.join("prog.pas"), "program prog;\nuses fpcunit;\nbegin end.\n").unwrap();
        std::fs::write(dir.join("lib/hidden_tests.pas"), "unit hidden_tests;\ntype X = class(TTestCase) end;\n").unwrap();
        std::fs::write(dir.join(".hidden/h.pas"), "unit h;\ntype X = class(TTestCase) end;\n").unwrap();
        let units = find_test_units(&dir);
        let names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["tests_a", "tests_b"]);
        assert_eq!(units[1].dir, dir.join("sub"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_unit_name_is_read_from_the_unit_line() {
        assert_eq!(unit_name("unit abc;\n"), Some("abc".into()));
        assert_eq!(unit_name("{ comment }\nUnit   spaced.name ;"), Some("spaced.name".into()));
        assert_eq!(unit_name("program p;"), None);
        assert_eq!(unit_name("unit_helper stuff"), None);
    }

    #[test]
    fn a_failed_method_is_found_in_its_unit() {
        let texts = vec![
            (PathBuf::from("a.pas"), "unit a;\nimplementation\nprocedure TOther.TestFails;\nbegin\nend;\n".to_string()),
            (
                PathBuf::from("b.pas"),
                "unit b;\nimplementation\n\nprocedure TDemoTests.TestFails;\nbegin\n  AssertEquals(1, 2);\nend;\n".to_string(),
            ),
        ];
        assert_eq!(method_location(&texts, "TDemoTests.TestFails"), Some((PathBuf::from("b.pas"), 4)));
        assert_eq!(method_location(&texts, "tdemotests.testfails"), Some((PathBuf::from("b.pas"), 4)), "case does not matter");
        assert_eq!(method_location(&texts, "TDemoTests.TestFail"), None, "a prefix is not a match");
    }

    #[test]
    fn events_update_the_results_and_totals() {
        let mut t = UnitTests::default();
        t.apply(Event::Listed("TA.TestOk".into()));
        t.apply(Event::Listed("TA.TestBad".into()));
        t.apply(Event::Listed("TA.TestBoom".into()));
        t.apply(Event::Started("TA.TestOk".into()));
        t.apply(Event::Ended("TA.TestOk".into()));
        t.apply(Event::Started("TA.TestBad".into()));
        t.apply(Event::Failed { id: "TA.TestBad".into(), error: false, class: "E".into(), message: "m".into() });
        t.apply(Event::Ended("TA.TestBad".into()));
        t.apply(Event::Started("TA.TestBoom".into()));
        t.apply(Event::Failed { id: "TA.TestBoom".into(), error: true, class: "Exception".into(), message: "boom".into() });
        t.apply(Event::Ended("TA.TestBoom".into()));
        assert_eq!(t.entries[0].state, State::Passed);
        assert_eq!(t.entries[1].state, State::Failed);
        assert_eq!(t.entries[2].state, State::Errored);
        assert_eq!(t.entries[2].failures[0].message, "boom");
        assert_eq!(t.failed_ids(), ["TA.TestBad", "TA.TestBoom"]);
        t.apply(Event::Done { runs: 3, failures: 1, errors: 1 });
        assert_eq!((t.runs, t.failures, t.errors), (3, 1, 1));
        assert_eq!(t.apply(Event::Output("the test's own line".into())), Some("the test's own line".into()));
    }

    /// The whole worker against the real compiler: the fixture unit is built, listed and run, and
    /// the results arrive through `poll`.
    #[test]
    fn runs_the_fixture_unit_and_reports_each_test() {
        let dir = std::env::temp_dir().join(format!("pascoda-unit-tests-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/unit_tests/tests_demo.pas");
        std::fs::copy(fixture, dir.join("tests_demo.pas")).unwrap();
        let mut t = UnitTests::default();
        let ctx = eframe::egui::Context::default();
        assert!(t.start(&ctx, &dir, Vec::new(), None));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
        while !matches!(t.phase, Phase::Finished | Phase::Failed(_)) {
            assert!(std::time::Instant::now() < deadline, "the run did not finish: {:?}", t.phase);
            t.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(t.phase, Phase::Finished);
        let states: Vec<(&str, State)> = t.entries.iter().map(|e| (e.id.as_str(), e.state)).collect();
        assert_eq!(
            states,
            [
                ("TDemoTests.TestPasses", State::Passed),
                ("TDemoTests.TestFails", State::Failed),
                ("TDemoTests.TestRaises", State::Errored),
            ]
        );
        assert_eq!((t.runs, t.failures, t.errors), (3, 1, 1));
        assert_eq!(t.failed_ids(), ["TDemoTests.TestFails", "TDemoTests.TestRaises"]);
        assert_eq!(method_location(&[(dir.join("tests_demo.pas"), std::fs::read_to_string(dir.join("tests_demo.pas")).unwrap())], "TDemoTests.TestFails").map(|(_, l)| l), Some(25));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_of_some_tests_keeps_the_results_of_the_others() {
        let mut t = UnitTests::default();
        t.entries = vec![entry("TA.One", State::Passed), entry("TA.Two", State::Failed)];
        // Starting a run of TA.Two only resets that one (the build fails here, as there are no units).
        let ctx = eframe::egui::Context::default();
        let empty = std::env::temp_dir().join(format!("pascoda-unit-tests-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        assert!(!t.start(&ctx, &empty, Vec::new(), Some(vec!["TA.Two".into()])));
        assert_eq!(t.phase, Phase::NoUnits);
        assert_eq!(t.entries[0].state, State::Passed, "not selected: kept");
        let _ = std::fs::remove_dir_all(&empty);
    }
}

#[cfg(test)]
mod toolbar_tests {
    use super::*;

    /// After a run the toolbar says how it went: green when all passed, red with the counts.
    #[test]
    fn the_toolbar_says_how_the_test_run_went() {
        let pal = crate::style::Palette::LIGHT;
        let mut app = crate::App::bare(None);
        assert!(app.tests_summary(&pal).is_none(), "nothing ran yet");

        app.unit_tests.phase = Phase::Finished;
        app.unit_tests.runs = 3;
        assert_eq!(app.tests_summary(&pal), Some(("3 tests passed".into(), pal.ok)));

        app.unit_tests.failures = 1;
        app.unit_tests.errors = 2;
        assert_eq!(
            app.tests_summary(&pal),
            Some(("3 tests: 1 failed, 2 raised errors".into(), pal.error))
        );

        app.unit_tests.phase = Phase::Failed("The tests did not build".into());
        assert_eq!(app.tests_summary(&pal), Some(("The tests did not build".into(), pal.error)));
    }
}
