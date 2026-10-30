//! The tools Pascoda needs besides the IDE itself: which of them can be found, and the command that
//! installs the ones that are missing.
//!
//! The app bundle ships them (`packaging/bundle.sh`), so an end user normally has nothing to install.
//! A copy without them (a source checkout without `vendor/`, a damaged bundle) shows what is missing
//! and offers [`install_command`], which the user approves in the Terminal.

use std::path::{Path, PathBuf};

/// One tool the IDE looks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    /// The Free Pascal compiler: shipped in the app (`bundled`), else one installed on the Mac.
    Fpc,
    /// The Lazarus sources: the LCL (GUI programs) and CodeTools (the language server).
    Lazarus,
    /// The FPC `rtl` and `packages` sources: standard units, the database units, FPCUnit.
    FpcSources,
    /// The Pascal language server.
    Pasls,
}

/// Lazarus commit and FPC tag the IDE was tested with.
pub const LAZARUS_COMMIT: &str = "2dbcbed8";
pub const FPC_TAG: &str = "release_3_2_2";
/// The language server revision the IDE was tested with.
pub const PASLS_COMMIT: &str = "818f633";

impl Part {
    pub const ALL: [Part; 4] = [Part::Fpc, Part::Lazarus, Part::FpcSources, Part::Pasls];

    pub fn name(self) -> &'static str {
        match self {
            Part::Fpc => "Free Pascal compiler (fpc)",
            Part::Lazarus => "Lazarus sources",
            Part::FpcSources => "Free Pascal sources",
            Part::Pasls => "Language server (pasls)",
        }
    }

    /// What stops working without it.
    pub fn used_for(self) -> &'static str {
        match self {
            Part::Fpc => "building and running programs",
            Part::Lazarus => "GUI programs and the forms they show",
            Part::FpcSources => "database components, unit tests and the standard units",
            Part::Pasls => "completion, hover and go to definition",
        }
    }

    /// Whether it is in one of `dirs` (folders that may hold `vendor/`); `fpc_found` is for the compiler.
    pub fn is_present_in(self, dirs: &[PathBuf], fpc_found: bool) -> bool {
        let has = |rel: &[&str]| dirs.iter().any(|d| rel.iter().all(|r| d.join("vendor").join(r).exists()));
        match self {
            Part::Fpc => fpc_found,
            Part::Lazarus => has(&["lazarus/lcl/interfaces/cocoa"]),
            Part::FpcSources => has(&["fpcsrc/rtl", "fpcsrc/packages/fcl-db/src", "fpcsrc/packages/fcl-fpcunit/src"]),
            Part::Pasls => has(&["pasls/dist/aarch64-darwin/pasls"]),
        }
    }
}

/// `~/Library/Application Support/Pascoda`: a folder that may hold `vendor/`, writable by the user, so
/// the tools can be installed without touching the app bundle.
pub fn user_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Pascoda"))
}

/// What is missing for a file at `start`, found the way the builds and the language server find it.
pub fn missing(start: &Path) -> Vec<Part> {
    missing_in(&crate::lcl::search_dirs(start), crate::locate_fpc().is_some())
}

