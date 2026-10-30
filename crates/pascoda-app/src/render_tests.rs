//! Draws the whole IDE offscreen and saves PNGs, so the look can be checked without a screen.
//! Output: `<target dir>/tmp/ui-*.png`.

use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn out_dir() -> PathBuf {
    crate::target_tmp()
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/debug.pas")
}

/// Renders `app` at `size` points and saves `<name>.png`; returns the path.
fn render(app: App, dark: bool, size: (f32, f32), name: &str) -> PathBuf {
    // The fonts must be in place before the first frame (menu shortcuts use SF Pro's ⌘).
    let mut harness = Harness::builder()
        .with_size(egui::vec2(size.0, size.1))
        .with_pixels_per_point(1.0)
        .with_theme(if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        })
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    // A fixed number of frames: the build bar repaints forever, so `run()` would never settle.
    harness.run_steps(3);
    let image = harness.render().expect("offscreen render");
    let path = out_dir().join(format!("{name}.png"));
    image.save(&path).unwrap();
    path
}

fn editing_app() -> App {
    let mut app = App::bare(Some(fixture()));
    app.docs[0].breakpoints.insert(8);
    app.console = "Free Pascal Compiler version 3.2.2 for AArch64\nCompiling debug.pas\nLinking debug\n11 lines compiled, 0.3 sec\n".into();
    app.lsp_ready = true;
    app
}

fn debugging_app() -> App {
    let mut app = editing_app();
    let file = fixture().canonicalize().unwrap();
    app.session = Some(debugger::Handle {
        cmds: std::sync::mpsc::channel().0,
        events: std::sync::mpsc::channel().1,
    });
    use debugger::Kind::*;
    let var = |name: &str, ty: &str, value: &str, kind, reference, children| debugger::Var {
        name: name.into(),
        type_name: ty.into(),
        value: value.into(),
        kind,
        reference,
        children,
    };
    let leaf = |name: &str, ty: &str, value: &str, kind| var(name, ty, value, kind, 0, None);
    let tag = leaf("TAG", "ShortString", "'origin'", String);
    let point = var(
        "POINT",
        "TPOINT2",
        "(...)",
        Record,
        3,
        Some(vec![
            leaf("X", "LongInt", "3", Integer),
            leaf("Y", "LongInt", "-4", Integer),
            tag,
        ]),
    );
    let thing = var(
        "THING",
        "TTHING",
        "$100140040",
        Class,
        4,
        Some(vec![
            leaf("ID", "LongInt", "7", Integer),
            leaf("TITLE", "AnsiString", "'Hello'", String),
        ]),
    );
    let numbers = var("NUMBERS", "array of LongInt", "3 elements", Array, 5, None);
    app.var_open = ["Globals/POINT", "Globals/THING"].map(Into::into).into();
    app.paused = Some(debugger::Paused {
        frames: vec![pascoda_dap::Frame {
            id: 1,
            name: "PASCALMAIN".into(),
            file: Some(file),
            line: 8,
        }],
        vars: vec![(
            "Globals".into(),
            vec![
                point,
                leaf("COUNT", "LongInt", "42", Integer),
                leaf("RATIO", "Double", "2.5", Float),
                leaf("FLAG", "Boolean", "True", Boolean),
                leaf("LETTER", "Char", "'a'", Char),
                leaf("NAME", "ShortString", "'Pascoda'", String),
                leaf("COLOR", "TCOLOR", "GREEN", Enum),
                leaf(
                    "COLORS",
                    "TCOLORS",
                    "<error: could not get size of type TCOLORS>",
                    Set,
                ),
                thing,
                numbers,
                leaf("NOTHING", "TTHING", "nil", Class),
                leaf("PTR", "Pointer", "$10003CBE8", Pointer),
            ],
        )],
    });
    app.nav_tab = shell::NavTab::Debug;
    app.debug_height = 360.0;
    app.bottom.tab = bottom::BottomTab::Debug;
    app
}

/// A terminal running `script` under `/bin/sh`, waited for until `marker` is on its screen.
fn terminal_with(script: &str, marker: &str) -> crate::terminal::Terminal {
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(script);
    let t = crate::terminal::Terminal::spawn(cmd, 12, 100, || {}).expect("spawn");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !t.screen_text().contains(marker) && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        t.screen_text().contains(marker),
        "no output: {}",
        t.screen_text()
    );
    t
}

/// The four tabs of the bottom pane, in both themes, with the pane tall enough to read.
#[test]
fn renders_the_references_in_the_search_tab() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.debug_height = 260.0;
    app.nav_tab = shell::NavTab::Search;
    app.show_navigator = true;
    let f = fixture();
    app.project_bar.title = "References to total".into();
    app.show_references(vec![
        pascoda_lsp::Location { path: f.clone(), line: 2, col: 4 },
        pascoda_lsp::Location { path: f.clone(), line: 8, col: 4 },
        pascoda_lsp::Location { path: f.clone(), line: 8, col: 13 },
    ]);
    render(app, false, (1100.0, 640.0), "ui-references");
}

#[test]
fn renders_the_document_outline() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    // A stub server, so the outline is shown as read for the current text (nothing is asked).
    let (requests, _asked) = std::sync::mpsc::channel();
    let (_answers, responses) = std::sync::mpsc::channel();
    app.lsp = Some(lsp::Handle { requests, responses });
    let path = app.docs[0].path.clone();
    let key = shell::text_key(&app.docs[0].text);
    app.outline.shown = Some((path.clone(), key));
    app.outline.symbols = vec![
        pascoda_lsp::Symbol { name: "TForm1".into(), kind: 5, line: 4, col: 2, depth: 0 },
        pascoda_lsp::Symbol { name: "Button1".into(), kind: 8, line: 5, col: 4, depth: 1 },
        pascoda_lsp::Symbol { name: "Button1Click".into(), kind: 6, line: 6, col: 4, depth: 1 },
        pascoda_lsp::Symbol { name: "Form1".into(), kind: 13, line: 10, col: 2, depth: 0 },
    ];
    app.nav_tab = shell::NavTab::Outline;
    render(app, false, (1100.0, 640.0), "ui-outline");
}

#[test]
fn renders_the_editor_with_a_font_and_background_from_the_preferences() {
    let _guard = crate::prefs::tests::APPLY.write().unwrap_or_else(|e| e.into_inner());
    let app = editing_app();
    let mut p = crate::prefs::Prefs { font: 2, ..crate::prefs::Prefs::default() };
    p.set_background(false, Some([0xFB, 0xF6, 0xE9]));
    p.apply();
    let png = render(app, false, (1100.0, 640.0), "ui-editor-courier-background");
    crate::prefs::Prefs::default().apply();
    assert!(png.exists());
}

#[test]
fn renders_the_component_packages_dialog_and_done_closes_it() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.import_packages_open = true;
    let png = render(app, false, (1100.0, 640.0), "ui-import-packages");
    assert!(png.exists());
    let mut app = editing_app();
    app.import_packages_open = true;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(3);
    harness.get_by_label("Done").click();
    harness.run_steps(3);
    assert!(!harness.state().import_packages_open, "Done closes the dialog");
}

#[test]
fn renders_a_data_module_surface_with_the_tray_only() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let dir = crate::target_tmp().join("datamodule-render");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("unit2.pas"),
        "unit Unit2;\n\n{$mode objfpc}{$H+}\n\ninterface\n\nuses\n  Classes, SysUtils;\n\ntype\n  TDataModule2 = class(TDataModule)\n  end;\n\nvar\n  DataModule2: TDataModule2;\n\nimplementation\n\n{$R *.lfm}\n\nend.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("unit2.lfm"),
        "object DataModule2: TDataModule2\n  OldCreateOrder = False\n  Height = 150\n  HorizontalOffset = 300\n  VerticalOffset = 200\n  Width = 200\nend\n",
    )
    .unwrap();
    let mut app = App::bare(Some(dir.join("unit2.pas")));
    app.show_form();
    let png = render(app, false, (1100.0, 640.0), "ui-datamodule");
    assert!(png.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renders_the_tests_pane() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.bottom.choose(crate::bottom::BottomTab::Tests, 0.0);
    app.unit_tests.entries = vec![
        crate::unit_tests::Entry { id: "TDemoTests.TestPasses".into(), state: crate::unit_tests::State::Passed, failures: Vec::new() },
        crate::unit_tests::Entry {
            id: "TDemoTests.TestFails".into(),
            state: crate::unit_tests::State::Failed,
            failures: vec![crate::unit_tests::Failure { error: false, class: "EAssertionFailedError".into(), message: "differs expected: <2> but was: <3>".into() }],
        },
        crate::unit_tests::Entry {
            id: "TDemoTests.TestRaises".into(),
            state: crate::unit_tests::State::Errored,
            failures: vec![crate::unit_tests::Failure { error: true, class: "Exception".into(), message: "boom".into() }],
        },
    ];
    app.unit_tests.runs = 3;
    app.unit_tests.failures = 1;
    app.unit_tests.errors = 1;
    app.unit_tests.phase = crate::unit_tests::Phase::Finished;
    app.unit_tests.selected = Some("TDemoTests.TestFails".into());
    render(app, false, (1100.0, 640.0), "ui-tests-pane");
}

