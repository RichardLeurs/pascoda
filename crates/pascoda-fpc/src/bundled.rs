//! The Free Pascal compiler that ships inside Pascoda.app.
//!
//! `packaging/fpc-bundle.sh` copies the compiler (`ppca64`, as `bin/fpc`) and the units Pascoda needs
//! into `vendor/fpc` (a source checkout) or `Contents/Resources/vendor/fpc` (the app). It is found like
//! the other vendored tools (`lcl::fixed_dirs`) and wins over an `fpc` on the `PATH`, so the compiler
//! is the tested one; `PASCODA_FPC` still overrides it.
//!
//! The compiler's configuration file names absolute folders, and an app can be moved, so none is
//! shipped: [`config_file`] writes one for where the compiler is now, into the user's cache.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Written by `fpc-bundle.sh` at the root of the bundled compiler; its text is the compiler version.
pub const MARKER: &str = ".pascoda-fpc";

/// Whether `prefix` is a compiler laid out by `fpc-bundle.sh`.
pub fn is_bundle(prefix: &Path) -> bool {
    prefix.join(MARKER).is_file() && prefix.join("bin/fpc").is_file()
}

/// The bundled compiler's folder, if there is one.
pub fn prefix() -> Option<PathBuf> {
    crate::lcl::fixed_dirs()
        .iter()
        .map(|d| d.join("vendor/fpc"))
        .find(|p| is_bundle(p))
}

/// The bundled compiler's executable.
pub fn fpc() -> Option<PathBuf> {
    prefix().map(|p| p.join("bin/fpc"))
}

/// The compiler version a bundle was made from (`3.2.2`).
pub fn version(prefix: &Path) -> Option<String> {
    let v = std::fs::read_to_string(prefix.join(MARKER)).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// The developer tools folder (`xcode-select -p`): the Command Line Tools or Xcode. Not `xcrun`, which
/// can pop up the installer when there are no tools.
fn developer_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = std::process::Command::new("xcode-select").arg("-p").output().ok()?;
        let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        (out.status.success() && dir.is_dir()).then_some(dir)
    })
    .clone()
}

/// What the linker needs from the developer tools: the macOS SDK, the folder of their `ld`, and the
/// folder of `libclang_rt.osx.a` (the Cocoa units refer to it).
#[derive(Default, Clone)]
pub struct Tools {
    pub sdk: Option<PathBuf>,
    pub bin: Option<PathBuf>,
    pub clang_rt: Option<PathBuf>,
}

/// The newest `<root>/<version>/lib/darwin` that has `libclang_rt.osx.a`.
fn clang_rt_in(root: &Path) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join("lib/darwin"))
        .filter(|d| d.join("libclang_rt.osx.a").exists())
        .collect();
    // Version folders are numbers (`17`, `21.0.0`): the largest name last, compared as numbers.
    versions.sort_by_key(|d| {
        d.parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().split('.').filter_map(|p| p.parse::<u32>().ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    });
    versions.pop()
}

/// The developer tools' SDK, linker folder and clang runtime.
pub fn tools() -> Tools {
    let Some(dev) = developer_dir() else {
        return Tools::default();
    };
    let sdk = [
        dev.join("SDKs/MacOSX.sdk"),
        dev.join("Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"),
    ]
    .into_iter()
    .find(|p| p.is_dir());
    let bin = Some(dev.join("usr/bin")).filter(|b| b.join("ld").exists());
    // The Command Line Tools keep it under usr/lib/clang, Xcode inside its default toolchain.
    let clang_rt = [
        dev.join("usr/lib/clang"),
        dev.join("Toolchains/XcodeDefault.xctoolchain/usr/lib/clang"),
    ]
    .iter()
    .find_map(|r| clang_rt_in(r));
    Tools { sdk, bin, clang_rt }
}

