//! Creating and deleting files and folders in the project tree. UI-free: the navigator decides
//! when to call these and what to do with the tabs and designers of a deleted item.

use std::path::{Path, PathBuf};

/// The Pascal files a new file can be (the tree lists them, and a new file must be one of these,
/// or it would be created and then never shown).
pub const FILE_EXTENSIONS: &[&str] = &["pas", "pp", "lpr", "dpr", "inc", "lfm"];

/// The files the tree lists: the new-file kinds, plus Delphi forms and project files, which are
/// read but not created here.
pub const TREE_EXTENSIONS: &[&str] = &["pas", "pp", "lpr", "dpr", "inc", "lfm", "dfm", "lpi", "dproj"];

/// Folders the tree hides (and so cannot create either).
pub const HIDDEN_DIRS: &[&str] = &["target", "vendor", "lib"];

/// A name typed for a new file or folder, trimmed and checked for the characters a path cannot
/// hold and for names the tree would hide.
pub fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name first".into());
    }
    if name.contains(['/', ':']) {
        return Err("A name cannot contain / or :".into());
    }
    if name.starts_with('.') {
        return Err("A name cannot start with a dot".into());
    }
    Ok(name)
}

/// Creates an empty file `name` in `dir`. A name without an extension gets `.pas`; an extension
/// the tree does not list is refused. An existing file is never overwritten.
pub fn new_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = check_name(name)?;
    let name = if Path::new(name).extension().is_none() {
        format!("{name}.pas")
    } else {
        name.to_string()
    };
    let ext = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !FILE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "The project lists only {} files",
            FILE_EXTENSIONS
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let path = dir.join(&name);
    if path.exists() {
        return Err(format!("“{name}” already exists"));
    }
    std::fs::File::create_new(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Creates the folder `name` in `dir`; an existing item is never replaced.
pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = check_name(name)?;
    if name.ends_with(".app") || HIDDEN_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!("The tree hides folders named “{name}”"));
    }
    let path = dir.join(name);
    if path.exists() {
        return Err(format!("“{name}” already exists"));
    }
    std::fs::create_dir(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// What a delete would remove: 1 for a file, the file and folder count below a folder.
pub fn count_items(path: &Path) -> usize {
    if !path.is_dir() {
        return 1;
    }
    let Ok(read) = std::fs::read_dir(path) else {
        return 1;
    };
    1 + read
        .filter_map(Result::ok)
        .map(|e| count_items(&e.path()))
        .sum::<usize>()
}

/// Moves a file or a whole folder to the Trash, so the user can take it back. Only items strictly
/// inside `root` can go: the project folder itself, and anything outside it, are refused. The
/// caller asks the user first.
#[cfg(not(test))]
pub fn delete(path: &Path, root: &Path) -> Result<(), String> {
    delete_with(path, root, |p| trash::delete(p).map_err(|e| e.to_string()))
}

/// Tests remove for real instead of using the Trash, so they do not fill the user's Trash.
#[cfg(test)]
pub fn delete(path: &Path, root: &Path) -> Result<(), String> {
    delete_with(path, root, remove_from_disk)
}

/// [`delete`] with the step that actually removes the item.
pub fn delete_with(
    path: &Path,
    root: &Path,
    remove: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    if path == root || !path.starts_with(root) {
        return Err("Only items inside the project can be deleted".into());
    }
    remove(path)
}

/// Removes a file or a whole folder from disk, for the tests.
#[cfg(test)]
fn remove_from_disk(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| e.to_string())
}

/// Renames a file or a folder inside its folder. A file given a name without an extension keeps its
/// own; the new extension must be one the tree lists. A unit moves with its form (`.lfm` or `.dfm`)
/// of the same name. Returns the moves made, or nothing when the name is unchanged. The project
/// folder itself and anything outside `root` are refused, and so is a name that is taken.
pub fn rename(path: &Path, new_name: &str, root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if path == root || !path.starts_with(root) {
        return Err("Only items inside the project can be renamed".into());
    }
    let name = check_name(new_name)?;
    let name = match (path.is_file(), Path::new(name).extension(), path.extension()) {
        (true, None, Some(old)) => format!("{name}.{}", old.to_string_lossy()),
        _ => name.to_string(),
    };
    if path.is_file() {
        let ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !TREE_EXTENSIONS.contains(&ext.as_str()) {
            return Err("The project lists only Pascal, form and project files".into());
        }
    } else if name.ends_with(".app") || HIDDEN_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!("The tree hides folders named “{name}”"));
    }
    let parent = path.parent().ok_or("The item has no folder")?;
    let target = parent.join(&name);
    if target == path {
        return Ok(Vec::new());
    }
    if target.exists() && !same_item(&target, path) {
        return Err(format!("“{name}” already exists"));
    }
    let mut moves = vec![(path.to_path_buf(), target.clone())];
    let is_unit = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("pas" | "pp")
    ) && path.is_file();
    if is_unit {
        for ext in ["lfm", "dfm"] {
            let form = path.with_extension(ext);
            if form.is_file() {
                let to = target.with_extension(ext);
                if to.exists() && !same_item(&to, &form) {
                    return Err(format!("“{}” already exists", to.display()));
                }
                moves.push((form, to));
                break;
            }
        }
    }
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to) in &moves {
        if let Err(e) = std::fs::rename(from, to) {
            // Put back what was already moved, so a failed rename leaves the project as it was.
            for (f, t) in done.iter().rev() {
                let _ = std::fs::rename(t, f);
            }
            return Err(e.to_string());
        }
        done.push((from.clone(), to.clone()));
    }
    Ok(moves)
}

