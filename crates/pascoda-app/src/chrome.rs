//! Window chrome: with the title bar hidden, AppKit puts the red/yellow/green buttons about 14 pt
//! below the window's top edge, but the toolbar's buttons are centred lower. This moves the
//! window buttons to the toolbar's centre line.

use objc2_app_kit::{NSApplication, NSWindowButton, NSWindowStyleMask};
use objc2_foundation::MainThreadMarker;

/// Self-test: the vertical centre of the close button, measured down from the top of the window.
pub fn close_button_center_from_top() -> Option<f64> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let window = app
        .windows()
        .iter()
        .find(|w| w.class().name().to_string_lossy().contains("Winit"))?;
    let content = window.contentView()?;
    let button = window.standardWindowButton(NSWindowButton::CloseButton)?;
    let r = button.convertRect_toView(button.bounds(), None);
    Some(content.bounds().size.height - (r.origin.y + r.size.height / 2.0))
}

/// Centres the window buttons on a line `toolbar_height / 2` below the top of the IDE window.
/// Cheap, and AppKit moves the buttons back on resize and full screen, so call it every frame.
/// Returns how far (points) the buttons were off before this call; 0 when already in place.
pub fn align_window_buttons(toolbar_height: f64) -> f64 {
    let Some(mtm) = MainThreadMarker::new() else {
        return 0.0;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app
        .windows()
        .iter()
        .find(|w| w.class().name().to_string_lossy().contains("Winit"))
    else {
        return 0.0;
    };
    // In full screen the buttons live in the title bar that slides in when the pointer touches
    // the top edge; AppKit lays that out itself, and moving the buttons every frame pushes them
    // out of it, so they flash and vanish. Leave them alone there.
    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return 0.0;
    }
    let Some(content) = window.contentView() else {
        return 0.0;
    };
    // Window coordinates have their origin at the bottom left.
    let target = content.bounds().size.height - toolbar_height / 2.0;
    let mut moved = 0.0_f64;
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        let Some(button) = window.standardWindowButton(kind) else {
            continue;
        };
        let in_window = button.convertRect_toView(button.bounds(), None);
        let delta = target - (in_window.origin.y + in_window.size.height / 2.0);
        if delta.abs() > 0.5 {
            // The button's superview is the (unflipped) title bar container.
            let mut origin = button.frame().origin;
            origin.y += delta;
            button.setFrameOrigin(origin);
            moved = moved.max(delta.abs());
        }
    }
    moved
}
