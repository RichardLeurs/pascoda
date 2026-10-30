//! Project model and templates for Pascoda.

pub mod foreign;
pub mod imports;
pub mod lpk;
pub mod packages;
pub mod program;

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
    let line = (!value.is_empty()).then(|| format!("{key} = \"{}\"", value.replace('"', "\\\"")));
    set_line(root, key, line)
}

/// The text after `key =` on its line, as written (comments included).
fn raw_value(root: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(root.join(SETTINGS_FILE)).ok()?;
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// The items of a list such as `["a", "b c"]`. Commas inside quotes do not split items.
fn parse_list(value: &str) -> Vec<String> {
    let (Some(start), Some(end)) = (value.find('['), value.rfind(']')) else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    let mut items = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for c in value[start + 1..end].chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                items.push(std::mem::take(&mut current).trim().to_string());
            }
            _ => current.push(c),
        }
    }
    items.push(current.trim().to_string());
    items.retain(|i| !i.is_empty());
    items
}

/// `["a", "b c"]` for `items`, with quotes and backslashes escaped.
fn format_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|i| format!("\"{}\"", i.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// The items of the list `key` in `pascoda.toml` (empty when it is not set).
fn get_list(root: &Path, key: &str) -> Vec<String> {
    raw_value(root, key)
        .map(|v| parse_list(&v))
        .unwrap_or_default()
}

/// Sets the list `key` (an empty list removes the line), keeping every other line.
fn set_list(root: &Path, key: &str, items: &[String]) -> std::io::Result<()> {
    let line = (!items.is_empty()).then(|| format!("{key} = {}", format_list(items)));
    set_line(root, key, line)
}

/// Replaces the line of `key` with `line` (or removes it when `line` is None), keeping every
/// other line. A file with no lines left is removed.
fn set_line(root: &Path, key: &str, line: Option<String>) -> std::io::Result<()> {
    let path = root.join(SETTINGS_FILE);
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let is_key = |l: &str| l.split_once('=').is_some_and(|(k, _)| k.trim() == key);
    let mut found = false;
    let mut lines: Vec<String> = Vec::new();
    for l in old.lines() {
        if is_key(l) {
            found = true;
            if let Some(line) = &line {
                lines.push(line.clone());
            }
        } else {
            lines.push(l.to_string());
        }
    }
    if !found && let Some(line) = line {
        lines.push(line);
    }
    if lines.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

/// The compiler settings a project keeps in `pascoda.toml`, on top of its `.lpi` or `.dproj`.
/// Paths are relative to the project folder. Switches must be ones a project file may pass
/// (`foreign::is_allowed_switch`); others are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerSettings {
    /// Names for `-d`.
    pub defines: Vec<String>,
    /// Folders for `-Fu`.
    pub unit_paths: Vec<String>,
    /// Folders for `-Fi`.
    pub include_paths: Vec<String>,
    /// Switches such as `-O2` or `-Cr`.
    pub switches: Vec<String>,
}

/// The compiler settings in `pascoda.toml` of `root`: the lists `defines`, `unit_paths`,
/// `include_paths` and `switches`.
pub fn load_compiler(root: &Path) -> CompilerSettings {
    CompilerSettings {
        defines: get_list(root, "defines"),
        unit_paths: get_list(root, "unit_paths"),
        include_paths: get_list(root, "include_paths"),
        switches: get_list(root, "switches"),
    }
}

/// Writes the compiler settings into `pascoda.toml` of `root`, keeping the other lines. Empty
/// lists are removed from the file.
pub fn save_compiler(root: &Path, settings: &CompilerSettings) -> std::io::Result<()> {
    set_list(root, "defines", &settings.defines)?;
    set_list(root, "unit_paths", &settings.unit_paths)?;
    set_list(root, "include_paths", &settings.include_paths)?;
    set_list(root, "switches", &settings.switches)
}

/// The run settings of `pascoda.toml`: the program's arguments (one list entry each) and the folder
/// it runs in, relative to the project's folder. An empty `workdir` is the executable's own folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSettings {
    pub args: Vec<String>,
    pub workdir: String,
}

