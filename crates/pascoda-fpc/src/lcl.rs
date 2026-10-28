//! Building GUI applications against the Lazarus Component Library (LCL) with the Cocoa
//! widgetset, using plain `fpc` (no Lazarus install, no lazbuild).

use crate::{BuildResult, Mode, Plan, Progress, allow_debugging, build_streaming};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Oldest macOS the generated apps support (`-WM` and `LSMinimumSystemVersion`).
pub const MIN_MACOS: &str = "14.0";

/// Units compiled into the cache so projects normally find them precompiled.
const WARM_UNITS: &[&str] = &[
    "Interfaces",
    "Forms",
    "Controls",
    "Graphics",
    "StdCtrls",
    "ExtCtrls",
    "Dialogs",
    "Menus",
    "ComCtrls",
    "Buttons",
    "Grids",
    "ActnList",
    "Clipbrd",
    "LCLType",
    "LCLIntf",
    "LResources",
];

#[derive(Debug, Clone)]
pub struct Lcl {
    /// Lazarus source tree (contains `lcl/`, `components/lazutils/`).
    pub lazarus_dir: PathBuf,
    /// Where precompiled LCL units live. Keyed by compiler version and Lazarus commit.
    pub cache: PathBuf,
}

impl Lcl {
    /// Looks for `vendor/lazarus` in `start` and its parents, then in `$PASCODA_HOME`, then in
    /// the `Resources` folder of the app bundle this program runs from, and (debug builds only)
    /// in the source tree this crate was built from. Projects and test scratch folders can live
    /// anywhere.
    pub fn find(start: &Path) -> Option<Self> {
        let dirs = search_dirs(start);
        let lazarus_dir = dirs
            .iter()
            .map(|d| d.join("vendor/lazarus"))
            .find(|d| d.join("lcl/interfaces/cocoa").is_dir())?;
        let home = std::env::var_os("HOME")?;
        let key = format!("{}-{}", fpc_version(), commit(&lazarus_dir));
        Some(Self {
            cache: PathBuf::from(home)
                .join("Library/Caches/Pascoda/lcl")
                .join(key),
            lazarus_dir,
        })
    }

    fn source_dirs(&self) -> Vec<PathBuf> {
        let l = &self.lazarus_dir;
        [
            "lcl",
            "lcl/nonwin32",
            "lcl/forms",
            "lcl/widgetset",
            "lcl/interfaces/cocoa",
            "components/lazutils",
            "components/freetype",
        ]
        .iter()
        .map(|p| l.join(p))
        .collect()
    }

    /// Arguments every LCL compile shares, for the cache and for projects alike.
    fn common_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "-dLCL".into(),
            "-dLCLcocoa".into(),
            format!("-WM{MIN_MACOS}").into(),
            // fpc 3.2.2 emits Objective-C method lists that Apple's current linker rejects;
            // the classic linker accepts them. Apple has deprecated that flag.
            "-k-ld_classic".into(),
            "-k-framework".into(),
            "-kUserNotifications".into(),
        ];
        args.push(format!("-Fi{}", self.lazarus_dir.join("lcl/include").display()).into());
        args
    }

    fn ready_marker(&self) -> PathBuf {
        self.cache.join(".ready")
    }

    /// Compiles the LCL into the cache once; later calls return immediately.
    pub fn ensure_built(&self) -> std::io::Result<BuildResult> {
        self.ensure_built_with(Plan::WHOLE, &mut |_| {})
    }

    /// Whether the LCL still has to be compiled (the first GUI build on a machine).
    pub fn needs_build(&self) -> bool {
        !self.ready_marker().exists()
    }

    /// [`Lcl::ensure_built`] with progress, filling `plan`'s share of the bar.
    pub fn ensure_built_with(
        &self,
        plan: Plan,
        on: &mut dyn FnMut(Progress),
    ) -> std::io::Result<BuildResult> {
        if self.ready_marker().exists() {
            return Ok(BuildResult {
                success: true,
                diagnostics: vec![],
                raw_output: String::new(),
            });
        }
        std::fs::create_dir_all(&self.cache)?;
        let warm = self.cache.join("warm.lpr");
        std::fs::write(
            &warm,
            format!(
                "program warm;\n{{$mode objfpc}}{{$H+}}\nuses {};\nbegin\nend.\n",
                WARM_UNITS.join(", ")
            ),
        )?;
        let mut args = self.common_args();
        args.push("-Cn".into()); // units only, no link
        args.push(format!("-FU{}", self.cache.display()).into());
        for d in self.source_dirs() {
            args.push(format!("-Fu{}", d.display()).into());
        }
        let result = build_streaming(&warm, Mode::ObjFpc, &args, plan, &mut |mut p| {
            p.label = p.label.replace("Compiling", "Preparing LCL:");
            on(p);
        })?;
        if result.success {
            std::fs::write(self.ready_marker(), b"")?;
        }
        Ok(result)
    }

    /// Search paths for a project: cached units first, sources as the fallback.
    fn project_args(&self, units_out: &Path) -> Vec<OsString> {
        let mut args = self.common_args();
        args.push(format!("-FU{}", units_out.display()).into());
        args.push(format!("-Fu{}", self.cache.display()).into());
        for d in self.source_dirs() {
            args.push(format!("-Fu{}", d.display()).into());
        }
        args
    }
}

