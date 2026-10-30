//! Component packages the user imported (File > Import Component Package...). Their `.lpk` paths are
//! kept, one per line, in `~/Library/Application Support/Pascoda/packages`. Their units join every
//! build and the language server, as the project's own packages do (see `packages::resolve`), and
//! their components appear in the form designer.

use std::io;
use std::path::{Path, PathBuf};

/// The store file in the user's Application Support folder.
pub fn default_store() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Pascoda/packages"))
}

/// The Pascoda components that ship with the app (`components/pascoda-vlc`): their package files,
/// found next to the app or in the checkout (see `pascoda_fpc::lcl::search_dirs`).
pub fn shipped_packages(start: &Path) -> Vec<PathBuf> {
    const SHIPPED: &[&str] = &["components/pascoda-vlc/PascodaVlc.lpk", "components/pascoda-data/PascodaData.lpk"];
    let dirs = pascoda_fpc::lcl::search_dirs(start);
    SHIPPED
        .iter()
        .filter_map(|rel| dirs.iter().map(|d| d.join(rel)).find(|p| p.is_file()))
        .collect()
}

/// The package files in `store` that still exist, in the order they were imported. Missing files
/// are left out, so a moved package stops joining the builds without an error.
pub fn list_from(store: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(store)
        .map(|text| parse(&text).into_iter().filter(|p| p.is_file()).collect())
        .unwrap_or_default()
}

/// The imported packages (see [`list_from`] on the default store).
/// The packages in use: the Pascoda components that ship with the app (always there, no import
/// needed), then the imported ones.
pub fn list() -> Vec<PathBuf> {
    let mut all = builtin();
    for file in default_store().map(|s| list_from(&s)).unwrap_or_default() {
        if !all.contains(&file) {
            all.push(file);
        }
    }
    all
}

/// The Pascoda packages that come with the app (see `shipped_packages`), found from the running
/// program: next to it in the bundle, or in the checkout when it is a development build.
pub fn builtin() -> Vec<PathBuf> {
    std::env::current_exe().map(|exe| shipped_packages(&exe)).unwrap_or_default()
}

/// Adds `package` to `store` (once). Returns the stored list.
pub fn add_to(store: &Path, package: &Path) -> io::Result<Vec<PathBuf>> {
    let mut all = parse(&std::fs::read_to_string(store).unwrap_or_default());
    if !all.iter().any(|p| p == package) {
        all.push(package.to_path_buf());
    }
    write(store, &all)?;
    Ok(all)
}

/// Removes `package` from `store`. Returns the stored list.
pub fn remove_from(store: &Path, package: &Path) -> io::Result<Vec<PathBuf>> {
    let mut all = parse(&std::fs::read_to_string(store).unwrap_or_default());
    all.retain(|p| p != package);
    write(store, &all)?;
    Ok(all)
}

fn write(store: &Path, all: &[PathBuf]) -> io::Result<()> {
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text: String = all.iter().map(|p| format!("{}\n", p.display())).collect();
    std::fs::write(store, text)
}

/// The paths in the store text: one per line, blank lines skipped, duplicates dropped.
pub fn parse(text: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let path = PathBuf::from(line);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pascoda-imports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_store_keeps_each_package_once_in_order() {
        let dir = temp("order");
        let store = dir.join("packages");
        let a = dir.join("a.lpk");
        let b = dir.join("b.lpk");
        for p in [&a, &b] {
            std::fs::write(p, "x").unwrap();
        }
        add_to(&store, &a).unwrap();
        add_to(&store, &b).unwrap();
        let all = add_to(&store, &a).unwrap();
        assert_eq!(all, [a.clone(), b.clone()], "a repeated import is not added twice");
        assert_eq!(list_from(&store), [a.clone(), b.clone()]);
        let all = remove_from(&store, &a).unwrap();
        assert_eq!(all, [b.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_that_was_moved_or_deleted_is_left_out() {
        let dir = temp("missing");
        let store = dir.join("packages");
        let kept = dir.join("kept.lpk");
        std::fs::write(&kept, "x").unwrap();
        let gone = dir.join("gone.lpk");
        add_to(&store, &gone).unwrap();
        add_to(&store, &kept).unwrap();
        assert_eq!(list_from(&store), [kept.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pascoda_packages_are_in_use_without_an_import() {
        // A development build finds the checkout's components (see `search_dirs`).
        let shipped = builtin();
        assert!(!shipped.is_empty(), "the checkout's packages are found from a test build");
        assert!(shipped.iter().any(|p| p.ends_with("components/pascoda-data/PascodaData.lpk")), "{shipped:?}");
        assert!(shipped.iter().any(|p| p.ends_with("components/pascoda-vlc/PascodaVlc.lpk")), "{shipped:?}");
        assert!(list().iter().all(|p| shipped.contains(p) || p.is_file()), "the list is the built-in packages and the imported ones");
    }

    #[test]
    fn a_missing_store_is_an_empty_list() {
        assert!(list_from(Path::new("/nonexistent/pascoda/packages")).is_empty());
        assert_eq!(parse("\n  \n/a.lpk\n/a.lpk\n"), [PathBuf::from("/a.lpk")]);
    }
}
