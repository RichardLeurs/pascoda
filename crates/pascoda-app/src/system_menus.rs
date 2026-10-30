//! Items macOS adds to the Edit menu by itself (Writing Tools, AutoFill, Start Dictation, Emoji &
//! Symbols). The IDE has no use for them, so they are taken out of AppKit's Edit menu. The bundle's
//! Info.plist switches off dictation and the character palette too (see `packaging/bundle.sh`).

use objc2_app_kit::{NSApplication, NSMenu};
use objc2_foundation::{MainThreadMarker, NSString};

/// The titles of the items to take out of the Edit menu, without the trailing `…` macOS puts on some.
const HIDDEN: [&str; 4] = ["Writing Tools", "AutoFill", "Start Dictation", "Emoji & Symbols"];

/// Removes the system's extra Edit menu items. macOS may add them after the menu is first shown,
/// so this runs every frame; it does nothing once they are gone.
pub fn hide_edit_items() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(edit) = app
        .mainMenu()
        .and_then(|m| m.itemWithTitle(&NSString::from_str("Edit")))
        .and_then(|i| i.submenu())
    else {
        return;
    };
    let mut removed = false;
    for i in (0..edit.numberOfItems()).rev() {
        let Some(item) = edit.itemAtIndex(i) else { continue };
        if item.isSeparatorItem() {
            continue;
        }
        let title = item.title().to_string();
        let name = title.trim_end_matches(['…', '.']).trim();
        if HIDDEN.contains(&name) {
            edit.removeItemAtIndex(i);
            removed = true;
        }
    }
    if removed {
        tidy_separators(&edit);
    }
}

/// Removes separators that would start the menu, end it, or sit next to another separator, so the
/// removed items leave no double lines behind.
fn tidy_separators(menu: &NSMenu) {
    for i in (0..menu.numberOfItems()).rev() {
        let Some(item) = menu.itemAtIndex(i) else { continue };
        if !item.isSeparatorItem() {
            continue;
        }
        let at_start = i == 0;
        let at_end = i + 1 == menu.numberOfItems();
        let after_separator = i > 0
            && menu.itemAtIndex(i - 1).is_some_and(|prev| prev.isSeparatorItem());
        if at_start || at_end || after_separator {
            menu.removeItemAtIndex(i);
        }
    }
}
