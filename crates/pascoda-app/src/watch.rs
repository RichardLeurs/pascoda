//! Noticing that files changed on disk behind the IDE's back (Claude Code in the terminal, git,
//! another editor). Three pieces, all free of UI so they can be tested without a window:
//!
//! - [`ignored`]: which paths are never interesting (`.git`, build output, bundles, `.DS_Store`).
//! - [`Coalescer`]: turns a storm of raw events into one [`Batch`] after things go quiet.
//! - [`Watcher`]: the `notify` watcher (FSEvents on macOS) plus a thread that debounces and
//!   hands batches to the UI over a channel.
//!
//! [`decide`] is the policy for one open file: what to do when the disk may have changed.

use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Quiet time after the last event before a batch is delivered.
pub const QUIET: Duration = Duration::from_millis(200);
/// A storm that never goes quiet is still delivered this often.
pub const MAX_WAIT: Duration = Duration::from_millis(1000);
/// More distinct paths than this in one batch collapse into "look at everything".
pub const MAX_PATHS: usize = 512;

const IGNORED_DIRS: &[&str] = &[".git", "target", "lib", "vendor"];
const IGNORED_NAMES: &[&str] = &[".DS_Store", "link.res", "ppas.sh"];
const IGNORED_EXTENSIONS: &[&str] = &["o", "ppu", "a", "compiled"];

/// Whether a change at `rel` (relative to the watched folder) is never worth a look: version
/// control, build output, inside an `.app` bundle, Finder droppings.
pub fn ignored(rel: &Path) -> bool {
    let comps: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((last, dirs)) = comps.split_last() else {
        return false;
    };
    if dirs
        .iter()
        .any(|d| IGNORED_DIRS.contains(&d.as_str()) || d.to_ascii_lowercase().ends_with(".app"))
    {
        return true;
    }
    // The folder itself (`lib`, `Hello.app`) is as uninteresting as what is inside.
    if IGNORED_DIRS.contains(&last.as_str()) || last.to_ascii_lowercase().ends_with(".app") {
        return true;
    }
    if IGNORED_NAMES.contains(&last.as_str()) {
        return true;
    }
    Path::new(last)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Applies [`ignored`] to absolute paths, relative to whichever watched folder holds them.
#[derive(Clone, Default, Debug)]
pub struct Filter {
    bases: Vec<PathBuf>,
}

impl Filter {
    pub fn new(bases: Vec<PathBuf>) -> Self {
        Self { bases }
    }

    pub fn ignores(&self, path: &Path) -> bool {
        let rel = self
            .bases
            .iter()
            .filter(|b| path.starts_with(b))
            .max_by_key(|b| b.components().count())
            .and_then(|b| path.strip_prefix(b).ok());
        rel.is_some_and(ignored)
    }
}

/// What the UI receives: the paths that changed since the last batch.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Batch {
    pub paths: BTreeSet<PathBuf>,
    /// The system lost track (or there were too many paths): check everything that is open.
    pub rescan: bool,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.rescan
    }
}

