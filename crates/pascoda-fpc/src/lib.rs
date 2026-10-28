//! Drives the Free Pascal compiler and parses its output into diagnostics.

pub mod check;
pub mod export;
pub mod lcl;

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Fatal,
    Error,
    Warning,
    Note,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub message: String,
}

/// Parses one fpc output line such as `hello.pas(3,5) Error: Identifier not found "x"`.
/// Returns `None` for progress lines and linker noise.
pub fn parse_line(line: &str) -> Option<Diagnostic> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let (l, c) = line[open + 1..close].split_once(',')?;
    let rest = line[close + 1..].trim_start();
    let (sev, message) = rest.split_once(':')?;
    let severity = match sev.trim() {
        "Fatal" => Severity::Fatal,
        "Error" => Severity::Error,
        "Warning" => Severity::Warning,
        "Note" => Severity::Note,
        "Hint" => Severity::Hint,
        _ => return None,
    };
    Some(Diagnostic {
        file: PathBuf::from(&line[..open]),
        line: l.trim().parse().ok()?,
        col: c.trim().parse().ok()?,
        severity,
        message: message.trim().to_string(),
    })
}

pub fn parse_output(output: &str) -> Vec<Diagnostic> {
    output.lines().filter_map(parse_line).collect()
}

/// Pascal dialect passed to fpc. ObjFPC gives a 32-bit `Integer`; fpc's own default does not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    ObjFpc,
    Delphi,
}

impl Mode {
    /// The word used in project settings and the status bar.
    pub fn name(self) -> &'static str {
        match self {
            Mode::ObjFpc => "objfpc",
            Mode::Delphi => "delphi",
        }
    }

    pub fn from_name(name: &str) -> Option<Mode> {
        match name.trim().to_ascii_lowercase().as_str() {
            "objfpc" => Some(Mode::ObjFpc),
            "delphi" => Some(Mode::Delphi),
            _ => None,
        }
    }

    /// Shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Mode::ObjFpc => "ObjFPC",
            Mode::Delphi => "Delphi",
        }
    }

    pub fn flag(self) -> &'static str {
        match self {
            Mode::ObjFpc => "-Mobjfpc",
            Mode::Delphi => "-Mdelphi",
        }
    }
}

#[derive(Debug)]
pub struct BuildResult {
    pub success: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub raw_output: String,
}

/// Where a build is, for a progress display.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// What the compiler is doing now, e.g. `Compiling unit1.pas`.
    pub label: String,
    /// 0..1 when the amount of work is known; `None` shows an indeterminate bar.
    pub fraction: Option<f32>,
    /// Units compiled so far in this fpc run (callers remember it to size the next build).
    pub done: usize,
}

/// How a build's fpc run maps onto the overall progress: `total` units are expected (if
/// known), and the run fills the part `lo..hi` of the whole bar.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub total: Option<usize>,
    pub lo: f32,
    pub hi: f32,
}

impl Plan {
    pub const WHOLE: Plan = Plan {
        total: None,
        lo: 0.0,
        hi: 1.0,
    };

    fn at(&self, f: f32) -> f32 {
        self.lo + (self.hi - self.lo) * f
    }
}

/// Reads one fpc output line: `Compiling x.pas` advances the unit count, `Linking` and the
/// like are fixed points near the end. `None` for every other line.
fn progress_for(line: &str, done: &mut usize, plan: &Plan) -> Option<Progress> {
    if let Some(unit) = line.strip_prefix("Compiling ") {
        *done += 1;
        let unit = unit.trim();
        let name = Path::new(unit)
            .file_name()
            .map_or(unit.to_string(), |n| n.to_string_lossy().into_owned());
        // Compiling fills 90% of the run; the rest is assembling, linking and signing.
        let fraction = plan
            .total
            .map(|t| plan.at(0.9 * ((*done - 1) as f32 / t.max(1) as f32).min(1.0)));
        return Some(Progress {
            label: format!("Compiling {name}"),
            fraction,
            done: *done,
        });
    }
    if !line.starts_with("Linking ") {
        return None;
    }
    let (label, f) = ("Linking", 0.94);
    Some(Progress {
        label: label.into(),
        fraction: Some(plan.at(f)),
        done: *done,
    })
}

/// Shown when no Free Pascal compiler can be found.
pub const NOT_FOUND_HINT: &str = "Free Pascal (fpc) was not found. Install it with `brew install fpc` \
(or from freepascal.org), then restart Pascoda.";

