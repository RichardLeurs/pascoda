//! Palette entries from imported component packages (`.lpk` files, see `pascoda_project::imports`).
//! A package's register units name its components with `RegisterComponents('Page', [...])`; each
//! page becomes a palette page and each class an entry. Whether a class is non-visual is read from
//! the base classes declared in the package's own units: a class that reaches `TComponent` without
//! a control in between is non-visual; one whose base the package does not declare is treated as
//! visual.

use super::{Entry, intern};
use pascoda_forms::sync::{declared_classes, registered_components};
use pascoda_project::lpk::Package;
use std::collections::HashMap;
use std::path::PathBuf;

/// The palette entries the packages offer. `dirs` are extra folders whose units are scanned for
/// classes (a package's search paths, such as the vendored database units): their base classes
/// decide non-visual, and the unit that declares a class is the one a form must `use`.
pub fn entries(packages: &[Package], dirs: &[PathBuf]) -> Vec<Entry> {
    // Every class declared in the packages' units and the folders: lower(name) -> (base, unit).
    let mut files: Vec<PathBuf> = packages.iter().flat_map(|p| p.units.iter().cloned()).collect();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else { continue };
        files.extend(read.flatten().map(|e| e.path()).filter(|p| {
            matches!(p.extension().and_then(|e| e.to_str()), Some("pas" | "pp"))
        }));
    }
    let mut declared: HashMap<String, (String, String)> = HashMap::new();
    for file in files {
        let (Ok(text), Some(unit)) = (std::fs::read_to_string(&file), file.file_stem()) else { continue };
        let unit = unit.to_string_lossy().into_owned();
        for (name, base) in declared_classes(&text) {
            declared.entry(name.to_ascii_lowercase()).or_insert((base, unit.clone()));
        }
    }
    let mut out = Vec::new();
    for p in packages {
        for reg in &p.register_units {
            let Ok(text) = std::fs::read_to_string(reg) else { continue };
            let reg_unit = reg.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            for (page, classes) in registered_components(&text) {
                for class in classes {
                    let unit = declared
                        .get(&class.to_ascii_lowercase())
                        .map_or_else(|| reg_unit.clone(), |(_, u)| u.clone());
                    let nonvisual = non_visual(&class, &declared);
                    out.push(Entry {
                        class: intern(&class),
                        text: "",
                        unit: intern(&unit),
                        page: intern(&page),
                        nonvisual,
                    });
                }
            }
        }
    }
    out
}

/// Reads the imported packages and makes their components the palette's. Run at start-up and
/// after the imports change. The folders the packages resolve to are scanned for base classes.
pub fn refresh() {
    let imports = pascoda_project::imports::list();
    let packages: Vec<Package> = imports
        .iter()
        .filter_map(|file| pascoda_project::packages::read(file))
        .collect();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for p in &packages {
        if let Some(folder) = p.file.parent() {
            dirs.extend(pascoda_project::packages::resolve_with(folder, &imports).unit_paths);
        }
    }
    dirs.sort();
    dirs.dedup();
    let found = entries(&packages, &dirs);
    crate::icons::set_icon_files(icon_files(&packages, &found));
    super::set_imported(found);
}

/// The icon file of each imported class: `<class>.png` in its package's folder, when there is one.
pub fn icon_files(packages: &[Package], found: &[Entry]) -> Vec<(String, PathBuf)> {
    found
        .iter()
        .filter_map(|e| {
            packages.iter().find_map(|p| {
                let png = p.file.parent()?.join(format!("{}.png", e.class.to_ascii_lowercase()));
                png.is_file().then(|| (e.class.to_string(), png))
            })
        })
        .collect()
}

