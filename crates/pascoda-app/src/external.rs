//! The app's side of external-change detection: owns the watcher, applies `watch::decide` to
//! open tabs and forms, and draws the banner that asks what to do with unsaved edits.

use super::*;
use crate::style::Palette;
use crate::watch::{self, Action, Batch, Disk, Kept};
use egui::{Frame, Margin, RichText};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// What an external change concerns: the text tab of a file, or the form designer's copy of an
/// `.lfm` (a form can be open in both).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Target {
    Tab,
    Form,
}

pub(crate) type Key = (PathBuf, Target);

/// Something the user has to decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Banner {
    /// The disk text differs and the tab has unsaved edits.
    Changed,
    Deleted,
    /// The file could not be read (or, for a form, parsed); the text is why.
    Unreadable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// Discard my edits, load the disk.
    Reload,
    /// Keep my edits; the next save overwrites the disk on purpose.
    Keep,
    /// The file is gone and the tab goes with it.
    Close,
}

/// How long the status-bar note stays, the last second of it fading.
const NOTICE_FOR: Duration = Duration::from_secs(5);

/// Watcher plus everything the app remembers about external changes.
#[derive(Default)]
pub(crate) struct External {
    pub watcher: Option<watch::Watcher>,
    pub rx: Option<std::sync::mpsc::Receiver<Batch>>,
    pub banners: BTreeMap<Key, Banner>,
    pub kept: HashMap<Key, Kept>,
    pub notice: Option<(String, Instant)>,
    /// Counts batches that touched the file system; the file tree lists fresh on every frame
    /// and only needs a repaint, this makes the refresh observable.
    pub tree_rev: u64,
    /// The folders last handed to the watcher.
    watching: Option<(Option<PathBuf>, Vec<PathBuf>)>,
}

impl App {
    /// Starts the watcher (not in `bare`: tests have no file system events to wait for).
    pub(crate) fn start_watching(&mut self, ctx: &egui::Context) {
        let repaint = ctx.clone();
        match watch::Watcher::start(watch::QUIET, move || repaint.request_repaint()) {
            Ok((w, rx)) => {
                self.external.watcher = Some(w);
                self.external.rx = Some(rx);
            }
            Err(e) => self
                .console
                .push_str(&format!("file watcher unavailable: {e}\n")),
        }
    }

    /// Follows the project folder and the folders of open files that live outside it.
    fn sync_watch(&mut self) {
        let Some(w) = &mut self.external.watcher else {
            return;
        };
        // Never the whole home folder or `/` (an app opened without a project has `.`).
        let root = lsp_worthy(&self.root).then(|| self.root.clone());
        let top = root.clone().unwrap_or_default();
        let mut extra: Vec<PathBuf> = self
            .docs
            .iter()
            .filter_map(|d| d.path.parent().map(Path::to_path_buf))
            .filter(|d| root.is_none() || !d.starts_with(&top))
            .collect();
        extra.sort();
        extra.dedup();
        let want = (root, extra);
        if self.external.watching.as_ref() == Some(&want) {
            return;
        }
        let errors = w.set_roots(want.0.as_deref(), &want.1);
        for e in errors {
            self.console.push_str(&format!("cannot watch {e}\n"));
        }
        self.external.watching = Some(want);
    }