/// Collects raw events and says when to deliver them: after `quiet` without a new event, or
/// `max_wait` after the first one, whichever comes first. Time is passed in, so tests need no
/// sleeping.
pub struct Coalescer {
    quiet: Duration,
    max_wait: Duration,
    pending: Batch,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Coalescer {
    pub fn new(quiet: Duration, max_wait: Duration) -> Self {
        Self {
            quiet,
            max_wait,
            pending: Batch::default(),
            first: None,
            last: None,
        }
    }

    /// One raw event; `None` means "rescan everything".
    pub fn push(&mut self, path: Option<PathBuf>, now: Instant) {
        match path {
            Some(p) if !self.pending.rescan => {
                self.pending.paths.insert(p);
                if self.pending.paths.len() > MAX_PATHS {
                    self.pending.paths.clear();
                    self.pending.rescan = true;
                }
            }
            Some(_) => {}
            None => {
                self.pending.paths.clear();
                self.pending.rescan = true;
            }
        }
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// When the pending batch is due, if there is one.
    pub fn deadline(&self) -> Option<Instant> {
        let (first, last) = (self.first?, self.last?);
        Some((last + self.quiet).min(first + self.max_wait))
    }

    pub fn take_if_due(&mut self, now: Instant) -> Option<Batch> {
        if self.deadline()? <= now {
            self.first = None;
            self.last = None;
            Some(std::mem::take(&mut self.pending))
        } else {
            None
        }
    }
}

enum Raw {
    Path(PathBuf),
    Rescan,
}

/// The `notify` watcher and its debouncing thread. Dropping it stops both.
pub struct Watcher {
    inner: notify::RecommendedWatcher,
    filter: Arc<Mutex<Filter>>,
    root: Option<PathBuf>,
    extra: BTreeSet<PathBuf>,
}

impl Watcher {
    /// `wake` is called (from the debouncing thread) whenever a batch is ready, so the UI can
    /// repaint.
    pub fn start(
        quiet: Duration,
        wake: impl Fn() + Send + 'static,
    ) -> notify::Result<(Self, Receiver<Batch>)> {
        let (raw_tx, raw_rx) = channel::<Raw>();
        let (out_tx, out_rx) = channel::<Batch>();
        let filter = Arc::new(Mutex::new(Filter::default()));
        let f = filter.clone();
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            send_raw(&raw_tx, &f, res);
        })?;
        std::thread::Builder::new()
            .name("pascoda-fs-watch".into())
            .spawn(move || debounce(raw_rx, out_tx, quiet, wake))
            .map_err(notify::Error::io)?;
        Ok((
            Self {
                inner,
                filter,
                root: None,
                extra: BTreeSet::new(),
            },
            out_rx,
        ))
    }

    /// Watches `root` recursively and `extra` folders (those of open files outside the root)
    /// without descending. Only the difference to the previous call is (un)registered.
    /// Returns what could not be watched.
    pub fn set_roots(&mut self, root: Option<&Path>, extra: &[PathBuf]) -> Vec<String> {
        let mut errors = Vec::new();
        let root = root.map(|r| r.canonicalize().unwrap_or_else(|_| r.to_path_buf()));
        let wanted: BTreeSet<PathBuf> = extra
            .iter()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
            .filter(|d| !root.as_ref().is_some_and(|r| d.starts_with(r)))
            .collect();
        if self.root != root {
            if let Some(old) = self.root.take() {
                let _ = self.inner.unwatch(&old);
            }
            if let Some(root) = root {
                match self.inner.watch(&root, RecursiveMode::Recursive) {
                    Ok(()) => self.root = Some(root),
                    Err(e) => errors.push(format!("{}: {e}", root.display())),
                }
            }
        }
        for gone in self.extra.difference(&wanted).cloned().collect::<Vec<_>>() {
            let _ = self.inner.unwatch(&gone);
            self.extra.remove(&gone);
        }
        for new in wanted.difference(&self.extra.clone()) {
            match self.inner.watch(new, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.extra.insert(new.clone());
                }
                Err(e) => errors.push(format!("{}: {e}", new.display())),
            }
        }
        let mut bases: Vec<PathBuf> = self.root.iter().cloned().collect();
        bases.extend(self.extra.iter().cloned());
        *self.filter.lock().unwrap() = Filter::new(bases);
        errors
    }

    /// The folders being watched: the root, then the extra ones.
    #[cfg(test)]
    pub fn watched(&self) -> Vec<PathBuf> {
        self.root.iter().chain(self.extra.iter()).cloned().collect()
    }
}