#[test]
fn renders_the_editor_with_word_wrap() {
    let _guard = crate::prefs::tests::APPLY.write().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.docs[0].text.push_str(&format!("\n// {}\n", "wrapped words ".repeat(30)));
    crate::prefs::Prefs { word_wrap: true, ..crate::prefs::Prefs::default() }.apply();
    let png = render(app, false, (1100.0, 640.0), "ui-editor-wrap");
    crate::prefs::Prefs::default().apply();
    assert!(png.exists());
}

#[test]
fn renders_the_preferences_window() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.preferences_open = true;
    render(app, false, (1100.0, 640.0), "ui-preferences");
}

#[test]
fn renders_the_preferences_window_dark() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.preferences_open = true;
    render(app, true, (1100.0, 640.0), "ui-preferences-dark");
}

#[test]
fn renders_the_watches_in_a_stopped_program() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = debugging_app();
    app.watch_list = vec!["count * 2".into(), "missing".into()];
    app.watch_input = "name".into();
    app.watch_eval = Some((
        "COUNT".into(),
        Ok(debugger::Var {
            name: "COUNT".into(),
            type_name: "LongInt".into(),
            value: "42".into(),
            kind: debugger::Kind::Integer,
            reference: 0,
            children: None,
        }),
    ));
    if let Some(p) = app.paused.as_mut() {
        p.set_scope(
            debugger::WATCH_SCOPE,
            vec![
                debugger::Var {
                    name: "count * 2".into(),
                    type_name: "LongInt".into(),
                    value: "84".into(),
                    kind: debugger::Kind::Integer,
                    reference: 0,
                    children: None,
                },
                debugger::Var {
                    name: "missing".into(),
                    type_name: String::new(),
                    value: "use of undeclared identifier 'MISSING'".into(),
                    kind: debugger::Kind::Other,
                    reference: 0,
                    children: None,
                },
            ],
        );
    }
    render(app, false, (1100.0, 640.0), "ui-watches");
}

#[test]
fn renders_the_project_options_window() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.project_options = project_options::Editor {
        open: true,
        mode: pascoda_fpc::Mode::Delphi,
        defines: "DEBUG\nFAST".into(),
        units: "lib\nnope".into(),
        includes: String::new(),
        switches: "-O2\n-o/tmp/x".into(),
        output: "bin/notes".into(),
        default_target: "/Users/me/notes/notes".into(),
        args: "--in\nreport.txt".into(),
        workdir: "data".into(),
        configuration: pascoda_project::Configuration::Release,
        project_file: Some(project_options::FileView {
            file_name: "notes.lpi".into(),
            rows: vec![
                ("Main program".into(), "notes.lpr".into()),
                ("Mode".into(), "ObjFPC".into()),
                ("Unit paths (-Fu)".into(), "frames, ../common".into()),
                ("Defines (-d)".into(), "FOO".into()),
                ("Switches".into(), "-O2, -g".into()),
            ],
            skipped: vec!["$(LazarusDir)/lcl".into()],
        }),
        error: String::new(),
    };
    render(app, false, (1100.0, 640.0), "ui-project-options");
}

#[test]
fn renders_the_project_options_window_dark() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.project_options = project_options::Editor {
        open: true,
        mode: pascoda_fpc::Mode::ObjFpc,
        defines: "DEBUG".into(),
        units: "lib".into(),
        includes: String::new(),
        switches: "-O2".into(),
        output: String::new(),
        default_target: "/Users/me/notes/notes".into(),
        args: String::new(),
        workdir: String::new(),
        configuration: pascoda_project::Configuration::Debug,
        project_file: None,
        error: String::new(),
    };
    render(app, true, (1100.0, 640.0), "ui-project-options-dark");
}

#[test]
fn renders_the_signature_hint_over_a_call() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.docs[0].text = "program p;\nbegin\n  WriteLn(Add(1, 2));\nend.\n".into();
    let at = app.docs[0].text.find("2)").unwrap();
    app.signature = Some(shell::SigHint {
        key: shell::text_key(&app.docs[0].text),
        at,
        sig: pascoda_lsp::Signature {
            label: "Add(a: Integer; b: Integer):Integer".into(),
            parameters: vec!["a: Integer".into(), "b: Integer".into()],
            active: Some(1),
        },
    });
    render(app, false, (1100.0, 640.0), "ui-signature-hint");
}

#[test]
fn renders_a_go_to_scrolled_to_a_far_line() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.docs[0].text = (1..=120).map(|i| format!("  line {i} := {i};\n")).collect();
    let offset = app.docs[0].text.find("line 90 ").unwrap();
    app.reveal = Some(offset..offset + 1);
    render(app, false, (1100.0, 640.0), "ui-reveal-line-90");
}

/// A click on the message of a problem (not on its red marker) goes to its line in the editor.
#[test]
fn clicking_a_problem_message_goes_to_its_line() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.debug_height = 250.0;
    app.bottom.tab = bottom::BottomTab::Problems;
    app.diagnostics = vec![Diagnostic {
        file: app.docs[0].path.clone(),
        line: 8,
        col: 5,
        severity: Severity::Error,
        message: "Identifier not found \"Foo\"".into(),
    }];
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .with_pixels_per_point(1.0)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    theme::apply(&harness.ctx);
    harness.run_steps(2);
    harness.get_by_label("Error: Identifier not found \"Foo\"").click();
    harness.run_steps(6);
    // The caret is on the problem's line and column (the editor scrolls to it the frame after).
    assert_eq!(harness.state().cursor_pos, (8, 5), "line 8, column 5");
}

/// A long search result is cut to one line that fits the row, ending with "...". Short ones are kept.
#[test]
fn a_search_result_is_cut_to_one_line_with_dots() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(300.0, 120.0))
        .with_pixels_per_point(1.0)
        .build_ui_state(
            |ui, out: &mut Vec<String>| {
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                let long = "   10:5    WriteLn('a very long line of text that will not fit in the sidebar at all');";
                out.push(shell::fit_one_line(ui, long, &font, 200.0));
                out.push(shell::fit_one_line(ui, "short", &font, 200.0));
            },
            Vec::new(),
        );
    harness.run_steps(2);
    let out = harness.state().clone();
    assert!(out[0].ends_with("..."), "{:?}", out[0]);
    assert!(out[0].chars().count() < 80, "cut: {:?}", out[0]);
    assert_eq!(out[1], "short", "text that fits is kept");
}

/// The Search tab in the sidebar with a result too long for its row.
#[test]
fn renders_the_search_results_on_one_line_each() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.nav_tab = shell::NavTab::Search;
    app.show_navigator = true;
    app.project_bar.query = "Write".into();
    app.project_bar.results = vec![find::FileHits {
        path: app.docs[0].path.clone(),
        hits: vec![find::Hit {
            line: 10,
            col: 3,
            text: "  WriteLn('a line of text far longer than the sidebar can show, so it must be cut');\n".into(),
            range: 0..0,
        }],
    }];
    app.project_bar.searched = true;
    render(app, false, (1100.0, 640.0), "ui-search-one-line");
}

#[test]
fn renders_the_breakpoint_box() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    let path = app.docs[0].path.clone();
    app.bp_edit = Some(shell::BpEdit {
        path,
        line: 9,
        condition: "i = 2".into(),
        hits: "3".into(),
        error: String::new(),
    });
    render(app, false, (1100.0, 640.0), "ui-breakpoint-box");
}

#[test]
fn renders_the_breakpoints_tab() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.nav_tab = shell::NavTab::Breakpoints;
    app.show_navigator = true;
    let path = app.docs[0].path.clone();
    app.docs[0].breakpoints.extend([8, 9]);
    app.docs[0].disabled.insert(9);
    app.docs[0].stops.insert(9, debugger::Stop { condition: "i = 2".into(), hits: String::new() });
    render(app, false, (1100.0, 640.0), "ui-breakpoints-tab");
    let _ = path;
}

#[test]
fn renders_the_change_value_box() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.value_edit = Some(shell::ValueEdit {
        target: "TOTAL".into(),
        name: "TOTAL".into(),
        kind: debugger::Kind::Integer,
        text: "9".into(),
        error: "A whole number, such as 42".into(),
    });
    render(app, false, (1100.0, 640.0), "ui-change-value");
}

#[test]
fn renders_the_go_to_line_box() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.go_to.open = true;
    app.go_to.text = "99".into();
    app.go_to.error = "Line 99 is not in the file (it has 12 lines)".into();
    render(app, false, (1100.0, 640.0), "ui-go-to-line");
}

#[test]
fn renders_the_find_bar_and_the_project_search() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.find.show(true);
    app.find.query = "end".into();
    app.find.with = "stop".into();
    app.find.current = Some(0);
    render(app, false, (1100.0, 640.0), "ui-find-bar");

    let mut app = editing_app();
    app.debug_height = 260.0;
    app.nav_tab = shell::NavTab::Search;
    app.show_navigator = true;
    app.project_bar.query = "Write".into();
    app.run_project_search();
    app.project_bar.with = "Print".into();
    render(app, false, (1100.0, 640.0), "ui-project-search");
}

