//! Reading Lazarus (`.lpi`) and Delphi (`.dproj`) project files. The main program, the search
//! paths, defines and switches become fpc arguments for builds. Nothing is written back, and
//! macros such as `$(LazarusDir)` are left out (listed in `skipped`), since Pascoda has no
//! Lazarus or Delphi installation to expand them from.

use pascoda_fpc::Mode;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lazarus,
    Delphi,
}

/// What a project file says that matters to a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreign {
    pub kind: Kind,
    /// The project file itself.
    pub file: PathBuf,
    /// The main program (`.lpr` or `.dpr`), relative paths resolved against the project's folder.
    pub main: Option<PathBuf>,
    /// Folders for `-Fu`.
    pub unit_paths: Vec<PathBuf>,
    /// Folders for `-Fi`.
    pub include_paths: Vec<PathBuf>,
    /// Names for `-d`.
    pub defines: Vec<String>,
    /// The executable's path (`-o`), relative paths resolved against the project's folder.
    pub target: Option<PathBuf>,
    /// The folder for compiled units (`-FU`), used when it exists.
    pub unit_output: Option<PathBuf>,
    /// The language mode the project asks for (`SyntaxMode`; Delphi for `.dproj`).
    pub mode: Option<Mode>,
    /// Switches passed on as written (`-O2`, `-Cr`, `-gh`).
    pub switches: Vec<String>,
    /// Entries that were left out, as written: macros, Windows-only defines, and options that
    /// Pascoda sets itself (`-M` modes, `-FU` output folders).
    pub skipped: Vec<String>,
}

/// Defines that only make sense on Windows. A Delphi project lists them for its Windows targets.
const WINDOWS_DEFINES: &[&str] = &["MSWINDOWS", "WIN32", "WIN64", "CPU386", "CPUX86", "CPUX64"];

impl Foreign {
    fn new(kind: Kind, file: &Path) -> Self {
        Self {
            kind,
            file: file.to_path_buf(),
            main: None,
            unit_paths: Vec::new(),
            include_paths: Vec::new(),
            defines: Vec::new(),
            target: None,
            unit_output: None,
            mode: None,
            switches: Vec::new(),
            skipped: Vec::new(),
        }
    }

    /// The fpc arguments for a build of this project. Folders that do not exist are left out.
    pub fn compiler_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        for p in self.unit_paths.iter().filter(|p| p.is_dir()) {
            args.push(format!("-Fu{}", p.display()).into());
        }
        for p in self.include_paths.iter().filter(|p| p.is_dir()) {
            args.push(format!("-Fi{}", p.display()).into());
        }
        for d in &self.defines {
            args.push(format!("-d{d}").into());
        }
        if let Some(t) = &self.target {
            args.push(format!("-o{}", t.display()).into());
        }
        if let Some(u) = self.unit_output.as_ref().filter(|u| u.is_dir()) {
            args.push(format!("-FU{}", u.display()).into());
        }
        args.extend(self.switches.iter().map(OsString::from));
        args
    }
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// The project file in `root` (not in its subfolders), or None. A `.lpi` comes before a `.dproj`,
/// then files are taken by name.
pub fn find(root: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && matches!(ext_lower(p).as_deref(), Some("lpi" | "dproj")))
        .collect();
    files.sort_by_key(|p| (ext_lower(p).as_deref() != Some("lpi"), p.clone()));
    files.into_iter().next()
}

/// Reads the project file at `file`, a `.lpi` or a `.dproj`.
pub fn read(file: &Path) -> Result<Foreign, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    match ext_lower(file).as_deref() {
        Some("lpi") => parse_lpi(&text, file),
        Some("dproj") => parse_dproj(&text, file),
        _ => Err(format!("{} is not a Lazarus or Delphi project", file.display())),
    }
}

/// `raw` (a path as the project wrote it, Windows separators allowed) against `dir`. A path with
/// a macro is not resolved: it goes to `skipped`.
fn resolve(dir: &Path, raw: &str, skipped: &mut Vec<String>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.contains('$') {
        skipped.push(raw.to_string());
        return None;
    }
    Some(dir.join(raw.replace('\\', "/")))
}

/// Parses a `;`-separated list of folders.
fn split_paths(dir: &Path, list: &str, out: &mut Vec<PathBuf>, skipped: &mut Vec<String>) {
    for raw in list.split(';') {
        if let Some(p) = resolve(dir, raw, skipped) {
            out.push(p);
        }
    }
}