/// Whether `class` derives from `TComponent` without a visual control in its base chain. A base the
/// scan did not find ends the walk as visual.
fn non_visual(class: &str, declared: &HashMap<String, (String, String)>) -> bool {
    let mut current = class.to_ascii_lowercase();
    for _ in 0..32 {
        let Some((base, _)) = declared.get(&current) else { return false };
        let base = base.to_ascii_lowercase();
        if base == "tcomponent" {
            return true;
        }
        if ["tcontrol", "twincontrol", "tgraphiccontrol", "tcustomcontrol"].contains(&base.as_str()) {
            return false;
        }
        current = base;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The imported entries are process-wide, so the tests that set them take this lock.
    static SET: Mutex<()> = Mutex::new(());

    fn temp_package(name: &str) -> (Package, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pascoda-designer-imports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("players.pas"),
            "unit players;\n{$mode objfpc}\ninterface\nuses Classes, Controls;\ntype\n  TPlayer = class(TComponent)\n  end;\n  TPlayerList = class(TPlayer)\n  end;\n  TVideoPanel = class(TCustomControl)\n  end;\nimplementation\nend.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("reg.pas"),
            "unit reg;\n{$mode objfpc}\ninterface\nprocedure Register;\nimplementation\nuses Classes, players;\nprocedure Register;\nbegin\n  RegisterComponents('Video',[TPlayer,TPlayerList,TVideoPanel]);\nend;\nend.\n",
        )
        .unwrap();
        let p = Package {
            file: dir.join("video.lpk"),
            name: "video".into(),
            units: vec![dir.join("players.pas"), dir.join("reg.pas")],
            register_units: vec![dir.join("reg.pas")],
            ..Package::default()
        };
        (p, dir)
    }

    #[test]
    fn a_package_gives_its_page_and_classes_with_their_units() {
        let (p, dir) = temp_package("entries");
        let got = entries(&[p], &[]);
        let names: Vec<(&str, &str, &str, bool)> = got.iter().map(|e| (e.page, e.class, e.unit, e.nonvisual)).collect();
        assert_eq!(
            names,
            [
                ("Video", "TPlayer", "players", true),
                ("Video", "TPlayerList", "players", true),
                ("Video", "TVideoPanel", "players", false),
            ],
            "TPlayer and TPlayerList reach TComponent (non-visual); TVideoPanel is a control"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imported_classes_are_known_to_the_designer_once_set() {
        let _guard = SET.lock().unwrap_or_else(|e| e.into_inner());
        let (p, dir) = temp_package("lookups");
        super::super::set_imported(entries(&[p], &[]));
        assert!(super::super::is_nonvisual("TPlayer"), "the tray, not the form");
        assert!(!super::super::is_nonvisual("TVideoPanel"));
        assert_eq!(super::super::units_for(&pascoda_forms::lfm::parse("object F: TForm\n  object V: TPlayer\n  end\nend\n").unwrap(), "unit1", &Default::default()), ["players"]);
        assert!(super::super::imported_entries().iter().any(|e| e.class == "TPlayer"));
        super::super::set_imported(Vec::new());
        assert!(!super::super::is_nonvisual("TPlayer"), "unknown again once the imports are cleared");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn placing_an_imported_component_registers_its_class_in_the_unit() {
        let _guard = SET.lock().unwrap_or_else(|e| e.into_inner());
        let (p, package_dir) = temp_package("register");
        let dir = std::env::temp_dir().join(format!("pascoda-imported-register-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("unit1.pas"),
            "unit unit1;\n\n{$mode objfpc}{$H+}\n\ninterface\n\nuses\n  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls;\n\ntype\n  TForm1 = class(TForm)\n  end;\n\nvar\n  Form1: TForm1;\n\nimplementation\n\n{$R *.lfm}\n\nend.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("unit1.lfm"),
            "object Form1: TForm1\n  Left = 300\n  Top = 200\n  Width = 400\n  Height = 300\nend\n",
        )
        .unwrap();
        super::super::set_imported(entries(&[p], &[]));
        let mut d = super::super::Designer::open(&dir.join("unit1.pas")).unwrap();
        d.add_component("TPlayer", &[], 0, 0).unwrap();
        let mut unit = std::fs::read_to_string(dir.join("unit1.pas")).unwrap();
        d.sync_unit(&mut unit);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        assert!(unit.contains("RegisterClass(TPlayer);"), "the class is registered: {unit}");
        assert!(unit.contains("initialization"), "{unit}");
        super::super::set_imported(Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&package_dir);
    }

    #[test]
    fn a_package_folder_icon_is_used_for_its_class() {
        let pkg = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-vlc/PascodaVlc.lpk");
        let package = pascoda_project::packages::read(&pkg).expect("the VLC package reads");
        let found = entries(std::slice::from_ref(&package), &[]);
        let icons = icon_files(std::slice::from_ref(&package), &found);
        let for_player = icons.iter().find(|(c, _)| c == "TPascodaVlcPlayer").expect("the player has an icon");
        assert!(for_player.1.ends_with("tpascodavlcplayer.png"), "{:?}", for_player.1);
        assert!(icons.iter().any(|(c, _)| c == "TPascodaVlcListPlayer"));
    }
}
