//! The demo projects under `examples/`, built and run the way the IDE builds them. Each one is
//! copied to a scratch folder first, so the build products never land in the checkout.

use std::path::{Path, PathBuf};

/// Tests run in parallel inside one process, so each copy gets its own folder.
static COPIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn demo(name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name);
    let n = COPIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pascoda-demo-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&src, &dir);
    dir
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            if entry.file_name() != "lib" {
                copy(&path, &target);
            }
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// Builds `program` in `dir` with the project's arguments, as Build & Run does, and runs it.
fn build_and_run(dir: &Path, program: &str) -> String {
    let (args, _) = crate::project_args(Some(dir));
    let source = dir.join(format!("{program}.lpr"));
    let result = pascoda_fpc::build_progress_with(&source, pascoda_fpc::Mode::ObjFpc, &args, None, &mut |_| {})
        .unwrap();
    assert!(result.success, "{program} does not build:\n{}", result.raw_output);
    let out = std::process::Command::new(dir.join(program)).output().unwrap();
    assert!(out.status.success(), "{program} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_library_demo_builds_with_its_unit_and_runs() {
    let dir = demo("library-demo");
    let out = build_and_run(&dir, "MathsDemo");
    assert!(out.contains("10! = 3628800"), "{out}");
    assert!(out.contains("2 3 5 7 11 13 17 19"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_package_demo_finds_its_unit_through_the_lpk() {
    let dir = demo("package-demo");
    let out = build_and_run(&dir, "PackageDemo");
    assert_eq!(out, "Hello pascal\nadocsap\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_console_demo_counts_what_it_reads() {
    let dir = demo("console-demo");
    let (args, _) = crate::project_args(Some(&dir));
    let source = dir.join("WordCount.lpr");
    let result = pascoda_fpc::build_progress_with(&source, pascoda_fpc::Mode::ObjFpc, &args, None, &mut |_| {}).unwrap();
    assert!(result.success, "{}", result.raw_output);
    let mut child = std::process::Command::new(dir.join("WordCount"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"one two three\n  four  \n").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2 lines, 4 words, 23 characters\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_test_demo_runs_its_tests_and_the_one_that_fails_on_purpose_fails() {
    let dir = demo("tests-demo");
    let mut t = crate::unit_tests::UnitTests::default();
    let ctx = eframe::egui::Context::default();
    assert!(t.start(&ctx, &dir, Vec::new(), None));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    while !matches!(t.phase, crate::unit_tests::Phase::Finished | crate::unit_tests::Phase::Failed(_)) {
        assert!(std::time::Instant::now() < deadline, "the run did not finish: {:?}", t.phase);
        t.poll();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(t.phase, crate::unit_tests::Phase::Finished);
    assert_eq!(t.entries.len(), 4);
    assert_eq!((t.runs, t.failures, t.errors), (4, 1, 0));
    assert_eq!(t.failed_ids(), ["TCalcTests.TestAddIsCommutative"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_unit_tests_from_the_menu_starts_the_tests_of_the_open_demo() {
    let dir = demo("tests-demo");
    let mut app = crate::App::bare(Some(dir.join("CalcTests.pas")));
    assert_eq!(app.root.canonicalize().unwrap(), dir.canonicalize().unwrap(), "opening a file makes its folder the project");
    let ctx = eframe::egui::Context::default();
    app.run_menu_list(&ctx, vec![crate::MenuAction::RunTests]);
    assert!(app.unit_tests.busy() || app.unit_tests.entries.len() > 0, "the run started: {:?}", app.unit_tests.phase);
    assert_eq!(app.bottom.tab, crate::bottom::BottomTab::Tests, "the Tests pane is shown");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    while app.unit_tests.busy() {
        assert!(std::time::Instant::now() < deadline, "the run did not finish: {:?}", app.unit_tests.phase);
        app.poll_unit_tests();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(app.unit_tests.entries.len(), 4, "the four tests are listed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_tests_pane_lists_the_demo_tests_without_running_them() {
    let dir = demo("tests-demo");
    let mut app = crate::App::bare(Some(dir.join("CalcTests.pas")));
    let ctx = eframe::egui::Context::default();
    app.list_unit_tests(&ctx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    while app.unit_tests.busy() {
        assert!(std::time::Instant::now() < deadline, "the listing did not finish: {:?}", app.unit_tests.phase);
        app.poll_unit_tests();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(app.unit_tests.phase, crate::unit_tests::Phase::Finished);
    assert_eq!(app.unit_tests.entries.len(), 4, "the four tests can be selected");
    assert_eq!(app.unit_tests.runs, 0, "nothing ran");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project folder holding the Pascoda VLC package and a GUI program that uses it.
fn vlc_project(name: &str, program: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pascoda-vlc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pkg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-vlc");
    for f in ["PascodaVlc.pas", "RegPascodaVlc.pas", "PascodaVlc.lpk"] {
        std::fs::copy(pkg.join(f), dir.join(f)).unwrap();
    }
    std::fs::write(dir.join(format!("{program}.lpr")), vlc_program(program)).unwrap();
    dir
}

fn vlc_program(name: &str) -> String {
    format!(
        r#"program {name};

{{$mode objfpc}}{{$H+}}

uses
  Interfaces, Forms, Controls, ExtCtrls, SysUtils, PascodaVlc;

var
  Form: TForm;
  Panel: TPanel;
  Player: TPascodaVlcPlayer;
  i: Integer;
  playing: Boolean;
begin
  Application.Initialize;
  Form := TForm.Create(nil);
  Form.SetBounds(100, 100, 400, 300);
  Panel := TPanel.Create(Form);
  Panel.Parent := Form;
  Panel.SetBounds(0, 0, 400, 300);
  Player := TPascodaVlcPlayer.Create(Form);
  Form.Show;
  for i := 1 to 10 do
  begin
    Application.ProcessMessages;
    Sleep(50);
  end;
  try
    Player.VideoPanel := Panel;
    Player.Open('/System/Library/Sounds/Glass.aiff');
    Player.Play;
    playing := False;
    for i := 1 to 60 do
    begin
      Application.ProcessMessages;
      Sleep(50);
      if Player.Playing then
        playing := True;
    end;
    WriteLn('PLAYING ', playing);
  except
    on E: EPascodaVlc do
      WriteLn('VLC ', E.Message);
  end;
  Flush(Output);
  Halt(0);
end.
"#
    )
}

fn gui_build(dir: &Path, program: &str) -> pascoda_fpc::BuildResult {
    let (args, _) = crate::project_args(Some(dir));
    let lcl = pascoda_fpc::lcl::Lcl::find(dir).expect("vendor/lazarus is needed for GUI programs");
    pascoda_fpc::lcl::build_gui_progress_with(
        &dir.join(format!("{program}.lpr")),
        pascoda_fpc::Mode::ObjFpc,
        &lcl,
        &args,
        None,
        &mut |_| {},
    )
    .unwrap()
}

#[test]
fn the_pascoda_vlc_package_builds_with_the_lcl_without_vlc_installed() {
    let dir = vlc_project("builds", "VlcPlayer");
    let built = gui_build(&dir, "VlcPlayer");
    assert!(built.success, "the package does not build:\n{}", built.raw_output);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_pascoda_vlc_player_plays_a_sound_when_vlc_is_installed() {
    if !Path::new("/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib").is_file() {
        return; // VLC.app is not installed on this Mac
    }
    let dir = vlc_project("plays", "VlcPlayer");
    let built = gui_build(&dir, "VlcPlayer");
    assert!(built.success, "{}", built.raw_output);
    let out = std::process::Command::new(dir.join("VlcPlayer")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("PLAYING TRUE"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_vlc_demo_builds_with_the_package_and_starts_its_window() {
    // Importing the package makes its units visible to the project: the test copies them in.
    let dir = std::env::temp_dir().join(format!("pascoda-vlc-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vlc-demo"), &dir);
    let pkg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-vlc");
    for f in ["PascodaVlc.pas", "RegPascodaVlc.pas", "PascodaVlc.lpk"] {
        std::fs::copy(pkg.join(f), dir.join(f)).unwrap();
    }
    let built = gui_build(&dir, "VlcDemo");
    assert!(built.success, "the demo does not build:\n{}", built.raw_output);
    let mut child = std::process::Command::new(dir.join("VlcDemo")).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(child.try_wait().unwrap().is_none(), "the window closed: the form did not load");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_pascoda_data_package_offers_the_database_components_on_the_data_page() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-data/PascodaData.lpk");
    let package = pascoda_project::packages::read(&file).expect("the data package reads");
    let folder = file.parent().unwrap();
    let dirs = pascoda_project::packages::resolve_with(folder, &[]).unit_paths;
    let found = crate::designer::imports::entries(std::slice::from_ref(&package), &dirs);
    if found.is_empty() {
        return; // the vendored FPC database sources are not checked out
    }
    let got: Vec<(&str, &str, &str, bool)> = found.iter().map(|e| (e.page, e.class, e.unit, e.nonvisual)).collect();
    for (class, unit) in [
        ("TSQLConnector", "sqldb"),
        ("TSQLTransaction", "sqldb"),
        ("TSQLQuery", "sqldb"),
        ("TSQLScript", "sqldb"),
        ("TDataSource", "db"),
        ("TSQLite3Connection", "sqlite3conn"),
    ] {
        let entry = got.iter().find(|(_, c, _, _)| *c == class).unwrap_or_else(|| panic!("{class} is offered: {got:?}"));
        assert_eq!(entry.0, "Data Controls", "{class} is on the Data Controls page");
        assert_eq!(entry.2, unit, "{class} is declared in {unit}, which a form must use");
        assert!(entry.3, "{class} is non-visual (it derives from TComponent)");
    }
    // Each data component has its Lazarus icon, by class name, in the package folder.
    let icons = crate::designer::imports::icon_files(std::slice::from_ref(&package), &found);
    let for_query = icons.iter().find(|(c, _)| c == "TSQLQuery").expect("the query has an icon");
    assert!(for_query.1.ends_with("tsqlquery.png"), "{:?}", for_query.1);
}

#[test]
fn a_program_with_the_database_components_builds_and_runs() {
    let dir = std::env::temp_dir().join(format!("pascoda-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pkg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-data");
    for f in ["RegPascodaData.pas", "PascodaData.lpk"] {
        std::fs::copy(pkg.join(f), dir.join(f)).unwrap();
    }
    std::fs::write(
        dir.join("DataCheck.lpr"),
        "program DataCheck;\n\n{$mode objfpc}{$H+}\n\nuses\n  Classes, SysUtils, db, sqldb, sqlite3conn, RegPascodaData;\n\nvar\n  Conn: TSQLConnector;\n  Trans: TSQLTransaction;\n  Query: TSQLQuery;\n  Source: TDataSource;\n  Lite: TSQLite3Connection;\nbegin\n  Conn := TSQLConnector.Create(nil);\n  Trans := TSQLTransaction.Create(nil);\n  Query := TSQLQuery.Create(nil);\n  Source := TDataSource.Create(nil);\n  Lite := TSQLite3Connection.Create(nil);\n  Query.DataBase := Conn;\n  Source.DataSet := Query;\n  WriteLn('OK');\nend.\n",
    )
    .unwrap();
    let out = build_and_run(&dir, "DataCheck");
    assert_eq!(out, "OK\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_db_demo_builds_and_its_query_finds_the_ideas() {
    let dir = std::env::temp_dir().join(format!("pascoda-db-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/db-demo"), &dir);
    // The data package is imported: its units join the project, as they do for the user.
    let pkg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-data");
    for f in ["RegPascodaData.pas", "PascodaData.lpk"] {
        std::fs::copy(pkg.join(f), dir.join(f)).unwrap();
    }
    let built = gui_build(&dir, "DbDemo");
    assert!(built.success, "the demo does not build:\n{}", built.raw_output);
    // The self-test prints the status line and the row count, then exits. A window that waits (an
    // error dialog) is killed after the time limit and the test fails with what it printed.
    let mut child = std::process::Command::new(dir.join("DbDemo"))
        .env("DBDEMO_SELFTEST", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the self-test did not finish: the window waits (a dialog?)");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "STATUS 6 ideas\nROWS 6\nTEXT ftString\n", "the query returns the six ideas");
    // Build & Run wraps a GUI program in a .app; from there the database is found beside the bundle.
    let bundle = pascoda_fpc::lcl::make_bundle(&dir.join("DbDemo"), "DbDemo").unwrap();
    let out = std::process::Command::new(bundle.join("Contents/MacOS/DbDemo"))
        .env("DBDEMO_SELFTEST", "1")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "STATUS 6 ideas\nROWS 6\nTEXT ftString\n",
        "run from its bundle, the program finds the database beside it"
    );
    // The window itself starts and stays up.
    let mut child = std::process::Command::new(dir.join("DbDemo")).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(child.try_wait().unwrap().is_none(), "the window closed: the database did not open");
    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_tests_demo_program_runs_its_tests_with_the_console_runner() {
    let dir = std::env::temp_dir().join(format!("pascoda-tests-program-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/tests-demo"), &dir);
    let (args, _) = crate::project_args(Some(&dir));
    let result = pascoda_fpc::build_progress_with(&dir.join("TestsDemo.lpr"), pascoda_fpc::Mode::ObjFpc, &args, None, &mut |_| {})
        .unwrap();
    assert!(result.success, "the tests program does not build:\n{}", result.raw_output);
    let out = std::process::Command::new(dir.join("TestsDemo")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("TestAddsTwoNumbers") || text.contains("CalcTests"), "the tests run: {text}");
    assert!(text.contains("Number of run tests: 4") && text.contains("Number of failures:  1"), "four tests, one fails on purpose: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}