/// The value of the child element `tag`, from a `Value` attribute (Lazarus style).
fn child_value<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.has_tag_name(tag))
        .and_then(|c| c.attribute("Value"))
}

/// One switch from a Lazarus `CustomOptions` value.
fn custom_option(dir: &Path, token: &str, f: &mut Foreign) {
    if let Some(name) = token.strip_prefix("-d").filter(|n| !n.is_empty()) {
        f.defines.push(name.to_string());
    } else if let Some(p) = token.strip_prefix("-Fu") {
        if let Some(p) = resolve(dir, p, &mut f.skipped) {
            f.unit_paths.push(p);
        }
    } else if let Some(p) = token.strip_prefix("-Fi") {
        if let Some(p) = resolve(dir, p, &mut f.skipped) {
            f.include_paths.push(p);
        }
    } else if let Some(p) = token.strip_prefix("-FU") {
        f.unit_output = resolve(dir, p, &mut f.skipped);
    } else if token.starts_with("-O") || token.starts_with("-g") || passes_through(token) {
        push_switch(&mut f.switches, token);
    } else {
        f.skipped.push(token.to_string());
    }
}

/// Whether a Lazarus `CustomOptions` token goes to fpc unchanged: syntax (`-S`, but not the mode
/// switches), verbosity (`-v`), code generation (`-C`), linking (`-X`), undefines (`-u`) and
/// rebuilding (`-B`). Tokens that would fight Pascoda's own choices are left out: `-M` (the mode),
/// `-o` and `-FE` (where the program goes), `-Cn` (no link), `-k` and `-l` (the linker), and
/// `-S2` and `-Sd` (mode switches).
fn passes_through(token: &str) -> bool {
    const LEFT_OUT: &[&str] = &["-M", "-o", "-FE", "-Cn", "-k", "-l", "-S2", "-Sd"];
    const ALLOWED: &[&str] = &["-S", "-v", "-C", "-X", "-u", "-B"];
    !LEFT_OUT.iter().any(|p| token.starts_with(p)) && ALLOWED.iter().any(|p| token.starts_with(p))
}

/// Reads a Lazarus project (`.lpi`): the first unit of the project is the main program; the
/// search paths and `CustomOptions` of the compiler options are read.
pub fn parse_lpi(xml: &str, file: &Path) -> Result<Foreign, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("{}: {e}", file.display()))?;
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut f = Foreign::new(Kind::Lazarus, file);

    let main = doc
        .descendants()
        .find(|n| n.has_tag_name("Unit0") && n.parent().is_some_and(|p| p.has_tag_name("Units")));
    if let Some(name) = main.and_then(|u| child_value(u, "Filename")) {
        f.main = resolve(dir, name, &mut f.skipped);
    }
    if let Some(paths) = doc.descendants().find(|n| n.has_tag_name("SearchPaths")) {
        if let Some(list) = child_value(paths, "OtherUnitFiles") {
            split_paths(dir, list, &mut f.unit_paths, &mut f.skipped);
        }
        if let Some(list) = child_value(paths, "IncludeFiles") {
            split_paths(dir, list, &mut f.include_paths, &mut f.skipped);
        }
        if let Some(out) = child_value(paths, "UnitOutputDirectory") {
            f.unit_output = resolve(dir, out, &mut f.skipped);
        }
    }
    if let Some(options) = doc
        .descendants()
        .find(|n| n.has_tag_name("CustomOptions"))
        .and_then(|n| n.attribute("Value"))
    {
        for token in options.split_whitespace() {
            custom_option(dir, token, &mut f);
        }
    }
    if let Some(compiler) = doc.descendants().find(|n| n.has_tag_name("CompilerOptions")) {
        lazarus_settings(compiler, dir, &mut f);
    }
    Ok(f)
}

