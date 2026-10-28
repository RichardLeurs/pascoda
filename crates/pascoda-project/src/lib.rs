//! Project model and templates for Pascoda.

use std::path::{Path, PathBuf};

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
