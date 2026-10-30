//! Project options in a window (Build > Project Options...): the language mode, defines, unit and
//! include search paths, compiler switches, and the target the build makes. They are kept in the
//! project's `pascoda.toml`, which the builds read each time they start. UI-free logic here; the
//! window is drawn by `shell.rs`.

use pascoda_fpc::Mode;
use pascoda_project::CompilerSettings;
use std::path::Path;

/// The text of each list, one entry per line, as the window shows it. Blank lines and repeats are
/// dropped when the text is read back.
pub fn lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|o| o == line) {
            out.push(line.to_string());
        }
    }
    out
}

/// The list as the window's text: one entry per line.
pub fn text(list: &[String]) -> String {
    list.join("\n")
}

/// What the project file (`.lpi` or `.dproj`) gives a build, for the window to show read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    /// The project file's name, as the section heading names it.
    pub file_name: String,
    /// Label and value rows, in the order the window shows them.
    pub rows: Vec<(String, String)>,
    /// Entries the build leaves out (macros, Windows-only defines, options Pascoda sets itself).
    pub skipped: Vec<String>,
}

/// The project file of the project in `root`, read as the builds read it (`None` when there is none
/// or it does not read). Paths are shown relative to the project's folder.
pub fn file_view(root: &Path) -> Option<FileView> {
    let file = pascoda_project::foreign::find(root)?;
    let f = pascoda_project::foreign::read(&file).ok()?;
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let list = |items: Vec<String>| if items.is_empty() { "none".to_string() } else { items.join(", ") };
    let rows = vec![
        ("Main program".to_string(), f.main.as_deref().map_or("none".to_string(), rel)),
        (
            "Mode".to_string(),
            match f.mode {
                Some(pascoda_fpc::Mode::Delphi) => "Delphi".to_string(),
                Some(pascoda_fpc::Mode::ObjFpc) => "ObjFPC".to_string(),
                None => "not set (builds use ObjFPC)".to_string(),
            },
        ),
        ("Unit paths (-Fu)".to_string(), list(f.unit_paths.iter().map(|p| rel(p)).collect())),
        ("Include paths (-Fi)".to_string(), list(f.include_paths.iter().map(|p| rel(p)).collect())),
        ("Defines (-d)".to_string(), list(f.defines.clone())),
        ("Switches".to_string(), list(f.switches.clone())),
        ("Target (-o)".to_string(), f.target.as_deref().map_or("none".to_string(), rel)),
        ("Unit output (-FU)".to_string(), f.unit_output.as_deref().map_or("none".to_string(), rel)),
    ];
    Some(FileView {
        file_name: file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        rows,
        skipped: f.skipped,
    })
}

#[derive(Default)]
pub struct Editor {
    pub open: bool,
    pub mode: Mode,
    pub defines: String,
    pub units: String,
    pub includes: String,
    pub switches: String,
    /// The `output` key: the executable's path, relative to the project. Empty for the default.
    pub output: String,
    /// The executable the build makes without an `output` (shown as the default).
    pub default_target: String,
    /// Why the last save was refused, if it was.
    pub error: String,
    /// The project file's values, shown read-only.
    pub project_file: Option<FileView>,
    /// The program's arguments, one per line, and the folder it runs in (empty: its own folder).
    pub args: String,
    pub workdir: String,
    /// The build configuration Run builds (Debug builds the debug configuration either way).
    pub configuration: pascoda_project::Configuration,
}

impl Editor {
    /// The window's values, read from the project in `root`. `default_target` is the executable the
    /// build makes without an `output`, or a note when there is none yet.
    pub fn load(root: &Path, default_target: String) -> Self {
        let settings = pascoda_project::load_compiler(root);
        Self {
            open: true,
            mode: pascoda_project::mode_for(root),
            defines: text(&settings.defines),
            units: text(&settings.unit_paths),
            includes: text(&settings.include_paths),
            switches: text(&settings.switches),
            output: pascoda_project::load_output(root).unwrap_or_default(),
            default_target,
            error: String::new(),
            project_file: file_view(root),
            configuration: pascoda_project::load_configuration(root),
            args: text(&pascoda_project::load_run(root).args),
            workdir: pascoda_project::load_run(root).workdir,
        }
    }

    /// The compiler settings the window holds.
    pub fn settings(&self) -> CompilerSettings {
        CompilerSettings {
            defines: lines(&self.defines),
            unit_paths: lines(&self.units),
            include_paths: lines(&self.includes),
            switches: lines(&self.switches),
        }
    }

    /// What is wrong with the values, but does not stop a save: folders that do not exist (the build
    /// leaves them out) and switches a project file may not pass (ignored).
    pub fn warnings(&self, root: &Path) -> Vec<String> {
        let settings = self.settings();
        let mut out = Vec::new();
        for (paths, what) in [(&settings.unit_paths, "unit"), (&settings.include_paths, "include")] {
            for p in paths {
                if !root.join(p.replace('\\', "/")).is_dir() {
                    out.push(format!("The {what} folder “{p}” does not exist, so the build leaves it out"));
                }
            }
        }
        for s in &settings.switches {
            if !pascoda_project::foreign::is_allowed_switch(s) {
                out.push(format!("“{s}” is not a switch a project may pass, so the build ignores it"));
            }
        }
        let workdir = self.workdir.trim();
        if !workdir.is_empty() && !root.join(workdir.replace('\\', "/")).is_dir() {
            out.push(format!("The working folder “{workdir}” does not exist, so the program would not start"));
        }
        let output = self.output.trim();
        if !output.is_empty() && root.join(output).parent().is_some_and(|d| !d.is_dir()) {
            out.push("The folder for the output does not exist; the build does not create it".to_string());
        }
        out
    }