    /// Once per frame: re-point the watcher, take finished batches, drop answered banners.
    pub(crate) fn poll_external(&mut self, ctx: &egui::Context) {
        self.sync_watch();
        let batches: Vec<Batch> = self
            .external
            .rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for b in batches {
            self.handle_external(Some(ctx), &b);
        }
        self.prune_banners();
        if let Some((_, at)) = &self.external.notice {
            if at.elapsed() >= NOTICE_FOR {
                self.external.notice = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn notice_clear(&mut self) {
        self.external.notice = None;
    }

    pub(crate) fn note(&mut self, msg: impl Into<String>) {
        self.external.notice = Some((msg.into(), Instant::now()));
    }

    /// The status-bar note and how opaque it is (1.0 until the last second).
    pub(crate) fn notice(&self) -> Option<(&str, f32)> {
        let (msg, at) = self.external.notice.as_ref()?;
        let left = NOTICE_FOR.checked_sub(at.elapsed())?;
        Some((msg, left.as_secs_f32().min(1.0)))
    }

    /// Looks at everything in `batch` that is open. Called with the debounced batches, and by
    /// tests with a fake event source.
    pub(crate) fn handle_external(&mut self, ctx: Option<&egui::Context>, batch: &Batch) {
        if batch.is_empty() {
            return;
        }
        self.external.tree_rev += 1;
        let hit = |p: &Path| batch.rescan || batch.paths.contains(p);
        for i in 0..self.docs.len() {
            if hit(&self.docs[i].path) {
                self.check_tab(i, ctx);
            }
        }
        for i in 0..self.designers.len() {
            if hit(&self.designers[i].lfm_path) {
                self.check_form(i, ctx);
            }
        }
    }

    fn file_name(path: &Path) -> String {
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    fn check_tab(&mut self, i: usize, ctx: Option<&egui::Context>) {
        let path = self.docs[i].path.clone();
        let key = (path.clone(), Target::Tab);
        let disk = watch::read_disk(&path);
        let doc = &self.docs[i];
        let action = watch::decide(&doc.text, &doc.saved, &disk, self.external.kept.get(&key));
        match (action, disk) {
            (Action::Reload, Disk::Text(text)) => {
                let changed = text != self.docs[i].text;
                crate::assistant::set_doc_text(&mut self.docs[i], text, true, ctx);
                self.clear_answer(&key);
                if changed {
                    self.note(format!(
                        "{} reloaded: changed on disk",
                        Self::file_name(&path)
                    ));
                }
            }
            (action, _) => self.apply_non_reload(key, action),
        }
    }

    fn check_form(&mut self, i: usize, ctx: Option<&egui::Context>) {
        let (path, key) = {
            let f = &self.designers[i];
            (f.lfm_path.clone(), (f.lfm_path.clone(), Target::Form))
        };
        let disk = watch::read_disk(&path);
        let f = &self.designers[i];
        let action = watch::decide(
            f.doc.source(),
            f.saved_source(),
            &disk,
            self.external.kept.get(&key),
        );
        match (action, disk) {
            (Action::Reload, Disk::Text(text)) => {
                let f = &mut self.designers[i];
                let changed = text != f.doc.source();
                let res = if changed {
                    f.replace_source(&text)
                } else {
                    Ok(())
                };
                match res {
                    Ok(()) => {
                        f.mark_saved();
                        self.clear_answer(&key);
                        if changed {
                            self.note(format!(
                                "{} reloaded: changed on disk",
                                Self::file_name(&path)
                            ));
                        }
                    }
                    Err(e) => {
                        self.external.banners.insert(
                            key,
                            Banner::Unreadable(format!("cannot load the form: {e}")),
                        );
                    }
                }
            }
            (action, _) => self.apply_non_reload(key, action),
        }
        let _ = ctx;
    }

    fn clear_answer(&mut self, key: &Key) {
        self.external.banners.remove(key);
        self.external.kept.remove(key);
    }

    fn apply_non_reload(&mut self, key: Key, action: Action) {
        match action {
            Action::Ignore | Action::Reload => {}
            Action::InSync => self.clear_answer(&key),
            Action::Banner => {
                self.external.banners.insert(key, Banner::Changed);
            }
            Action::Deleted => {
                self.external.banners.insert(key, Banner::Deleted);
            }
            Action::Unreadable(why) => {
                self.external.banners.insert(key, Banner::Unreadable(why));
            }
        }
    }

    /// Drops banners that no longer have anything to ask: the tab or form is gone, or a
    /// "changed" banner whose tab has no unsaved edits any more (saved, reverted, or reloaded
    /// by the assistant).
    fn prune_banners(&mut self) {
        let docs = &self.docs;
        let forms = &self.designers;
        let stale = |key: &Key, banner: &Banner| match key.1 {
            Target::Tab => docs
                .iter()
                .find(|d| d.path == key.0)
                .is_none_or(|d| *banner == Banner::Changed && !d.dirty()),
            Target::Form => forms
                .iter()
                .find(|f| f.lfm_path == key.0)
                .is_none_or(|f| *banner == Banner::Changed && !f.dirty()),
        };
        self.external.banners.retain(|k, b| !stale(k, b));
        let (docs, forms) = (&self.docs, &self.designers);
        self.external.kept.retain(|k, _| match k.1 {
            Target::Tab => docs.iter().any(|d| d.path == k.0),
            Target::Form => forms.iter().any(|f| f.lfm_path == k.0),
        });
    }

    /// The user saved: whatever was said about the disk is settled.
    pub(crate) fn forget_external(&mut self, path: &Path) {
        for target in [Target::Tab, Target::Form] {
            self.clear_answer(&(path.to_path_buf(), target));
        }
        if let Some(f) = self.designers.iter().find(|f| f.pas_path == path) {
            let key = (f.lfm_path.clone(), Target::Form);
            self.clear_answer(&key);
        }
    }

    /// The banners that concern the active tab: its own and its form's.
    pub(crate) fn active_banners(&self) -> Vec<(Key, Banner, String)> {
        let Some(doc) = self.docs.get(self.active) else {
            return Vec::new();
        };
        let mut keys = vec![(doc.path.clone(), Target::Tab)];
        if let Some(f) = self.designers.iter().find(|f| f.pas_path == doc.path) {
            keys.push((f.lfm_path.clone(), Target::Form));
        }
        keys.into_iter()
            .filter_map(|k| {
                let b = self.external.banners.get(&k)?.clone();
                let name = Self::file_name(&k.0);
                Some((k, b, name))
            })
            .collect()
    }

    /// Carries out the user's answer to a banner.
    pub(crate) fn resolve_external(
        &mut self,
        ctx: Option<&egui::Context>,
        key: Key,
        what: Resolution,
    ) {
        let disk = watch::read_disk(&key.0);
        match (what, key.1) {
            (Resolution::Reload, Target::Tab) => {
                let Disk::Text(text) = disk else {
                    return self.apply_disk_problem(key, disk);
                };
                if let Some(doc) = self.docs.iter_mut().find(|d| d.path == key.0) {
                    crate::assistant::set_doc_text(doc, text, true, ctx);
                }
                self.clear_answer(&key);
            }
            (Resolution::Reload, Target::Form) => {
                let Disk::Text(text) = disk else {
                    return self.apply_disk_problem(key, disk);
                };
                let Some(f) = self.designers.iter_mut().find(|f| f.lfm_path == key.0) else {
                    return;
                };
                match f.replace_source(&text) {
                    Ok(()) => {
                        f.mark_saved();
                        self.clear_answer(&key);
                    }
                    Err(e) => {
                        let why = format!("cannot load the form: {e}");
                        self.external.banners.insert(key, Banner::Unreadable(why));
                    }
                }
            }
            (Resolution::Keep, target) => {
                match disk {
                    Disk::Text(t) => {
                        self.external.kept.insert(key.clone(), Kept::Text(t));
                    }
                    Disk::Missing => {
                        self.external.kept.insert(key.clone(), Kept::Deleted);
                        // The text is all that is left of the file: count it as unsaved.
                        match target {
                            Target::Tab => {
                                if let Some(d) = self.docs.iter_mut().find(|d| d.path == key.0) {
                                    d.saved.clear();
                                }
                            }
                            Target::Form => {
                                if let Some(f) =
                                    self.designers.iter_mut().find(|f| f.lfm_path == key.0)
                                {
                                    f.mark_unsaved();
                                }
                            }
                        }
                    }
                    Disk::Unreadable(_) => {}
                }
                self.external.banners.remove(&key);
            }
            (Resolution::Close, Target::Tab) => {
                self.designers
                    .retain(|f| f.pas_path != key.0 && f.lfm_path != key.0);
                if let Some(i) = self.docs.iter().position(|d| d.path == key.0) {
                    self.docs.remove(i);
                    self.active = self.active.min(self.docs.len().saturating_sub(1));
                    if !self.active_is_unit_with_form() {
                        self.form_view = false;
                    }
                }
                self.clear_answer(&key);
            }
            (Resolution::Close, Target::Form) => {
                self.designers.retain(|f| f.lfm_path != key.0);
                self.form_view = false;
                self.clear_answer(&key);
            }
        }
    }

    /// "Reload" was clicked but the disk can not give text now: say so instead.
    fn apply_disk_problem(&mut self, key: Key, disk: Disk) {
        let banner = match disk {
            Disk::Missing => Banner::Deleted,
            Disk::Unreadable(why) => Banner::Unreadable(why),
            Disk::Text(_) => return,
        };
        self.external.banners.insert(key, banner);
    }
}

/// The banner strip: what happened and the buttons. Returns the user's answer.
pub(crate) fn banner_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    key: &Key,
    name: &str,
    banner: &Banner,
) -> Option<Resolution> {
    let what = match key.1 {
        Target::Tab => name.to_string(),
        Target::Form => format!("{name} (form)"),
    };
    let message = match banner {
        Banner::Changed => format!("{what} changed on disk"),
        Banner::Deleted => format!("{what} was deleted on disk"),
        Banner::Unreadable(why) => format!("{what}: cannot read ({why})"),
    };
    let mut answer = None;
    Frame::new()
        .fill(if pal.dark {
            pal.warning.gamma_multiply(0.22)
        } else {
            pal.warning.gamma_multiply(0.18)
        })
        .inner_margin(Margin::symmetric(12, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(message).size(12.5).color(pal.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let buttons: &[(&str, Resolution)] = match banner {
                        Banner::Changed => &[
                            ("Keep mine", Resolution::Keep),
                            ("Reload", Resolution::Reload),
                        ],
                        Banner::Deleted => &[
                            ("Keep (save to recreate)", Resolution::Keep),
                            ("Close tab", Resolution::Close),
                        ],
                        Banner::Unreadable(_) => &[("Keep mine", Resolution::Keep)],
                    };
                    for (label, r) in buttons {
                        if ui.button(*label).clicked() {
                            answer = Some(*r);
                        }
                    }
                });
            });
        });
    crate::style::hline(ui, pal);
    answer
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::collections::BTreeSet;

