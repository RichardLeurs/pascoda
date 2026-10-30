//! The text of the About panel: what Pascoda is, who holds the copyright, and what it is
//! built with. macOS shows the credits in a scrolling box under the version.

use std::process::Command;

pub const NAME: &str = "Pascoda";
pub const COPYRIGHT: &str = "Copyright © Richard Leurs 2026";
//Copyright © 2026 Richard Leurs. All rights reserved.
const INFO: &str = "A modern IDE for Pascal on macOS, written in Rust. It \
compiles with Free Pascal, debugs with lldb, completes code with the Pascal language server, \
and designs forms with live native macOS controls.";

/// Rust crates the IDE is built on: `(crate, version, what it is used for, licence)`.
/// `tests::versions_match_cargo_lock` keeps the versions honest.
pub const CRATES: &[(&str, &str, &str, &str)] = &[
    (
        "eframe / egui",
        "0.37.0",
        "user interface",
        "MIT or Apache-2.0",
    ),
    ("muda", "0.21.2", "native menu bar", "Apache-2.0 or MIT"),
    ("rfd", "0.17.2", "file dialogs", "MIT"),
    (
        "serde_json",
        "1.0.154",
        "debugger and language-server messages",
        "MIT or Apache-2.0",
    ),
    (
        "image",
        "0.25.10",
        "decoding the component icons",
        "MIT or Apache-2.0",
    ),
    (
        "portable-pty",
        "0.9.0",
        "pseudo-terminal for the integrated terminal",
        "MIT",
    ),
    (
        "notify",
        "8.2.1",
        "noticing files changed by other programs (FSEvents)",
        "CC0-1.0",
    ),
    (
        "vt100",
        "0.18.1",
        "terminal emulator state for the integrated terminal",
        "MIT",
    ),
    (
        "libc",
        "0.2.191",
        "signals for stopping shells",
        "MIT or Apache-2.0",
    ),
    ("objc2", "0.6.5", "calling AppKit from Rust", "MIT"),
    (
        "objc2-app-kit",
        "0.3.2",
        "AppKit controls",
        "Zlib or Apache-2.0 or MIT",
    ),
    ("objc2-foundation", "0.3.2", "Foundation types", "MIT"),
    (
        "raw-window-handle",
        "0.6.2",
        "access to the native window",
        "MIT or Apache-2.0 or Zlib",
    ),
];

/// The Free Pascal version on this machine, if the compiler is installed.
fn fpc_version() -> Option<String> {
    let out = Command::new(pascoda_fpc::locate_fpc()?)
        .arg("-iV")
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

/// The credits text. `fpc` is the installed compiler version, when known.
pub fn credits(fpc: Option<&str>) -> String {
    let mut s = format!("{INFO}\n\nBuilt with:\n");
    for (name, version, purpose, licence) in CRATES {
        s += &format!("\u{2022} {name} {version}: {purpose} ({licence})\n");
    }
    s += "\nUsed by Pascoda and the programs it builds:\n";
    s += &format!(
        "\u{2022} Free Pascal {}: the compiler, included in the app with its source offer (GPL; runtime library LGPL with linking exception)\n",
        fpc.unwrap_or("(not found)")
    );
    s += "\u{2022} Lazarus Component Library, Cocoa widgetset: runs the forms you design, and Lazarus's component icons (modified LGPL, closed-source programs allowed)\n";
    s += "\u{2022} lldb-dap, from Xcode: the debugger\n";
    s += "\u{2022} pascal-language-server (pasls): code completion (GPL-3.0, run as a separate program)\n";
    s
}

pub fn metadata() -> muda::AboutMetadata {
    muda::AboutMetadata {
        name: Some(NAME.into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: Some(COPYRIGHT.into()),
        credits: Some(credits(fpc_version().as_deref())),
        ..Default::default()
    }
}

/// Self-test: picks "About Pascoda" from the app menu, as a user would. False when the menu or
/// the item is not there.
pub fn open_via_menu() -> bool {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, ns_string};
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(app_menu) = app
        .mainMenu()
        .and_then(|m| m.itemAtIndex(0))
        .and_then(|i| i.submenu())
    else {
        return false;
    };
    let Some(item) = app_menu.itemWithTitle(ns_string!("About Pascoda")) else {
        return false;
    };
    app_menu.performActionForItemAtIndex(app_menu.indexOfItem(&item));
    true
}

/// Self-test: renders the content of the key window (the About panel once it is open) to a PNG
/// and returns its title and size.
pub fn snapshot_key_window(path: &std::path::Path) -> Result<String, String> {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSApplication, NSBitmapImageFileType};
    use objc2_foundation::{MainThreadMarker, NSDictionary, NSString};
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    // The panel is not the key window when the app was started from a terminal, so find it:
    // the visible window that is not the IDE's own (winit) window.
    let windows = app.windows();
    let names: Vec<String> = windows
        .iter()
        .map(|w| {
            format!(
                "{} visible={}",
                w.class().name().to_string_lossy(),
                w.isVisible()
            )
        })
        .collect();
    let window = windows
        .iter()
        .find(|w| w.isVisible() && !w.class().name().to_string_lossy().contains("Winit"))
        .ok_or_else(|| format!("no About window among {names:?}"))?;
    let view = window.contentView().ok_or("no content view")?;
    let bounds = view.bounds();
    let rep = view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or("no bitmap")?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let props = NSDictionary::<NSString, AnyObject>::new();
    let data =
        unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }
            .ok_or("PNG encoding failed")?;
    std::fs::write(path, data.to_vec()).map_err(|e| e.to_string())?;
    Ok(format!(
        "{:?} {}x{}",
        window.title().to_string(),
        bounds.size.width,
        bounds.size.height
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn credits_cover_info_dependencies_and_tools() {
        let text = credits(Some("3.2.2"));
        assert!(text.contains("A modern IDE for Pascal"));
        for (name, version, _, licence) in CRATES {
            assert!(
                text.contains(&format!("{name} {version}")) && text.contains(licence),
                "{name}"
            );
        }
        assert!(
            text.contains("Free Pascal 3.2.2")
                && text.contains("lldb-dap")
                && text.contains("pasls")
        );
        assert!(credits(None).contains("Free Pascal (not found)"));
        assert!(COPYRIGHT.starts_with("Copyright") && COPYRIGHT.ends_with("Richard Leurs 2026"));
    }

    /// The list above must name the versions the program is actually built with.
    #[test]
    fn versions_match_cargo_lock() {
        let lock =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
                .unwrap();
        for (name, version, _, _) in CRATES {
            for krate in name.split(" / ") {
                let krate = if krate == "eframe" || krate == "egui" {
                    krate
                } else {
                    name
                };
                let entry = format!("name = \"{krate}\"\nversion = \"{version}\"");
                assert!(
                    lock.contains(&entry),
                    "{krate} {version} is not in Cargo.lock"
                );
            }
        }
    }
}
