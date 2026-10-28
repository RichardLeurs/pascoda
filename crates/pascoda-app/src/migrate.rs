//! One-time move of the settings folder from the app's former name.

use std::path::Path;

/// Folder name under `~/Library/Application Support` before the rename.
const LEGACY_DIR: &str = "Pascora";
const DIR: &str = "Pascoda";

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The old folder was renamed to the new name.
    Moved,
    /// Nothing to do: no old folder, or the new one already exists (it wins; both are left).
    Skipped,
    Failed(String),
}

/// Renames `<base>/Pascora` to `<base>/Pascoda` when only the old one exists.
pub fn app_support(base: &Path) -> Outcome {
    let (old, new) = (base.join(LEGACY_DIR), base.join(DIR));
    if !old.is_dir() || new.exists() {
        return Outcome::Skipped;
    }
    match std::fs::rename(&old, &new) {
        Ok(()) => Outcome::Moved,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Runs the migration for the current user at start-up.
pub fn run() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let base = Path::new(&home).join("Library/Application Support");
    if let Outcome::Failed(e) = app_support(&base) {
        eprintln!("could not move the old settings folder: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-migrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn old_only_is_moved() {
        let b = scratch("old");
        std::fs::create_dir_all(b.join("Pascora")).unwrap();
        std::fs::write(b.join("Pascora/appearance"), "dark").unwrap();
        assert_eq!(app_support(&b), Outcome::Moved);
        assert!(!b.join("Pascora").exists());
        assert_eq!(
            std::fs::read_to_string(b.join("Pascoda/appearance")).unwrap(),
            "dark"
        );
        assert_eq!(app_support(&b), Outcome::Skipped);
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn new_only_is_untouched() {
        let b = scratch("new");
        std::fs::create_dir_all(b.join("Pascoda")).unwrap();
        std::fs::write(b.join("Pascoda/appearance"), "light").unwrap();
        assert_eq!(app_support(&b), Outcome::Skipped);
        assert_eq!(
            std::fs::read_to_string(b.join("Pascoda/appearance")).unwrap(),
            "light"
        );
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn both_present_leaves_both_and_new_wins() {
        let b = scratch("both");
        for (d, v) in [("Pascora", "dark"), ("Pascoda", "light")] {
            std::fs::create_dir_all(b.join(d)).unwrap();
            std::fs::write(b.join(d).join("appearance"), v).unwrap();
        }
        assert_eq!(app_support(&b), Outcome::Skipped);
        assert_eq!(
            std::fs::read_to_string(b.join("Pascoda/appearance")).unwrap(),
            "light"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("Pascora/appearance")).unwrap(),
            "dark"
        );
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn neither_does_nothing() {
        let b = scratch("none");
        assert_eq!(app_support(&b), Outcome::Skipped);
        assert!(!b.join("Pascoda").exists());
        std::fs::remove_dir_all(&b).unwrap();
    }
}
