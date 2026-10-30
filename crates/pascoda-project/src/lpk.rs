//! Lazarus packages (`.lpk`), read: the package's name, the units it holds, the packages it requires
//! and its search paths. Nothing is written back, and macros such as `$(LazarusDir)` are left out
//! (as the project files leave them out). Resolving the required packages so that their units are
//! found is a separate step (item 13 of the specs).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Package {
    /// The package file itself.
    pub file: PathBuf,
    pub name: String,
    /// The unit files the package holds (`.pas` and `.pp`), relative paths resolved against the
    /// package's folder.
    pub units: Vec<PathBuf>,
    /// The units that have a register procedure (`HasRegisterProc`): where the components are
    /// named to the IDE. A subset of `units`.
    pub register_units: Vec<PathBuf>,
    /// The packages it requires, by name (`LCL`, `FCL`, ...).
    pub requires: Vec<String>,
    /// Folders for `-Fu` and `-Fi`.
    pub unit_paths: Vec<PathBuf>,
    pub include_paths: Vec<PathBuf>,
}

/// A `;`-separated list of folders or files from a package, resolved against `dir`. Entries with a
/// macro are left out.
fn split(dir: &Path, list: &str) -> Vec<PathBuf> {
    list.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains('$'))
        .map(|s| dir.join(s.replace('\\', "/")))
        .collect()
}

fn value<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    node.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.attribute("Value"))
}

/// The first of `tags` that the node has, as its `Value`. Lazarus writes `FileName` in older
/// packages and `Filename` in current ones (`lazvlc.lpk`), so both are accepted.
fn value_of<'a>(node: roxmltree::Node<'a, 'a>, tags: &[&str]) -> Option<&'a str> {
    tags.iter().find_map(|tag| value(node, tag))
}

/// Reads a package from the text of its `.lpk` file.
pub fn parse_lpk(xml: &str, file: &Path) -> Result<Package, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("{}: {e}", file.display()))?;
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut p = Package { file: file.to_path_buf(), ..Package::default() };
    if let Some(package) = doc.descendants().find(|n| n.has_tag_name("Package")) {
        if let Some(name) = value(package, "Name") {
            p.name = name.to_string();
        }
    }
    for item in doc
        .descendants()
        .find(|n| n.has_tag_name("Files"))
        .into_iter()
        .flat_map(|files| files.children().filter(|c| c.is_element()))
    {
        let Some(name) = value_of(item, &["FileName", "Filename"]) else { continue };
        let is_unit = matches!(
            Path::new(name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
            Some("pas" | "pp")
        );
        if is_unit && !name.contains('$') {
            let unit = dir.join(name.replace('\\', "/"));
            if value(item, "HasRegisterProc").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
                p.register_units.push(unit.clone());
            }
            p.units.push(unit);
        }
    }
    for item in doc
        .descendants()
        .find(|n| n.has_tag_name("RequiredPkgs"))
        .into_iter()
        .flat_map(|reqs| reqs.children().filter(|c| c.is_element()))
    {
        if let Some(name) = value(item, "PackageName") {
            if !name.contains('$') {
                p.requires.push(name.to_string());
            }
        }
    }
    if let Some(paths) = doc.descendants().find(|n| n.has_tag_name("SearchPaths")) {
        if let Some(list) = value(paths, "OtherUnitFiles") {
            p.unit_paths = split(dir, list);
        }
        if let Some(list) = value(paths, "IncludeFiles") {
            p.include_paths = split(dir, list);
        }
    }
    Ok(p)
}

/// Reads the package at `file`, a `.lpk`.
pub fn read(file: &Path) -> Result<Package, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    parse_lpk(&text, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LPK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CONFIG>
  <Package Version="5">
    <Name Value="shapes"/>
    <Files Count="3">
      <Item1><FileName Value="shapes.pas"/><Type Value="Unit"/><UnitName Value="shapes"/></Item1>
      <Item2><FileName Value="src/circle.pp"/><Type Value="Unit"/></Item2>
      <Item3><FileName Value="readme.txt"/></Item3>
    </Files>
    <RequiredPkgs Count="2">
      <Item1><PackageName Value="LCL"/></Item1>
      <Item2><PackageName Value="$(LazarusDir)/x"/></Item2>
    </RequiredPkgs>
    <CompilerOptions>
      <SearchPaths>
        <IncludeFiles Value="inc"/>
        <OtherUnitFiles Value="src;../common;$(LazarusDir)/lcl"/>
      </SearchPaths>
    </CompilerOptions>
  </Package>
</CONFIG>"#;

    #[test]
    fn reads_the_name_units_requirements_and_search_paths() {
        let file = Path::new("/work/shapes/shapes.lpk");
        let p = parse_lpk(LPK, file).unwrap();
        assert_eq!(p.name, "shapes");
        assert_eq!(p.units, [PathBuf::from("/work/shapes/shapes.pas"), PathBuf::from("/work/shapes/src/circle.pp")]);
        assert_eq!(p.requires, ["LCL"], "a macro requirement is left out");
        assert_eq!(p.unit_paths, [PathBuf::from("/work/shapes/src"), PathBuf::from("/work/shapes/../common")]);
        assert_eq!(p.include_paths, [PathBuf::from("/work/shapes/inc")]);
        assert_eq!(p.file, file);
        assert!(parse_lpk("<CONFIG>", file).is_err(), "a broken file is an error");
    }

    #[test]
    fn current_packages_use_filename_and_mark_their_register_units() {
        let xml = r#"<CONFIG><Package Version="4"><Name Value="vlcish"/>
          <Files Count="3">
            <Item1><Filename Value="reg.pas"/><HasRegisterProc Value="True"/><UnitName Value="reg"/></Item1>
            <Item2><Filename Value="player.pp"/><UnitName Value="player"/></Item2>
            <Item3><Filename Value="res.res"/></Item3>
          </Files></Package></CONFIG>"#;
        let p = parse_lpk(xml, Path::new("/work/vlcish/vlcish.lpk")).unwrap();
        assert_eq!(p.units, [PathBuf::from("/work/vlcish/reg.pas"), PathBuf::from("/work/vlcish/player.pp")]);
        assert_eq!(p.register_units, [PathBuf::from("/work/vlcish/reg.pas")]);
    }

    #[test]
    fn the_vendored_lazvlc_package_lists_its_units_and_register_unit() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/lazarus/components/vlc/lazvlc.lpk");
        if !file.is_file() {
            return; // the vendored Lazarus tree is not checked out
        }
        let p = read(&file).unwrap();
        assert_eq!(p.name, "lazvlc");
        let names: Vec<String> = p.units.iter().map(|u| u.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["reglazvlc.pas", "lclvlc.pp"]);
        let reg: Vec<String> = p.register_units.iter().map(|u| u.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(reg, ["reglazvlc.pas"]);
    }
}
