//! The message window for tools Pascoda cannot find (see `pascoda_fpc::vendor`): what is missing, what
//! stops working without it, and an "Install in Terminal" button that types the install command at
//! the prompt for the user to approve with Enter.

use crate::{App, bottom::BottomTab, style};
use eframe::egui;
use pascoda_fpc::vendor::{self, Part};

impl App {
    /// A feature needed one of `relevant`. When some of those are missing the window opens, and this
    /// returns true. Nothing happens when they are all there.
    pub(crate) fn offer_vendor_install(&mut self, relevant: &[Part]) -> bool {
        let missing = vendor::missing(&self.root);
        let wanted: Vec<Part> = relevant.iter().copied().filter(|p| missing.contains(p)).collect();
        if wanted.is_empty() {
            return false;
        }
        self.vendor_dialog = Some(wanted);
        true
    }

    /// A build failed: when it says a tool is missing (Free Pascal, the Lazarus or FPC sources), offer
    /// to install it.
    pub(crate) fn offer_vendor_for_build(&mut self, output: &str, diagnostics: &[pascoda_fpc::Diagnostic]) {
        let mut relevant = Vec::new();
        if output.contains(pascoda_fpc::NOT_FOUND_HINT) {
            relevant.push(Part::Fpc);
        }
        if output.contains("vendor/lazarus not found")
            || diagnostics.iter().any(|d| d.message.starts_with("Can't find unit"))
        {
            relevant.extend([Part::Lazarus, Part::FpcSources]);
        }
        if !relevant.is_empty() {
            self.offer_vendor_install(&relevant);
        }
    }

    /// The command the window's button types: it installs `parts` under the user's `vendor` folder.
    pub(crate) fn vendor_install_command(&self, parts: &[Part]) -> Option<String> {
        let root = vendor::user_root()?;
        // The language server is built against Lazarus; use the copy that is already there.
        let lazarus = pascoda_fpc::lcl::search_dirs(&self.root)
            .into_iter()
            .map(|d| d.join("vendor/lazarus"))
            .find(|d| d.join("lcl/interfaces/cocoa").is_dir());
        Some(vendor::install_command(parts, &root, lazarus.as_deref()))
    }

    /// Types `command` at the Terminal's prompt (no Enter) and shows the Terminal.
    pub(crate) fn type_in_terminal(&mut self, ctx: &egui::Context, command: String) {
        self.bottom.pending_input = Some(command);
        self.bottom.choose(BottomTab::Terminal, ctx.input(|i| i.time));
    }

    /// Draws the window while it is open.
    pub(crate) fn show_vendor_dialog(&mut self, ctx: &egui::Context) {
        let Some(parts) = self.vendor_dialog.clone() else {
            return;
        };
        let command = self.vendor_install_command(&parts);
        let pal = style::Palette::of(ctx);
        let (mut install, mut close) = (false, false);
        style::dialog(ctx, "vendor-dialog", None, 460.0, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Some tools are missing").strong().size(14.0));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("This copy of Pascoda cannot find:")
                    .size(13.0)
                    .color(pal.text_secondary),
            );
            for p in &parts {
                ui.label(egui::RichText::new(format!("\u{2022} {}: {}", p.name(), p.used_for())).size(13.0));
            }
            ui.add_space(6.0);
            if let Some(c) = &command {
                ui.label(
                    egui::RichText::new(
                        "Install in Terminal types this command at the prompt; press Enter there to run it, then restart Pascoda.",
                    )
                    .size(12.5)
                    .color(pal.text_secondary),
                );
                ui.add_space(4.0);
                ui.add(egui::Label::new(egui::RichText::new(c).monospace().size(11.5)).selectable(true).wrap());
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(command.is_some(), egui::Button::new("Install in Terminal")).clicked() {
                        install = true;
                    }
                });
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if install && let Some(c) = command {
            self.vendor_dialog = None;
            self.type_in_terminal(ctx, c);
        } else if close {
            self.vendor_dialog = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_button_types_the_command_without_running_it() {
        let mut app = App::bare(None);
        let ctx = egui::Context::default();
        app.type_in_terminal(&ctx, "brew install fpc".into());
        assert_eq!(app.bottom.pending_input.as_deref(), Some("brew install fpc"));
        assert!(!app.bottom.pending_input.as_ref().unwrap().contains('\n'), "Enter is the user's");
        assert_eq!(app.bottom.tab, BottomTab::Terminal);
    }

    #[test]
    fn a_build_that_names_a_missing_tool_opens_the_window_for_what_is_missing() {
        let mut app = App::bare(None);
        // Nothing is asked about tools that are not named.
        app.offer_vendor_for_build("1 error", &[]);
        assert!(app.vendor_dialog.is_none());
        // fpc missing: the hint text is what the build prints.
        let had_fpc = pascoda_fpc::locate_fpc().is_some();
        app.offer_vendor_for_build(pascoda_fpc::NOT_FOUND_HINT, &[]);
        assert_eq!(app.vendor_dialog.is_some(), !had_fpc, "only when fpc really cannot be found");
    }
}