#[test]
fn renders_the_bottom_tabs() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use bottom::BottomTab::*;
    let diagnostics = || {
        vec![
            Diagnostic {
                file: fixture(),
                line: 8,
                col: 5,
                severity: Severity::Error,
                message: "Identifier not found \"Foo\"".into(),
            },
            Diagnostic {
                file: fixture(),
                line: 3,
                col: 1,
                severity: Severity::Warning,
                message: "Local variable \"i\" does not seem to be initialized".into(),
            },
        ]
    };
    for dark in [false, true] {
        let suffix = if dark { "-dark" } else { "" };
        let size = (1100.0, 640.0);
        let mut app = editing_app();
        app.debug_height = 250.0;
        app.diagnostics = diagnostics();
        app.bottom.tab = Problems;
        render(app, dark, size, &format!("ui-bottom-problems{suffix}"));

        let mut app = debugging_app();
        app.debug_height = 250.0;
        app.nav_tab = shell::NavTab::Files;
        render(app, dark, size, &format!("ui-bottom-debug{suffix}"));

        let mut app = editing_app();
        app.debug_height = 250.0;
        app.bottom.tab = Output;
        app.console.push_str(
            "Debug build, 1 warning(s)\n\n--- program output ---\nHello from Pascoda\nsum = 42\n",
        );
        render(app, dark, size, &format!("ui-bottom-output{suffix}"));

        let mut app = editing_app();
        app.debug_height = 250.0;
        app.bottom.tab = Terminal;
        app.bottom.view_shown = true;
        app.bottom.terminal = Some(terminal_with(
            "printf '\\033[1;32muser@mac\\033[0m \\033[1;34m~/proj\\033[0m %% ls\\n'; \
             printf '\\033[1;34mexamples\\033[0m  \\033[32mbuild.sh\\033[0m  main.pas  \\033[4munderlined\\033[0m  \\033[7minverse\\033[0m\\n'; \
             printf '\\033[31mred\\033[0m \\033[33myellow\\033[0m \\033[36mcyan\\033[0m \\033[38;5;208morange256\\033[0m \\033[38;2;120;80;255mtruecolor\\033[0m \\033[48;5;236m bg \\033[0m\\n'; \
             printf '\\033[1;32muser@mac\\033[0m \\033[1;34m~/proj\\033[0m %% '; sleep 30",
            "~/proj",
        ));
        render(app, dark, size, &format!("ui-bottom-terminal{suffix}"));
    }
}

#[test]
fn renders_the_ide() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let a = render(editing_app(), false, (1368.0, 800.0), "ui-light");
    let b = render(debugging_app(), false, (1368.0, 800.0), "ui-light-debug");
    let c = render(debugging_app(), true, (1368.0, 800.0), "ui-dark-debug");
    println!("{}\n{}\n{}", a.display(), b.display(), c.display());
}

/// The form designer: the native controls are not part of this render (they are AppKit views),
/// so this shows the window frame, canvas, component tray and inspector. The tray components are
/// added in memory only; nothing is saved to the fixture.
#[test]
fn renders_the_form_designer() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let unit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui/unit1.pas");
    let mut app = App::bare(Some(unit));
    app.lsp_ready = true;
    app.show_form();
    let d = &mut app.designers[0];
    for class in ["TMainMenu", "TOpenDialog", "TActionList", "TImageList"] {
        d.add_component(class, &[], 0, 0).unwrap();
    }
    let timer = d.add_component("TTimer", &[], 0, 0).unwrap();
    d.select(timer);
    let a = render(app, false, (1368.0, 800.0), "ui-form-light");
    println!("{}", a.display());
}

/// A multi-selection: outlines on all, handles on the primary (drawn by egui here; in the app
/// the native overlay draws the same), and the inspector's layout and arrange sections.
#[test]
fn renders_a_multi_selection() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let unit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui/unit1.pas");
    let mut app = App::bare(Some(unit));
    app.lsp_ready = true;
    app.show_form();
    let d = &mut app.designers[0];
    let a = d.add_component("TButton", &[], 16, 72).unwrap();
    let b = d.add_component("TEdit", &[], 120, 96).unwrap();
    let c = d.add_component("TCheckBox", &[], 40, 128).unwrap();
    d.select_paths(vec![b, a, c]);
    let a = render(app, false, (1368.0, 800.0), "ui-form-multi");
    println!("{}", a.display());
}

/// The form's `.lfm` open as text: the toolbar must still offer Code | Form.
#[test]
fn renders_the_lfm_tab() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let lfm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui/unit1.lfm");
    let mut app = App::bare(Some(lfm));
    app.lsp_ready = true;
    assert!(app.has_form());
    let a = render(app, false, (1368.0, 500.0), "ui-lfm-tab");
    println!("{}", a.display());
}

/// Squiggles, gutter dots and the hover popup, with a build error and a warning in the unit.
#[test]
fn renders_diagnostics_and_hover() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui_kittest::kittest::Queryable;
    use hints::{FileMarks, Mark};
    let mut app = editing_app();
    let mark = |line, col, severity, message: &str| Mark {
        line,
        col,
        severity,
        message: message.into(),
    };
    app.marks = vec![FileMarks {
        path: app.docs[0].path.clone(),
        text: app.docs[0].text.clone(),
        items: vec![
            mark(6, 2, Severity::Error, "Identifier not found \"totl\""),
            mark(
                8,
                4,
                Severity::Warning,
                "Variable \"i\" does not seem to be initialized",
            ),
        ],
    }];
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 640.0))
        .with_pixels_per_point(1.0)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    theme::apply(&harness.ctx);
    harness.run();
    // Rest the pointer on the second mark (`total`, line 9), then on `Pascoda`'s identifier.
    let editor = harness.get_by_label("Code editor").rect();
    let at = |line: f32, col: f32| {
        egui::pos2(editor.min.x + 7.9 * col, editor.min.y + 18.0 * (line + 0.5))
    };
    harness.hover_at(at(8.0, 6.0));
    for _ in 0..8 {
        harness.step();
    }
    assert!(
        harness
            .state()
            .hover
            .popup
            .as_ref()
            .is_some_and(|p| p.text.starts_with("Warning")),
        "no popup for the mark"
    );
    let image = harness.render().expect("offscreen render");
    let path = out_dir().join("ui-diagnostics-hover.png");
    image.save(&path).unwrap();
    println!("{}", path.display());
}

/// Clicking a structured row toggles it open (and asks the debugger for its children);
/// rows without children do nothing.
#[test]
fn variable_rows_open_and_close() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui_kittest::kittest::Queryable;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 800.0))
        .with_pixels_per_point(1.0)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), {
            // The pane now also holds the tab strip: make it tall enough for every row.
            let mut app = debugging_app();
            app.debug_height = 440.0;
            app
        });
    theme::apply(&harness.ctx);
    harness.run();
    harness.get_by_label_contains("NUMBERS:").click();
    harness.run();
    assert!(harness.state().var_open.contains("Globals/NUMBERS"));
    harness.get_by_label_contains("POINT:").click();
    harness.run();
    assert!(!harness.state().var_open.contains("Globals/POINT"));
    harness.get_by_label_contains("COUNT:").click();
    harness.run();
    assert!(!harness.state().var_open.contains("Globals/COUNT"));
}

/// A form with a main menu (nested items, a separator, shortcuts, a checked and a disabled
/// item), opened in the item editor with an item in a sub-menu selected. In memory only.
fn menu_app() -> App {
    use designer::menu::NewItem;
    let unit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui/unit1.pas");
    let mut app = App::bare(Some(unit));
    app.lsp_ready = true;
    app.show_form();
    let d = &mut app.designers[0];
    let menu = d.add_component("TMainMenu", &[], 0, 0).unwrap();
    d.add_component("TActionList", &[], 0, 0).unwrap();
    let cap = |d: &mut designer::Designer, p: &[usize], s: &str| {
        d.set_prop(p, "Caption", pascoda_forms::PropValue::Str(s.into()))
            .unwrap();
    };
    let file = d.add_item(&menu, NewItem::Item).unwrap();
    cap(d, &file, "&File");
    let new = d.add_item(&file, NewItem::SubItem).unwrap();
    cap(d, &new, "New");
    d.set_shortcut(&new, "Cmd+N").unwrap();
    let open = d.add_item(&new, NewItem::Item).unwrap();
    cap(d, &open, "Open\u{2026}");
    d.set_shortcut(&open, "Cmd+O").unwrap();
    d.select(open.clone());
    d.ensure_handler("OnClick").unwrap();
    let recent = d.add_item(&open, NewItem::Item).unwrap();
    cap(d, &recent, "Open Recent");
    let a = d.add_item(&recent, NewItem::SubItem).unwrap();
    cap(d, &a, "Project A");
    d.set_flag(&a, "Checked", true, false).unwrap();
    let b = d.add_item(&a, NewItem::Item).unwrap();
    cap(d, &b, "Project B");
    let sep = d.add_item(&recent, NewItem::Separator).unwrap();
    let save = d.add_item(&sep, NewItem::Item).unwrap();
    cap(d, &save, "Save As\u{2026}");
    d.set_shortcut(&save, "Cmd+Shift+S").unwrap();
    d.set_flag(&save, "Enabled", false, true).unwrap();
    let edit = d.add_item(&file, NewItem::Item).unwrap();
    cap(d, &edit, "&Edit");
    let undo = d.add_item(&edit, NewItem::SubItem).unwrap();
    cap(d, &undo, "Undo");
    d.set_shortcut(&undo, "Cmd+Z").unwrap();
    let view = d.add_item(&edit, NewItem::Item).unwrap();
    cap(d, &view, "&View");
    d.select(a);
    app.designer_ui.item_edit = Some("MainMenu1".into());
    app
}