pub fn load_run(root: &Path) -> RunSettings {
    RunSettings {
        args: get_list(root, "args"),
        workdir: get_key(root, "workdir").unwrap_or_default().trim().to_string(),
    }
}

/// Writes the run settings into `pascoda.toml`, keeping the other lines.
pub fn save_run(root: &Path, s: &RunSettings) -> std::io::Result<()> {
    set_list(root, "args", &s.args)?;
    set_key(root, "workdir", s.workdir.trim())
}

/// The folder the program runs in: the `workdir` of `pascoda.toml` (relative to `folder`, the
/// project's build folder) when it is set, else the folder of the executable.
pub fn run_folder(folder: &Path, exe: &Path) -> PathBuf {
    let workdir = load_run(folder).workdir;
    if !workdir.is_empty() {
        return folder.join(workdir.replace('\\', "/"));
    }
    exe.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| folder.to_path_buf(), Path::to_path_buf)
}

/// The build configuration: Debug (debug information, the default, what the debugger uses) or Release
/// (optimised, no debug information, in a `release` folder beside the debug executable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Configuration {
    #[default]
    Debug,
    Release,
}

impl Configuration {
    pub fn name(self) -> &'static str {
        match self {
            Configuration::Debug => "Debug",
            Configuration::Release => "Release",
        }
    }
}

/// The `configuration` key of `pascoda.toml`; Debug when it is missing or unknown.
pub fn load_configuration(root: &Path) -> Configuration {
    match get_key(root, "configuration").as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("release") => Configuration::Release,
        _ => Configuration::Debug,
    }
}

/// Sets the `configuration` key (Debug is the default, so it is kept as its name too).
pub fn save_configuration(root: &Path, c: Configuration) -> std::io::Result<()> {
    set_key(root, "configuration", &c.name().to_ascii_lowercase())
}

/// The executable of `source` in a configuration: the debug one (`executable_for`), and for Release
/// the one in the `release` folder beside it.
pub fn executable_in(source: &Path, c: Configuration) -> PathBuf {
    let debug = executable_for(source);
    match c {
        Configuration::Debug => debug,
        Configuration::Release => {
            let folder = debug.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            folder.join("release").join(debug.file_name().unwrap_or_default())
        }
    }
}

/// Removes what builds leave in the folder of `source`: the executables (debug and release), the
/// `lib` folder of compiled units, the `release` folder, and the compiled and linker scratch files
/// (`.o`, `.ppu`, `.or`, `link*.res`, `ppas.sh`). The sources, the project files and bundles are not
/// touched. Returns what was removed.
pub fn clean_build(source: &Path) -> std::io::Result<Vec<PathBuf>> {
    let folder = source.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut removed = Vec::new();
    for exe in [executable_in(source, Configuration::Debug), executable_in(source, Configuration::Release)] {
        if exe != source && exe.is_file() {
            std::fs::remove_file(&exe)?;
            removed.push(exe);
        }
    }
    for dir in [folder.join("lib"), folder.join("release")] {
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)?;
            removed.push(dir);
        }
    }
    for entry in std::fs::read_dir(folder)?.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let scratch = (name.starts_with("link") && name.ends_with(".res")) || name == "ppas.sh";
        if path.is_file() && (matches!(ext.as_str(), "o" | "ppu" | "or") || scratch) {
            std::fs::remove_file(&path)?;
            removed.push(path);
        }
    }
    Ok(removed)
}

