//! Package dependencies. A folder's packages are its Lazarus (`.lpk`) and Delphi (`.dpk`) packages.
//! Each package's `requires` are found among the folder's packages (and those below it), and their
//! folders and search paths join the build and the language server. A name that is neither found nor
//! built in (FCL, RTL, LCL) is reported as missing.

use crate::lpk::{self, Package};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Packages that come with the compiler or the LCL, and are not looked up in the project.
pub const BUILT_IN: &[&str] = &["FCL", "RTL", "LCL", "LCLBase", "LazUtils"];

/// Reads a Delphi package (`package Name; requires ...; contains ...; end.`). Its `contains` units are
/// the unit files (`unit1 in 'unit1.pas'`, or the bare name next to the package).
pub fn parse_dpk(text: &str, file: &Path) -> Package {
    let dir = file.parent().unwrap_or(Path::new("."));
    let text = strip_comments(text);
    let lower = text.to_ascii_lowercase();
    let mut p = Package { file: file.to_path_buf(), ..Package::default() };
    if let Some(i) = lower.find("package") {
        let rest = &text[i + "package".len()..];
        if let Some(end) = rest.find(';') {
            p.name = rest[..end].trim().to_string();
        }
    }
    let section = |keyword: &str| -> Option<String> {
        let start = lower.find(keyword)? + keyword.len();
        let end = lower[start..].find(';').map(|e| start + e)?;
        Some(text[start..end].to_string())
    };
    if let Some(list) = section("requires") {
        p.requires = list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(list) = section("contains") {
        for entry in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            // `unit1 in 'unit1.pas'`: the file is named; otherwise the unit is next to the package.
            let file = match entry.split_once(" in ") {
                Some((_, path)) => path.trim().trim_matches('\'').to_string(),
                None => format!("{}.pas", entry.split_whitespace().next().unwrap_or_default()),
            };
            p.units.push(dir.join(file.replace('\\', "/")));
        }
    }
    p
}

/// `text` without its `{ ... }`, `(* ... *)` and `// ...` comments.
fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('{', _) => {
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                }
            }
            ('(', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for d in chars.by_ref() {
                    if last == '*' && d == ')' {
                        break;
                    }
                    last = d;
                }
            }
            ('/', Some('/')) => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Reads the package at `file`: a `.lpk` or a `.dpk`. `None` for other files or ones that do not read.
pub fn read(file: &Path) -> Option<Package> {
    match file.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "lpk" => lpk::read(file).ok(),
        "dpk" => std::fs::read_to_string(file).ok().map(|t| parse_dpk(&t, file)),
        _ => None,
    }
}

/// The packages in `folder` itself (its `.lpk` and `.dpk` files), by file name.
pub fn in_folder(folder: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && read(p).is_some())
        .collect();
    out.sort();
    out
}

/// Every package below `folder` (not hidden, build or vendored folders), by lower-case name.
fn index(folder: &Path) -> BTreeMap<String, PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut BTreeMap<String, PathBuf>) {
        let Ok(read_dir) = std::fs::read_dir(dir) else { return };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                let hidden = name.starts_with('.') || ["lib", "target", "vendor", "release"].contains(&name.as_str());
                if depth < 6 && !hidden && !name.ends_with(".app") {
                    walk(&path, depth + 1, out);
                }
            } else if let Some(p) = read(&path) {
                if !p.name.is_empty() {
                    out.entry(p.name.to_ascii_lowercase()).or_insert(path);
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(folder, 0, &mut out);
    out
}

/// What the packages of a folder add to a build: the folders and search paths of the packages they
/// require, and the requirements that could not be found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    pub unit_paths: Vec<PathBuf>,
    pub include_paths: Vec<PathBuf>,
    /// The packages resolved, by name.
    pub packages: Vec<String>,
    pub missing: Vec<String>,
}

/// Resolves the requirements of the packages in `folder`, and of the packages they require in turn.
pub fn resolve(folder: &Path) -> Resolution {
    resolve_with(folder, &crate::imports::list())
}

