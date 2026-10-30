//! The session the app restores at launch: the project folder, the open tabs in their order, the
//! active tab and whether it was showing its form. Kept in
//! `~/Library/Application Support/Pascoda/session`, one `key value` line each. Unsaved text is not
//! kept: quitting with unsaved changes asks first, so what is open at exit is saved or discarded.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Session {
    pub root: Option<PathBuf>,
    /// The open tabs, in tab order.
    pub tabs: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    /// The active tab was showing its form.
    pub form_view: bool,
}

impl Session {
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(' ') else { continue };
            let path = PathBuf::from(value);
            match key {
                "root" => out.root = Some(path),
                "tab" => out.tabs.push(path),
                "active" => out.active = Some(path),
                "form" => out.form_view = value == "1",
                _ => {}
            }
        }
        out
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if let Some(r) = &self.root {
            out.push_str(&format!("root {}\n", r.display()));
        }
        for t in &self.tabs {
            out.push_str(&format!("tab {}\n", t.display()));
        }
        if let Some(a) = &self.active {
            out.push_str(&format!("active {}\n", a.display()));
        }
        out.push_str(&format!("form {}\n", if self.form_view { 1 } else { 0 }));
        out
    }

    pub fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/Pascoda/session"))
    }

    /// The saved session, or nothing when there is none (or it cannot be read).
    pub fn load(file: &Path) -> Option<Self> {
        std::fs::read_to_string(file).ok().map(|t| Self::parse(&t))
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

    #[test]
    fn the_session_round_trips_and_ignores_unknown_lines() {
        let s = Session {
            root: Some(PathBuf::from("/work/app")),
            tabs: vec![PathBuf::from("/work/app/a.pas"), PathBuf::from("/work/app/my file.pas")],
            active: Some(PathBuf::from("/work/app/my file.pas")),
            form_view: true,
        };
        assert_eq!(Session::parse(&s.to_text()), s, "paths with spaces survive");
        assert_eq!(Session::parse("nonsense\nroot /x\n"), Session { root: Some(PathBuf::from("/x")), ..Session::default() });
    }

    #[test]
    fn a_saved_session_loads_back_from_its_file() {
        let d = std::env::temp_dir().join(format!("pascoda-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let file = d.join("session");
        assert!(Session::load(&file).is_none(), "nothing saved yet");
        let s = Session { root: Some(d.clone()), tabs: vec![d.join("a.pas")], active: None, form_view: false };
        s.save(&file);
        assert_eq!(Session::load(&file), Some(s));
        let _ = std::fs::remove_dir_all(&d);
    }
}