fn send_raw(tx: &Sender<Raw>, filter: &Mutex<Filter>, res: notify::Result<notify::Event>) {
    let Ok(event) = res else {
        let _ = tx.send(Raw::Rescan);
        return;
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    if event.need_rescan() {
        let _ = tx.send(Raw::Rescan);
    }
    let filter = filter.lock().unwrap().clone();
    for p in event.paths {
        if !filter.ignores(&p) {
            let _ = tx.send(Raw::Path(p));
        }
    }
}

fn debounce(raw: Receiver<Raw>, out: Sender<Batch>, quiet: Duration, wake: impl Fn()) {
    let mut c = Coalescer::new(quiet, MAX_WAIT.max(quiet));
    loop {
        let wait = c.deadline().map_or(Duration::from_secs(3600), |d| {
            d.saturating_duration_since(Instant::now())
        });
        match raw.recv_timeout(wait) {
            Ok(Raw::Path(p)) => c.push(Some(p), Instant::now()),
            Ok(Raw::Rescan) => c.push(None, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if let Some(batch) = c.take_if_due(Instant::now()) {
            if out.send(batch).is_err() {
                return;
            }
            wake();
        }
    }
}

// ---- policy ------------------------------------------------------------------------------

/// What reading a file from disk gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disk {
    Text(String),
    Missing,
    Unreadable(String),
}

/// Reads `path` as UTF-8 text; never panics.
pub fn read_disk(path: &Path) -> Disk {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(t) => Disk::Text(t),
            Err(_) => Disk::Unreadable("it is not valid UTF-8 text".into()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Disk::Missing,
        Err(e) => Disk::Unreadable(e.to_string()),
    }
}

/// A change the user already answered with "Keep mine".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The disk text that was seen and ignored.
    Text(String),
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do (an answered change, or an event that changed nothing we hold).
    Ignore,
    /// The disk has what the tab already saved (our own write, or a revert): drop any banner.
    InSync,
    /// No unsaved edits: take the disk text silently.
    Reload,
    /// Unsaved edits and a different disk text: ask.
    Banner,
    Deleted,
    Unreadable(String),
}

/// What to do for one open file, given its text, the text it last saved or loaded, and the
/// disk. Pure, so every case is a unit test.
pub fn decide(text: &str, saved: &str, disk: &Disk, kept: Option<&Kept>) -> Action {
    match disk {
        Disk::Text(d) => {
            if d == saved {
                Action::InSync
            } else if text != saved && kept == Some(&Kept::Text(d.clone())) {
                Action::Ignore
            } else if text == saved || d == text {
                Action::Reload
            } else {
                Action::Banner
            }
        }
        Disk::Missing if kept == Some(&Kept::Deleted) => Action::Ignore,
        Disk::Missing => Action::Deleted,
        Disk::Unreadable(why) => Action::Unreadable(why.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::App;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn ignore_rules() {
        for rel in [
            ".git/index",
            ".git",
            "target/debug/x",
            "lib/aarch64-darwin/unit1.o",
            "unit1.o",
            "sub/unit1.ppu",
            "Hello.app/Contents/Info.plist",
            "Hello.app",
            "sub/Hello.APP/Contents/MacOS/hello",
            ".DS_Store",
            "sub/.DS_Store",
            "link.res",
            "ppas.sh",
            "vendor/lazarus/x.pas",
        ] {
            assert!(ignored(Path::new(rel)), "{rel} should be ignored");
        }
        for rel in [
            "unit1.pas",
            "sub/unit1.lfm",
            "notes.lpr",
            ".gitignore",
            "library.pas",
            "app.pas",
            "targets.txt",
            "README.md",
        ] {
            assert!(!ignored(Path::new(rel)), "{rel} should be reported");
        }
    }

    #[test]
    fn filter_is_relative_to_the_watched_folder() {
        // A project that itself lives under a `target` folder is still watched.
        let f = Filter::new(vec![p("/x/target/tmp/proj"), p("/y/other")]);
        assert!(!f.ignores(&p("/x/target/tmp/proj/unit1.pas")));
        assert!(f.ignores(&p("/x/target/tmp/proj/.git/HEAD")));
        assert!(f.ignores(&p("/x/target/tmp/proj/lib/a.o")));
        assert!(!f.ignores(&p("/y/other/a.pas")));
        assert!(f.ignores(&p("/y/other/.DS_Store")));
        // Outside every base: reported, the app decides.
        assert!(!f.ignores(&p("/elsewhere/a.pas")));
    }

    #[test]
    fn a_storm_is_one_batch_after_things_go_quiet() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut c = Coalescer::new(ms(200), ms(1000));
        assert!(c.take_if_due(t0).is_none());
        for i in 0..200 {
            c.push(Some(p("/a/b.pas")), t0 + ms(i));
            c.push(Some(p(&format!("/a/f{}.pas", i % 10))), t0 + ms(i));
            assert!(c.take_if_due(t0 + ms(i)).is_none(), "still busy at {i}");
        }
        assert!(c.take_if_due(t0 + ms(300)).is_none());
        let batch = c.take_if_due(t0 + ms(400)).expect("quiet for 200 ms");
        assert_eq!(batch.paths.len(), 11);
        assert!(!batch.rescan);
        assert!(c.take_if_due(t0 + ms(5000)).is_none(), "delivered once");
    }

    #[test]
    fn a_storm_that_never_stops_is_still_delivered() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut c = Coalescer::new(ms(200), ms(1000));
        for i in 0..2000 {
            c.push(Some(p("/a/b.pas")), t0 + ms(i * 10));
        }
        let due = c.take_if_due(t0 + ms(1000)).expect("max wait reached");
        assert_eq!(due.paths.len(), 1);
    }

    #[test]
    fn too_many_paths_collapse_into_a_rescan() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(QUIET, MAX_WAIT);
        for i in 0..(MAX_PATHS + 10) {
            c.push(Some(p(&format!("/a/{i}.pas"))), t0);
        }
        c.push(Some(p("/a/late.pas")), t0);
        let b = c.take_if_due(t0 + Duration::from_secs(5)).unwrap();
        assert!(b.rescan && b.paths.is_empty());
        let mut c = Coalescer::new(QUIET, MAX_WAIT);
        c.push(Some(p("/a/x")), t0);
        c.push(None, t0);
        let b = c.take_if_due(t0 + Duration::from_secs(5)).unwrap();
        assert!(b.rescan && b.paths.is_empty());
    }

    #[test]
    fn decisions() {
        use Action::*;
        let t = |s: &str| Disk::Text(s.into());
        // Same as what we saved: our own write, or nothing.
        assert_eq!(decide("a", "a", &t("a"), None), InSync);
        assert_eq!(decide("edited", "a", &t("a"), None), InSync);
        // Clean tab: take the disk.
        assert_eq!(decide("a", "a", &t("b"), None), Reload);
        // Dirty tab, different disk: ask.
        assert_eq!(decide("edited", "a", &t("b"), None), Banner);
        // Dirty tab whose text happens to equal the disk: nothing to ask.
        assert_eq!(decide("b", "a", &t("b"), None), Reload);
        // Keep mine: the same disk text is not asked again, a newer one is.
        let kept = Kept::Text("b".into());
        assert_eq!(decide("edited", "a", &t("b"), Some(&kept)), Ignore);
        assert_eq!(decide("edited", "a", &t("c"), Some(&kept)), Banner);
        // Once the tab is clean again the disk wins even if it was kept.
        assert_eq!(decide("a", "a", &t("b"), Some(&kept)), Reload);
        // Gone or unreadable.
        assert_eq!(decide("a", "a", &Disk::Missing, None), Deleted);
        assert_eq!(decide("x", "a", &Disk::Missing, None), Deleted);
        assert_eq!(
            decide("x", "a", &Disk::Missing, Some(&Kept::Deleted)),
            Ignore
        );
        assert_eq!(
            decide("a", "a", &Disk::Unreadable("denied".into()), None),
            Unreadable("denied".into())
        );
    }

    #[test]
    fn reading_never_panics() {
        let dir = crate::target_tmp().join("watch-read");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_disk(&dir.join("nope")), Disk::Missing);
        std::fs::write(dir.join("bin"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert!(matches!(read_disk(&dir.join("bin")), Disk::Unreadable(_)));
        std::fs::write(dir.join("ok"), "héllo").unwrap();
        assert_eq!(read_disk(&dir.join("ok")), Disk::Text("héllo".into()));
        // A directory is not text either.
        assert!(matches!(read_disk(&dir), Disk::Unreadable(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- real watcher (FSEvents) -------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::target_tmp().join(format!("watch-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn start() -> (Watcher, Receiver<Batch>) {
        Watcher::start(Duration::from_millis(150), || {}).unwrap()
    }

    /// Collects batches until `done(all paths so far)` or the timeout; returns them all.
    fn collect_until(
        rx: &Receiver<Batch>,
        timeout: Duration,
        done: impl Fn(&BTreeSet<PathBuf>, bool) -> bool,
    ) -> Vec<Batch> {
        let end = Instant::now() + timeout;
        let mut all = Vec::new();
        let mut seen = BTreeSet::new();
        let mut rescan = false;
        while Instant::now() < end {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(50)) {
                seen.extend(b.paths.iter().cloned());
                rescan |= b.rescan;
                all.push(b);
                if done(&seen, rescan) {
                    break;
                }
            }
        }
        all
    }

    fn sees(rx: &Receiver<Batch>, path: &Path) -> bool {
        let p = path.to_path_buf();
        let got = collect_until(rx, Duration::from_secs(8), |s, r| r || s.contains(&p));
        got.iter().any(|b| b.rescan || b.paths.contains(&p))
    }

    /// FSEvents needs a moment after `watch` before it reports reliably.
    fn settle(rx: &Receiver<Batch>) {
        std::thread::sleep(Duration::from_millis(400));
        while rx.try_recv().is_ok() {}
    }

    #[test]
    fn real_events_for_create_modify_rename_and_delete() {
        let dir = scratch("basic");
        let (mut w, rx) = start();
        assert!(w.set_roots(Some(&dir), &[]).is_empty());
        settle(&rx);

        let f = dir.join("unit1.pas");
        std::fs::write(&f, "one").unwrap();
        assert!(sees(&rx, &f), "create");
        std::fs::write(&f, "two").unwrap();
        assert!(sees(&rx, &f), "modify");
        // Atomic save: write a temp file and rename it over the target.
        let tmp = dir.join(".unit1.pas.tmp");
        std::fs::write(&tmp, "three").unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        assert!(sees(&rx, &f), "atomic rename-save");
        assert_eq!(read_disk(&f), Disk::Text("three".into()));
        std::fs::remove_file(&f).unwrap();
        assert!(sees(&rx, &f), "delete");
        assert_eq!(read_disk(&f), Disk::Missing);
        // Inside a new sub folder.
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let g = dir.join("sub/deep.pas");
        std::fs::write(&g, "x").unwrap();
        assert!(sees(&rx, &g), "recursive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_burst_of_writes_is_a_few_batches() {
        let dir = scratch("burst");
        let (mut w, rx) = start();
        w.set_roots(Some(&dir), &[]);
        settle(&rx);
        let f = dir.join("storm.pas");
        let started = Instant::now();
        for i in 0..200 {
            std::fs::write(&f, format!("write {i}")).unwrap();
            std::fs::write(dir.join(format!("f{}.pas", i % 20)), "x").unwrap();
        }
        let wrote_for = started.elapsed();
        let got = collect_until(&rx, Duration::from_secs(8), |s, r| r || s.contains(&f));
        // Let it go quiet, then count what else arrives.
        std::thread::sleep(Duration::from_millis(600));
        let late: Vec<Batch> = rx.try_iter().collect();
        let batches = got.len() + late.len();
        assert!(batches >= 1);
        assert!(
            batches <= 2 + (wrote_for.as_millis() as usize / 150),
            "{batches} batches for 400 writes in {wrote_for:?}"
        );
        assert!(
            got.iter()
                .chain(&late)
                .any(|b| b.rescan || b.paths.contains(&f)),
            "the storm's file is in a batch"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignored_folders_are_not_delivered() {
        let dir = scratch("ignored");
        for d in [".git", "lib", "target", "Hello.app/Contents"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let (mut w, rx) = start();
        w.set_roots(Some(&dir), &[]);
        settle(&rx);
        let noise = [
            dir.join(".git/index"),
            dir.join("lib/unit1.o"),
            dir.join("lib/unit1.ppu"),
            dir.join("target/x.pas"),
            dir.join("Hello.app/Contents/Info.plist"),
            dir.join(".DS_Store"),
            dir.join("unit1.o"),
        ];
        for n in &noise {
            std::fs::write(n, "x").unwrap();
        }
        let sentinel = dir.join("sentinel.pas");
        std::fs::write(&sentinel, "x").unwrap();
        let got = collect_until(&rx, Duration::from_secs(8), |s, r| {
            r || s.contains(&sentinel)
        });
        std::thread::sleep(Duration::from_millis(500));
        let all: Vec<Batch> = got.into_iter().chain(rx.try_iter()).collect();
        assert!(
            all.iter().any(|b| b.paths.contains(&sentinel)),
            "sentinel arrived"
        );
        for b in &all {
            for n in &noise {
                assert!(!b.paths.contains(n), "{} was delivered", n.display());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn re_pointing_follows_the_root_and_extra_folders() {
        let a = scratch("repoint-a");
        let b = scratch("repoint-b");
        let outside = scratch("repoint-out");
        let (mut w, rx) = start();
        assert_eq!(w.set_roots(Some(&a), &[]), Vec::<String>::new());
        assert_eq!(w.watched(), vec![a.clone()]);
        settle(&rx);
        let fa = a.join("a.pas");
        std::fs::write(&fa, "x").unwrap();
        assert!(sees(&rx, &fa));

        // Move to b and also watch a folder outside it (an open file lives there).
        w.set_roots(Some(&b), &[outside.clone(), b.join("inside-root")]);
        assert_eq!(w.watched(), vec![b.clone(), outside.clone()]);
        settle(&rx);
        std::fs::write(&fa, "changed").unwrap();
        let fb = b.join("b.pas");
        std::fs::write(&fb, "x").unwrap();
        let fo = outside.join("o.pas");
        std::fs::write(&fo, "x").unwrap();
        let got = collect_until(&rx, Duration::from_secs(8), |s, _| {
            s.contains(&fb) && s.contains(&fo)
        });
        std::thread::sleep(Duration::from_millis(400));
        let all: Vec<Batch> = got.into_iter().chain(rx.try_iter()).collect();
        assert!(all.iter().any(|x| x.paths.contains(&fb)), "new root");
        assert!(all.iter().any(|x| x.paths.contains(&fo)), "extra folder");
        assert!(
            !all.iter().any(|x| x.paths.contains(&fa)),
            "the old root is not watched any more"
        );
        // Extra folders are not recursive and are dropped when no longer wanted.
        w.set_roots(Some(&b), &[]);
        assert_eq!(w.watched(), vec![b.clone()]);
        settle(&rx);
        std::fs::write(&fo, "again").unwrap();
        std::fs::write(&fb, "again").unwrap();
        let got = collect_until(&rx, Duration::from_secs(8), |s, _| s.contains(&fb));
        std::thread::sleep(Duration::from_millis(400));
        let all: Vec<Batch> = got.into_iter().chain(rx.try_iter()).collect();
        assert!(!all.iter().any(|x| x.paths.contains(&fo)));
        for d in [a, b, outside] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn the_app_reloads_a_tab_changed_by_another_program() {
        let dir = crate::external::tests::project("realwatch");
        let mut app = App::bare(Some(dir.join("unit1.pas")));
        let (mut w, rx) = start();
        w.set_roots(Some(&app.root), &[]);
        settle(&rx);
        let path = app.docs[0].path.clone();
        std::fs::write(&path, "unit changed_by_cli;\n").unwrap();
        let got = collect_until(&rx, Duration::from_secs(8), |s, r| r || s.contains(&path));
        let ctx = eframe::egui::Context::default();
        for b in got {
            app.handle_external(Some(&ctx), &b);
        }
        assert_eq!(app.docs[0].text, "unit changed_by_cli;\n");
        // Our own save comes back as an event and changes nothing.
        app.docs[0].text.push_str("// mine\n");
        app.save_doc(0);
        let mine = app.docs[0].text.clone();
        let got = collect_until(&rx, Duration::from_secs(8), |s, r| r || s.contains(&path));
        for b in got {
            app.handle_external(Some(&ctx), &b);
        }
        assert_eq!(app.docs[0].text, mine);
        assert!(app.external.banners.is_empty());
    }
}
