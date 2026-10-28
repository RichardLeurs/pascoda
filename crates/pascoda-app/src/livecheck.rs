//! Squiggles while typing: after a short pause, the active Pascal file's current text goes to
//! `fpc` (syntax and semantic check, no linking) on a worker thread. `pasls` publishes no
//! diagnostics (see CLAUDE.md), so this is where live errors come from.

use crate::hints::{FileMarks, Mark};
use pascoda_fpc::Diagnostic;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// How long the text must stay unchanged before it is checked.
pub const QUIET: Duration = Duration::from_millis(700);

pub struct Done {
    pub path: PathBuf,
    pub text: String,
    pub result: std::io::Result<Vec<Diagnostic>>,
}

#[derive(Default)]
pub struct LiveCheck {
    /// Off in tests and in the offscreen renders.
    pub enabled: bool,
    seen: Option<((PathBuf, u64), Instant)>,
    checked: Option<(PathBuf, u64)>,
    running: Option<Receiver<Done>>,
}

fn key(path: &Path, text: &str) -> (PathBuf, u64) {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    (path.to_path_buf(), h.finish())
}

impl LiveCheck {
    /// Forget what was checked: the next pause checks the current text again (settings changed).
    pub fn forget(&mut self) {
        self.checked = None;
        self.seen = None;
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// True when `text` of `path` has been still for [`QUIET`] and was not checked yet. The
    /// caller then calls [`LiveCheck::start`].
    pub fn due(&mut self, path: &Path, text: &str, now: Instant) -> bool {
        let k = key(path, text);
        if self.checked.as_ref() == Some(&k) {
            return false;
        }
        match &self.seen {
            Some((seen, since)) if *seen == k => now.duration_since(*since) >= QUIET,
            _ => {
                self.seen = Some((k, now));
                false
            }
        }
    }

    /// Checks `text` in the background. `lcl` is whether the LCL is available and built.
    pub fn start(
        &mut self,
        ctx: &eframe::egui::Context,
        path: PathBuf,
        text: String,
        mode: pascoda_fpc::Mode,
    ) {
        self.checked = Some(key(&path, &text));
        let (tx, rx) = channel();
        self.running = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let lcl = pascoda_fpc::lcl::Lcl::find(&path).filter(|l| !l.needs_build());
            let mut result = pascoda_fpc::check::check_text(&path, &text, mode, lcl.as_ref());
            if lcl.is_none()
                && let Ok(d) = &mut result
            {
                // Without a built LCL every GUI unit would be flagged; not the user's fault.
                d.retain(|d| !d.message.contains("Can't find unit"));
            }
            let _ = tx.send(Done { path, text, result });
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self) -> Option<Done> {
        let done = self.running.as_ref()?.try_recv();
        match done {
            Ok(d) => {
                self.running = None;
                Some(d)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(_) => {
                self.running = None;
                None
            }
        }
    }
}

/// The marks for `path` (valid for `text`) from check diagnostics; `None` when there are none.
pub fn marks(path: &Path, text: &str, diags: &[Diagnostic]) -> Option<FileMarks> {
    let items: Vec<Mark> = pascoda_fpc::check::squiggles(diags)
        .map(|d| Mark {
            line: d.line.saturating_sub(1),
            col: d.col.saturating_sub(1),
            severity: d.severity,
            message: d.message.clone(),
        })
        .collect();
    (!items.is_empty()).then(|| FileMarks {
        path: path.to_path_buf(),
        text: text.to_string(),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pascoda_fpc::Severity;

    #[test]
    fn waits_for_a_pause_and_checks_each_text_once() {
        let mut l = LiveCheck::default();
        let p = Path::new("a.pas");
        let t0 = Instant::now();
        assert!(!l.due(p, "x", t0), "first sight only starts the clock");
        assert!(!l.due(p, "x", t0 + QUIET / 2));
        assert!(!l.due(p, "xy", t0 + QUIET), "typing restarts it");
        assert!(!l.due(p, "xy", t0 + QUIET + QUIET / 2));
        assert!(l.due(p, "xy", t0 + QUIET * 2));
        l.checked = Some(key(p, "xy"));
        assert!(!l.due(p, "xy", t0 + QUIET * 5), "already checked");
        assert!(
            !l.due(Path::new("b.pas"), "xy", t0 + QUIET * 6),
            "other file restarts"
        );
    }

    #[test]
    fn diagnostics_become_zero_based_marks_without_hints() {
        let d = |line, severity| Diagnostic {
            file: "a.pas".into(),
            line,
            col: 5,
            severity,
            message: "m".into(),
        };
        let m = marks(
            Path::new("a.pas"),
            "t",
            &[
                d(3, Severity::Error),
                d(4, Severity::Hint),
                d(5, Severity::Warning),
            ],
        )
        .unwrap();
        assert_eq!(m.items.len(), 2);
        assert_eq!((m.items[0].line, m.items[0].col), (2, 4));
        assert!(marks(Path::new("a.pas"), "t", &[d(1, Severity::Note)]).is_none());
    }
}