    /// Writes the values into the project's `pascoda.toml`, keeping its other lines.
    pub fn save(&self, root: &Path) -> Result<(), String> {
        pascoda_project::save_compiler(root, &self.settings()).map_err(|e| e.to_string())?;
        pascoda_project::save_output(root, &self.output).map_err(|e| e.to_string())?;
        pascoda_project::save_configuration(root, self.configuration).map_err(|e| e.to_string())?;
        pascoda_project::save_run(
            root,
            &pascoda_project::RunSettings { args: lines(&self.args), workdir: self.workdir.trim().to_string() },
        )
        .map_err(|e| e.to_string())?;
        pascoda_project::save_mode(root, self.mode).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-proj-opts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_project_file_is_shown_as_the_build_reads_it() {
        let root = tmp("lpi");
        std::fs::write(
            root.join("app.lpi"),
            r#"<CONFIG><CompilerOptions>
  <SearchPaths><OtherUnitFiles Value="frames;$(LazarusDir)/lcl"/></SearchPaths>
  <Parsing><SyntaxOptions><SyntaxMode Value="Delphi"/></SyntaxOptions></Parsing>
  <Other><CustomOptions Value="-dFOO -O2"/></Other>
</CompilerOptions></CONFIG>"#,
        )
        .unwrap();
        let view = file_view(&root).expect("the .lpi is read");
        assert_eq!(view.file_name, "app.lpi");
        let row = |label: &str| view.rows.iter().find(|(l, _)| l == label).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(row("Mode"), "Delphi");
        assert_eq!(row("Defines (-d)"), "FOO");
        assert_eq!(row("Switches"), "-O2");
        assert!(view.skipped.iter().any(|s| s.contains("LazarusDir")), "{:?}", view.skipped);
        // The window also gets it through the editor.
        assert!(Editor::load(&root, String::new()).project_file.is_some());
        let _ = std::fs::remove_dir_all(&root);
        let bare = tmp("nolpi");
        assert!(file_view(&bare).is_none(), "no project file, nothing to show");
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn run_arguments_and_the_working_folder_are_saved_and_checked() {
        let root = tmp("run");
        std::fs::create_dir_all(root.join("data")).unwrap();
        let e = Editor {
            args: "--in\nmy file.txt".into(),
            workdir: "data".into(),
            ..Editor::default()
        };
        assert!(e.warnings(&root).is_empty());
        e.save(&root).unwrap();
        let back = Editor::load(&root, String::new());
        assert_eq!(back.args, "--in\nmy file.txt");
        assert_eq!(back.workdir, "data");

        let missing = Editor { workdir: "nope".into(), ..Editor::default() };
        assert!(missing.warnings(&root).iter().any(|w| w.contains("working folder “nope”")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lines_are_trimmed_and_unique() {
        assert_eq!(lines("  DEBUG \n\n lib \nDEBUG\n"), ["DEBUG", "lib"]);
        assert_eq!(lines(""), Vec::<String>::new());
        assert_eq!(text(&["a".into(), "b".into()]), "a\nb");
    }

    #[test]
    fn options_round_trip_through_pascoda_toml_and_keep_its_other_lines() {
        let root = tmp("round");
        std::fs::write(root.join(pascoda_project::SETTINGS_FILE), "app_name = \"notes\"\n").unwrap();
        let mut e = Editor::load(&root, "notes".into());
        assert_eq!(e.mode, Mode::ObjFpc);
        e.mode = Mode::Delphi;
        e.defines = "DEBUG\nFAST\n".into();
        e.units = "lib".into();
        e.switches = "-O2".into();
        e.save(&root).unwrap();

        let text = std::fs::read_to_string(root.join(pascoda_project::SETTINGS_FILE)).unwrap();
        assert!(text.contains("app_name = \"notes\""), "{text}");
        let back = Editor::load(&root, String::new());
        assert_eq!(back.mode, Mode::Delphi);
        assert_eq!(back.settings(), e.settings());
        assert_eq!(back.settings().defines, ["DEBUG", "FAST"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_output_is_saved_and_a_missing_folder_for_it_is_warned_about() {
        let root = tmp("output");
        let mut e = Editor { output: "bin/app".into(), ..Editor::default() };
        assert!(e.warnings(&root).iter().any(|w| w.contains("output")), "the bin folder is missing");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        assert!(e.warnings(&root).is_empty());
        e.save(&root).unwrap();
        assert_eq!(Editor::load(&root, String::new()).output, "bin/app");
        e.output.clear();
        e.save(&root).unwrap();
        assert_eq!(Editor::load(&root, String::new()).output, "", "cleared");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn warnings_name_missing_folders_and_refused_switches() {
        let root = tmp("warn");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        let e = Editor {
            units: "lib\nnope".into(),
            includes: "missing".into(),
            switches: "-O2\n-o/tmp/x".into(),
            ..Editor::default()
        };
        let w = e.warnings(&root);
        assert!(w.iter().any(|m| m.contains("unit folder “nope”")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("include folder “missing”")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("“-o/tmp/x”")), "{w:?}");
        assert!(!w.iter().any(|m| m.contains("lib") || m.contains("-O2")), "{w:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