    /// A fresh GUI project (program, unit1.pas, unit1.lfm) under the target dir's tmp folder.
    pub(crate) fn project(name: &str) -> PathBuf {
        let parent = crate::target_tmp().join(format!("ext-{name}"));
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).unwrap();
        let lpr = pascoda_project::create_gui_app(&parent, "proj").unwrap();
        lpr.parent().unwrap().to_path_buf()
    }

    fn batch(paths: &[&Path]) -> Batch {
        Batch {
            paths: paths
                .iter()
                .map(|p| p.to_path_buf())
                .collect::<BTreeSet<_>>(),
            rescan: false,
        }
    }

    fn app_with(dir: &Path, file: &str) -> App {
        App::bare(Some(dir.join(file)))
    }

    fn banner_of(app: &App, path: &Path, target: Target) -> Option<Banner> {
        app.external
            .banners
            .get(&(
                path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
                target,
            ))
            .cloned()
    }

    #[test]
    fn a_clean_tab_reloads_and_the_caret_follows() {
        let dir = project("clean");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        let ctx = egui::Context::default();
        let old = app.docs[0].text.clone();
        let chars = old.chars().count();
        // Caret at the very end.
        let mut state = egui::TextEdit::load_state(&ctx, app.docs[0].editor_id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(chars))));
        state.store(&ctx, app.docs[0].editor_id);

        let new = format!("// added by someone else\n{old}");
        std::fs::write(&path, &new).unwrap();
        app.handle_external(Some(&ctx), &batch(&[&path]));
        assert_eq!(app.docs[0].text, new);
        assert_eq!(app.docs[0].saved, new);
        assert!(app.external.banners.is_empty());
        let (msg, _) = app.notice().expect("a note");
        assert_eq!(msg, "unit1.pas reloaded: changed on disk");
        let caret = egui::TextEdit::load_state(&ctx, app.docs[0].editor_id)
            .and_then(|s| s.cursor.char_range())
            .unwrap()
            .primary
            .index
            .0;
        assert_eq!(caret, new.chars().count(), "caret moved with the text");

        // The file shrinks to almost nothing: the caret stays in range.
        std::fs::write(&path, "unit u;\n").unwrap();
        app.handle_external(Some(&ctx), &batch(&[&path]));
        let caret = egui::TextEdit::load_state(&ctx, app.docs[0].editor_id)
            .and_then(|s| s.cursor.char_range())
            .unwrap()
            .primary
            .index
            .0;
        assert!(caret <= "unit u;\n".chars().count());
    }

    #[test]
    fn a_change_to_an_unrelated_file_is_left_alone() {
        let dir = project("unrelated");
        let mut app = app_with(&dir, "unit1.pas");
        let other = dir.join("other.pas");
        std::fs::write(&other, "unit other;").unwrap();
        let before = app.docs[0].text.clone();
        app.handle_external(None, &batch(&[&other.canonicalize().unwrap()]));
        assert_eq!(app.docs[0].text, before);
        assert!(app.notice().is_none() && app.external.banners.is_empty());
        assert_eq!(app.external.tree_rev, 1, "the tree is told");
    }

    #[test]
    fn a_dirty_tab_keeps_its_text_and_asks() {
        let dir = project("dirty");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        let saved = app.docs[0].saved.clone();
        app.docs[0].text.push_str("// my edit\n");
        std::fs::write(&path, format!("{saved}// theirs\n")).unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert!(app.docs[0].text.ends_with("// my edit\n"));
        assert_eq!(app.docs[0].saved, saved, "saved text is what we last knew");
        assert_eq!(banner_of(&app, &path, Target::Tab), Some(Banner::Changed));

        // Keep mine: dismissed, and the same disk text does not ask again.
        let key = (path.clone(), Target::Tab);
        app.resolve_external(None, key.clone(), Resolution::Keep);
        assert!(app.external.banners.is_empty());
        app.handle_external(None, &batch(&[&path]));
        assert!(app.external.banners.is_empty());
        // A newer external change asks again.
        std::fs::write(&path, format!("{saved}// theirs again\n")).unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert_eq!(banner_of(&app, &path, Target::Tab), Some(Banner::Changed));
        // Saving overwrites on purpose and settles it; the event for our write is a no-op.
        app.resolve_external(None, key.clone(), Resolution::Keep);
        app.save_doc(0);
        assert!(app.external.kept.is_empty() && app.external.banners.is_empty());
        let mine = app.docs[0].text.clone();
        app.handle_external(None, &batch(&[&path]));
        assert_eq!(app.docs[0].text, mine);
        assert!(app.external.banners.is_empty() && app.notice().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
    }

    #[test]
    fn reload_discards_my_edits() {
        let dir = project("reload");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        app.docs[0].text.push_str("// my edit\n");
        std::fs::write(&path, "unit theirs;\n").unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert!(banner_of(&app, &path, Target::Tab).is_some());
        app.resolve_external(None, (path.clone(), Target::Tab), Resolution::Reload);
        assert_eq!(app.docs[0].text, "unit theirs;\n");
        assert!(!app.docs[0].dirty() && app.external.banners.is_empty());
    }

    #[test]
    fn a_banner_goes_away_when_the_disk_is_reverted() {
        let dir = project("revert");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        let saved = app.docs[0].saved.clone();
        app.docs[0].text.push_str("// my edit\n");
        std::fs::write(&path, "other\n").unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert!(banner_of(&app, &path, Target::Tab).is_some());
        std::fs::write(&path, &saved).unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert!(app.external.banners.is_empty());
    }

    #[test]
    fn a_deleted_file_asks_and_can_be_recreated_or_closed() {
        let dir = project("deleted");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        let text = app.docs[0].text.clone();
        std::fs::remove_file(&path).unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert_eq!(banner_of(&app, &path, Target::Tab), Some(Banner::Deleted));
        assert_eq!(app.docs[0].text, text, "the text is untouched");

        // Keep: the tab counts as unsaved so Save recreates the file.
        let key = (path.clone(), Target::Tab);
        app.resolve_external(None, key.clone(), Resolution::Keep);
        assert!(app.docs[0].dirty() && app.external.banners.is_empty());
        app.handle_external(None, &batch(&[&path]));
        assert!(app.external.banners.is_empty(), "not asked twice");
        app.save_doc(0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        // Close tab.
        std::fs::remove_file(&path).unwrap();
        app.handle_external(None, &batch(&[&path]));
        app.resolve_external(None, key, Resolution::Close);
        assert!(app.docs.is_empty() && app.external.banners.is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_read_shows_a_banner_and_keeps_the_tab() {
        let dir = project("binary");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        let text = app.docs[0].text.clone();
        std::fs::write(&path, [0xffu8, 0xfe, 0x00]).unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert!(matches!(
            banner_of(&app, &path, Target::Tab),
            Some(Banner::Unreadable(_))
        ));
        assert_eq!(app.docs[0].text, text);
        // It becomes readable again: the banner is replaced by the normal reload.
        std::fs::write(&path, "unit fixed;\n").unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert_eq!(app.docs[0].text, "unit fixed;\n");
        assert!(app.external.banners.is_empty());
    }

    #[test]
    fn the_form_designer_reloads_an_external_lfm_change() {
        let dir = project("form");
        let mut app = app_with(&dir, "unit1.pas");
        app.show_form();
        let lfm = app.designers[0].lfm_path.clone();
        let text = std::fs::read_to_string(&lfm).unwrap();
        assert!(text.contains("'Form1'"));
        let new = text.replace("'Form1'", "'Renamed'");
        std::fs::write(&lfm, &new).unwrap();
        app.handle_external(None, &batch(&[&lfm]));
        assert_eq!(app.designers[0].doc.source(), new);
        assert!(!app.designers[0].dirty());
        assert!(app.external.banners.is_empty());
        assert!(app.notice().unwrap().0.contains("unit1.lfm reloaded"));
        // Our own save is not external.
        app.designers[0]
            .add_component("TButton", &[], 8, 8)
            .unwrap();
        app.save_doc(0);
        let mine = app.designers[0].doc.source().to_string();
        app.notice_clear();
        app.handle_external(None, &batch(&[&lfm]));
        assert_eq!(app.designers[0].doc.source(), mine);
        assert!(app.notice().is_none() && app.external.banners.is_empty());
    }

    #[test]
    fn a_dirty_form_asks_before_an_lfm_change_is_taken() {
        let dir = project("formdirty");
        let mut app = app_with(&dir, "unit1.pas");
        app.show_form();
        let lfm = app.designers[0].lfm_path.clone();
        app.designers[0]
            .add_component("TButton", &[], 8, 8)
            .unwrap();
        let mine = app.designers[0].doc.source().to_string();
        let theirs = std::fs::read_to_string(&lfm)
            .unwrap()
            .replace("'Form1'", "'Theirs'");
        std::fs::write(&lfm, &theirs).unwrap();
        app.handle_external(None, &batch(&[&lfm]));
        assert_eq!(app.designers[0].doc.source(), mine);
        assert_eq!(banner_of(&app, &lfm, Target::Form), Some(Banner::Changed));
        app.resolve_external(
            None,
            (lfm.canonicalize().unwrap(), Target::Form),
            Resolution::Reload,
        );
        assert_eq!(app.designers[0].doc.source(), theirs);
        assert!(!app.designers[0].dirty());
        // Undo brings back the form from before the reload.
        assert!(app.designers[0].undo());
        assert_eq!(app.designers[0].doc.source(), mine);
    }

    #[test]
    fn an_open_lfm_text_tab_follows_the_disk() {
        let dir = project("lfmtab");
        let mut app = app_with(&dir, "unit1.lfm");
        let path = app.docs[0].path.clone();
        let new = app.docs[0].text.replace("'Form1'", "'Via text'");
        std::fs::write(&path, &new).unwrap();
        app.handle_external(None, &batch(&[&path]));
        assert_eq!(app.docs[0].text, new);
    }

    #[test]
    fn a_rescan_checks_every_open_file() {
        let dir = project("rescan");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        std::fs::write(&path, "unit rescanned;\n").unwrap();
        app.handle_external(
            None,
            &Batch {
                paths: BTreeSet::new(),
                rescan: true,
            },
        );
        assert_eq!(app.docs[0].text, "unit rescanned;\n");
    }

    #[test]
    fn batches_from_a_fake_event_source_are_polled_each_frame() {
        let dir = project("poll");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.external.rx = Some(rx);
        std::fs::write(&path, "unit polled;\n").unwrap();
        tx.send(batch(&[&path])).unwrap();
        app.poll_external(&egui::Context::default());
        assert_eq!(app.docs[0].text, "unit polled;\n");
        assert!(app.external.rx.is_some());
    }

    #[test]
    fn the_file_tree_lists_new_and_removed_files_and_keeps_expansion() {
        let dir = project("tree");
        let mut app = app_with(&dir, "unit1.pas");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        app.expanded.insert(sub.clone());
        let names = |d: &Path| -> Vec<String> {
            crate::shell::listing(d)
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert!(!names(&dir).contains(&"added.pas".to_string()));
        std::fs::write(dir.join("added.pas"), "unit added;").unwrap();
        app.handle_external(None, &batch(&[&dir.join("added.pas")]));
        assert!(names(&dir).contains(&"added.pas".to_string()));
        std::fs::remove_file(dir.join("added.pas")).unwrap();
        app.handle_external(None, &batch(&[&dir.join("added.pas")]));
        assert!(!names(&dir).contains(&"added.pas".to_string()));
        assert_eq!(app.external.tree_rev, 2);
        assert!(app.expanded.contains(&sub), "expanded state is by path");
    }

    fn render_app(app: App, dark: bool, name: &str) -> Harness<'static, App> {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 520.0))
            .with_pixels_per_point(1.0)
            .with_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            })
            .with_context_setup(crate::theme::apply)
            .build_ui_state(|ui, app: &mut App| app.draw(ui, None), app);
        harness.run_steps(3);
        let image = harness.render().expect("offscreen render");
        image
            .save(crate::target_tmp().join(format!("{name}.png")))
            .unwrap();
        harness
    }

    fn app_with_banner(name: &str) -> (App, PathBuf) {
        let dir = project(name);
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        app.docs[0].text.push_str("// my edit\n");
        std::fs::write(&path, "unit theirs;\n").unwrap();
        app.handle_external(None, &batch(&[&path]));
        (app, path)
    }

    #[test]
    fn renders_the_banner_in_both_themes() {
        for (dark, name) in [
            (false, "ui-banner-changed"),
            (true, "ui-banner-changed-dark"),
        ] {
            let (app, _) = app_with_banner(if dark { "bannerd" } else { "bannerl" });
            let h = render_app(app, dark, name);
            h.get_by_label("Reload");
            h.get_by_label("Keep mine");
        }
    }

    #[test]
    fn the_banner_buttons_work_in_the_window() {
        let (app, path) = app_with_banner("clicks");
        let mut h = render_app(app, false, "ui-banner-clicks");
        h.get_by_label("Keep mine").click();
        h.run_steps(3);
        assert!(h.state().external.banners.is_empty());
        assert!(h.state().docs[0].text.ends_with("// my edit\n"));
        assert!(h.query_by_label("Keep mine").is_none());

        let (app, path2) = app_with_banner("clicks2");
        let mut h = render_app(app, false, "ui-banner-clicks2");
        h.get_by_label("Reload").click();
        h.run_steps(3);
        assert_eq!(h.state().docs[0].text, "unit theirs;\n");
        assert!(h.state().external.banners.is_empty());
        let _ = (path, path2);
    }

    #[test]
    fn the_deleted_banner_closes_the_tab() {
        let dir = project("delclick");
        let mut app = app_with(&dir, "unit1.pas");
        let path = app.docs[0].path.clone();
        std::fs::remove_file(&path).unwrap();
        app.handle_external(None, &batch(&[&path]));
        let mut h = render_app(app, false, "ui-banner-deleted");
        h.get_by_label("Keep (save to recreate)");
        h.get_by_label("Close tab").click();
        h.run_steps(3);
        assert!(h.state().docs.is_empty());
    }

    #[test]
    fn the_status_note_shows_and_fades() {
        let dir = project("note");
        let mut app = app_with(&dir, "unit1.pas");
        app.note("unit1.pas reloaded: changed on disk");
        let (_, opacity) = app.notice().unwrap();
        assert!((opacity - 1.0).abs() < f32::EPSILON);
        app.external.notice = Some((
            "x".into(),
            Instant::now() - NOTICE_FOR + Duration::from_millis(400),
        ));
        assert!(app.notice().unwrap().1 < 0.5);
        app.external.notice = Some((
            "x".into(),
            Instant::now() - NOTICE_FOR - Duration::from_secs(1),
        ));
        assert!(app.notice().is_none());
        // No offscreen render here (slow under load, and the note expires after 5 seconds).
        let mut a = app_with(&dir, "unit1.pas");
        a.note("unit1.pas reloaded: changed on disk");
        // A start time in the future never expires, however slow the harness is.
        a.external.notice.as_mut().unwrap().1 = Instant::now() + Duration::from_secs(3600);
        let mut h = Harness::builder()
            .with_size(egui::vec2(900.0, 520.0))
            .with_context_setup(crate::theme::apply)
            .build_ui_state(|ui, app: &mut App| app.draw(ui, None), a);
        h.run_steps(2);
        h.get_by_label_contains("reloaded: changed on disk");
    }
}
