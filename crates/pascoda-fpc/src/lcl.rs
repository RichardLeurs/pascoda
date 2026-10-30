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
        source_dirs(&self.lazarus_dir)
    }

    /// Arguments every LCL compile shares, for the cache and for projects alike.
    fn common_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = defines(&self.lazarus_dir)
            .into_iter()
            .map(Into::into)
            .collect();
        args.extend([
            format!("-WM{MIN_MACOS}").into(),
            // fpc 3.2.2 emits Objective-C method lists that Apple's current linker rejects;
            // the classic linker accepts them. Apple has deprecated that flag.
            "-k-ld_classic".into(),
            "-k-framework".into(),
            "-kUserNotifications".into(),
        ]);
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
    pub(crate) fn project_args(&self, units_out: &Path) -> Vec<OsString> {
        let mut args = self.common_args();
        args.push(format!("-FU{}", units_out.display()).into());
        args.push(format!("-Fu{}", self.cache.display()).into());
        for d in self.source_dirs() {
            args.push(format!("-Fu{}", d.display()).into());
        }
        args
    }
}

/// The LCL's source folders, relative to the Lazarus tree.
fn source_dirs(lazarus: &Path) -> Vec<PathBuf> {
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
    .map(|p| lazarus.join(p))
    .collect()
}

/// The defines and include folder the LCL is compiled with.
fn defines(lazarus: &Path) -> Vec<String> {
    vec![
        "-dLCL".into(),
        "-dLCLcocoa".into(),
        format!("-Fi{}", lazarus.join("lcl/include").display()),
    ]
}

/// Compiler switches that let a tool parse the LCL's sources as a project would (pasls).
/// Without them the LCL's `{$I lcl_defines.inc}` is not found and no LCL type resolves.
pub fn pasls_options(lazarus: &Path) -> Vec<String> {
    let mut options = defines(lazarus);
    options.extend(
        source_dirs(lazarus)
            .iter()
            .map(|d| format!("-Fu{}", d.display())),
    );
    options
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
    build_gui_progress_with(source, mode, lcl, &[], total, on)
}

/// [`build_gui_progress`] with extra compiler arguments, such as a project file's search paths.
pub fn build_gui_progress_with(
    source: &Path,
    mode: Mode,
    lcl: &Lcl,
    extra: &[OsString],
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
    let mut args = lcl.project_args(&units_out);
    args.extend(extra.iter().cloned());
    build_streaming(source, mode, &args, plan, on)
}

/// Wraps `exe` into `<dir>/<name>.app` so macOS treats it as a real application (Dock icon,
/// menu bar, light/dark appearance). Returns the bundle path.
pub fn make_bundle(exe: &Path, name: &str) -> std::io::Result<PathBuf> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    bundle(exe, name, dir, true, &BundleInfo::default())
}

/// What an exported app says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleInfo {
    /// `com.example.app`; `com.pascoda.<name>` when empty.
    pub bundle_id: String,
    /// `1.0` when empty.
    pub version: String,
    /// A `.icns` (copied) or `.png` (converted with `sips`) file.
    pub icon: Option<PathBuf>,
}

impl BundleInfo {
    /// `Err` with a sentence for the user when a field cannot be used.
    pub fn check(&self) -> Result<(), String> {
        let id = self.bundle_id.trim();
        if !id.is_empty()
            && (!id.contains('.')
                || id.starts_with('.')
                || id.ends_with('.')
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        {
            return Err(
                "The bundle identifier uses letters, digits, '-' and dots, like com.example.myapp."
                    .into(),
            );
        }
        let v = self.version.trim();
        if !v.is_empty() && !v.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err("The version uses digits and dots, like 1.0 or 2.1.3.".into());
        }
        if let Some(i) = &self.icon {
            let ext = i
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if !i.is_file() || !matches!(ext.as_deref(), Some("icns" | "png")) {
                return Err("The icon must be an existing .icns or .png file.".into());
            }
        }
        Ok(())
    }
}

