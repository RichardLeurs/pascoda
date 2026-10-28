//! Project model and templates for Pascoda.

use pascoda_fpc::Mode;
use std::path::{Path, PathBuf};

/// The project's settings file, in the project folder.
pub const SETTINGS_FILE: &str = "pascoda.toml";

/// The value of `key = "value"` in `pascoda.toml` (quotes and a trailing `# comment` removed).
fn get_key(root: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(root.join(SETTINGS_FILE)).ok()?;
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| {
            let v = v.trim();
            // a quoted value may hold '#'; an unquoted one ends at it
            match v.strip_prefix('"').and_then(|r| r.rsplit_once('"')) {
                Some((inner, _)) => inner.replace("\\\"", "\""),
                None => v.split('#').next().unwrap_or("").trim().to_string(),
            }
        })
    })
}

/// Sets `key` (an empty value removes the line), keeping every other line.
fn set_key(root: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let path = root.join(SETTINGS_FILE);
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let line = format!("{key} = \"{}\"", value.replace('"', "\\\""));
    let is_key = |l: &str| l.split_once('=').is_some_and(|(k, _)| k.trim() == key);
    let mut found = false;
    let mut lines: Vec<String> = Vec::new();
    for l in old.lines() {
        if is_key(l) {
            found = true;
            if !value.is_empty() {
                lines.push(line.clone());
            }
        } else {
            lines.push(l.to_string());
        }
    }
    if !found && !value.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

/// The language mode of the project in `root`: `mode = "objfpc"` or `"delphi"` in
/// `pascoda.toml`; ObjFPC when the file, the line or the value is missing.
pub fn load_mode(root: &Path) -> Mode {
    get_key(root, "mode")
        .and_then(|v| Mode::from_name(&v))
        .unwrap_or_default()
}

/// Writes the mode into `pascoda.toml`, keeping every other line of an existing file.
pub fn save_mode(root: &Path, mode: Mode) -> std::io::Result<()> {
    set_key(root, "mode", mode.name())
}

/// What the export dialog remembers for a project (all optional; empty = default).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSettings {
    pub app_name: String,
    pub bundle_id: String,
    pub version: String,
    /// A `.icns` or `.png`, absolute or relative to the project folder.
    pub icon: String,
}

pub fn load_export(root: &Path) -> ExportSettings {
    let g = |k| get_key(root, k).unwrap_or_default();
    ExportSettings {
        app_name: g("app_name"),
        bundle_id: g("bundle_id"),
        version: g("version"),
        icon: g("icon"),
    }
}

pub fn save_export(root: &Path, s: &ExportSettings) -> std::io::Result<()> {
    set_key(root, "app_name", s.app_name.trim())?;
    set_key(root, "bundle_id", s.bundle_id.trim())?;
    set_key(root, "version", s.version.trim())?;
    set_key(root, "icon", s.icon.trim())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub main_source: PathBuf,
}

/// A name that is safe as a Pascal program name and a folder name.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The first of `GuiApp1`, `GuiApp2`, ... that does not exist in `parent`.
pub fn free_project_name(parent: &Path) -> String {
    (1..)
        .map(|n| format!("GuiApp{n}"))
        .find(|name| !parent.join(name).exists())
        .expect("an unused name exists")
}

/// Creates `<parent>/<name>/` holding a GUI program (`<name>.lpr`), one form unit
/// (`unit1.pas` + `unit1.lfm`). Returns the program file. Refuses to touch an existing folder.
pub fn create_gui_app(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    if !is_valid_name(name) {
        return Err(std::io::Error::other(format!(
            "`{name}` is not a valid project name"
        )));
    }
    let dir = parent.join(name);
    if dir.exists() {
        return Err(std::io::Error::other(format!(
            "{} already exists",
            dir.display()
        )));
    }
    std::fs::create_dir_all(&dir)?;
    let lpr = dir.join(format!("{name}.lpr"));
    std::fs::write(&lpr, program_source(name))?;
    std::fs::write(dir.join("unit1.pas"), UNIT1_PAS)?;
    std::fs::write(dir.join("unit1.lfm"), UNIT1_LFM)?;
    Ok(lpr)
}

fn program_source(name: &str) -> String {
    format!(
        "program {name};\n\n{{$mode objfpc}}{{$H+}}\n\nuses\n  Interfaces, // this includes the LCL widgetset\n  Forms, Unit1;\n\nbegin\n  RequireDerivedFormResource := True;\n  Application.Scaled := True;\n  Application.Initialize;\n  Application.CreateForm(TForm1, Form1);\n  Application.Run;\nend.\n"
    )
}

const UNIT1_PAS: &str = "unit Unit1;\n\n{$mode objfpc}{$H+}\n\ninterface\n\nuses\n  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls;\n\ntype\n  TForm1 = class(TForm)\n  end;\n\nvar\n  Form1: TForm1;\n\nimplementation\n\n{$R *.lfm}\n\nend.\n";

const UNIT1_LFM: &str = "object Form1: TForm1\n  Left = 300\n  Height = 300\n  Top = 200\n  Width = 400\n  Caption = 'Form1'\nend\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(is_valid_name("GuiApp1") && is_valid_name("My_app"));
        assert!(
            !is_valid_name("")
                && !is_valid_name("1app")
                && !is_valid_name("my app")
                && !is_valid_name("a/b")
        );
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-mode-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_to_objfpc_and_round_trips() {
        let d = tmp("rt");
        assert_eq!(load_mode(&d), Mode::ObjFpc);
        save_mode(&d, Mode::Delphi).unwrap();
        assert_eq!(load_mode(&d), Mode::Delphi);
        save_mode(&d, Mode::ObjFpc).unwrap();
        assert_eq!(load_mode(&d), Mode::ObjFpc);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn other_lines_survive_and_odd_values_fall_back() {
        let d = tmp("keep");
        std::fs::write(d.join(SETTINGS_FILE), "name = \"x\"\nmode = \"bogus\"\n").unwrap();
        assert_eq!(load_mode(&d), Mode::ObjFpc);
        save_mode(&d, Mode::Delphi).unwrap();
        let t = std::fs::read_to_string(d.join(SETTINGS_FILE)).unwrap();
        assert_eq!(t, "name = \"x\"\nmode = \"delphi\"\n");
        std::fs::write(d.join(SETTINGS_FILE), "  mode=Delphi # comment\n").unwrap();
        assert_eq!(load_mode(&d), Mode::Delphi);
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn export_settings_round_trip_beside_the_mode() {
        let d = std::env::temp_dir().join(format!("pascoda-exp-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(load_export(&d), ExportSettings::default());
        save_mode(&d, Mode::Delphi).unwrap();
        let s = ExportSettings {
            app_name: "My \"Notes\" # app".into(),
            bundle_id: "com.example.notes".into(),
            version: "1.2".into(),
            icon: "art/icon.png".into(),
        };
        save_export(&d, &s).unwrap();
        assert_eq!(load_export(&d), s);
        assert_eq!(load_mode(&d), Mode::Delphi, "the mode line is untouched");
        // emptying a field removes its line
        save_export(&d, &ExportSettings::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join(SETTINGS_FILE)).unwrap(),
            "mode = \"delphi\"\n"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