/// The text of `fpc.cfg` for the compiler in `prefix`: the unit folders of the bundle, and the macOS SDK
/// and tools the linker needs. The switches are those of Free Pascal's own sample file, so a build
/// behaves as it does with an installed compiler.
pub fn config_text(prefix: &Path, version: &str, tools: &Tools) -> String {
    let units = prefix.join("lib/fpc").join(version).join("units");
    let mut t = String::from(
        "# Written by Pascoda for the Free Pascal compiler that ships with it. It is rewritten when a\n\
         # build starts, so do not edit it: put your own settings in ~/.fpc.cfg or the project's fpc.cfg.\n\
         #IFDEF RELEASE\n  -O2\n  -Xs\n#ENDIF\n\
         #IFDEF DEBUG\n  -gl\n  -Crtoi\n#ELSE\n  -Xs\n#ENDIF\n\
         #ifdef darwin\n-ap\n#endif\n\
         -Sgic\n",
    );
    t.push_str(&format!("-Fu{}/$fpctarget/*\n", units.display()));
    t.push_str(&format!("-Fu{}/$fpctarget/rtl\n", units.display()));
    if let Some(sdk) = &tools.sdk {
        t.push_str(&format!("-Fl{}/usr/lib\n", sdk.display()));
    }
    if let Some(rt) = &tools.clang_rt {
        t.push_str(&format!("-Fl{}\n", rt.display()));
    }
    if let Some(bin) = &tools.bin {
        t.push_str(&format!("-FD{}\n", bin.display()));
    }
    t
}

/// A short stable name for a folder (FNV-1a), so each place the app has been run from has its own file.
fn short_hash(path: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.to_string_lossy().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", h as u32)
}

/// Writes the configuration file of the bundled compiler in `prefix` under `cache` and returns its path.
/// A file that is already right is left alone, so builds running side by side do not race.
pub fn write_config(prefix: &Path, cache: &Path, tools: &Tools) -> Option<PathBuf> {
    let version = version(prefix)?;
    let dir = cache.join(format!("{version}-{}", short_hash(prefix)));
    let file = dir.join("fpc.cfg");
    let text = config_text(prefix, &version, tools);
    if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
        return Some(file);
    }
    std::fs::create_dir_all(&dir).ok()?;
    // Written beside it and moved into place, so a reader never sees half a file.
    let tmp = dir.join(format!("fpc.cfg.{}", std::process::id()));
    std::fs::write(&tmp, &text).ok()?;
    std::fs::rename(&tmp, &file).ok()?;
    Some(file)
}