/// The settings of a Lazarus `CompilerOptions` element that map onto switches: checks,
/// optimisation level, heap tracing, line info and smart linking; also the syntax mode and the
/// target file. Debug information is not read: Pascoda always builds with `-g` for the debugger.
fn lazarus_settings(options: roxmltree::Node, dir: &Path, f: &mut Foreign) {
    let set = |path: &[&str]| {
        value_at(options, path).is_some_and(|v| v.eq_ignore_ascii_case("true"))
    };
    for (path, switch) in [
        (&["CodeGeneration", "Checks", "IOChecks"][..], "-Ci"),
        (&["CodeGeneration", "Checks", "RangeChecks"][..], "-Cr"),
        (&["CodeGeneration", "Checks", "OverflowChecks"][..], "-Co"),
        (&["Linking", "Debugging", "UseHeaptrc"][..], "-gh"),
        (&["Linking", "Debugging", "UseLineInfoUnit"][..], "-gl"),
        (&["Linking", "LinkSmart"][..], "-XX"),
    ] {
        if set(path) {
            push_switch(&mut f.switches, switch);
        }
    }
    let level = value_at(options, &["CodeGeneration", "Optimizations", "OptimizationLevel"])
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|l| *l > 0);
    if let Some(level) = level {
        push_switch(&mut f.switches, &format!("-O{level}"));
    }
    f.mode = match value_at(options, &["Parsing", "SyntaxOptions", "SyntaxMode"]) {
        Some(m) if m.eq_ignore_ascii_case("Delphi") => Some(Mode::Delphi),
        Some(m) if m.eq_ignore_ascii_case("ObjFPC") => Some(Mode::ObjFpc),
        _ => None,
    };
    if let Some(name) = value_at(options, &["Target", "Filename"]) {
        f.target = resolve(dir, name, &mut f.skipped);
    }
}

/// The `Value` at the end of a path of child elements, such as `Checks/IOChecks`.
fn value_at<'i>(mut node: roxmltree::Node<'i, 'i>, path: &[&str]) -> Option<&'i str> {
    for name in path {
        node = node.children().find(|c| c.has_tag_name(*name))?;
    }
    node.attribute("Value")
}

/// Adds a switch unless it is already there.
fn push_switch(switches: &mut Vec<String>, switch: &str) {
    if !switches.iter().any(|s| s == switch) {
        switches.push(switch.to_string());
    }
}

/// The executable a build of `source` writes: the target of the project file in the source's
/// folder when it names one (`-o`), else the source's name without its extension.
pub fn executable_of(source: &Path) -> PathBuf {
    source
        .parent()
        .and_then(find)
        .and_then(|file| read(&file).ok())
        .and_then(|project| project.target)
        .unwrap_or_else(|| source.with_extension(""))
}

/// Whether a Delphi property group (or property) with this `Condition` applies to a macOS
/// debug build: the base settings and the Debug configuration, but not Release, and not
/// platform-specific ones (`Platform`, `Win32`, `Win64`, ...).
fn applies(condition: Option<&str>) -> bool {
    let Some(c) = condition else {
        return true;
    };
    if c.contains("Platform") || c.contains("_Win") {
        return false;
    }
    c.contains("Base") || c.contains("Debug")
}