#[test]
fn renders_the_menu_editor() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let a = render(menu_app(), false, (1368.0, 1000.0), "ui-menu-editor");
    let b = render(menu_app(), true, (1368.0, 1000.0), "ui-menu-editor-dark");
    println!("{}\n{}", a.display(), b.display());
    // a narrower window: the editor must stay inside its area, clear of the inspector
    render(menu_app(), false, (1100.0, 800.0), "ui-menu-editor-narrow");
    for (w, assistant) in [(900.0, false), (1368.0, true), (1100.0, true)] {
        let mut app = menu_app();
        app.assistant.open = assistant;
        render(
            app,
            false,
            (w, 800.0),
            &format!("ui-menu-editor-w{w}-{assistant}"),
        );
    }
}

/// The editor's buttons reach the designer: Add Item, Add Sub-item, Indent, Done.
#[test]
fn menu_editor_buttons_edit_the_form() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui_kittest::kittest::Queryable;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), menu_app());
    harness.run();
    let rows = |h: &Harness<App>| {
        let d = &h.state().designers[0];
        d.item_rows(&[2]).len()
    };
    let before = rows(&harness);
    harness.get_by_label("Add Item").click();
    harness.run();
    assert_eq!(rows(&harness), before + 1);
    harness.get_by_label("Add Sub-item").click();
    harness.run();
    assert_eq!(rows(&harness), before + 2);
    let sel = harness.state().designers[0].selected.clone();
    assert_eq!(sel.len(), 5, "a sub-item of the new item: {sel:?}");
    harness.get_by_label("Outdent").click();
    harness.run();
    assert_eq!(harness.state().designers[0].selected.len(), 4);
    harness.get_by_label("Done").click();
    harness.run();
    assert!(harness.state().designer_ui.item_edit.is_none());
}

/// The inspector's anchor-side and border-spacing sections for a label anchored to a button.
#[test]
fn renders_anchor_sides_in_the_inspector() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use designer::geometry::{AnchorSide, Edge, SideRef};
    let unit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui/unit1.pas");
    let mut app = App::bare(Some(unit));
    app.lsp_ready = true;
    app.show_form();
    let d = &mut app.designers[0];
    d.set_anchor_side(
        &[1],
        Edge::Top,
        Some(AnchorSide {
            control: "Button1".into(),
            side: SideRef::End,
        }),
    )
    .unwrap();
    d.set_anchor_side(
        &[1],
        Edge::Left,
        Some(AnchorSide {
            control: "Button1".into(),
            side: SideRef::Start,
        }),
    )
    .unwrap();
    d.set_border_spacing(&[1], "Top", 6).unwrap();
    d.select(vec![1]);
    let a = render(app, false, (1368.0, 800.0), "ui-anchor-sides");
    println!("{}", a.display());
}

/// The action list opens in the same editor (from the inspector's button) and adds actions.
#[test]
fn action_list_editor_adds_actions() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui_kittest::kittest::Queryable;
    let mut app = menu_app();
    app.designer_ui.item_edit = None;
    app.designers[0].select(vec![3]); // ActionList1
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 1000.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run();
    harness.get_by_label("Edit Actions\u{2026}").click();
    harness.run();
    assert_eq!(
        harness.state().designer_ui.item_edit.as_deref(),
        Some("ActionList1")
    );
    harness.get_by_label("Add Action").click();
    harness.run();
    harness.get_by_label("Add Action").click();
    harness.run();
    let d = &harness.state().designers[0];
    let names: Vec<_> = d
        .item_rows(&[3])
        .iter()
        .map(|(p, _)| d.doc.component(p).unwrap().name.clone())
        .collect();
    assert_eq!(names, ["Action1", "Action2"]);
    let image = harness.render().expect("offscreen render");
    let path = out_dir().join("ui-action-editor.png");
    image.save(&path).unwrap();
    println!("{}", path.display());
}

#[test]
fn renders_build_progress() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.building = Some(std::sync::mpsc::channel().1);
    app.build_progress = Some(pascoda_fpc::Progress {
        label: "Compiling unit1.pas".into(),
        fraction: Some(0.4),
        done: 3,
    });
    let path = render(app, false, (1100.0, 300.0), "ui-build-progress");
    assert!(path.exists());
}

/// A form larger than the canvas (the notes example, 640x480, in a short window): scroll bars
/// appear and the form scrolls; rendered at the top and scrolled to the end with the bottom
/// control selected. A copy of the example is used, so nothing is written to it.
#[test]
fn renders_a_large_form_scrolled() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gui-demo");
    let dir = out_dir().join("notes-scroll");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(&src).unwrap().flatten() {
        if e.path().is_file() {
            std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
        }
    }
    let unit = dir.join("mainunit.pas");
    let mut app = App::bare(Some(unit.clone()));
    app.lsp_ready = true;
    app.show_form();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1200.0, 640.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(3);
    let view = harness
        .state()
        .designer_ui
        .view
        .expect("the canvas was laid out");
    assert!(
        view.max_scroll().y > 0.0,
        "the form must be taller than the canvas: {view:?}"
    );
    assert_eq!(view.scroll, egui::Vec2::ZERO);
    let top = out_dir().join("ui-form-scroll-top.png");
    harness
        .render()
        .expect("offscreen render")
        .save(&top)
        .unwrap();

    // Select the status bar (docked at the bottom) and scroll far past the end: clamped.
    let key = harness.state().designers[0].pas_path.clone();
    let status = harness.state().designers[0]
        .doc
        .root()
        .children
        .iter()
        .position(|c| c.name == "StatusBar1")
        .unwrap();
    harness.state_mut().designers[0].select(vec![status]);
    harness
        .state_mut()
        .designer_ui
        .scroll
        .insert(key.clone(), egui::vec2(1e6, 1e6));
    harness.run_steps(3);
    let view = harness.state().designer_ui.view.unwrap();
    assert_eq!(view.scroll, view.max_scroll(), "clamped to the end");
    assert_eq!(harness.state().designer_ui.scroll[&key], view.max_scroll());
    // The form's bottom edge now sits a margin above the canvas's bottom.
    assert_eq!(
        view.client().bottom(),
        view.area.bottom() - designer::viewport::MARGIN
    );
    let end = out_dir().join("ui-form-scroll-end.png");
    harness
        .render()
        .expect("offscreen render")
        .save(&end)
        .unwrap();
    println!("{}\n{}", top.display(), end.display());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the assistant panel ------------------------------------------------------------------

fn assistant_app() -> App {
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.use_key("not-a-real-key");
    app.assistant.usage = pascoda_ai::Usage {
        input_tokens: 5230,
        output_tokens: 412,
        cache_read_input_tokens: 4800,
        ..Default::default()
    };
    app
}

#[test]
fn renders_the_assistant_key_card() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.key = assistant::KeyState::Missing;
    let p = render(app, false, (1368.0, 800.0), "ui-assistant-key");
    println!("{}", p.display());
}

#[test]
fn renders_the_empty_assistant() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let p = render(
        assistant_app(),
        false,
        (1368.0, 800.0),
        "ui-assistant-empty",
    );
    println!("{}", p.display());
}

fn chat_items() -> Vec<assistant::Item> {
    use assistant::*;
    let md = "I looked at the program. The problem is the **missing semicolon** after `total := 0`.\n\n\
## Fix\n\n\
1. Add the semicolon\n\
2. Rebuild with `Cmd+R`\n\n\
- `WriteLn` needs no unit for plain strings\n\
- keep the *mode* line first\n\n\
```pascal\nprogram DebugMe;\nvar\n  i, total: Integer;\nbegin\n  total := 0; // reset\n  for i := 1 to 3 do\n    total := total + i;\n  WriteLn(total);\nend.\n```\n\n\
```pascal\nInc(total);\n```\n\n\
Want me to apply it?";
    vec![
        Item::User {
            text: "Why does it not compile?".into(),
            sent: "[Context from the IDE]\nActive file: debug.pas (caret at line 8)\n[End of context]\n\nWhy does it not compile?".into(),
            chips: vec!["debug.pas".into(), "1 error".into()],
        },
        Item::Tool(ToolRow {
            id: "1".into(),
            label: "Read debug.pas".into(),
            state: ToolState::Done,
            output: "program debug;".into(),
        }),
        Item::Tool(ToolRow {
            id: "2".into(),
            label: "Get build errors".into(),
            state: ToolState::Done,
            output: String::new(),
        }),
        Item::Tool(ToolRow {
            id: "3".into(),
            label: "Build".into(),
            state: ToolState::Failed("The user rejected this change.".into()),
            output: String::new(),
        }),
        Item::Assistant(md.into()),
    ]
}

#[test]
fn renders_the_assistant_chat() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = assistant_app();
    app.assistant.items = chat_items();
    app.assistant
        .items
        .push(assistant::Item::Note("Stopped.".into()));
    let p = render(app, false, (1368.0, 1300.0), "ui-assistant-chat");
    println!("{}", p.display());
    let mut app = assistant_app();
    app.assistant.items = chat_items();
    let p = render(app, true, (1368.0, 1300.0), "ui-assistant-chat-dark");
    println!("{}", p.display());
}