/// `<dir>/<name>.app` around `exe`. `debuggable` re-signs for lldb (a development build); a
/// release bundle gets a plain ad-hoc signature instead.
pub(crate) fn bundle(
    exe: &Path,
    name: &str,
    dir: &Path,
    debuggable: bool,
    info: &BundleInfo,
) -> std::io::Result<PathBuf> {
    let bundle = dir.join(format!("{name}.app"));
    let macos = bundle.join("Contents/MacOS");
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle)?;
    }
    std::fs::create_dir_all(&macos)?;
    std::fs::copy(exe, macos.join(name))?;
    let icon_file = match &info.icon {
        Some(icon) => {
            let res = bundle.join("Contents/Resources");
            std::fs::create_dir_all(&res)?;
            let to = res.join("AppIcon.icns");
            if icon
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("icns"))
            {
                std::fs::copy(icon, &to)?;
            } else {
                png_to_icns(icon, &to)?;
            }
            Some("AppIcon")
        }
        None => None,
    };
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        info_plist(name, info, icon_file),
    )?;
    if debuggable {
        allow_debugging(&bundle)?;
    } else {
        let st = Command::new("codesign")
            .args(["--force", "--deep", "-s", "-"])
            .arg(&bundle)
            .output()?;
        if !st.status.success() {
            return Err(std::io::Error::other(format!(
                "codesign failed: {}",
                String::from_utf8_lossy(&st.stderr).trim()
            )));
        }
    }
    Ok(bundle)
}

/// Makes an `.icns` from a PNG: every size of an iconset with `sips`, then `iconutil`.
fn png_to_icns(png: &Path, out: &Path) -> std::io::Result<()> {
    let set = std::env::temp_dir().join(format!("pascoda-icon-{}.iconset", std::process::id()));
    let _ = std::fs::remove_dir_all(&set);
    std::fs::create_dir_all(&set)?;
    let result = (|| {
        for (px, name) in [
            (16, "icon_16x16"),
            (32, "icon_16x16@2x"),
            (32, "icon_32x32"),
            (64, "icon_32x32@2x"),
            (128, "icon_128x128"),
            (256, "icon_128x128@2x"),
            (256, "icon_256x256"),
            (512, "icon_256x256@2x"),
            (512, "icon_512x512"),
            (1024, "icon_512x512@2x"),
        ] {
            let st = Command::new("sips")
                .args(["-z", &px.to_string(), &px.to_string()])
                .arg(png)
                .arg("--out")
                .arg(set.join(format!("{name}.png")))
                .output()?;
            if !st.status.success() {
                return Err(std::io::Error::other(format!(
                    "could not read the icon: {}",
                    String::from_utf8_lossy(&st.stderr).trim()
                )));
            }
        }
        let st = Command::new("iconutil")
            .args(["-c", "icns"])
            .arg(&set)
            .arg("-o")
            .arg(out)
            .output()?;
        if !st.status.success() {
            return Err(std::io::Error::other(format!(
                "could not build the icon: {}",
                String::from_utf8_lossy(&st.stderr).trim()
            )));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&set);
    result
}

/// No `NSPrincipalClass`: LCL creates its own `NSApplication` subclass, and if AppKit makes a
/// plain one first the app dies with `unrecognized selector lclSetLCLMainLoop:`.
fn info_plist(name: &str, info: &BundleInfo, icon: Option<&str>) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let id = match info.bundle_id.trim() {
        "" => format!("com.pascoda.{}", name.replace(' ', "-")),
        id => id.to_string(),
    };
    let version = match info.version.trim() {
        "" => "1.0",
        v => v,
    };
    let icon = icon
        .map(|i| format!("<key>CFBundleIconFile</key><string>{i}</string>\n"))
        .unwrap_or_default();
    let (id, version, name) = (esc(&id), esc(version), esc(name));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>{name}</string>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleName</key><string>{name}</string>
{icon}<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
<key>LSMinimumSystemVersion</key><string>{MIN_MACOS}</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#
    )
}

fn fpc_version() -> String {
    Command::new(crate::locate_fpc().unwrap_or_else(|| "fpc".into()))
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
    dirs.extend(fixed_dirs());
    dirs
}

/// The folders that may hold `vendor/` whatever file is open: `$PASCODA_HOME`, the app bundle, the
/// user's `Application Support/Pascoda`, and (debug builds) the source tree.
pub fn fixed_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(std::env::var_os("PASCODA_HOME").map(PathBuf::from));
    // Contents/MacOS/<exe> -> Contents/Resources
    if let Ok(exe) = std::env::current_exe()
        && let Some(contents) = exe.parent().and_then(Path::parent)
    {
        dirs.push(contents.join("Resources"));
    }
    // Where the tools are installed when the bundle does not have them (see `vendor::install_command`).
    dirs.extend(crate::vendor::user_root());
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