/// [`resolve`] with the imported packages given (the user's imports, see `imports`). Each imported
/// package's folder and search paths are on the path whether or not something requires it: the user
/// imported it for the designer, and its units are what the program uses.
pub fn resolve_with(folder: &Path, imported: &[PathBuf]) -> Resolution {
    let mut known = index(folder);
    let mut out = Resolution::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = Vec::new();
    for file in in_folder(folder) {
        if let Some(p) = read(&file) {
            if let Some(dir) = p.file.parent() {
                out.unit_paths.push(dir.to_path_buf());
            }
            queue.extend(p.requires);
        }
    }
    // A project that names a vendored library (database components, FPCUnit tests) needs its units on
    // the path, whether or not a package asks for them.
    for key in needed_vendored(folder) {
        let dirs = vendored(folder, key);
        if !dirs.is_empty() {
            out.packages.push(key.to_string());
            out.unit_paths.extend(dirs);
        }
    }
    for file in imported {
        let Some(p) = read(file) else { continue };
        known.entry(p.name.to_ascii_lowercase()).or_insert_with(|| file.clone());
        out.packages.push(p.name.clone());
        if let Some(dir) = file.parent() {
            out.unit_paths.push(dir.to_path_buf());
        }
        out.unit_paths.extend(p.unit_paths.clone());
        out.include_paths.extend(p.include_paths.clone());
        queue.extend(p.requires);
    }
    while let Some(name) = queue.pop() {
        let key = name.to_ascii_lowercase();
        if !seen.insert(key.clone()) || BUILT_IN.iter().any(|b| b.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let Some(file) = known.get(&key) else {
            let dirs = vendored(folder, &key);
            if dirs.is_empty() {
                out.missing.push(name);
            } else {
                out.packages.push(name.clone());
                out.unit_paths.extend(dirs);
            }
            continue;
        };
        let Some(p) = read(file) else { continue };
        out.packages.push(p.name.clone());
        if let Some(dir) = file.parent() {
            out.unit_paths.push(dir.to_path_buf());
        }
        out.unit_paths.extend(p.unit_paths);
        out.include_paths.extend(p.include_paths);
        queue.extend(p.requires);
    }
    out.unit_paths.sort();
    out.unit_paths.dedup();
    out.include_paths.sort();
    out.include_paths.dedup();
    out.missing.sort();
    out.missing.dedup();
    out
}

/// The vendored libraries a project needs, found by the names its Pascal units, programs and forms
/// use (`fcl-db` for database components; FPCUnit with the libraries its console runner uses).
pub fn needed_vendored(folder: &Path) -> Vec<&'static str> {
    const NEEDS: &[(&[&str], &[&str])] = &[
        (
            &["tdbgrid", "dbgrids", "dbctrls", "tdatasource", "tdataset", "tbufdataset", "tsqlconnector", "tsqlite3connection", "tsqlquery"],
            &["fcl-db"],
        ),
        (&["fpcunit", "consoletestrunner"], &["fcl-fpcunit", "fcl-base", "fcl-xml"]),
    ];
    let mut found: Vec<&'static str> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    collect_texts(folder, 0, &mut texts);
    for (names, keys) in NEEDS {
        if texts.iter().any(|t| names.iter().any(|n| t.contains(n))) {
            for key in *keys {
                if !found.contains(key) {
                    found.push(key);
                }
            }
        }
    }
    found
}

/// The lower-cased text of the Pascal units, programs and forms in `folder`, a few levels down.
/// Hidden folders, build output and bundles are skipped, as the project tree skips them.
fn collect_texts(dir: &Path, depth: usize, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            let hidden = name.starts_with('.') || ["lib", "target", "vendor", "node_modules"].contains(&name.as_str());
            if depth < 4 && !hidden && !name.ends_with(".app") {
                collect_texts(&p, depth + 1, out);
            }
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("pas" | "pp" | "lpr" | "lfm")) {
            if let Ok(text) = std::fs::read_to_string(&p) {
                out.push(text.to_ascii_lowercase());
            }
        }
    }
}

/// The source folders of a package the vendored FPC tree provides (not a `.lpk` in the project):
/// `libvlc` (FPC's libvlc binding) and `fcl-db` (the database units: DB, SQLdb, SQLite connections,
/// datasets). Empty when the name is neither. Found the way the other vendored sources are.
fn vendored(folder: &Path, key: &str) -> Vec<PathBuf> {
    // (folder under vendor/fpcsrc/packages, subfolders that hold the units)
    let (package, subdirs): (&str, &[&str]) = match key {
        "libvlc" => ("libvlc", &["src"]),
        "fcl-db" => ("fcl-db/src", &["base", "sqldb", "sqldb/sqlite", "sqldb/postgres", "sqlite", "sql", "dbase", "memds"]),
        "fcl-fpcunit" => ("fcl-fpcunit/src", &["."]),
        "fcl-base" => ("fcl-base/src", &["."]),
        "fcl-xml" => ("fcl-xml/src", &["."]),
        _ => return Vec::new(),
    };
    let Some(base) = pascoda_fpc::lcl::search_dirs(folder)
        .iter()
        .map(|d| d.join("vendor/fpcsrc/packages").join(package))
        .find(|d| d.is_dir())
    else {
        return Vec::new();
    };
    subdirs.iter().map(|sub| base.join(sub)).filter(|d| d.is_dir()).collect()
}