/// Reads a Delphi project (`.dproj`): `MainSource`, `DCC_UnitSearchPath`, `DCC_IncludePath` and
/// `DCC_Define` from the base and Debug settings. The values of all applicable groups are
/// combined, and `$(…)` references to other settings are dropped. Boolean switches take the
/// value of the last applicable group, so Debug overrides Base.
pub fn parse_dproj(xml: &str, file: &Path) -> Result<Foreign, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("{}: {e}", file.display()))?;
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut f = Foreign::new(Kind::Delphi, file);
    let mut flags: BTreeMap<&str, bool> = BTreeMap::new();
    let mut exe_output: Option<String> = None;
    let mut dcu_output: Option<String> = None;

    for group in doc
        .descendants()
        .filter(|n| n.has_tag_name("PropertyGroup") && applies(n.attribute("Condition")))
    {
        for prop in group
            .children()
            .filter(|n| n.is_element() && applies(n.attribute("Condition")))
        {
            let text = prop.text().unwrap_or_default();
            match prop.tag_name().name() {
                "MainSource" => f.main = resolve(dir, text, &mut f.skipped),
                "DCC_UnitSearchPath" => {
                    split_paths(dir, &list_without_references(text), &mut f.unit_paths, &mut f.skipped)
                }
                "DCC_IncludePath" => split_paths(
                    dir,
                    &list_without_references(text),
                    &mut f.include_paths,
                    &mut f.skipped,
                ),
                "DCC_Define" => {
                    for name in text.split(';').map(str::trim).filter(|n| !n.is_empty()) {
                        if name.starts_with("$(") {
                            continue;
                        }
                        if WINDOWS_DEFINES.contains(&name) {
                            f.skipped.push(name.to_string());
                        } else if !f.defines.iter().any(|d| d == name) {
                            f.defines.push(name.to_string());
                        }
                    }
                }
                name @ ("DCC_RangeChecking" | "DCC_OverflowChecking" | "DCC_IOChecking"
                | "DCC_Optimize" | "DCC_Assertions") => {
                    flags.insert(name, text.trim().eq_ignore_ascii_case("true"));
                }
                "DCC_ExeOutput" => exe_output = Some(text.trim().to_string()),
                "DCC_DcuOutput" => dcu_output = Some(text.trim().to_string()),
                _ => {}
            }
        }
    }
    for (name, switch) in [
        ("DCC_RangeChecking", "-Cr"),
        ("DCC_OverflowChecking", "-Co"),
        ("DCC_IOChecking", "-Ci"),
        ("DCC_Optimize", "-O2"),
        ("DCC_Assertions", "-Sa"),
    ] {
        if flags.get(name) == Some(&true) {
            push_switch(&mut f.switches, switch);
        }
    }
    // The output folder of the program; the file name is the main source's.
    if let (Some(out), Some(stem)) = (exe_output, f.main.as_ref().and_then(|m| m.file_stem())) {
        f.target = resolve(dir, &out, &mut f.skipped).map(|folder| folder.join(stem));
    }
    if let Some(out) = dcu_output {
        f.unit_output = resolve(dir, &out, &mut f.skipped);
    }
    f.mode = Some(Mode::Delphi);
    Ok(f)
}