/// Finds the `fpc` executable: `$PASCODA_FPC`, then `$PATH`, then the usual install folders.
/// An app started from Finder has a minimal `PATH` without Homebrew, so the folders matter.
pub fn locate_fpc() -> Option<PathBuf> {
    find_fpc(
        std::env::var_os("PASCODA_FPC").map(PathBuf::from),
        std::env::var_os("PATH"),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn find_fpc(
    forced: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = forced {
        return p.is_file().then_some(p);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = path {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/local/lib/fpc/bin",
        ]
        .map(PathBuf::from),
    );
    if let Some(home) = home {
        dirs.push(home.join(".fpc/bin"));
    }
    dirs.into_iter()
        .map(|d| d.join("fpc"))
        .find(|p| p.is_file())
}

/// Compiles `source` with debug info. Runs fpc in the source's directory.
pub fn build(source: &Path, mode: Mode) -> std::io::Result<BuildResult> {
    build_with(source, mode, &[])
}

/// Like [`build`], reporting progress as fpc works. `total` is how many units the last build of
/// this program compiled, if known.
pub fn build_progress(
    source: &Path,
    mode: Mode,
    total: Option<usize>,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<BuildResult> {
    build_streaming(
        source,
        mode,
        &[],
        Plan {
            total,
            ..Plan::WHOLE
        },
        on,
    )
}

/// Like [`build`], with extra compiler arguments (search paths, defines, linker flags).
pub(crate) fn build_with(
    source: &Path,
    mode: Mode,
    extra: &[std::ffi::OsString],
) -> std::io::Result<BuildResult> {
    build_streaming(source, mode, extra, Plan::WHOLE, &mut |_| {})
}

pub(crate) fn build_streaming(
    source: &Path,
    mode: Mode,
    extra: &[std::ffi::OsString],
    plan: Plan,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<BuildResult> {
    build_streaming_as(source, mode, extra, plan, false, on)
}

/// `release`: optimised and stripped, no debug info, and not re-signed for the debugger. The
/// caller says where the program goes (`-FE`) through `extra`.
pub(crate) fn build_streaming_as(
    source: &Path,
    mode: Mode,
    extra: &[std::ffi::OsString],
    plan: Plan,
    release: bool,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<BuildResult> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    let dir = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file = source.file_name().unwrap_or(source.as_os_str());
    let fpc = locate_fpc()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, NOT_FOUND_HINT))?;
    let mut child = Command::new(fpc)
        .args(if release {
            &["-O2", "-Xs"][..]
        } else {
            &["-g"][..]
        })
        .arg(mode.flag())
        .arg("-vewnhi")
        .args(extra)
        .arg(file)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // stderr is drained on its own thread so a full pipe never stalls fpc.
    let mut stderr = child.stderr.take().expect("piped");
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stdout_text = String::new();
    let mut done = 0;
    on(Progress {
        label: "Starting compiler".into(),
        fraction: plan.total.map(|_| plan.at(0.0)),
        done,
    });
    for line in BufReader::new(child.stdout.take().expect("piped")).split(b'\n') {
        let line = String::from_utf8_lossy(&line?).into_owned();
        if let Some(p) = progress_for(&line, &mut done, &plan) {
            on(p);
        }
        stdout_text.push_str(&line);
        stdout_text.push('\n');
    }
    let status = child.wait()?;
    let stderr_text = err_reader.join().unwrap_or_default();
    let mut raw_output = without_linker_noise(&format!("{stdout_text}{stderr_text}"));
    if status.success() && !release {
        on(Progress {
            label: "Signing".into(),
            fraction: Some(plan.at(0.98)),
            done,
        });
        if let Err(e) = allow_debugging(&dir.join(Path::new(file).with_extension(""))) {
            raw_output.push_str(&format!("warning: could not sign for debugging: {e}\n"));
        }
    }
    Ok(BuildResult {
        success: status.success(),
        diagnostics: parse_output(&raw_output),
        raw_output,
    })
}

/// Apple's linker warns about two things the user cannot act on: fpc itself passes the renamed
/// `-macosx_version_min`, and GUI builds need the deprecated `-ld_classic` (see `lcl`). Both are
/// dropped from the output the IDE shows.
fn without_linker_noise(output: &str) -> String {
    output
        .split_inclusive('\n')
        .filter(|l| {
            !l.contains("-macosx_version_min has been renamed")
                && !l.contains("-ld_classic is deprecated")
        })
        .collect()
}

const GET_TASK_ALLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>
"#;

/// Re-signs `exe` ad hoc with `get-task-allow`. fpc binaries are only linker-signed, and
/// lldb cannot launch them ("attach failed") without this entitlement.
pub fn allow_debugging(exe: &Path) -> std::io::Result<()> {
    let plist = std::env::temp_dir().join(format!("pascoda-{}.entitlements", std::process::id()));
    std::fs::write(&plist, GET_TASK_ALLOW)?;
    let status = Command::new("codesign")
        .args(["-f", "-s", "-", "--entitlements"])
        .arg(&plist)
        .arg(exe)
        .output();
    let _ = std::fs::remove_file(&plist);
    let out = status?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_follows_compiling_and_linking_lines() {
        let plan = Plan {
            total: Some(4),
            lo: 0.0,
            hi: 1.0,
        };
        let mut done = 0;
        let p = progress_for("Compiling /a/b/unit1.pas", &mut done, &plan).unwrap();
        assert_eq!(
            (p.label.as_str(), p.fraction, p.done),
            ("Compiling unit1.pas", Some(0.0), 1)
        );
        let p = progress_for("Compiling main.lpr", &mut done, &plan).unwrap();
        assert!((p.fraction.unwrap() - 0.9 * 0.25).abs() < 1e-6);
        // More units than expected never push past the compile share of the bar.
        for _ in 0..10 {
            progress_for("Compiling x.pas", &mut done, &plan);
        }
        let p = progress_for("Compiling x.pas", &mut done, &plan).unwrap();
        assert!(p.fraction.unwrap() <= 0.9 + 1e-6);
        assert_eq!(
            progress_for("Linking hello", &mut done, &plan)
                .unwrap()
                .label,
            "Linking"
        );
        assert!(progress_for("Assembling hello", &mut done, &plan).is_none());
        assert!(progress_for("unit1.pas(3,1) Error: x", &mut done, &plan).is_none());
        // Unknown size: no fraction while compiling, a place on the bar once linking.
        let mut n = 0;
        let unknown = Plan::WHOLE;
        assert_eq!(
            progress_for("Compiling a.pas", &mut n, &unknown)
                .unwrap()
                .fraction,
            None
        );
        // A sub-range of a longer bar.
        let sub = Plan {
            total: Some(1),
            lo: 0.7,
            hi: 1.0,
        };
        let mut n = 0;
        assert!(
            (progress_for("Compiling a.pas", &mut n, &sub)
                .unwrap()
                .fraction
                .unwrap()
                - 0.7)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn known_linker_warnings_are_dropped() {
        let raw = "ld: warning: -ld_classic is deprecated and will be removed in a future release\n-macosx_version_min has been renamed to -macos_version_min\nreal.pas(3,1) Error: x\n";
        assert_eq!(without_linker_noise(raw), "real.pas(3,1) Error: x\n");
    }

    #[test]
    fn parses_error() {
        let d = parse_line(r#"bad.pas(4,3) Error: Identifier not found "Foo""#).unwrap();
        assert_eq!((d.line, d.col, d.severity), (4, 3, Severity::Error));
        assert_eq!(d.message, r#"Identifier not found "Foo""#);
    }

    #[test]
    fn built_binary_is_debuggable() {
        let src = Path::new("../../tests/fixtures/debug.pas");
        let built = build(src, Mode::default()).expect("fpc should start");
        assert!(built.success, "{}", built.raw_output);
        // lldb hangs on programs under ~/Documents, so debug a copy outside it.
        let copy = std::env::temp_dir().join("pascoda-debuggable");
        std::fs::copy("../../tests/fixtures/debug", &copy).unwrap();
        let out = Command::new("lldb")
            .args([
                "-b",
                "-o",
                "breakpoint set -f debug.pas -l 10",
                "-o",
                "run",
                "-o",
                "kill",
            ])
            .arg(&copy)
            .output()
            .expect("lldb should start");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("stop reason = breakpoint"), "{text}");
    }

    #[test]
    fn ignores_linker_noise() {
        assert!(parse_line("ld: warning: -multiply_defined is obsolete").is_none());
        assert!(parse_line("-macosx_version_min has been renamed to -macos_version_min").is_none());
        assert!(parse_line("Compiling hello.pas").is_none());
    }
}

#[cfg(test)]
mod locate_tests {
    use super::*;

    #[test]
    fn a_forced_path_wins_and_a_missing_one_means_not_found() {
        let tmp = std::env::temp_dir().join(format!("pascoda-fpc-find-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("fpc");
        std::fs::write(&fake, "").unwrap();
        assert_eq!(find_fpc(Some(fake.clone()), None, None), Some(fake.clone()));
        assert_eq!(find_fpc(Some(tmp.join("nope")), None, None), None);
        // searched through PATH next
        assert_eq!(
            find_fpc(None, Some(tmp.clone().into_os_string()), None),
            Some(fake)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