#[test]
fn renders_a_pending_proposal() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use assistant::*;
    let mut app = assistant_app();
    let abs = fixture().canonicalize().unwrap();
    let old = app.docs[0].text.clone();
    let change = pascoda_ai::Change::Edit {
        old_text: "    total := total + i;".into(),
        new_text: "    begin\n      total := total + i;\n      Inc(total);\n    end;".into(),
    };
    let p = pascoda_ai::Proposal {
        id: "e1".into(),
        path: "debug.pas".into(),
        abs_path: abs.clone(),
        change: change.clone(),
    };
    let preview = preview_of(&p, Some(old));
    assert!(preview.is_ok());
    app.assistant.items = chat_items()[..2].to_vec();
    app.assistant.items.push(Item::Assistant(
        "The loop never counts the last step. This edit fixes it:".into(),
    ));
    app.assistant.items.push(Item::Tool(ToolRow {
        id: "e1".into(),
        label: "Edit debug.pas".into(),
        state: ToolState::Running,
        output: String::new(),
    }));
    app.assistant.items.push(Item::Card(Card {
        id: "e1".into(),
        path: "debug.pas".into(),
        abs,
        change,
        state: CardState::Pending,
        auto: false,
        undo: None,
        local: false,
        preview,
    }));
    app.assistant.items.push(Item::Card(Card {
        id: "b1".into(),
        path: String::new(),
        abs: PathBuf::new(),
        change: pascoda_ai::Change::Build,
        state: CardState::Pending,
        auto: false,
        undo: None,
        local: false,
        preview: Ok((vec![], (0, 0))),
    }));
    let path = render(app, false, (1368.0, 1200.0), "ui-assistant-diff");
    println!("{}", path.display());
}

/// The panel next to the form designer (native views are not part of the render).
#[test]
fn renders_the_assistant_beside_the_form() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let unit = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui/unit1.pas");
    let mut app = App::bare(Some(unit));
    app.lsp_ready = true;
    app.show_form();
    app.assistant.open = true;
    app.assistant.use_key("not-a-real-key");
    app.assistant.items = chat_items();
    let p = render(app, false, (1368.0, 800.0), "ui-assistant-form");
    println!("{}", p.display());
}

/// Typing into the real input field: Enter sends, Shift+Enter adds a line.
#[test]
fn enter_sends_and_shift_enter_adds_a_line() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    // The mock answers with one text reply; nothing leaves this machine.
    let (url, _) = assistant::mock_server(vec![assistant::text_reply("Hi there")]);
    let mut app = assistant_app();
    app.assistant.base_url = Some(url);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    harness.get_by_label("Message").focus();
    harness.run_steps(2);
    harness.get_by_label("Message").type_text("first");
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    harness.run_steps(2);
    harness.get_by_label("Message").type_text("second");
    harness.run_steps(2);
    assert_eq!(harness.state().assistant.input, "first\nsecond");
    assert!(harness.state().assistant.items.is_empty());
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    let a = &harness.state().assistant;
    assert!(a.input.is_empty(), "Enter sent the message: {:?}", a.input);
    assert!(matches!(&a.items[0], assistant::Item::User { text, .. } if text == "first\nsecond"));
    // The answer streams in through the app's poll.
    for _ in 0..400 {
        harness.run_steps(1);
        if !harness.state().assistant.streaming() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let a = &harness.state().assistant;
    assert!(!a.streaming(), "{:?}", a.items);
    assert!(matches!(a.items.last(), Some(assistant::Item::Assistant(t)) if t == "Hi there"));
}

/// Clicking Apply / Reject on a card in the real panel.
#[test]
fn card_buttons_apply_and_reject() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use assistant::*;
    let mut app = assistant_app();
    let abs = app.docs[0].path.clone();
    let old = app.docs[0].text.clone();
    for (id, new) in [("1", "'One'"), ("2", "'Two'")] {
        let change = pascoda_ai::Change::Edit {
            old_text: "'Pascora'".into(),
            new_text: new.into(),
        };
        let p = pascoda_ai::Proposal {
            id: id.into(),
            path: "debug.pas".into(),
            abs_path: abs.clone(),
            change: change.clone(),
        };
        app.assistant.items.push(Item::Card(Card {
            id: id.into(),
            path: "debug.pas".into(),
            abs: abs.clone(),
            preview: preview_of(&p, Some(old.clone())),
            change,
            state: CardState::Pending,
            auto: false,
            undo: None,
            local: true,
        }));
    }
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 900.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    // Two cards, two Dismiss buttons: the first one is for card "1"
    harness.get_all_by_label("Dismiss").next().unwrap().click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().docs[0].text,
        old,
        "dismissing changes nothing"
    );
    harness.get_by_label("Apply").click();
    harness.run_steps(3);
    let a = &harness.state();
    assert!(
        a.docs[0].text.contains("name := 'Two';"),
        "{}",
        a.docs[0].text
    );
    assert!(matches!(&a.assistant.items[0], Item::Card(c) if c.state == CardState::Rejected));
    assert!(matches!(&a.assistant.items[1], Item::Card(c) if c.state == CardState::Applied));
}

// ---- the Claude Code (subscription) provider ----------------------------------------------------

fn cli_app(cli: assistant::CliState) -> App {
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.provider = assistant::Provider::ClaudeCode;
    app.assistant.cli = cli;
    app
}

fn cli_found(logged_in: Option<bool>) -> assistant::CliState {
    assistant::CliState::Found(pascoda_ai::claude_code::CliStatus {
        binary: "/Users/me/.local/bin/claude".into(),
        version: Some("2.1.312".into()),
        logged_in,
    })
}

#[test]
fn renders_the_claude_code_status_cards() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    for (cli, name) in [
        (assistant::CliState::Missing, "ui-assistant-cli-missing"),
        (cli_found(Some(false)), "ui-assistant-cli-login"),
        (assistant::CliState::Checking, "ui-assistant-cli-checking"),
    ] {
        let p = render(cli_app(cli), false, (1368.0, 800.0), name);
        println!("{}", p.display());
    }
    let p = render(
        cli_app(assistant::CliState::Missing),
        true,
        (1368.0, 800.0),
        "ui-assistant-cli-missing-dark",
    );
    println!("{}", p.display());
}

#[test]
fn renders_claude_code_settings_and_chat() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = cli_app(cli_found(Some(true)));
    app.assistant.show_settings = true;
    app.assistant.items = chat_items();
    let p = render(app, false, (1368.0, 1300.0), "ui-assistant-cli-settings");
    println!("{}", p.display());
    // the API provider's settings with the switch
    let mut app = assistant_app();
    app.assistant.show_settings = true;
    let p = render(app, false, (1368.0, 800.0), "ui-assistant-api-settings");
    println!("{}", p.display());
    let mut app = cli_app(cli_found(Some(true)));
    app.assistant.items = chat_items();
    let p = render(app, true, (1368.0, 1300.0), "ui-assistant-cli-chat-dark");
    println!("{}", p.display());
}

/// Clicking the provider switch in the real panel.
#[test]
fn the_provider_switch_changes_the_card() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let _keychain = assistant::KEYCHAIN
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.key = assistant::KeyState::Missing;
    // The CLI check is not started here: a state with a result already.
    app.assistant.cli = assistant::CliState::Missing;
    app.assistant.show_settings = true;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    assert_eq!(harness.state().assistant.provider, assistant::Provider::Api);
    harness.get_by_label("Claude Code CLI").click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().assistant.provider,
        assistant::Provider::ClaudeCode
    );
    // the CLI card is up: it says so and the key field is gone
    assert!(harness.query_by_label("Check again").is_some());
    assert!(harness.query_by_label("API key").is_none());
    harness.get_by_label("Claude Console API").click();
    harness.run_steps(3);
    assert_eq!(harness.state().assistant.provider, assistant::Provider::Api);
    assert!(harness.query_by_label("API key").is_some());
}

// ---- the OpenAI provider ------------------------------------------------------------------

fn openai_app() -> App {
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.provider = assistant::Provider::OpenAi;
    app.assistant.openai_key_state = assistant::KeyState::Missing; // no Keychain lookup
    app
}

fn openai_chat_app() -> App {
    let mut app = openai_app();
    app.assistant.use_openai_key("not-a-real-key");
    app.assistant.openai.model = "my-model".into();
    app.assistant.usage = pascoda_ai::Usage {
        input_tokens: 2100,
        output_tokens: 380,
        cache_read_input_tokens: 1800,
        ..Default::default()
    };
    app
}

#[test]
fn renders_the_openai_key_card() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let p = render(
        openai_app(),
        false,
        (1368.0, 900.0),
        "ui-assistant-openai-key",
    );
    println!("{}", p.display());
    // A local server typed in, the message after a rejected key, dark mode
    let mut app = openai_app();
    app.assistant.openai_edit.base_url = "http://localhost:11434/v1".into();
    app.assistant.openai_edit.model = "llama3.1".into();
    app.assistant.key_message = Some(
        "The API key was rejected (Incorrect API key provided). Check the key in Settings.".into(),
    );
    let p = render(
        app,
        true,
        (1368.0, 900.0),
        "ui-assistant-openai-key-local-dark",
    );
    println!("{}", p.display());
}