pub fn missing_in(dirs: &[PathBuf], fpc_found: bool) -> Vec<Part> {
    Part::ALL
        .into_iter()
        .filter(|p| !p.is_present_in(dirs, fpc_found))
        .collect()
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// One shell line that installs `parts` under `root/vendor`, pinned to the tested versions. It is one
/// line (joined with `&&`) so that typing it into the Terminal without Enter keeps it from running
/// before the user has read it. `lazarus` is where the Lazarus sources are when they are already
/// there (the language server is built against them).
pub fn install_command(parts: &[Part], root: &Path, lazarus: Option<&Path>) -> String {
    let vendor = root.join("vendor");
    let mut steps: Vec<String> = Vec::new();
    if parts.contains(&Part::Fpc) {
        steps.push("brew install fpc".into());
    }
    let needs_dir = parts.iter().any(|p| *p != Part::Fpc);
    if needs_dir {
        steps.push(format!("mkdir -p {v} && cd {v}", v = quote(&vendor)));
    }
    let wants_lazarus = parts.contains(&Part::Lazarus) || (parts.contains(&Part::Pasls) && lazarus.is_none());
    if wants_lazarus {
        steps.push(format!(
            "git clone --filter=blob:none --no-checkout https://gitlab.com/freepascal.org/lazarus/lazarus.git lazarus && git -C lazarus checkout {LAZARUS_COMMIT}"
        ));
    }
    if parts.contains(&Part::FpcSources) {
        steps.push(format!(
            "git clone --depth 1 --branch {FPC_TAG} --filter=blob:none --sparse https://gitlab.com/freepascal.org/fpc/source.git fpcsrc && git -C fpcsrc sparse-checkout set rtl packages"
        ));
    }
    if parts.contains(&Part::Pasls) {
        let lazarus_dir = match lazarus {
            Some(dir) if !wants_lazarus => quote(dir),
            _ => "\"$PWD/../../lazarus\"".to_string(),
        };
        steps.push(format!(
            "git clone --recurse-submodules https://github.com/genericptr/pascal-language-server.git pasls && git -C pasls checkout {PASLS_COMMIT} && git -C pasls submodule update --init --recursive && (cd pasls/src && LAZARUSDIR={lazarus_dir} bash build_fpc.sh)"
        ));
    }
    steps.join(" && ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(base: &Path, files: &[&str]) {
        for f in files {
            let p = base.join("vendor").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x").unwrap();
            if !f.contains('.') && !f.ends_with("pasls") {
                let _ = std::fs::remove_file(&p);
                std::fs::create_dir_all(&p).unwrap();
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pascoda-vendor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn everything_is_missing_from_an_empty_folder() {
        let d = scratch("empty");
        assert_eq!(missing_in(&[d.clone()], false), Part::ALL);
        assert_eq!(missing_in(&[d.clone()], true), [Part::Lazarus, Part::FpcSources, Part::Pasls]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn each_part_is_found_in_any_of_the_folders() {
        let a = scratch("a");
        let b = scratch("b");
        tree(&a, &["lazarus/lcl/interfaces/cocoa"]);
        tree(&b, &["fpcsrc/rtl", "fpcsrc/packages/fcl-db/src", "fpcsrc/packages/fcl-fpcunit/src", "pasls/dist/aarch64-darwin/pasls"]);
        let dirs = [a.clone(), b.clone()];
        assert!(missing_in(&dirs, true).is_empty(), "{:?}", missing_in(&dirs, true));
        assert_eq!(missing_in(&[a.clone()], true), [Part::FpcSources, Part::Pasls]);
        assert_eq!(missing_in(&[b.clone()], true), [Part::Lazarus]);
        // Half a tree is not enough: FPCUnit and the database units are needed.
        let c = scratch("c");
        tree(&c, &["fpcsrc/rtl"]);
        assert!(missing_in(&[c.clone()], true).contains(&Part::FpcSources));
        for d in [a, b, c] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn the_install_command_is_one_pinned_line_for_what_is_missing() {
        let root = Path::new("/Users/x/Library/Application Support/Pascoda");
        let all = install_command(&Part::ALL, root, None);
        assert!(!all.contains('\n'), "one line, so it waits for Enter");
        assert!(all.starts_with("brew install fpc && mkdir -p '/Users/x/Library/Application Support/Pascoda/vendor'"));
        assert!(all.contains(&format!("checkout {LAZARUS_COMMIT}")));
        assert!(all.contains(&format!("--branch {FPC_TAG}")));
        assert!(all.contains(&format!("checkout {PASLS_COMMIT}")));
        assert!(all.contains("LAZARUSDIR=\"$PWD/../../lazarus\""), "the server is built against the Lazarus clone");

        // Only the language server, Lazarus already somewhere else: no second Lazarus clone.
        let only = install_command(&[Part::Pasls], root, Some(Path::new("/opt/lazarus")));
        assert!(!only.contains("lazarus.git") && !only.contains("brew"), "{only}");
        assert!(only.contains("LAZARUSDIR='/opt/lazarus'"), "{only}");

        // Only the compiler: no folder is made.
        assert_eq!(install_command(&[Part::Fpc], root, None), "brew install fpc");
    }
}