/// Units in the LCL cache of this machine's build of it; the first GUI build compiles about
/// this many, which sizes its progress bar.
pub const LCL_UNITS: usize = 160;

/// Builds a GUI program (a `.lpr` using the LCL) and returns the plain executable's result.
pub fn build_gui(source: &Path, mode: Mode, lcl: &Lcl) -> std::io::Result<BuildResult> {
    build_gui_progress(source, mode, lcl, None, &mut |_| {})
}

/// [`build_gui`] with progress. `total` is the number of units the last build of this program
/// compiled. The first build on a machine also compiles the LCL, which takes the first 70% of the bar.
pub fn build_gui_progress(
    source: &Path,
    mode: Mode,
    lcl: &Lcl,
    total: Option<usize>,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<BuildResult> {
    let lo = if lcl.needs_build() { 0.7 } else { 0.0 };
    if lo > 0.0 {
        let warm_plan = Plan {
            total: Some(LCL_UNITS),
            lo: 0.0,
            hi: lo,
        };
        let warm = lcl.ensure_built_with(warm_plan, on)?;
        if !warm.success {
            return Ok(warm);
        }
    }
    let dir = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let units_out = dir.join("lib");
    std::fs::create_dir_all(&units_out)?;
    let units_out = units_out.canonicalize()?;
    let plan = Plan { total, lo, hi: 1.0 };
    build_streaming(source, mode, &lcl.project_args(&units_out), plan, on)
}

/// Wraps `exe` into `<dir>/<name>.app` so macOS treats it as a real application (Dock icon,
/// menu bar, light/dark appearance). Returns the bundle path.
pub fn make_bundle(exe: &Path, name: &str) -> std::io::Result<PathBuf> {
    let bundle = exe
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{name}.app"));
    let macos = bundle.join("Contents/MacOS");
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle)?;
    }
    std::fs::create_dir_all(&macos)?;
    std::fs::copy(exe, macos.join(name))?;
    std::fs::write(bundle.join("Contents/Info.plist"), info_plist(name))?;
    allow_debugging(&bundle)?;
    Ok(bundle)
}

/// No `NSPrincipalClass`: LCL creates its own `NSApplication` subclass, and if AppKit makes a
/// plain one first the app dies with `unrecognized selector lclSetLCLMainLoop:`.
fn info_plist(name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>{name}</string>
<key>CFBundleIdentifier</key><string>com.pascoda.{name}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>1.0</string>
<key>LSMinimumSystemVersion</key><string>{MIN_MACOS}</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#
    )
}

fn fpc_version() -> String {
    Command::new("fpc")
        .arg("-iV")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Folders that may hold `vendor/`, best first. See `Lcl::find`.
pub fn search_dirs(start: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = start.ancestors().map(Path::to_path_buf).collect();
    dirs.extend(std::env::var_os("PASCODA_HOME").map(PathBuf::from));
    // Contents/MacOS/<exe> -> Contents/Resources
    if let Ok(exe) = std::env::current_exe()
        && let Some(contents) = exe.parent().and_then(Path::parent)
    {
        dirs.push(contents.join("Resources"));
    }
    if cfg!(debug_assertions) {
        dirs.extend(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .map(Path::to_path_buf),
        );
    }
    dirs
}

fn commit(lazarus_dir: &Path) -> String {
    // A bundled copy has no .git; bundle.sh records the commit it was copied from.
    if let Ok(c) = std::fs::read_to_string(lazarus_dir.join(".pascoda-commit")) {
        let c = c.trim();
        if !c.is_empty() {
            return c.into();
        }
    }
    commit_from_git(lazarus_dir)
}

fn commit_from_git(lazarus_dir: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(lazarus_dir)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
