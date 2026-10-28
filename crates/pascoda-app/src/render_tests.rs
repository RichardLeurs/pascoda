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
fn renders_the_bottom_tabs() {
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
    let a = render(menu_app(), false, (1368.0, 1000.0), "ui-menu-editor");
    let b = render(menu_app(), true, (1368.0, 1000.0), "ui-menu-editor-dark");
    println!("{}\n{}", a.display(), b.display());
}

/// The editor's buttons reach the designer: Add Item, Add Sub-item, Indent, Done.
#[test]
fn menu_editor_buttons_edit_the_form() {
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
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/notes");
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
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.key = assistant::KeyState::Missing;
    let p = render(app, false, (1368.0, 800.0), "ui-assistant-key");
    println!("{}", p.display());
}

#[test]
fn renders_the_empty_assistant() {
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
    let _keychain = assistant::KEYCHAIN
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.key = assistant::KeyState::Missing;
    // The CLI check is not started here: a state with a result already.
    app.assistant.cli = assistant::CliState::Missing;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 800.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    assert_eq!(harness.state().assistant.provider, assistant::Provider::Api);
    harness.get_by_label("Claude Code (subscription)").click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().assistant.provider,
        assistant::Provider::ClaudeCode
    );
    // the CLI card is up: it says so and the key field is gone
    assert!(harness.query_by_label("Check again").is_some());
    assert!(harness.query_by_label("API key").is_none());
    harness.get_by_label("Anthropic API key").click();
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
fn the_openai_card_takes_a_local_endpoint() {
    let mut app = editing_app();
    app.assistant.open = true;
    app.assistant.key = assistant::KeyState::Missing;
    app.assistant.openai_key_state = assistant::KeyState::Missing; // never read the real Keychain
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1368.0, 900.0))
        .with_pixels_per_point(1.0)
        .with_context_setup(theme::apply)
        .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
    harness.run_steps(2);
    harness.get_by_label("OpenAI API key").click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().assistant.provider,
        assistant::Provider::OpenAi
    );
    assert!(!harness.state().assistant.ready());
    // the OpenAI card names no other vendor
    assert!(harness.query_by_label("OpenAI base URL").is_some());
    assert!(harness.query_by_label("Check again").is_none());
    // Point at a local server (no key needed there)
    let url = harness.get_by_label("OpenAI base URL");
    url.focus();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(1);
    harness
        .get_by_label("OpenAI base URL")
        .type_text("http://localhost:11434/v1");
    harness.run_steps(2);
    harness.get_by_label("OpenAI model name").focus();
    harness.run_steps(2);
    harness
        .get_by_label("OpenAI model name")
        .type_text("llama3.1");
    harness.run_steps(2);
    assert_eq!(
        harness.state().assistant.openai_edit.base_url,
        "http://localhost:11434/v1"
    );
    harness.get_by_label("Save").click();
    harness.run_steps(3);
    let a = &harness.state().assistant;
    assert!(a.ready(), "a local server needs no key");
    assert_eq!(a.openai.model, "llama3.1");
    // the chat is up with the model field and the neutral placeholder; the settings stay open
    // (only the gear closes them), so the Model label is there twice
    assert!(harness.query_all_by_label("Model").next().is_some());
    assert!(harness.query_by_label("Message").is_some());
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
    assert!(
        harness
            .query_by_label("Edits applied automatically")
            .is_none()
    );
    harness.get_by_label("Allow edits this session").click();
    harness.run_steps(3);
    assert!(harness.state().assistant.auto_edits);
    harness.get_by_label("Edits applied automatically").click();
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