/// Builds every unit of a package, in the order it lists them, with `args` (the folder's arguments,
/// which include the resolved packages). fpc compiles the units a unit uses from source, so the order
/// does not matter. The result is the units' combined output and diagnostics.
pub fn build(pkg: &Package, args: &[std::ffi::OsString], mode: pascoda_fpc::Mode) -> pascoda_fpc::BuildResult {
    let mut raw = String::new();
    let mut diagnostics = Vec::new();
    let mut success = true;
    for unit in &pkg.units {
        let label = unit.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if !unit.is_file() {
            raw.push_str(&format!("--- {label}: the file is missing\n"));
            success = false;
            continue;
        }
        match pascoda_fpc::build_progress_with(unit, mode, args, None, &mut |_| {}) {
            Ok(r) => {
                raw.push_str(&format!("--- {label}\n{}", r.raw_output));
                success &= r.success;
                diagnostics.extend(r.diagnostics);
            }
            Err(e) => {
                raw.push_str(&format!("--- {label}: could not start the compiler: {e}\n"));
                success = false;
            }
        }
    }
    pascoda_fpc::BuildResult { success, diagnostics, raw_output: raw }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delphi_package_gives_its_name_requires_and_units() {
        let text = "package Shapes;\n// a comment with requires inside\n{$R *.res}\nrequires\n  rtl,\n  Helper; {old: Other}\ncontains\n  circle in 'src\\circle.pas',\n  square;\nend.\n";
        let p = parse_dpk(text, Path::new("/w/shapes/Shapes.dpk"));
        assert_eq!(p.name, "Shapes");
        assert_eq!(p.requires, ["rtl", "Helper"]);
        assert_eq!(
            p.units,
            [PathBuf::from("/w/shapes/src/circle.pas"), PathBuf::from("/w/shapes/square.pas")]
        );
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;

    #[test]
    fn an_imported_package_joins_the_path_from_outside_the_project() {
        let base = std::env::temp_dir().join(format!("pascoda-import-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let project = base.join("project");
        let lib = base.join("components/shapes");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(lib.join("src")).unwrap();
        std::fs::write(lib.join("src/shapes.pas"), "unit shapes;\ninterface\nimplementation\nend.\n").unwrap();
        let lpk = lib.join("shapes.lpk");
        std::fs::write(
            &lpk,
            r#"<CONFIG><Package Version="4"><Name Value="shapes"/>
              <Files Count="1"><Item1><Filename Value="src/shapes.pas"/></Item1></Files>
              <RequiredPkgs Count="1"><Item1><PackageName Value="LCL"/></Item1></RequiredPkgs>
              <CompilerOptions><SearchPaths><OtherUnitFiles Value="src"/></SearchPaths></CompilerOptions>
              </Package></CONFIG>"#,
        )
        .unwrap();
        let without = resolve_with(&project, &[]);
        assert!(!without.unit_paths.iter().any(|d| d.ends_with("shapes/src")), "not imported: not on the path");
        let with = resolve_with(&project, &[lpk.clone()]);
        assert!(with.unit_paths.contains(&lib), "the package's folder is on the path: {:?}", with.unit_paths);
        assert!(with.unit_paths.iter().any(|d| d.ends_with("shapes/src")), "and its search path");
        assert_eq!(with.packages, ["shapes"]);
        assert!(with.missing.is_empty(), "LCL is built in: {:?}", with.missing);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod vendored_tests {
    use super::*;

    #[test]
    fn the_fcl_db_folders_are_on_the_path_for_a_package_that_requires_fcl_db() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-data");
        let dirs = vendored(&project, "fcl-db");
        if dirs.is_empty() {
            return; // the vendored FPC sources are not checked out
        }
        assert!(dirs.iter().any(|d| d.ends_with("base")), "{dirs:?}");
        assert!(dirs.iter().any(|d| d.ends_with("sqldb/sqlite")), "{dirs:?}");
        assert!(vendored(&project, "no-such-package").is_empty());
    }
}

#[cfg(test)]
mod database_path_tests {
    use super::*;

    fn mentions_database(folder: &Path) -> bool {
        needed_vendored(folder).contains(&"fcl-db")
    }

    #[test]
    fn a_project_that_names_fpcunit_gets_the_test_libraries() {
        let dir = std::env::temp_dir().join(format!("pascoda-fpcunit-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tests.pas"), "unit tests; interface uses fpcunit; implementation end.").unwrap();
        let needed = needed_vendored(&dir);
        assert!(needed.contains(&"fcl-fpcunit") && needed.contains(&"fcl-base") && needed.contains(&"fcl-xml"), "{needed:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_that_names_a_database_component_gets_the_database_units() {
        let base = std::env::temp_dir().join(format!("pascoda-db-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let plain = base.join("plain");
        let db = base.join("db");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(plain.join("unit1.lfm"), "object Form1: TForm1\nend\n").unwrap();
        std::fs::write(db.join("unit1.lfm"), "object Form1: TForm1\n  object Grid1: TDBGrid\n  end\nend\n").unwrap();
        assert!(!mentions_database(&plain));
        assert!(mentions_database(&db));
        if vendored(&db, "fcl-db").is_empty() {
            let _ = std::fs::remove_dir_all(&base);
            return; // the vendored FPC database sources are not checked out
        }
        assert!(resolve_with(&db, &[]).unit_paths.iter().any(|d| d.ends_with("sqldb/sqlite")));
        assert!(!resolve_with(&plain, &[]).unit_paths.iter().any(|d| d.ends_with("sqldb/sqlite")));
        let _ = std::fs::remove_dir_all(&base);
    }
}
