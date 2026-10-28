//! Syntax and semantic check of one file's *current text* (not yet saved), without linking.
//! The editor uses it for squiggles while typing.

use crate::{Diagnostic, Mode, Severity, locate_fpc, parse_output};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A check that runs longer than this is given up (a huge unit chain compiling from source).
const TIMEOUT: Duration = Duration::from_secs(20);

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether `path` is a file the compiler can be asked to check on its own.
pub fn checkable(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("pas" | "pp" | "lpr" | "dpr")
    )
}

/// Compiles `text` as if it were the file `source` and returns what fpc reports about `source`
/// (other files' problems are dropped). The text is written to a scratch folder, so nothing in
/// the project changes; units it uses are read from `source`'s folder (and its `lib/`).
/// `lcl` adds the LCL's search paths. Needs fpc; `Err(NotFound)` without it.
pub fn check_text(
    source: &Path,
    text: &str,
    mode: Mode,
    lcl: Option<&crate::lcl::Lcl>,
) -> std::io::Result<Vec<Diagnostic>> {
    let fpc = locate_fpc()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, crate::NOT_FOUND_HINT))?;
    let name = source
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?;
    let dir = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = dir.canonicalize()?;
    let work = std::env::temp_dir().join(format!(
        "pascoda-check-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&work)?;
    let result = run(&fpc, &work, &dir, name, text, mode, lcl);
    let _ = std::fs::remove_dir_all(&work);
    let mut diags = result?;
    let wanted = dir.join(name);
    diags.retain(|d| same_file(&d.file, name, &work, &wanted));
    for d in &mut diags {
        d.file = wanted.clone();
    }
    Ok(diags)
}

/// fpc names the checked file by the bare name we passed; other files by their search path.
fn same_file(file: &Path, name: &std::ffi::OsStr, work: &Path, wanted: &Path) -> bool {
    file == Path::new(name) || file == work.join(name) || file == wanted
}

fn run(
    fpc: &Path,
    work: &Path,
    dir: &Path,
    name: &std::ffi::OsStr,
    text: &str,
    mode: Mode,
    lcl: Option<&crate::lcl::Lcl>,
) -> std::io::Result<Vec<Diagnostic>> {
    std::fs::write(work.join(name), text)?;
    let mut args: Vec<OsString> = vec![
        "-s".into(),
        mode.flag().into(),
        "-vewnhi".into(),
        format!("-FE{}", work.display()).into(),
        format!("-FU{}", work.display()).into(),
        format!("-Fi{}", dir.display()).into(),
        format!("-Fo{}", dir.display()).into(),
    ];
    if let Some(lcl) = lcl {
        args.extend(lcl.project_args(work));
    }
    let lib = dir.join("lib");
    if lib.is_dir() {
        args.push(format!("-Fu{}", lib.display()).into());
    }
    args.push(format!("-Fu{}", dir.display()).into());
    let mut child = Command::new(fpc)
        .args(&args)
        .arg(name)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Both pipes are drained on their own threads so a full pipe never stalls fpc.
    let drain = |mut r: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            s.push_str(&String::from_utf8_lossy(&b));
            s
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("piped")));
    let err = drain(Box::new(child.stderr.take().expect("piped")));
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the syntax check took too long",
            ));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let text = format!(
        "{}{}",
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default()
    );
    Ok(parse_output(&text))
}

/// The diagnostics worth a squiggle: what the editor underlines.
pub fn squiggles(diags: &[Diagnostic]) -> impl Iterator<Item = &Diagnostic> {
    diags.iter().filter(|d| {
        matches!(
            d.severity,
            Severity::Fatal | Severity::Error | Severity::Warning
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn only_pascal_sources_are_checkable() {
        assert!(checkable(Path::new("a/unit1.pas")));
        assert!(checkable(Path::new("a/Prog.LPR")));
        assert!(!checkable(Path::new("a/unit1.lfm")));
        assert!(!checkable(Path::new("a/readme.md")));
    }

    #[test]
    fn unsaved_text_is_checked_and_the_project_is_untouched() {
        if locate_fpc().is_none() {
            eprintln!("skipped: no fpc");
            return;
        }
        // A private copy: other tests build in the shared fixtures folder at the same time.
        let proj = std::env::temp_dir().join(format!("pascoda-check-test-{}", std::process::id()));
        std::fs::create_dir_all(&proj).unwrap();
        let src = proj.join("hello.pas");
        std::fs::copy(fixture("hello.pas"), &src).unwrap();
        let before: Vec<_> = std::fs::read_dir(src.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let good = std::fs::read_to_string(&src).unwrap();
        let none = check_text(&src, &good, Mode::default(), None).unwrap();
        assert!(squiggles(&none).next().is_none(), "{none:?}");
        let bad = good.replace("WriteLn", "WriteLnn");
        let d = check_text(&src, &bad, Mode::default(), None).unwrap();
        let e = squiggles(&d).next().expect("an error");
        assert_eq!(e.severity, Severity::Error);
        assert!(e.message.contains("WriteLnn"), "{e:?}");
        assert_eq!(
            e.file,
            src.parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .join("hello.pas")
        );
        let after: Vec<_> = std::fs::read_dir(src.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            before.len(),
            after.len(),
            "no scratch files left in the project"
        );
    }

    #[test]
    fn a_missing_unit_in_another_file_is_not_blamed_on_this_one() {
        if locate_fpc().is_none() {
            return;
        }
        let src = fixture("hello.pas");
        let text = "program hello;\nuses NoSuchUnitXyz;\nbegin end.\n";
        let d = check_text(&src, text, Mode::default(), None).unwrap();
        let e = squiggles(&d).next().expect("fatal for the unit");
        assert!(e.message.contains("NoSuchUnitXyz"), "{e:?}");
        assert_eq!(e.line, 2);
    }
}
