//! Pascal unit tests written for FPCUnit. Pascoda writes a small runner program that uses the
//! project's test units and FPCUnit's own sources, builds it with the same compiler arguments as
//! the project, and runs it. The runner reports each test as a line on stdout (`@@pascoda` then
//! tab-separated fields), so the IDE shows results as they come and does not depend on the layout of
//! FPCUnit's reports. Other output of the tests passes through unchanged.

use crate::{BuildResult, Mode, Progress};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The protocol prefix on every runner line.
pub const PREFIX: &str = "@@pascoda";

/// The runner program's name (its source is `PascodaTests.pas`).
pub const PROGRAM: &str = "PascodaTests";

/// The FPCUnit sources in `vendor/fpcsrc`, found the way the other vendored tools are.
pub fn fpcunit_src(start: &Path) -> Option<PathBuf> {
    crate::lcl::search_dirs(start)
        .iter()
        .map(|d| d.join("vendor/fpcsrc/packages/fcl-fpcunit/src"))
        .find(|d| d.join("fpcunit.pp").is_file())
}

/// The Pascal program that runs the given test units. Each unit is in `uses`, which registers its
/// tests. `--list` prints every test; `--test ID` runs one; with no option every test runs.
pub fn runner_source(units: &[String]) -> String {
    let uses: Vec<&str> = units.iter().map(String::as_str).collect();
    let uses = std::iter::once("SysUtils")
        .chain(["fpcunit", "testregistry"])
        .chain(uses)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"program {PROGRAM};

{{$mode objfpc}}{{$H+}}

uses {uses};

type
  TPascodaListener = class(TInterfacedObject, ITestListener)
  public
    procedure AddFailure(ATest: TTest; AFailure: TTestFailure);
    procedure AddError(ATest: TTest; AError: TTestFailure);
    procedure StartTest(ATest: TTest);
    procedure EndTest(ATest: TTest);
    procedure StartTestSuite(ATestSuite: TTestSuite);
    procedure EndTestSuite(ATestSuite: TTestSuite);
  end;

function Escape(const S: string): string;
var
  i: Integer;
begin
  Result := '';
  for i := 1 to Length(S) do
    case S[i] of
      '\': Result := Result + '\\';
      #10: Result := Result + '\n';
      #13: ;
      #9: Result := Result + ' ';
    else
      Result := Result + S[i];
    end;
end;

procedure Send(const Fields: string);
begin
  WriteLn('{PREFIX}' + #9 + Fields);
  Flush(Output);
end;

function IdOf(ATest: TTest): string;
begin
  Result := ATest.TestSuiteName + '.' + ATest.TestName;
end;

procedure Collect(ATest: TTest);
var
  i: Integer;
begin
  if ATest is TTestSuite then
    for i := 0 to TTestSuite(ATest).ChildTestCount - 1 do
      Collect(TTestSuite(ATest).Test[i])
  else
    Send('LIST' + #9 + IdOf(ATest));
end;

function Find(ATest: TTest; const Id: string): TTest;
var
  i: Integer;
begin
  Result := nil;
  if ATest is TTestSuite then
    for i := 0 to TTestSuite(ATest).ChildTestCount - 1 do
    begin
      Result := Find(TTestSuite(ATest).Test[i], Id);
      if Result <> nil then
        Exit;
    end
  else if IdOf(ATest) = Id then
    Result := ATest;
end;

procedure TPascodaListener.AddFailure(ATest: TTest; AFailure: TTestFailure);
begin
  Send('FAIL' + #9 + IdOf(ATest) + #9 + AFailure.ExceptionClassName + #9 +
    Escape(AFailure.ExceptionMessage));
end;

procedure TPascodaListener.AddError(ATest: TTest; AError: TTestFailure);
begin
  Send('ERROR' + #9 + IdOf(ATest) + #9 + AError.ExceptionClassName + #9 +
    Escape(AError.ExceptionMessage));
end;

procedure TPascodaListener.StartTest(ATest: TTest);
begin
  Send('START' + #9 + IdOf(ATest));
end;

procedure TPascodaListener.EndTest(ATest: TTest);
begin
  Send('END' + #9 + IdOf(ATest));
end;

procedure TPascodaListener.StartTestSuite(ATestSuite: TTestSuite);
begin
end;

procedure TPascodaListener.EndTestSuite(ATestSuite: TTestSuite);
begin
end;

var
  Result: TTestResult;
  Listener: ITestListener;
  Selected: TTest;
  Failed: Integer;
begin
  if (ParamCount >= 1) and (ParamStr(1) = '--list') then
  begin
    Collect(GetTestRegistry);
    Halt(0);
  end;
  Result := TTestResult.Create;
  Listener := TPascodaListener.Create;
  Result.AddListener(Listener);
  try
    if (ParamCount >= 2) and (ParamStr(1) = '--test') then
    begin
      Selected := Find(GetTestRegistry, ParamStr(2));
      if Selected = nil then
      begin
        Send('ERROR' + #9 + ParamStr(2) + #9 + 'EUnknownTest' + #9 + 'No test has this name');
        Halt(2);
      end;
      Selected.Run(Result);
    end
    else
      GetTestRegistry.Run(Result);
    Failed := Result.NumberOfFailures + Result.NumberOfErrors;
    Send('DONE' + #9 + IntToStr(Result.RunTests) + #9 + IntToStr(Result.NumberOfFailures) + #9 +
      IntToStr(Result.NumberOfErrors));
  finally
    Result.Free;
  end;
  if Failed > 0 then
    Halt(1);
end.
"#
    )
}

/// One line of the runner's output, as the IDE shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A test exists (from `--list`).
    Listed(String),
    /// A test started.
    Started(String),
    /// A test finished (its failures came before this).
    Ended(String),
    /// A test failed (`error` false) or raised an exception (`error` true). The runner does not
    /// know the line (fpc's line info is not available for this runner), so the IDE finds the
    /// test's method in its source.
    Failed {
        id: String,
        error: bool,
        class: String,
        message: String,
    },
    /// The run is over.
    Done { runs: u32, failures: u32, errors: u32 },
    /// Any other line: the tests' own output, or a message from the runner.
    Output(String),
}

/// Reads one line of runner output.
pub fn parse_line(line: &str) -> Event {
    let Some(rest) = line.strip_prefix(PREFIX).and_then(|r| r.strip_prefix('\t')) else {
        return Event::Output(line.to_string());
    };
    let f: Vec<&str> = rest.split('\t').collect();
    let num = |s: Option<&&str>| s.and_then(|s| s.parse().ok()).unwrap_or(0);
    match f.first().copied() {
        Some("LIST") if f.len() >= 2 => Event::Listed(f[1].to_string()),
        Some("START") if f.len() >= 2 => Event::Started(f[1].to_string()),
        Some("END") if f.len() >= 2 => Event::Ended(f[1].to_string()),
        Some(kind @ ("FAIL" | "ERROR")) if f.len() >= 4 => Event::Failed {
            id: f[1].to_string(),
            error: kind == "ERROR",
            class: f[2].to_string(),
            message: unescape(&f[3..].join("\t")),
        },
        Some("DONE") => Event::Done {
            runs: num(f.get(1)),
            failures: num(f.get(2)),
            errors: num(f.get(3)),
        },
        _ => Event::Output(line.to_string()),
    }
}

/// Undoes the runner's escaping: `\\` is a backslash and `\n` a line break.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A Pascal unit that has tests: its name (the `unit` line) and the folder it is in.
#[derive(Debug, Clone, PartialEq)]
pub struct TestUnit {
    pub name: String,
    pub dir: PathBuf,
}

/// Where the runner program and its binary are built, under the project folder.
pub fn runner_dir(folder: &Path) -> PathBuf {
    folder.join("lib").join("tests")
}

/// Writes the runner for `units` and builds it with `extra` (the project's compiler arguments,
/// without `-o`) and the FPCUnit sources. Returns the build and the binary's path.
pub fn build(
    folder: &Path,
    units: &[TestUnit],
    extra: &[OsString],
    fpcunit: &Path,
    total: Option<usize>,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<(BuildResult, PathBuf)> {
    let dir = runner_dir(folder);
    std::fs::create_dir_all(dir.join("units"))?;
    let source = dir.join(format!("{PROGRAM}.pas"));
    let names: Vec<String> = units.iter().map(|u| u.name.clone()).collect();
    std::fs::write(&source, runner_source(&names))?;
    let mut args: Vec<OsString> = extra
        .iter()
        .filter(|a| !a.to_string_lossy().starts_with("-o"))
        .cloned()
        .collect();
    args.push(format!("-Fu{}", fpcunit.display()).into());
    let mut seen = std::collections::BTreeSet::new();
    for unit in units {
        if seen.insert(unit.dir.clone()) {
            args.push(format!("-Fu{}", unit.dir.display()).into());
        }
    }
    args.push(format!("-FU{}", dir.join("units").display()).into());
    let binary = dir.join(PROGRAM);
    args.push(format!("-o{}", binary.display()).into());
    let result = crate::build_progress_with(&source, Mode::ObjFpc, &args, total, on)?;
    Ok((result, binary))
}

/// Runs the runner (or any program that speaks the protocol) with `args`, passing each output line
/// to `on` as it comes. Returns the exit code, or `None` when it was stopped by a signal.
pub fn run(
    binary: &Path,
    args: &[&str],
    cancel: &std::sync::atomic::AtomicBool,
    on: &mut dyn FnMut(Event),
) -> std::io::Result<Option<i32>> {
    use std::sync::atomic::Ordering;
    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let out = child.stdout.take();
    let err = child.stderr.take();
    let tx2 = tx.clone();
    let reader = std::thread::spawn(move || {
        if let Some(out) = out {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
        if let Some(err) = err {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx2.send(line);
            }
        }
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
        }
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(line) => on(parse_line(&line)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait()? {
                    // The readers finish once the pipes close; drain what is left.
                    let _ = reader.join();
                    let _ = stderr_reader.join();
                    while let Ok(line) = rx.try_recv() {
                        on(parse_line(&line));
                    }
                    return Ok(status.code());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait()?;
                return Ok(status.code());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runner_uses_fpcunit_and_each_test_unit() {
        let src = runner_source(&["tests_demo".to_string(), "tests_more".to_string()]);
        assert!(src.starts_with("program PascodaTests;"));
        assert!(src.contains("uses SysUtils, fpcunit, testregistry, tests_demo, tests_more;"));
        assert!(src.contains("TPascodaListener = class(TInterfacedObject, ITestListener)"));
        assert!(src.contains("ParamStr(1) = '--list'"));
        assert!(src.contains("Send('START' + #9 + IdOf(ATest));"));
        // The protocol prefix is written into the program, not left as a placeholder.
        assert!(src.contains("WriteLn('@@pascoda' + #9 + Fields);"));
        assert!(!src.contains('{') || src.contains("{$mode"), "braces are Pascal comments or directives");
    }

    #[test]
    fn the_runner_with_no_units_still_uses_fpcunit() {
        let src = runner_source(&[]);
        assert!(src.contains("uses SysUtils, fpcunit, testregistry;"));
    }

    #[test]
    fn protocol_lines_become_events() {
        assert_eq!(parse_line("@@pascoda\tLIST\tTDemoTests.TestPasses"), Event::Listed("TDemoTests.TestPasses".into()));
        assert_eq!(parse_line("@@pascoda\tSTART\tTDemoTests.TestPasses"), Event::Started("TDemoTests.TestPasses".into()));
        assert_eq!(parse_line("@@pascoda\tEND\tTDemoTests.TestPasses"), Event::Ended("TDemoTests.TestPasses".into()));
        assert_eq!(
            parse_line("@@pascoda\tDONE\t2\t1\t0"),
            Event::Done { runs: 2, failures: 1, errors: 0 }
        );
    }

    #[test]
    fn failures_keep_multi_line_messages() {
        let e = parse_line("@@pascoda\tFAIL\tTDemoTests.TestFails\tEAssertionFailedError\tdiffers\\nexpected 2\\\\ got 3");
        assert_eq!(
            e,
            Event::Failed {
                id: "TDemoTests.TestFails".into(),
                error: false,
                class: "EAssertionFailedError".into(),
                message: "differs\nexpected 2\\ got 3".into(),
            }
        );
        let e = parse_line("@@pascoda\tERROR\tT.Test\tEInvalidOp\tboom");
        assert!(matches!(e, Event::Failed { error: true, .. }));
    }

    #[test]
    fn other_output_passes_through() {
        assert_eq!(parse_line("hello from the test"), Event::Output("hello from the test".into()));
        assert_eq!(parse_line("@@pascoda\tUNKNOWN\tx"), Event::Output("@@pascoda\tUNKNOWN\tx".into()));
    }
}