#[test]
fn renders_the_openai_chat_and_settings() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = openai_chat_app();
    app.assistant.items = chat_items();
    let p = render(app, false, (1368.0, 1300.0), "ui-assistant-openai-chat");
    println!("{}", p.display());
    let mut app = openai_chat_app();
    app.assistant.items = chat_items();
    let p = render(app, true, (1368.0, 1300.0), "ui-assistant-openai-chat-dark");
    println!("{}", p.display());
    let mut app = openai_chat_app();
    app.assistant.show_settings = true;
    app.assistant.openai_edit = app.assistant.openai.clone();
    let p = render(app, false, (1368.0, 1000.0), "ui-assistant-openai-settings");
    println!("{}", p.display());
    // no model yet: the field shows its hint
    let mut app = openai_chat_app();
    app.assistant.openai.model.clear();
    let p = render(app, false, (1368.0, 800.0), "ui-assistant-openai-nomodel");
    println!("{}", p.display());
}

/// Clicking through the OpenAI card: choose the provider, point at a local server, save.
#[test]
fn the_openai_card_has_only_the_key() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.key = assistant::KeyState::Missing;
    app.assistant.openai_key_state = assistant::KeyState::Missing; // never read the real Keychain
    app.assistant.show_settings = true;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 900.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    harness.get_by_label("OpenAI API").click();
    harness.run_steps(3);
    assert_eq!(harness.state().assistant.provider, assistant::Provider::OpenAi);
    assert!(!harness.state().assistant.ready(), "the key is needed");
    // The endpoint is fixed and the model is picked under the message box: no other fields here.
    assert!(harness.query_by_label("OpenAI API key").is_some());
    assert!(harness.query_by_label("OpenAI base URL").is_none());
    assert!(harness.query_by_label("OpenAI model name").is_none());
    assert!(harness.query_by_label("Server address").is_none());
}

// ---- "Allow edits this session" and "save first" ----------------------------------------------

/// A card for an edit of the first open tab.
fn edit_card(
    app: &App,
    id: &str,
    old: &str,
    new: &str,
    state: assistant::CardState,
) -> assistant::Card {
    let abs = app.docs[0].path.clone();
    let change = pascoda_ai::Change::Edit {
        old_text: old.into(),
        new_text: new.into(),
    };
    let p = pascoda_ai::Proposal {
        id: id.into(),
        path: "debug.pas".into(),
        abs_path: abs.clone(),
        change: change.clone(),
    };
    let preview = assistant::preview_of(&p, Some(app.docs[0].text.clone()));
    assert!(preview.is_ok(), "{preview:?}");
    assistant::Card {
        id: id.into(),
        path: "debug.pas".into(),
        abs,
        change,
        state,
        local: false,
        preview,
        auto: false,
        undo: None,
    }
}

#[test]
fn renders_auto_mode_with_applied_cards_and_revert() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use assistant::*;
    let mut app = assistant_app();
    app.assistant.set_auto_edits(true);
    app.assistant.items = chat_items()[..2].to_vec();
    app.assistant.items.push(Item::Assistant(
        "I fixed the loop and added a counter.".into(),
    ));
    // 1: applied, the file still has the text the edit left: Revert is on
    let mut c1 = edit_card(
        &app,
        "e1",
        "    total := total + i;",
        "    begin\n      total := total + i;\n      Inc(total);\n    end;",
        CardState::Applied,
    );
    c1.auto = true;
    c1.undo = Some(Box::new(Undo {
        before: Some(app.docs[0].text.clone()),
        after: app.docs[0].text.clone(),
        unit: None,
        on_disk: false,
    }));
    // 2: applied, but the file changed since: Revert is off
    let mut c2 = edit_card(&app, "e2", "total := 0;", "total := 1;", CardState::Applied);
    c2.auto = true;
    c2.undo = Some(Box::new(Undo {
        before: Some(app.docs[0].text.clone()),
        after: "something else".into(),
        unit: None,
        on_disk: false,
    }));
    // 3: taken back
    let mut c3 = edit_card(
        &app,
        "e3",
        "total := 0;",
        "total := 2;",
        CardState::Reverted,
    );
    c3.auto = true;
    app.assistant.items.push(Item::Card(c1));
    app.assistant.items.push(Item::Card(c2));
    app.assistant.items.push(Item::Card(c3));
    let p = render(app, false, (1368.0, 1300.0), "ui-assistant-auto-on");
    println!("{}", p.display());
    // dark mode, second time on (no warning line any more)
    let mut app = assistant_app();
    app.assistant.set_auto_edits(true);
    app.assistant.set_auto_edits(false);
    app.assistant.set_auto_edits(true);
    app.assistant.items = chat_items()[..2].to_vec();
    let p = render(app, true, (1368.0, 800.0), "ui-assistant-auto-on-dark");
    println!("{}", p.display());
}

#[test]
fn renders_the_save_first_card_for_claude_code() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use assistant::*;
    let mut app = cli_app(cli_found(Some(true)));
    app.assistant.backend_writes = true;
    app.assistant.items = chat_items()[..2].to_vec();
    app.docs[0].text.push_str("// typed, not saved\n");
    let c = edit_card(
        &app,
        "e1",
        "    total := total + i;",
        "    begin\n      total := total + i;\n      Inc(total);\n    end;",
        CardState::Pending,
    );
    app.assistant.items.push(Item::Card(c));
    let p = render(app, false, (1368.0, 1000.0), "ui-assistant-save-first");
    println!("{}", p.display());
}

/// The switch in the real panel: a click turns it on (badge, warning), a click on the badge
/// turns it off. A card for a dirty file offers "Save and apply". Works on a copy: the
/// fixture itself is never written.
#[test]
fn the_edit_switch_and_the_save_first_button_work_with_clicks() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let dir = crate::target_tmp().join("render-save-first");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture(), dir.join("debug.pas")).unwrap();
    let mut app = App::bare(Some(dir.join("debug.pas")));
    app.assistant.open = true;
    app.assistant.provider = assistant::Provider::ClaudeCode;
    app.assistant.cli = cli_found(Some(true));
    app.assistant.backend_writes = true;
    app.docs[0].text.push_str("// typed, not saved\n");
    let c = edit_card(
        &app,
        "e1",
        "    total := total + i;",
        "    Inc(total);",
        assistant::CardState::Pending,
    );
    app.assistant.items.push(assistant::Item::Card(c));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 900.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    assert!(!harness.state().assistant.auto_edits);
    harness.get_by_label("Allow edits this session").click();
    harness.run_steps(3);
    assert!(harness.state().assistant.auto_edits);
    harness.get_by_label("Allow edits this session").click();
    harness.run_steps(3);
    assert!(!harness.state().assistant.auto_edits);
    // the dirty file: "Save and apply" saves the tab, then answers
    assert!(harness.query_by_label("Apply").is_none());
    harness.get_by_label("Save and apply").click();
    harness.run_steps(3);
    let st = harness.state();
    assert!(!st.docs[0].dirty(), "the tab was saved");
    assert!(
        std::fs::read_to_string(dir.join("debug.pas"))
            .unwrap()
            .contains("typed, not saved")
    );
    assert!(matches!(st.assistant.items.last(),
        Some(assistant::Item::Card(c)) if c.state == assistant::CardState::Writing));
}

/// Typing into the terminal tab reaches a real shell and nothing else: Tab stays in the
/// terminal, Ctrl+C interrupts, Cmd+K clears, and the editor's text is untouched.
#[test]
fn the_terminal_tab_takes_the_keyboard() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui::{Event, Key, Modifiers};
    let mut app = editing_app();
    app.bottom.tab = bottom::BottomTab::Terminal;
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.env("PS1", "$ ");
    app.bottom.terminal =
        Some(crate::terminal::Terminal::spawn(cmd, 10, 80, || {}).expect("spawn"));
    let before = app.docs[0].text.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    h.run_steps(3);
    assert!(
        h.state().bottom.terminal_focused,
        "the terminal asks for focus when shown"
    );

    let key = |h: &Harness<'_, App>, k: Key, m: Modifiers| {
        h.event_modifiers(
            Event::Key {
                key: k,
                physical_key: Some(k),
                pressed: true,
                repeat: false,
                modifiers: m,
            },
            m,
        );
    };
    let screen = |h: &Harness<'_, App>| h.state().bottom.terminal.as_ref().unwrap().screen_text();
    let wait = |h: &mut Harness<'_, App>, what: &str| {
        for _ in 0..200 {
            h.run_steps(1);
            if screen(h).contains(what) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("`{what}` never appeared:\n{}", screen(h));
    };

    h.event(Event::Text("echo hello-$((6*7))".into()));
    key(&h, Key::Enter, Modifiers::NONE);
    wait(&mut h, "hello-42");

    // Tab is sent to the shell (it completes a name) and does not move the focus away.
    h.event(Event::Text("ech".into()));
    key(&h, Key::Tab, Modifiers::NONE);
    h.run_steps(3);
    assert!(h.state().bottom.terminal_focused);

    // Ctrl+C interrupts a running command.
    key(&h, Key::C, Modifiers::CTRL);
    h.event(Event::Text("sleep 30".into()));
    key(&h, Key::Enter, Modifiers::NONE);
    h.run_steps(3);
    std::thread::sleep(std::time::Duration::from_millis(300));
    key(&h, Key::C, Modifiers::CTRL);
    h.event(Event::Text("echo after-$((1+1))".into()));
    key(&h, Key::Enter, Modifiers::NONE);
    wait(&mut h, "after-2");

    // Cmd+K clears the screen.
    key(&h, Key::K, Modifiers::COMMAND);
    h.run_steps(3);
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.run_steps(2);
    assert!(!screen(&h).contains("hello-42"), "{}", screen(&h));

    assert!(h.state().bottom.terminal_focused);
    assert_eq!(h.state().docs[0].text, before, "the editor saw keystrokes");
}