/// The `output` key of `pascoda.toml`: the executable's path, relative to the project's folder.
/// `None` when it is not set.
pub fn load_output(root: &Path) -> Option<String> {
    get_key(root, "output").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Sets the `output` key (an empty value removes it), keeping the other lines.
pub fn save_output(root: &Path, output: &str) -> std::io::Result<()> {
    set_key(root, "output", output.trim())
}

/// The executable a build of `source` writes and the app runs: the `output` of `pascoda.toml` in
/// the source's folder when it is set, else what the project file says, else the source's name
/// without its extension (`foreign::executable_of`).
pub fn executable_for(source: &Path) -> PathBuf {
    if let Some(folder) = source.parent()
        && let Some(output) = load_output(folder)
    {
        return folder.join(output);
    }
    foreign::executable_of(source)
}

/// The fpc arguments for a build in `folder`: those of its `.lpi` or `.dproj` (if any), then the
/// compiler settings of `pascoda.toml`. Folders that do not exist and switches a project may not
/// pass are left out. An `output` in `pascoda.toml` replaces the project file's `-o`.
pub fn compiler_args(folder: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = foreign::find(folder)
        .and_then(|file| foreign::read(&file).ok())
        .map(|project| project.compiler_args())
        .unwrap_or_default();
    if let Some(output) = load_output(folder) {
        args.retain(|a| !a.to_string_lossy().starts_with("-o"));
        args.push(format!("-o{}", folder.join(output).display()).into());
    }
    let settings = load_compiler(folder);
    args.extend(settings.defines.iter().map(|d| format!("-d{d}").into()));
    for (paths, flag) in [(&settings.unit_paths, "-Fu"), (&settings.include_paths, "-Fi")] {
        for p in paths {
            let dir = folder.join(p.replace('\\', "/"));
            if dir.is_dir() {
                args.push(format!("{flag}{}", dir.display()).into());
            }
        }
    }
    // The packages of the folder: the folders and search paths of what they require.
    let resolved = packages::resolve(folder);
    for p in resolved.unit_paths.iter().filter(|p| p.is_dir()) {
        args.push(format!("-Fu{}", p.display()).into());
    }
    for p in resolved.include_paths.iter().filter(|p| p.is_dir()) {
        args.push(format!("-Fi{}", p.display()).into());
    }
    args.extend(
        settings
            .switches
            .iter()
            .filter(|s| foreign::is_allowed_switch(s))
            .map(|s| s.into()),
    );
    args
}

/// The language mode for the project in `root`: `pascoda.toml`'s mode when it sets one, else the
/// mode the project file asks for, else ObjFPC.
pub fn mode_for(root: &Path) -> Mode {
    if get_key(root, "mode").is_some() {
        return load_mode(root);
    }
    foreign::find(root)
        .and_then(|file| foreign::read(&file).ok())
        .and_then(|project| project.mode)
        .unwrap_or_default()
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

/// What a new project starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// A window with one form (the LCL).
    Gui,
    /// A program that writes to the console.
    Console,
    /// A unit with a routine, and a program that uses it (the unit is the library).
    Library,
    /// A Lazarus package: a `.lpk` that lists a unit.
    Package,
}

/// Creates `<parent>/<name>/` for a `template` and returns its main program (`<name>.lpr`). The GUI
/// template is `create_gui_app`. Refuses to touch an existing folder.
pub fn create_project(parent: &Path, name: &str, template: Template) -> std::io::Result<PathBuf> {
    if template == Template::Gui {
        return create_gui_app(parent, name);
    }
    if !is_valid_name(name) {
        return Err(std::io::Error::other(format!("`{name}` is not a valid project name")));
    }
    let dir = parent.join(name);
    if dir.exists() {
        return Err(std::io::Error::other(format!("{} already exists", dir.display())));
    }
    std::fs::create_dir_all(&dir)?;
    let lpr = dir.join(format!("{name}.lpr"));
    match template {
        Template::Console => {
            std::fs::write(&lpr, console_source(name))?;
            Ok(lpr)
        }
        Template::Library => {
            // The demo program has its own name: a program `Maths` would share its executable's
            // name with the unit `Maths` (their object files clash on a case-insensitive disk).
            std::fs::write(dir.join(format!("{name}.pas")), library_unit_source(name))?;
            let demo = dir.join(format!("{name}Demo.lpr"));
            std::fs::write(&demo, library_program_source(name))?;
            Ok(demo)
        }
        Template::Package => {
            std::fs::write(dir.join(format!("{name}.pas")), package_unit_source(name))?;
            let lpk = dir.join(format!("{name}.lpk"));
            std::fs::write(&lpk, package_source(name))?;
            Ok(lpk)
        }
        Template::Gui => unreachable!("handled above"),
    }
}

fn console_source(name: &str) -> String {
    format!(
        "program {name};\n\n{{$mode objfpc}}{{$H+}}\n\nuses\n  SysUtils;\n\nbegin\n  WriteLn('Hello from {name}');\nend.\n"
    )
}

fn library_unit_source(name: &str) -> String {
    format!(
        "unit {name};\n\n{{$mode objfpc}}{{$H+}}\n\ninterface\n\n// Routines the library offers. Add more here and in the implementation.\nfunction Greeting(const Who: string): string;\n\nimplementation\n\nfunction Greeting(const Who: string): string;\nbegin\n  Result := 'Hello, ' + Who;\nend;\n\nend.\n"
    )
}

fn package_unit_source(name: &str) -> String {
    format!(
        "unit {name};\n\n{{$mode objfpc}}{{$H+}}\n\ninterface\n\nfunction Version: string;\n\nimplementation\n\nfunction Version: string;\nbegin\n  Result := '1.0';\nend;\n\nend.\n"
    )
}

fn package_source(name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CONFIG>\n  <Package Version=\"5\">\n    <Name Value=\"{name}\"/>\n    <Files Count=\"1\">\n      <Item1>\n        <FileName Value=\"{name}.pas\"/>\n        <Type Value=\"Unit\"/>\n        <UnitName Value=\"{name}\"/>\n      </Item1>\n    </Files>\n    <RequiredPkgs Count=\"0\"/>\n    <CompilerOptions>\n      <SearchPaths>\n        <IncludeFiles Value=\".\"/>\n        <OtherUnitFiles Value=\".\"/>\n      </SearchPaths>\n    </CompilerOptions>\n  </Package>\n</CONFIG>\n"
    )
}

