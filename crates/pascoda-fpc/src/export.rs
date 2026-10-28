//! Export a finished program: an optimised build packaged as a `.app` (GUI programs) or a plain
//! executable (console programs), optionally inside a `.dmg`.

use crate::lcl::{self, BundleInfo, Lcl};
use crate::{BuildResult, Mode, Plan, Progress, build_streaming_as};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct Exported {
    /// What the compiler said. When it failed, `product` is `None`.
    pub build: BuildResult,
    /// `<name>.app`, or the executable of a console program.
    pub product: Option<PathBuf>,
    /// `<name>.dmg` when asked for.
    pub dmg: Option<PathBuf>,
}

/// A name that is a safe file and bundle name.
pub fn clean_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() { "Program".into() } else { s }
}

/// Builds `source` for release and writes the product (and `.dmg`) into `out_dir`. `lcl` is
/// `Some` for a GUI program (a `.lpr` using the LCL). Nothing is written next to the source:
/// the compiler works in a scratch folder.
pub fn export(
    source: &Path,
    mode: Mode,
    lcl: Option<&Lcl>,
    out_dir: &Path,
    name: &str,
    info: &BundleInfo,
    dmg: bool,
    total: Option<usize>,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<Exported> {
    info.check().map_err(std::io::Error::other)?;
    let name = clean_name(name);
    std::fs::create_dir_all(out_dir)?;
    let work = std::env::temp_dir().join(format!(
        "pascoda-export-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let units = work.join("units");
    std::fs::create_dir_all(&units)?;
    let result = run(
        source, mode, lcl, out_dir, &name, info, dmg, total, &work, &units, on,
    );
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[allow(clippy::too_many_arguments)]
fn run(
    source: &Path,
    mode: Mode,
    lcl: Option<&Lcl>,
    out_dir: &Path,
    name: &str,
    info: &BundleInfo,
    dmg: bool,
    total: Option<usize>,
    work: &Path,
    units: &Path,
    on: &mut dyn FnMut(Progress),
) -> std::io::Result<Exported> {
    let mut lo = 0.0;
    if let Some(lcl) = lcl
        && lcl.needs_build()
    {
        lo = 0.7;
        let warm = lcl.ensure_built_with(
            Plan {
                total: Some(lcl::LCL_UNITS),
                lo: 0.0,
                hi: lo,
            },
            on,
        )?;
        if !warm.success {
            return Ok(Exported {
                build: warm,
                product: None,
                dmg: None,
            });
        }
    }
    let mut extra: Vec<OsString> = match lcl {
        Some(l) => l.project_args(units),
        None => vec![format!("-FU{}", units.display()).into()],
    };
    extra.push(format!("-FE{}", work.display()).into());
    let plan = Plan { total, lo, hi: 0.9 };
    // The units compiled so far ride along in every later message: the app stores the last one
    // to size the bar of the next export.
    let mut done = 0;
    let build = build_streaming_as(source, mode, &extra, plan, true, &mut |p| {
        done = p.done;
        on(p);
    })?;
    if !build.success {
        return Ok(Exported {
            build,
            product: None,
            dmg: None,
        });
    }
    let stem = source.file_stem().unwrap_or_default();
    let exe = work.join(stem);
    on(Progress {
        label: "Packaging".into(),
        fraction: Some(0.93),
        done,
    });
    let product = if lcl.is_some() {
        lcl::bundle(&exe, name, out_dir, false, info)?
    } else {
        let to = out_dir.join(name);
        let _ = std::fs::remove_file(&to);
        std::fs::copy(&exe, &to)?;
        to
    };
    let dmg_path = if dmg {
        on(Progress {
            label: "Creating the disk image".into(),
            fraction: Some(0.96),
            done,
        });
        Some(make_dmg(&product, name, out_dir, lcl.is_some())?)
    } else {
        None
    };
    Ok(Exported {
        build,
        product: Some(product),
        dmg: dmg_path,
    })
}

/// `<out_dir>/<name>.dmg` holding `product` (and an Applications shortcut for an app).
fn make_dmg(product: &Path, name: &str, out_dir: &Path, is_app: bool) -> std::io::Result<PathBuf> {
    let stage = std::env::temp_dir().join(format!(
        "pascoda-dmg-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let content = stage.join(name);
    std::fs::create_dir_all(&content)?;
    let result = (|| {
        // ditto keeps signatures, symlinks and extended attributes.
        let target = content.join(product.file_name().unwrap_or_default());
        let st = Command::new("ditto").arg(product).arg(&target).output()?;
        if !st.status.success() {
            return Err(std::io::Error::other("ditto failed"));
        }
        if is_app {
            std::os::unix::fs::symlink("/Applications", content.join("Applications"))?;
        }
        let dmg = out_dir.join(format!("{name}.dmg"));
        let _ = std::fs::remove_file(&dmg);
        let out = Command::new("diskutil")
            .args([
                "image",
                "create",
                "from",
                "--format",
                "ULFO",
                "--volumeName",
                name,
            ])
            .arg(&content)
            .arg(&dmg)
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "diskutil failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(dmg)
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(clean_name("My App"), "My App");
        assert_eq!(clean_name("a/b:c"), "a_b_c");
        assert_eq!(clean_name("  "), "Program");
    }
}