#[test]
fn the_bottom_pane_switches_by_itself_unless_the_user_just_chose() {
    use bottom::BottomTab::*;
    let ctx = egui::Context::default();
    let mut app = editing_app();
    app.auto_tab(&ctx, Output);
    assert_eq!(app.bottom.tab, Output);
    app.auto_tab(&ctx, Debug);
    assert_eq!(app.bottom.tab, Debug);
    app.bottom.choose(Problems, ctx.input(|i| i.time));
    app.auto_tab(&ctx, Output);
    assert_eq!(
        app.bottom.tab, Problems,
        "the user's click wins for a moment"
    );
}

#[test]
fn renders_the_context_chips_and_what_was_sent() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use assistant::*;
    // A scratch copy of the notes example; the opened unit has an unsaved edit and a second
    // tab is open, so every chip shows.
    let dir = assistant::overview::tests::notes_copy("render-context");
    let mut app = App::bare(Some(dir.join("mainunit.pas")));
    app.open_path(dir.join("aboutunit.pas"));
    app.active = 0;
    app.docs[1].text.push_str("// unsaved edit\n");
    app.assistant.open = true;
    app.assistant.use_key("not-a-real-key");
    app.assistant
        .project
        .set_index(assistant::overview::Index::scan(&app.root));
    app.assistant.chips.tabs = true;
    let ctx = egui::Context::default();
    let snap = app.assistant_snapshot(&ctx);
    let (sent, chips) = compose(
        app.assistant.chips,
        &snap,
        "Which units use the settings form?",
    );
    app.assistant.items = vec![
        Item::User {
            text: "Which units use the settings form?".into(),
            sent,
            chips,
        },
        Item::Assistant("Let me read `settingsunit.pas` and search for its uses.".into()),
    ];
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 1000.0))
        .with_pixels_per_point(1.0)
        .with_theme(egui::Theme::Light)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(3);
    harness.get_by_label("What was sent").click();
    harness.run_steps(3);
    let path = out_dir().join("ui-assistant-context.png");
    harness
        .render()
        .expect("offscreen render")
        .save(&path)
        .unwrap();
    println!("{}", path.display());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Opening a file from another folder moves the terminal there: a shell nobody has typed in is
/// started again in the new folder; one in use stays and offers a `cd` button.
#[test]
fn the_terminal_follows_the_project_folder() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let a = crate::target_tmp().join("term-follow-a");
    let b = crate::target_tmp().join("term-follow-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (a, b) = (a.canonicalize().unwrap(), b.canonicalize().unwrap());
    let make = |app: App| {
        Harness::builder()
            .with_size(egui::vec2(1000.0, 600.0))
            .with_pixels_per_point(1.0)
            .with_context_setup(theme::apply)
            .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app)
    };

    // Untouched shell: restarted in the new folder.
    let mut app = editing_app();
    app.bottom.tab = bottom::BottomTab::Terminal;
    app.root = a.clone();
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.cwd(&a);
    app.bottom.terminal =
        Some(crate::terminal::Terminal::spawn(cmd, 10, 80, || {}).expect("spawn"));
    app.bottom.terminal_root = Some(a.clone());
    app.root = b.clone();
    let mut h = make(app);
    h.run_steps(3);
    assert_eq!(h.state().bottom.terminal_root.as_deref(), Some(b.as_path()));
    assert!(h.state().bottom.terminal.is_some());
    drop(h);

    // A shell in use: left alone, with a button that sends `cd`.
    let mut app = editing_app();
    app.bottom.tab = bottom::BottomTab::Terminal;
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.env("PS1", "$ ");
    cmd.cwd(&a);
    let term = crate::terminal::Terminal::spawn(cmd, 10, 80, || {}).expect("spawn");
    term.write(b"echo busy\n");
    assert!(term.typed());
    app.bottom.terminal = Some(term);
    app.bottom.terminal_root = Some(a.clone());
    app.root = b.clone();
    let mut h = make(app);
    h.run_steps(3);
    assert_eq!(h.state().bottom.terminal_root.as_deref(), Some(a.as_path()));
    h.get_by_label("cd to project folder").click();
    h.run_steps(3);
    assert_eq!(h.state().bottom.terminal_root.as_deref(), Some(b.as_path()));
    let want = b.display().to_string();
    for _ in 0..200 {
        h.run_steps(1);
        if h.state()
            .bottom
            .terminal
            .as_ref()
            .unwrap()
            .screen_text()
            .contains(&want)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "the cd never showed:\n{}",
        h.state().bottom.terminal.as_ref().unwrap().screen_text()
    );
}

#[test]
fn the_terminal_wheel_scrolls_the_history() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui::{Event, MouseWheelUnit};
    let mut app = editing_app();
    app.bottom.tab = bottom::BottomTab::Terminal;
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.env("PS1", "$ ");
    let term = crate::terminal::Terminal::spawn(cmd, 10, 80, || {}).expect("spawn");
    term.write(b"seq 1 300\n");
    app.bottom.terminal = Some(term);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    for _ in 0..200 {
        h.run_steps(1);
        let t = h.state().bottom.terminal.as_ref().unwrap();
        if t.screen_text().contains("300") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let scrollback = |h: &Harness<'_, App>| {
        h.state()
            .bottom
            .terminal
            .as_ref()
            .unwrap()
            .parser()
            .screen()
            .scrollback()
    };
    assert_eq!(scrollback(&h), 0);
    // The pointer over the terminal pane, then the wheel (positive y = towards older output).
    h.hover_at(egui::pos2(400.0, 480.0));
    h.run_steps(1);
    h.event(Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 120.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(3);
    assert!(scrollback(&h) > 0, "the wheel should scroll the history");
    h.run_steps(2);
    let image = h.render().expect("offscreen render");
    image
        .save(out_dir().join("ui-terminal-scrollbar.png"))
        .unwrap();
}

// ---- the export dialog ---------------------------------------------------------------------

#[test]
fn renders_and_drives_the_export_dialog() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use pascoda_project::ExportSettings;
    for dark in [false, true] {
        let mut app = editing_app();
        app.export_dialog = Some(export_dialog::ExportDialog::new(
            ExportSettings::default(),
            "debug",
            true,
        ));
        render(
            app,
            dark,
            (900.0, 560.0),
            &format!("ui-export{}", if dark { "-dark" } else { "" }),
        );
    }
    // console programs only get the name
    let mut app = editing_app();
    app.export_dialog = Some(export_dialog::ExportDialog::new(
        ExportSettings::default(),
        "debug",
        false,
    ));
    render(app, false, (900.0, 560.0), "ui-export-console");

    // typing a bad identifier shows the sentence and keeps the dialog open
    let mut app = editing_app();
    app.export_dialog = Some(export_dialog::ExportDialog::new(
        ExportSettings::default(),
        "debug",
        true,
    ));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 560.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    harness.get_by_label("Bundle identifier").click();
    harness.run_steps(2);
    harness
        .get_by_label("Bundle identifier")
        .type_text("nodots");
    harness.run_steps(2);
    assert_eq!(
        harness
            .state()
            .export_dialog
            .as_ref()
            .unwrap()
            .settings
            .bundle_id,
        "nodots"
    );
    harness.get_by_label("Export...").click();
    harness.run_steps(3);
    let d = harness.state().export_dialog.as_ref().expect("still open");
    assert!(
        d.error
            .as_deref()
            .unwrap_or("")
            .contains("bundle identifier")
    );
    harness.get_by_label("Cancel").click();
    harness.run_steps(2);
    assert!(harness.state().export_dialog.is_none());
}

// ---- unsaved changes ---------------------------------------------------------------------------

#[test]
fn a_changed_tab_is_red_and_closing_it_asks() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    for dark in [false, true] {
        let mut app = editing_app();
        app.docs[0].text.push_str("// typed, not saved\n");
        render(
            app,
            dark,
            (900.0, 560.0),
            &format!("ui-tab-changed{}", if dark { "-dark" } else { "" }),
        );
    }
    let mut app = editing_app();
    app.docs[0].text.push_str("// typed, not saved\n");
    app.request_close(0);
    render(app, false, (900.0, 560.0), "ui-close-asks");

    // Don't Save closes the tab and leaves the file alone; Cancel keeps the tab
    let on_disk = std::fs::read_to_string(fixture()).unwrap();
    let mut app = editing_app();
    app.docs[0].text.push_str("// typed, not saved\n");
    app.request_close(0);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 560.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    harness.get_by_label("Cancel").click();
    harness.run_steps(2);
    assert_eq!(harness.state().docs.len(), 1);
    assert!(harness.state().confirm.is_none());
    harness.state_mut().request_close(0);
    harness.run_steps(2);
    harness.get_by_label("Don't Save").click();
    harness.run_steps(2);
    assert!(harness.state().docs.is_empty());
    assert_eq!(std::fs::read_to_string(fixture()).unwrap(), on_disk);
}