fn library_program_source(name: &str) -> String {
    format!(
        "program {name}Demo;\n\n{{$mode objfpc}}{{$H+}}\n\nuses\n  {name};\n\nbegin\n  WriteLn({name}.Greeting('world'));\nend.\n"
    )
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

/// Creates the next form pair in `dir`: `unitN.pas` (a form unit for `TFormN`) and `unitN.lfm`
/// (its form), for the first N from 1 that no `unitN` in the project `root` uses, so the names
/// stay unique in the program's `uses` clause. Returns the unit file.
pub fn new_form_unit(dir: &Path, root: &Path) -> std::io::Result<PathBuf> {
    let taken = unit_numbers_in(root);
    let n = (1..)
        .find(|n| !taken.contains(n) && !dir.join(format!("unit{n}.pas")).exists() && !dir.join(format!("unit{n}.lfm")).exists())
        .expect("a free number exists");
    let pas = dir.join(format!("unit{n}.pas"));
    std::fs::write(&pas, form_unit_source(n))?;
    std::fs::write(dir.join(format!("unit{n}.lfm")), form_lfm_source(n))?;
    Ok(pas)
}

/// Creates the next data module pair in `dir`: `unitN.pas` (a unit for `TDataModuleN`, with the
/// global `DataModuleN`) and `unitN.lfm` (its data module), numbered like the forms so no `unitN`
/// is reused in the project `root`. Returns the unit file.
pub fn new_data_module_unit(dir: &Path, root: &Path) -> std::io::Result<PathBuf> {
    let taken = unit_numbers_in(root);
    let n = (1..)
        .find(|n| !taken.contains(n) && !dir.join(format!("unit{n}.pas")).exists() && !dir.join(format!("unit{n}.lfm")).exists())
        .expect("a free number exists");
    let pas = dir.join(format!("unit{n}.pas"));
    std::fs::write(&pas, data_module_unit_source(n))?;
    std::fs::write(dir.join(format!("unit{n}.lfm")), data_module_lfm_source(n))?;
    Ok(pas)
}

/// The N of every `unitN` file (any extension) below `root`, skipping the folders the tree hides.
fn unit_numbers_in(root: &Path) -> std::collections::HashSet<u32> {
    fn walk(dir: &Path, depth: usize, out: &mut std::collections::HashSet<u32>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for e in read.filter_map(Result::ok) {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                let hidden = name.starts_with('.') || ["lib", "target", "vendor"].contains(&name.as_str()) || name.ends_with(".app");
                if depth < 8 && !hidden {
                    walk(&p, depth + 1, out);
                }
            } else if let Some(n) = p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("unit")) {
                if let Ok(n) = n.parse::<u32>() {
                    out.insert(n);
                }
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    walk(root, 0, &mut out);
    out
}

fn form_unit_source(n: u32) -> String {
    format!(
        "unit Unit{n};\n\n{{$mode objfpc}}{{$H+}}\n\ninterface\n\nuses\n  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls;\n\ntype\n  TForm{n} = class(TForm)\n  end;\n\nvar\n  Form{n}: TForm{n};\n\nimplementation\n\n{{$R *.lfm}}\n\nend.\n"
    )
}

fn data_module_unit_source(n: u32) -> String {
    format!(
        "unit Unit{n};\n\n{{$mode objfpc}}{{$H+}}\n\ninterface\n\nuses\n  Classes, SysUtils;\n\ntype\n  TDataModule{n} = class(TDataModule)\n  end;\n\nvar\n  DataModule{n}: TDataModule{n};\n\nimplementation\n\n{{$R *.lfm}}\n\nend.\n"
    )
}

fn data_module_lfm_source(n: u32) -> String {
    format!(
        "object DataModule{n}: TDataModule{n}\n  OldCreateOrder = False\n  Height = 150\n  HorizontalOffset = 300\n  VerticalOffset = 200\n  Width = 200\nend\n"
    )
}

fn form_lfm_source(n: u32) -> String {
    format!(
        "object Form{n}: TForm{n}\n  Left = 300\n  Height = 300\n  Top = 200\n  Width = 400\n  Caption = 'Form{n}'\nend\n"
    )
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
    fn new_form_units_take_the_next_free_number() {
        let d = std::env::temp_dir().join(format!("pascoda-new-form-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let first = new_form_unit(&d, &d).unwrap();
        assert_eq!(first, d.join("unit1.pas"));
        let pas = std::fs::read_to_string(&first).unwrap();
        assert!(pas.starts_with("unit Unit1;") && pas.contains("TForm1 = class(TForm)"));
        assert!(pas.contains("Form1: TForm1;") && pas.contains("{$R *.lfm}"));
        let lfm = std::fs::read_to_string(d.join("unit1.lfm")).unwrap();
        assert!(lfm.starts_with("object Form1: TForm1"));
        // A stray .lfm alone takes its number, so the pair never mixes two forms.
        std::fs::write(d.join("unit2.lfm"), "object Form2: TForm2\nend\n").unwrap();
        assert_eq!(new_form_unit(&d, &d).unwrap(), d.join("unit3.pas"));
        // A unit of the same number in another folder is taken too.
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(new_form_unit(&sub, &d).unwrap(), sub.join("unit4.pas"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn run_settings_round_trip_and_choose_the_folder_the_program_runs_in() {
        let d = std::env::temp_dir().join(format!("pascoda-run-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("data")).unwrap();
        std::fs::write(d.join(SETTINGS_FILE), "app_name = \"app\"\n").unwrap();
        let exe = d.join("bin/app");
        assert_eq!(run_folder(&d, &exe), d.join("bin"), "unset: the executable's folder");

        let s = RunSettings { args: vec!["--file".into(), "my data.txt".into()], workdir: "data".into() };
        save_run(&d, &s).unwrap();
        assert_eq!(load_run(&d), s, "arguments with spaces survive");
        assert_eq!(run_folder(&d, &exe), d.join("data"));
        let text = std::fs::read_to_string(d.join(SETTINGS_FILE)).unwrap();
        assert!(text.contains("app_name = \"app\""), "{text}");

        save_run(&d, &RunSettings::default()).unwrap();
        assert_eq!(load_run(&d), RunSettings::default(), "cleared");
        assert_eq!(run_folder(&d, &exe), d.join("bin"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_output_key_sets_the_executable_and_replaces_the_project_file_target() {
        let d = std::env::temp_dir().join(format!("pascoda-output-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("bin")).unwrap();
        let source = d.join("app.lpr");
        std::fs::write(&source, "program app;\nbegin\nend.\n").unwrap();
        assert_eq!(load_output(&d), None);
        assert_eq!(executable_for(&source), d.join("app"), "without the key: the source's name");

        save_output(&d, " bin/tool ").unwrap();
        assert_eq!(load_output(&d).as_deref(), Some("bin/tool"));
        assert_eq!(executable_for(&source), d.join("bin/tool"));
        let args: Vec<String> = compiler_args(&d).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args.iter().filter(|a| a.starts_with("-o")).collect::<Vec<_>>(),
            [&format!("-o{}", d.join("bin/tool").display())],
            "one -o, the key's"
        );

        save_output(&d, "").unwrap();
        assert_eq!(load_output(&d), None, "an empty value removes the key");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_configuration_selects_the_executable_and_clean_removes_only_build_products() {
        let d = std::env::temp_dir().join(format!("pascoda-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::create_dir_all(d.join("data")).unwrap();
        let source = d.join("app.lpr");
        std::fs::write(&source, "program app;\nbegin\nend.\n").unwrap();
        assert_eq!(load_configuration(&d), Configuration::Debug, "Debug by default");
        save_configuration(&d, Configuration::Release).unwrap();
        assert_eq!(load_configuration(&d), Configuration::Release);
        assert_eq!(executable_in(&source, Configuration::Debug), d.join("app"));
        assert_eq!(executable_in(&source, Configuration::Release), d.join("release/app"));

        // Build products go; the sources, the data folder and the project's own files stay.
        std::fs::write(d.join("app"), "bin").unwrap();
        std::fs::create_dir_all(d.join("release")).unwrap();
        std::fs::write(d.join("release/app"), "bin").unwrap();
        std::fs::write(d.join("app.o"), "obj").unwrap();
        std::fs::write(d.join("unit1.ppu"), "unit").unwrap();
        std::fs::write(d.join("link34.res"), "res").unwrap();
        std::fs::write(d.join("ppas.sh"), "sh").unwrap();
        let removed = clean_build(&source).unwrap();
        assert!(!d.join("app").exists() && !d.join("release").exists() && !d.join("lib").exists());
        assert!(!d.join("app.o").exists() && !d.join("unit1.ppu").exists() && !d.join("link34.res").exists() && !d.join("ppas.sh").exists());
        assert!(source.is_file() && d.join("data").is_dir() && d.join(SETTINGS_FILE).is_file(), "kept");
        assert!(removed.len() >= 6, "{removed:?}");
        assert!(clean_build(&source).unwrap().is_empty(), "nothing left to clean");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn each_template_writes_its_files_and_refuses_an_existing_folder() {
        let d = std::env::temp_dir().join(format!("pascoda-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let console = create_project(&d, "Tool", Template::Console).unwrap();
        assert!(std::fs::read_to_string(&console).unwrap().contains("WriteLn('Hello from Tool')"));
        assert!(!d.join("Tool/Tool.pas").exists(), "a console program is one file");
        let lib = create_project(&d, "Maths", Template::Library).unwrap();
        assert!(d.join("Maths/Maths.pas").is_file(), "the library unit");
        assert!(std::fs::read_to_string(&lib).unwrap().contains("uses\n  Maths;"));
        assert!(create_project(&d, "Tool", Template::Console).is_err(), "never overwrites");
        let _ = std::fs::remove_dir_all(&d);
    }

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
mod project_mode_tests {
    use super::*;

    /// `pascoda.toml` wins; without it the project file's syntax mode decides.
    #[test]
    fn the_mode_comes_from_the_settings_then_the_project_file() {
        let d = std::env::temp_dir().join(format!("pascoda-mode-for-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("app.lpi"),
            r#"<CONFIG><CompilerOptions><Parsing><SyntaxOptions><SyntaxMode Value="Delphi"/></SyntaxOptions></Parsing></CompilerOptions></CONFIG>"#,
        )
        .unwrap();
        assert_eq!(mode_for(&d), Mode::Delphi);
        std::fs::write(d.join(SETTINGS_FILE), "mode = \"objfpc\"\n").unwrap();
        assert_eq!(mode_for(&d), Mode::ObjFpc, "pascoda.toml wins");
        let _ = std::fs::remove_dir_all(&d);
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

#[cfg(test)]
mod compiler_settings_tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_round_trip_and_keep_the_other_lines() {
        let d = folder("round");
        std::fs::write(d.join(SETTINGS_FILE), "name = \"app\"\nmode = \"delphi\"\n").unwrap();
        let settings = CompilerSettings {
            defines: vec!["DEBUG".into(), "A,B".into()],
            unit_paths: vec!["lib dir".into(), "say \"hi\"".into()],
            include_paths: vec!["inc".into()],
            switches: vec!["-O2".into(), "-Cr".into()],
        };
        save_compiler(&d, &settings).unwrap();
        assert_eq!(load_compiler(&d), settings, "commas, spaces and quotes survive");
        let text = std::fs::read_to_string(d.join(SETTINGS_FILE)).unwrap();
        assert!(text.contains("name = \"app\"") && text.contains("mode = \"delphi\""), "{text}");
        assert_eq!(load_mode(&d), Mode::Delphi, "the mode line is untouched");

        save_compiler(&d, &CompilerSettings::default()).unwrap();
        assert_eq!(load_compiler(&d), CompilerSettings::default());
        assert!(!std::fs::read_to_string(d.join(SETTINGS_FILE)).unwrap().contains("defines"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn compiler_args_take_settings_and_leave_out_missing_folders_and_unsafe_switches() {
        let d = folder("args");
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::write(
            d.join(SETTINGS_FILE),
            "defines = [\"FROMTOML\"]\nunit_paths = [\"lib\", \"missing\"]\nswitches = [\"-O2\", \"-o/tmp/x\", \"main.pas\"]\n",
        )
        .unwrap();
        let args: Vec<String> = compiler_args(&d)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-dFROMTOML".to_string()));
        assert!(args.contains(&format!("-Fu{}", d.join("lib").display())));
        assert!(!args.iter().any(|a| a.contains("missing")));
        assert!(args.contains(&"-O2".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("-o/") || a == "main.pas"), "{args:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_data_module_is_a_unit_with_its_global_and_shares_the_numbering() {
        let dir = std::env::temp_dir().join(format!("pascoda-new-dm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // unit1 is a form already: the data module takes unit2.
        std::fs::write(dir.join("unit1.pas"), "unit Unit1;\n").unwrap();
        let pas = new_data_module_unit(&dir, &dir).unwrap();
        assert_eq!(pas, dir.join("unit2.pas"));
        let text = std::fs::read_to_string(&pas).unwrap();
        assert!(text.contains("TDataModule2 = class(TDataModule)"), "{text}");
        assert!(text.contains("DataModule2: TDataModule2;"), "{text}");
        assert!(text.contains("{$R *.lfm}"), "{text}");
        let lfm = std::fs::read_to_string(dir.join("unit2.lfm")).unwrap();
        assert!(lfm.starts_with("object DataModule2: TDataModule2\n"), "{lfm}");
        assert!(lfm.contains("HorizontalOffset = 300") && lfm.contains("VerticalOffset = 200"), "{lfm}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