/// Whether two paths name the same file or folder (they may differ in case on macOS).
fn same_item(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Where `path` ends up when `from` is moved to `to`, if it is `from` or inside it.
pub fn moved(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    path.strip_prefix(from).ok().map(|rest| to.join(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("  ").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("a:b").is_err());
        assert!(check_name(".hidden").is_err());
        assert_eq!(check_name(" unit2 ").unwrap(), "unit2");
    }

    #[test]
    fn new_file_adds_pas_and_refuses_unlisted_or_existing() {
        let d = scratch("file");
        let p = new_file(&d, "unit2").unwrap();
        assert_eq!(p.file_name().unwrap(), "unit2.pas");
        assert!(p.is_file());
        assert!(new_file(&d, "unit2.pas").is_err(), "never overwrite");
        assert!(new_file(&d, "notes.txt").is_err(), "the tree would hide it");
        assert!(new_file(&d, "Form1.LFM").is_ok(), "extensions are case-insensitive");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn new_folder_refuses_hidden_names_and_existing_items() {
        let d = scratch("dir");
        let p = new_folder(&d, "sub").unwrap();
        assert!(p.is_dir());
        assert!(new_folder(&d, "sub").is_err());
        assert!(new_folder(&d, "lib").is_err(), "the tree hides lib");
        assert!(new_folder(&d, "Foo.app").is_err(), "bundles are hidden");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_removes_files_and_folders_but_not_the_project() {
        let root = scratch("delete");
        let f = new_file(&root, "a.pas").unwrap();
        let dir = new_folder(&root, "sub").unwrap();
        new_file(&dir, "b.pas").unwrap();
        assert_eq!(count_items(&dir), 2);

        assert!(delete(&root, &root).is_err(), "the project folder stays");
        assert!(delete(&root.join("..").join("outside"), &root).is_err());
        delete(&f, &root).unwrap();
        assert!(!f.exists());
        delete(&dir, &root).unwrap();
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The real Trash, so this is run by hand (`cargo test -- --ignored real_trash`): it moves
    /// one scratch file to the user's Trash and checks that the original is gone.
    #[test]
    #[ignore = "moves a file to the real Trash"]
    fn real_trash_moves_the_item() {
        let root = scratch("real-trash");
        let file = root.join("pascoda-trash-check.pas");
        std::fs::write(&file, "unit check;\n").unwrap();
        trash::delete(&file).expect("the Trash accepts the file");
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_keeps_the_extension_and_refuses_taken_names() {
        let root = scratch("rename-file");
        let f = new_file(&root, "unit2").unwrap();
        let moves = rename(&f, "unit3", &root).unwrap();
        assert_eq!(moves, vec![(f.clone(), root.join("unit3.pas"))]);
        assert!(!f.exists() && root.join("unit3.pas").is_file());
        new_file(&root, "taken").unwrap();
        assert!(rename(&root.join("unit3.pas"), "taken.pas", &root).is_err());
        assert!(rename(&root.join("unit3.pas"), "notes.txt", &root).is_err(), "the tree would hide it");
        assert!(rename(&root.join("unit3.pas"), "unit3.pas", &root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_unit_moves_with_its_form() {
        let root = scratch("rename-unit");
        let pas = new_file(&root, "main.pas").unwrap();
        std::fs::write(root.join("main.dfm"), "object Form1: TForm1\nend\n").unwrap();
        let moves = rename(&pas, "app", &root).unwrap();
        assert_eq!(moves.len(), 2);
        assert!(root.join("app.pas").is_file() && root.join("app.dfm").is_file());
        assert!(!root.join("main.dfm").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_renames_with_its_contents_and_the_project_cannot() {
        let root = scratch("rename-dir");
        let dir = new_folder(&root, "sub").unwrap();
        new_file(&dir, "inner.pas").unwrap();
        rename(&dir, "renamed", &root).unwrap();
        assert!(root.join("renamed/inner.pas").is_file());
        assert!(rename(&root, "elsewhere", &root).is_err(), "the project folder stays");
        assert!(rename(&root.join("renamed"), "lib", &root).is_err(), "the tree hides lib");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_case_change_alone_is_a_rename() {
        let root = scratch("rename-case");
        let f = new_file(&root, "lower.pas").unwrap();
        rename(&f, "Lower.pas", &root).unwrap();
        let names: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["Lower.pas".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn moved_maps_a_path_inside_a_renamed_folder() {
        let from = Path::new("/p/sub");
        let to = Path::new("/p/new");
        assert_eq!(moved(Path::new("/p/sub/a.pas"), from, to), Some(PathBuf::from("/p/new/a.pas")));
        assert_eq!(moved(from, from, to), Some(PathBuf::from("/p/new")));
        assert_eq!(moved(Path::new("/p/subway.pas"), from, to), None);
    }
}
