//! Recent files and projects, for File > Open Recent. Kept in
//! `~/Library/Application Support/Pascoda/recent`, one entry per line (`file <path>` or
//! `project <path>`), newest first. The menu lists only entries that still exist.

use std::path::{Path, PathBuf};

/// How many files, and how many projects, the lists keep.
pub const LIMIT: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Recent {
    pub files: Vec<PathBuf>,
    /// Project folders, newest first.
    pub projects: Vec<PathBuf>,
}

/// One item of the Open Recent menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub project: bool,
}

/// Moves `path` to the front of `list`, drops repeats, and keeps `LIMIT` entries. A path with a line
/// break cannot be stored, so it is ignored.
fn note(list: &mut Vec<PathBuf>, path: &Path) {
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\n') {
        return;
    }
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(LIMIT);
}

impl Recent {
    pub fn note_file(&mut self, path: &Path) {
        note(&mut self.files, path);
    }

    pub fn note_project(&mut self, folder: &Path) {
        note(&mut self.projects, folder);
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.projects.clear();
    }

    /// The Open Recent menu's items: projects, then files, those that still exist.
    pub fn menu_entries(&self) -> Vec<Entry> {
        let projects = self.projects.iter().filter(|p| p.is_dir()).map(|p| Entry { path: p.clone(), project: true });
        let files = self.files.iter().filter(|p| p.is_file()).map(|p| Entry { path: p.clone(), project: false });
        projects.chain(files).collect()
    }

    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("file ") {
                out.files.push(PathBuf::from(p));
            } else if let Some(p) = line.strip_prefix("project ") {
                out.projects.push(PathBuf::from(p));
            }
        }
        out.files.truncate(LIMIT);
        out.projects.truncate(LIMIT);
        out
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for p in &self.projects {
            out.push_str(&format!("project {}\n", p.display()));
        }
        for p in &self.files {
            out.push_str(&format!("file {}\n", p.display()));
        }
        out
    }

    /// The file the lists are kept in, under the user's Application Support folder.
    pub fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/Pascoda/recent"))
    }

    pub fn load(file: &Path) -> Self {
        std::fs::read_to_string(file).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, file: &Path) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, self.to_text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-recent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn newest_first_without_repeats_and_within_the_limit() {
        let mut r = Recent::default();
        for i in 0..(LIMIT + 3) {
            r.note_file(Path::new(&format!("/p/f{i}.pas")));
        }
        assert_eq!(r.files.len(), LIMIT);
        assert_eq!(r.files[0], PathBuf::from(format!("/p/f{}.pas", LIMIT + 2)));
        r.note_file(Path::new(&format!("/p/f{}.pas", LIMIT)));
        assert_eq!(r.files[0], PathBuf::from(format!("/p/f{LIMIT}.pas")));
        assert_eq!(r.files.iter().filter(|p| p.ends_with(format!("f{LIMIT}.pas"))).count(), 1);
        r.note_file(Path::new("bad\nname"));
        assert!(!r.files.iter().any(|p| p.to_string_lossy().contains("bad")), "line breaks are not stored");
    }

    #[test]
    fn the_lists_round_trip_through_the_file() {
        let d = tmp("round");
        let mut r = Recent::default();
        r.note_file(&d.join("a.pas"));
        r.note_project(&d);
        let file = d.join("recent");
        r.save(&file);
        assert_eq!(Recent::load(&file), r);
        assert_eq!(Recent::parse("junk\nfile /x.pas\n"), Recent { files: vec![PathBuf::from("/x.pas")], projects: vec![] });
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_menu_lists_projects_then_files_that_still_exist() {
        let d = tmp("menu");
        std::fs::write(d.join("a.pas"), "unit a;\n").unwrap();
        let mut r = Recent::default();
        r.note_file(&d.join("a.pas"));
        r.note_file(&d.join("gone.pas"));
        r.note_project(&d);
        r.note_project(&d.join("gone-folder"));
        let entries = r.menu_entries();
        assert_eq!(
            entries,
            vec![
                Entry { path: d.clone(), project: true },
                Entry { path: d.join("a.pas"), project: false },
            ]
        );
        r.clear();
        assert!(r.menu_entries().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