/// The configuration file for the bundled compiler in `prefix` (see [`write_config`]), in
/// `~/Library/Caches/Pascoda/fpc`.
pub fn config_file(prefix: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let cache = Path::new(&home).join("Library/Caches/Pascoda/fpc");
    write_config(prefix, &cache, &tools())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-bundled-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_bundle(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/fpc"), "").unwrap();
        std::fs::write(dir.join(MARKER), format!("{version}\n")).unwrap();
    }

    #[test]
    fn a_bundle_needs_its_marker_and_its_compiler() {
        let d = scratch("marker");
        assert!(!is_bundle(&d));
        fake_bundle(&d, "3.2.2");
        assert!(is_bundle(&d));
        assert_eq!(version(&d).as_deref(), Some("3.2.2"));
        std::fs::remove_file(d.join("bin/fpc")).unwrap();
        assert!(!is_bundle(&d), "a marker without the compiler is not a bundle");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_config_names_the_bundles_units_and_the_sdk() {
        let text = config_text(
            Path::new("/Apps/Pascoda.app/Contents/Resources/vendor/fpc"),
            "3.2.2",
            &Tools {
                sdk: Some("/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk".into()),
                bin: Some("/Library/Developer/CommandLineTools/usr/bin".into()),
                clang_rt: Some("/Library/Developer/CommandLineTools/usr/lib/clang/17/lib/darwin".into()),
            },
        );
        assert!(text.contains("-Fu/Apps/Pascoda.app/Contents/Resources/vendor/fpc/lib/fpc/3.2.2/units/$fpctarget/*\n"));
        assert!(text.contains("units/$fpctarget/rtl\n"));
        assert!(text.contains("-Fl/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/lib\n"));
        assert!(text.contains("-Fl/Library/Developer/CommandLineTools/usr/lib/clang/17/lib/darwin\n"), "Cocoa units link libclang_rt");
        assert!(text.contains("-FD/Library/Developer/CommandLineTools/usr/bin\n"));
        assert!(text.contains("#ifdef darwin\n-ap\n#endif"), "the switches of the sample file are kept");
        // Without developer tools there are no tool lines.
        let none = config_text(Path::new("/x"), "3.2.2", &Tools::default());
        assert!(!none.contains("-Fl") && !none.contains("-FD"));
    }

    /// With a bundled compiler present (`packaging/fpc-bundle.sh vendor/fpc`), it is the one used, and it
    /// compiles, links and runs a program with the configuration written for it. Skipped without one.
    #[test]
    fn the_bundled_compiler_is_chosen_and_builds_a_program() {
        let Some(fpc) = fpc() else {
            eprintln!("skipped: no vendor/fpc (packaging/fpc-bundle.sh vendor/fpc)");
            return;
        };
        if std::env::var_os("PASCODA_FPC").is_some() {
            return; // forced to another compiler
        }
        assert_eq!(crate::locate_fpc().as_deref(), Some(fpc.as_path()), "wins over the one on PATH");
        let dir = scratch("builds");
        let src = dir.join("hello.pas");
        std::fs::write(
            &src,
            "program hello;\n{$mode objfpc}{$H+}\nuses SysUtils, Classes, fgl;\nvar L: specialize TFPGList<Integer>;\nbegin\n  L := specialize TFPGList<Integer>.Create;\n  L.Add(21);\n  WriteLn(Format('shipped %d', [L[0] * 2]));\n  L.Free;\nend.\n",
        )
        .unwrap();
        let built = crate::build(&src, crate::Mode::ObjFpc).unwrap();
        assert!(built.success, "{}", built.raw_output);
        let out = std::process::Command::new(dir.join("hello")).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "shipped 42\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newest_clang_runtime_with_the_library_is_chosen() {
        let root = scratch("clang");
        for v in ["9", "17", "17.0.0", "21.0.0", "100"] {
            std::fs::create_dir_all(root.join(v).join("lib/darwin")).unwrap();
        }
        // Only some versions have the library; 100 has none.
        for v in ["9", "17", "21.0.0"] {
            std::fs::write(root.join(v).join("lib/darwin/libclang_rt.osx.a"), "").unwrap();
        }
        assert_eq!(clang_rt_in(&root), Some(root.join("21.0.0/lib/darwin")), "numbers, not text: 21 beats 9");
        assert_eq!(clang_rt_in(&root.join("none")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_config_follows_the_bundle_when_it_moves() {
        let cache = scratch("cache");
        let a = scratch("a");
        let b = scratch("b");
        fake_bundle(&a, "3.2.2");
        fake_bundle(&b, "3.2.2");
        let fa = write_config(&a, &cache, &Tools::default()).unwrap();
        let fb = write_config(&b, &cache, &Tools::default()).unwrap();
        assert_ne!(fa, fb, "each place has its own file");
        assert!(std::fs::read_to_string(&fa).unwrap().contains(&a.display().to_string()));
        assert!(std::fs::read_to_string(&fb).unwrap().contains(&b.display().to_string()));
        // Asking again leaves a right file as it is, and repairs a wrong one.
        let before = std::fs::metadata(&fa).unwrap().modified().unwrap();
        assert_eq!(write_config(&a, &cache, &Tools::default()).unwrap(), fa);
        assert_eq!(std::fs::metadata(&fa).unwrap().modified().unwrap(), before);
        std::fs::write(&fa, "stale").unwrap();
        write_config(&a, &cache, &Tools::default()).unwrap();
        assert!(std::fs::read_to_string(&fa).unwrap().contains("-Fu"));
        for d in [cache, a, b] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}