/// A `;` list with its `$(Name)` references removed (they point at other settings; the values
/// of those settings are read separately).
fn list_without_references(text: &str) -> String {
    text.split(';')
        .filter(|entry| !entry.trim().starts_with("$("))
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LPI: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CONFIG>
  <ProjectOptions>
    <Units Count="2">
      <Unit0>
        <Filename Value="project1.lpr"/>
        <IsPartOfProject Value="True"/>
      </Unit0>
      <Unit1>
        <Filename Value="unit1.pas"/>
      </Unit1>
    </Units>
  </ProjectOptions>
  <CompilerOptions>
    <SearchPaths>
      <IncludeFiles Value="inc"/>
      <OtherUnitFiles Value="frames;../common;$(LazarusDir)/lcl"/>
    </SearchPaths>
    <Other>
      <CustomOptions Value="-dFOO -Mdelphi -O2 -g -FUout"/>
    </Other>
  </CompilerOptions>
</CONFIG>"#;

    const DPROJ: &str = r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <PropertyGroup>
    <MainSource>App.dpr</MainSource>
  </PropertyGroup>
  <PropertyGroup Condition="'$(Config)'=='Base' or '$(Base)'!=''">
    <DCC_UnitSearchPath>..\lib;$(DCC_UnitSearchPath)</DCC_UnitSearchPath>
    <DCC_Define>MYFLAG;$(DCC_Define)</DCC_Define>
  </PropertyGroup>
  <PropertyGroup Condition="('$(Platform)'=='Win32') and '$(Base)'!=''">
    <DCC_Define>WIN32;$(DCC_Define)</DCC_Define>
  </PropertyGroup>
  <PropertyGroup Condition="'$(Config)'=='Release' or '$(Cfg_2)'!=''">
    <DCC_Define>RELEASEONLY;$(DCC_Define)</DCC_Define>
  </PropertyGroup>
  <PropertyGroup Condition="'$(Config)'=='Debug' or '$(Cfg_1)'!=''">
    <DCC_Define>DEBUG;$(DCC_Define)</DCC_Define>
  </PropertyGroup>
</Project>"#;

    #[test]
    fn a_lazarus_project_gives_its_main_program_paths_and_options() {
        let file = Path::new("/work/app/project1.lpi");
        let dir = Path::new("/work/app");
        let f = parse_lpi(LPI, file).unwrap();
        assert_eq!(f.kind, Kind::Lazarus);
        assert_eq!(f.main, Some(dir.join("project1.lpr")));
        assert_eq!(f.unit_paths, vec![dir.join("frames"), dir.join("../common")]);
        assert_eq!(f.include_paths, vec![dir.join("inc")]);
        assert_eq!(f.defines, vec!["FOO"]);
        assert_eq!(f.switches, vec!["-O2", "-g"]);
        assert_eq!(f.unit_output, Some(dir.join("out")));
        assert_eq!(f.skipped, vec!["$(LazarusDir)/lcl", "-Mdelphi"]);
    }

    #[test]
    fn a_delphi_project_gives_its_base_and_debug_settings_only() {
        let file = Path::new("/work/app/App.dproj");
        let dir = Path::new("/work/app");
        let f = parse_dproj(DPROJ, file).unwrap();
        assert_eq!(f.kind, Kind::Delphi);
        assert_eq!(f.main, Some(dir.join("App.dpr")));
        assert_eq!(f.unit_paths, vec![dir.join("../lib")], "backslashes read as folders");
        assert_eq!(f.defines, vec!["MYFLAG", "DEBUG"], "Release and Win32 settings are left out");
        assert!(f.include_paths.is_empty());
    }

    #[test]
    fn a_windows_define_in_a_project_is_skipped_not_passed() {
        let text = DPROJ.replace("MYFLAG;", "MYFLAG;MSWINDOWS;");
        let f = parse_dproj(&text, Path::new("/w/App.dproj")).unwrap();
        assert!(!f.defines.iter().any(|d| d == "MSWINDOWS"));
        assert!(f.skipped.iter().any(|s| s == "MSWINDOWS"));
    }

    #[test]
    fn compiler_arguments_keep_only_folders_that_exist() {
        let dir = std::env::temp_dir().join(format!("pascoda-foreign-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        let mut f = parse_lpi(LPI, &dir.join("project1.lpi")).unwrap();
        f.unit_paths.push(dir.join("missing"));
        let args: Vec<String> = f
            .compiler_args()
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&format!("-Fu{}", dir.join("frames").display())));
        assert!(!args.iter().any(|a| a.contains("missing")));
        assert!(args.contains(&"-dFOO".to_string()));
        assert!(args.contains(&"-O2".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_prefers_a_lazarus_project_in_the_folder_itself() {
        let dir = std::env::temp_dir().join(format!("pascoda-find-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("App.dproj"), DPROJ).unwrap();
        std::fs::write(dir.join("app.lpi"), LPI).unwrap();
        std::fs::write(dir.join("sub/other.lpi"), LPI).unwrap();
        assert_eq!(find(&dir), Some(dir.join("app.lpi")));
        std::fs::remove_file(dir.join("app.lpi")).unwrap();
        assert_eq!(find(&dir), Some(dir.join("App.dproj")));
        std::fs::remove_file(dir.join("App.dproj")).unwrap();
        assert_eq!(find(&dir), None, "subfolders are not searched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sample_projects_of_the_vendored_sources_read() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor");
        let dproj = root.join("lazarus/components/lazmetafile/demos/MetaFileCreator/MetaFileCreator.dproj");
        if dproj.is_file() {
            let f = read(&dproj).unwrap();
            assert!(f.main.unwrap().ends_with("MetaFileCreator.dpr"));
        }
        let lpi = root.join("lazarus/ide/lazarus.lpi");
        if lpi.is_file() {
            assert!(read(&lpi).is_ok());
        }
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;

    #[test]
    fn lazarus_checks_optimisation_heap_and_target_map_onto_switches() {
        let lpi = r#"<CONFIG><CompilerOptions>
  <Target><Filename Value="../bin/app"/></Target>
  <Parsing><SyntaxOptions><SyntaxMode Value="Delphi"/></SyntaxOptions></Parsing>
  <CodeGeneration>
    <Checks><IOChecks Value="True"/><RangeChecks Value="False"/><OverflowChecks Value="True"/></Checks>
    <Optimizations><OptimizationLevel Value="2"/></Optimizations>
  </CodeGeneration>
  <Linking>
    <Debugging><UseHeaptrc Value="True"/><UseLineInfoUnit Value="False"/></Debugging>
    <LinkSmart Value="True"/>
  </Linking>
  <Other><CustomOptions Value="-O2 -dX"/></Other>
</CompilerOptions></CONFIG>"#;
        let dir = Path::new("/work/app");
        let f = parse_lpi(lpi, &dir.join("app.lpi")).unwrap();
        // `-O2` comes from CustomOptions, which is read first; the level does not repeat it.
        assert_eq!(f.switches, vec!["-O2", "-Ci", "-Co", "-gh", "-XX"]);
        assert_eq!(f.mode, Some(Mode::Delphi));
        assert_eq!(f.target, Some(dir.join("../bin/app")));
        assert_eq!(f.defines, vec!["X"]);
    }

    #[test]
    fn delphi_switches_take_the_debug_value_over_base() {
        let dproj = r#"<Project>
  <PropertyGroup><MainSource>App.dpr</MainSource>
    <DCC_RangeChecking>true</DCC_RangeChecking><DCC_Optimize>true</DCC_Optimize>
    <DCC_IOChecking>true</DCC_IOChecking><DCC_ExeOutput>.\bin</DCC_ExeOutput>
  </PropertyGroup>
  <PropertyGroup Condition="'$(Config)'=='Debug' or '$(Cfg_1)'!=''">
    <DCC_Optimize>false</DCC_Optimize>
    <DCC_Assertions>true</DCC_Assertions>
  </PropertyGroup>
</Project>"#;
        let dir = Path::new("/work/app");
        let f = parse_dproj(dproj, &dir.join("App.dproj")).unwrap();
        assert_eq!(f.switches, vec!["-Cr", "-Ci", "-Sa"], "Debug turns optimisation off");
        assert_eq!(f.target, Some(dir.join("bin/App")));
        assert_eq!(f.mode, Some(Mode::Delphi));
    }

    #[test]
    fn a_macro_output_folder_is_skipped_and_the_default_stays() {
        let dproj = r#"<Project><PropertyGroup><MainSource>App.dpr</MainSource>
  <DCC_ExeOutput>.\$(Platform)\$(Config)</DCC_ExeOutput></PropertyGroup></Project>"#;
        let f = parse_dproj(dproj, Path::new("/work/App.dproj")).unwrap();
        assert_eq!(f.target, None);
        assert!(f.skipped.iter().any(|s| s.contains("$(Platform)")));
        assert_eq!(executable_of(Path::new("/work/App.dpr")), Path::new("/work/App"));
    }
}

#[cfg(test)]
mod passthrough_tests {
    use super::*;

    #[test]
    fn syntax_checks_and_linking_pass_and_the_project_keeps_its_own_choices() {
        let dir = Path::new("/work/app");
        let lpi = r#"<CONFIG><CompilerOptions><Other><CustomOptions Value="-Cr -Co -Sc -vw -Xs -Cn -S2 -M2 -o/tmp/x -k-ld_classic -FUunits"/></Other></CompilerOptions></CONFIG>"#;
        let f = parse_lpi(lpi, &dir.join("app.lpi")).unwrap();
        assert_eq!(f.switches, vec!["-Cr", "-Co", "-Sc", "-vw", "-Xs"]);
        assert_eq!(f.unit_output, Some(dir.join("units")));
        assert_eq!(f.skipped, vec!["-Cn", "-S2", "-M2", "-o/tmp/x", "-k-ld_classic"]);
    }

    #[test]
    fn a_unit_output_folder_is_passed_only_when_it_exists() {
        let dir = std::env::temp_dir().join(format!("pascoda-fu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lpi = r#"<CONFIG><CompilerOptions><SearchPaths><UnitOutputDirectory Value="units"/></SearchPaths></CompilerOptions></CONFIG>"#;
        let mut f = parse_lpi(lpi, &dir.join("app.lpi")).unwrap();
        let args = |f: &Foreign| f.compiler_args().iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(!args(&f).iter().any(|a| a.starts_with("-FU")), "the folder is missing");
        std::fs::create_dir_all(dir.join("units")).unwrap();
        f = parse_lpi(lpi, &dir.join("app.lpi")).unwrap();
        assert!(args(&f).contains(&format!("-FU{}", dir.join("units").display())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_delphi_output_folder_maps_to_the_unit_output() {
        let dproj = r#"<Project><PropertyGroup><MainSource>App.dpr</MainSource>
  <DCC_DcuOutput>.\dcu</DCC_DcuOutput></PropertyGroup></Project>"#;
        let f = parse_dproj(dproj, Path::new("/work/App.dproj")).unwrap();
        assert_eq!(f.unit_output, Some(Path::new("/work").join("dcu")));
    }
}