/// A full-screen program that asked for mouse reports (Codex, vim, htop) gets the wheel as a
/// mouse report, not as arrow keys; a plain shell keeps the IDE's own scrolling.
#[test]
fn the_wheel_reaches_a_program_that_asked_for_the_mouse() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui::Event;
    let mut app = editing_app();
    app.bottom.tab = bottom::BottomTab::Terminal;
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.args([
        "-c",
        "printf '\\033[?1049h\\033[?1000h\\033[?1006h'; stty -icanon -echo; cat -v",
    ]);
    app.bottom.terminal =
        Some(crate::terminal::Terminal::spawn(cmd, 10, 80, || {}).expect("spawn"));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    let screen = |h: &Harness<'_, App>| h.state().bottom.terminal.as_ref().unwrap().screen_text();
    for _ in 0..50 {
        h.run_steps(1);
        if h.state().bottom.terminal.as_ref().unwrap().wants_mouse() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(h.state().bottom.terminal.as_ref().unwrap().wants_mouse());
    // over the terminal, which fills the bottom pane
    let pos = egui::pos2(300.0, 520.0);
    h.event(Event::PointerMoved(pos));
    h.run_steps(2);
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 3.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..100 {
        h.run_steps(1);
        if screen(&h).contains("^[[<64;") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("no wheel report arrived:\n{}", screen(&h));
}

/// The toolbar's activity bar during an export: the same bar as a build, labelled with the step.
#[test]
fn the_toolbar_shows_export_progress() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    for dark in [false, true] {
        let mut app = editing_app();
        let (_tx, rx) = std::sync::mpsc::channel();
        app.exporting = Some(Exporting {
            rx,
            out: std::path::PathBuf::from("/tmp"),
        });
        app.build_progress = Some(pascoda_fpc::Progress {
            label: "Creating the disk image".into(),
            fraction: Some(0.6),
            done: 3,
        });
        assert!(app.working());
        render(
            app,
            dark,
            (900.0, 300.0),
            &format!("ui-export-progress{}", if dark { "-dark" } else { "" }),
        );
    }
    // the status text before the first progress message
    let mut app = editing_app();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.exporting = Some(Exporting {
        rx,
        out: std::path::PathBuf::from("/tmp"),
    });
    let (text, _) = app.activity(&style::Palette::LIGHT);
    assert_eq!(text, "Exporting...");
}

/// The inline name field under the project, and the delete prompt above the file list.
#[test]
fn renders_the_project_tree_actions() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("pascoda-tree-render-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("debug.pas"), "program debug;\nbegin\nend.\n").unwrap();
    std::fs::write(root.join("sub/inner.pas"), "unit inner;\n").unwrap();
    let mut app = App::bare(Some(root.join("debug.pas")));
    // The app's root is canonical (`/private/var` on macOS); the new row must match it.
    let canonical = app.root.clone();
    app.tree_new = Some(shell::TreeNew::new(canonical, false));
    render(app, false, (1368.0, 800.0), "ui-tree-new-file");

    let mut app = App::bare(Some(root.join("debug.pas")));
    app.tree_delete = Some(root.join("sub"));
    render(app, true, (1368.0, 800.0), "ui-tree-delete-dark");
    let _ = std::fs::remove_dir_all(&root);
}

/// The name field that replaces a row while it is renamed.
#[test]
fn renders_the_rename_field() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("pascoda-rename-render-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("debug.pas"), "program debug;\nbegin\nend.\n").unwrap();
    let mut app = App::bare(Some(root.join("debug.pas")));
    let canonical = app.root.clone();
    app.tree_rename = Some(shell::TreeRename::new(canonical.join("debug.pas"), "debug.pas"));
    render(app, false, (1368.0, 800.0), "ui-tree-rename");
    let _ = std::fs::remove_dir_all(&root);
}


/// Tab in the editor inserts spaces up to the next tab stop and keeps the focus there.
#[test]
fn tab_in_the_editor_inserts_spaces_to_the_next_stop() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use egui::text::{CCursor, CCursorRange};
    use egui::{Event, Key, Modifiers};
    let mut app = editing_app();
    app.docs[0].text = "ab".into();
    let id = app.docs[0].editor_id;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 640.0))
        .with_pixels_per_point(1.0)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    theme::apply(&harness.ctx);
    harness.run_steps(2);
    harness.ctx.memory_mut(|m| m.request_focus(id));
    let mut state = egui::TextEdit::load_state(&harness.ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(2))));
    state.store(&harness.ctx, id);
    harness.run_steps(2);
    assert!(harness.ctx.memory(|m| m.has_focus(id)), "the editor has the focus");

    let tab = |h: &Harness<'_, App>| {
        h.event_modifiers(
            Event::Key {
                key: Key::Tab,
                physical_key: Some(Key::Tab),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
    };
    tab(&harness);
    harness.run_steps(3);
    // Column 2 with 4-wide tab stops: two spaces.
    assert_eq!(harness.state().docs[0].text, "ab  ");
    assert!(harness.ctx.memory(|m| m.has_focus(id)), "Tab keeps the focus in the editor");
}

/// Regression: a palette page row wider than the free area (the db demo's many pages, or a long
/// "unit not updated" note) widened the designer's Ui, so the canvas reached under the inspector:
/// the form sat too far right and, in the real window, the native views covered the inspector.
#[test]
fn a_wide_palette_row_keeps_the_canvas_beside_the_inspector() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/db-demo");
    let dir = out_dir().join("db-demo-canvas");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(&src).unwrap().flatten() {
        if e.path().is_file() {
            std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
        }
    }
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.lsp_ready = true;
    app.show_form();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(980.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(3);
    let before = harness.state().designer_ui.view.expect("the canvas was laid out");
    // A row far wider than the window; then select a tray component and the grid, as in the bug.
    harness.state_mut().designers[0].sync_error = Some("x".repeat(400));
    let names = ["DBGrid1", "SQLConnector1", "StatusLabel"];
    for name in names {
        let i = harness.state().designers[0]
            .doc
            .root()
            .children
            .iter()
            .position(|c| c.name == name)
            .unwrap();
        harness.state_mut().designers[0].select(vec![i]);
        harness.run_steps(3);
        let view = harness.state().designer_ui.view.unwrap();
        assert!(
            view.area.right() <= before.area.right() + 0.5,
            "{name}: the canvas {:?} reaches past where it was {:?}",
            view.area,
            before.area
        );
        assert!(
            view.client().left() >= view.area.left(),
            "{name}: the form starts inside the canvas"
        );
    }
    harness
        .render()
        .expect("offscreen render")
        .save(out_dir().join("ui-form-wide-palette-row.png"))
        .unwrap();
}

/// Selecting a form file in the tree shows the form and its properties pane.
#[test]
fn renders_the_inspector_after_selecting_a_form_file() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui");
    let mut app = App::bare(Some(root.join("unit1.pas")));
    app.lsp_ready = true;
    app.select_in_tree(root.join("unit1.lfm"));
    println!("form_view={} active={:?} designers={}", app.form_view, app.docs.get(app.active).map(|d| d.path.clone()), app.designers.len());
    let a = render(app, false, (1200.0, 800.0), "ui-tree-form-selected-1200");
    println!("{}", a.display());
}

/// The OpenAI Compatible settings page for Ollama: the server's address, the optional key and the
/// privacy note.
#[test]
fn renders_the_ollama_settings() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.provider = assistant::Provider::Ollama;
    app.assistant.compat = pascoda_ai::openai::OpenAiSettings {
        base_url: "http://127.0.0.1:11434/v1".into(),
        model: "qwen3:4b".into(),
    };
    app.assistant.compat_edit = app.assistant.compat.clone();
    app.assistant.compat_key_state = assistant::KeyState::Missing; // never read the real Keychain
    app.assistant.show_settings = true;
    let a = render(app, false, (1368.0, 900.0), "ui-assistant-ollama-settings");
    println!("{}", a.display());
}

/// The "Some tools are missing" window: what is missing, and the button types the install command
/// at the Terminal's prompt without running it.
#[test]
fn the_vendor_window_types_its_command_in_the_terminal_without_running_it() {
    let _ui = crate::prefs::tests::APPLY.read().unwrap_or_else(|e| e.into_inner());
    use pascoda_fpc::vendor::Part;
    let mut app = editing_app();
    app.vendor_dialog = Some(vec![Part::Fpc]);
    // A shell with a known prompt; started in the project folder, so the pane keeps it.
    let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
    cmd.env("PS1", "$ ");
    app.bottom.terminal = Some(crate::terminal::Terminal::spawn(cmd, 10, 80, || {}).expect("spawn"));
    app.bottom.terminal_root = Some(app.root.clone());

    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    h.run_steps(3);
    // The window says what is missing and what it is for.
    assert!(h.query_by_label_contains("Free Pascal compiler").is_some());
    assert!(h.query_by_label("Install in Terminal").is_some());
    let png = h.render().expect("render");
    png.save(out_dir().join("ui-vendor-dialog.png")).unwrap();

    h.get_by_label("Install in Terminal").click();
    h.run_steps(3);
    assert!(h.state().vendor_dialog.is_none(), "the window closes");
    assert_eq!(h.state().bottom.tab, bottom::BottomTab::Terminal);
    let screen = |h: &Harness<'_, App>| h.state().bottom.terminal.as_ref().unwrap().screen_text();
    for _ in 0..200 {
        h.run_steps(1);
        if screen(&h).contains("brew install fpc") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let text = screen(&h);
    assert!(text.contains("$ brew install fpc"), "typed at the prompt:\n{text}");
    // Not run: no second prompt, no output.
    std::thread::sleep(std::time::Duration::from_millis(300));
    h.run_steps(2);
    let text = screen(&h);
    assert_eq!(text.matches("$ ").count(), 1, "the command waits for Enter:\n{text}");
}
