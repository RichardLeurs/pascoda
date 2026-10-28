//! The "Export Application" dialog: name, bundle identifier, version and icon, remembered per
//! project in `pascoda.toml`.

use eframe::egui;
use pascoda_fpc::lcl::BundleInfo;
use pascoda_project::ExportSettings;
use std::path::{Path, PathBuf};

pub struct ExportDialog {
    pub settings: ExportSettings,
    /// A GUI program becomes an app bundle; a console program is only a renamed executable.
    pub gui: bool,
    pub error: Option<String>,
}

pub enum Answer {
    Cancel,
    /// Valid settings: ask for a folder and export.
    Export(String, BundleInfo),
}

/// The icon path as the compiler side wants it: relative paths are from the project folder.
pub fn resolve_icon(root: &Path, icon: &str) -> Option<PathBuf> {
    let icon = icon.trim();
    (!icon.is_empty()).then(|| {
        let p = Path::new(icon);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    })
}

impl ExportDialog {
    pub fn new(mut settings: ExportSettings, program_name: &str, gui: bool) -> Self {
        if settings.app_name.trim().is_empty() {
            settings.app_name = program_name.to_string();
        }
        ExportDialog {
            settings,
            gui,
            error: None,
        }
    }

    /// What Export will create, with the file names as they will really be (the name as typed,
    /// made safe for a file name).
    pub fn summary(&self) -> String {
        let name = pascoda_fpc::export::clean_name(&self.settings.app_name);
        if self.gui {
            format!(
                "Creates {name}.app and {name}.dmg in the folder you choose. The identifier is what macOS knows the app by; use your own reverse-domain name."
            )
        } else {
            format!(
                "Creates the executable {name} and {name}.dmg holding it, in the folder you choose."
            )
        }
    }

    /// The checked result of the form, or the sentence to show.
    pub fn validate(&self, root: &Path) -> Result<(String, BundleInfo), String> {
        let name = self.settings.app_name.trim();
        if name.is_empty() {
            return Err("The app needs a name.".into());
        }
        let info = BundleInfo {
            bundle_id: self.settings.bundle_id.trim().to_string(),
            version: self.settings.version.trim().to_string(),
            icon: resolve_icon(root, &self.settings.icon),
        };
        if self.gui {
            info.check()?;
        }
        Ok((name.to_string(), info))
    }

    pub fn show(&mut self, ctx: &egui::Context, root: &Path) -> Option<Answer> {
        let mut answer = None;
        crate::style::dialog(
            ctx,
            "export-dialog",
            Some("Export Application"),
            380.0,
            |ui| {
                let gui = self.gui;
                egui::Grid::new("export-fields")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        let l = ui.label("App name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings.app_name)
                                .desired_width(240.0),
                        )
                        .labelled_by(l.id);
                        ui.end_row();
                        if gui {
                            let l = ui.label("Bundle identifier");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.settings.bundle_id)
                                    .hint_text("com.example.myapp")
                                    .desired_width(240.0),
                            )
                            .labelled_by(l.id);
                            ui.end_row();
                            let l = ui.label("Version");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.settings.version)
                                    .hint_text("1.0")
                                    .desired_width(240.0),
                            )
                            .labelled_by(l.id);
                            ui.end_row();
                            let l = ui.label("Icon");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.settings.icon)
                                        .hint_text(".png or .icns")
                                        .desired_width(160.0),
                                )
                                .labelled_by(l.id);
                                if ui.button("Choose...").clicked()
                                    && let Some(f) = rfd::FileDialog::new()
                                        .add_filter("Icon", &["png", "icns"])
                                        .set_directory(root)
                                        .pick_file()
                                {
                                    self.settings.icon = f.to_string_lossy().into_owned();
                                }
                            });
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                ui.label(egui::RichText::new(self.summary()).weak());
                if let Some(e) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        answer = Some(Answer::Cancel);
                    }
                    if ui.button("Export...").clicked() {
                        match self.validate(root) {
                            Ok((name, info)) => {
                                self.error = None;
                                answer = Some(Answer::Export(name, info));
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            },
        );
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(Answer::Cancel);
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(gui: bool) -> ExportDialog {
        ExportDialog::new(ExportSettings::default(), "notes", gui)
    }

    #[test]
    fn the_name_defaults_to_the_program_and_empty_fields_are_fine() {
        let d = dialog(true);
        assert_eq!(d.settings.app_name, "notes");
        let (name, info) = d.validate(Path::new("/p")).unwrap();
        assert_eq!(name, "notes");
        assert_eq!(info, BundleInfo::default());
    }

    #[test]
    fn the_summary_names_the_real_files() {
        let mut d = dialog(true);
        assert!(
            d.summary().contains("notes.app and notes.dmg"),
            "{}",
            d.summary()
        );
        d.settings.app_name = "My App/1".into();
        assert!(
            d.summary().contains("My App_1.app and My App_1.dmg"),
            "{}",
            d.summary()
        );
        let c = dialog(false);
        assert!(
            c.summary().contains("executable notes and notes.dmg"),
            "{}",
            c.summary()
        );
        assert!(!d.summary().contains('<'));
    }

    #[test]
    fn bad_fields_are_explained_and_relative_icons_follow_the_project() {
        let mut d = dialog(true);
        d.settings.bundle_id = "nodots".into();
        assert!(
            d.validate(Path::new("/p"))
                .unwrap_err()
                .contains("bundle identifier")
        );
        d.settings.bundle_id = "com.example.n".into();
        d.settings.icon = "art/i.png".into();
        assert!(
            d.validate(Path::new("/p")).unwrap_err().contains("icon"),
            "missing file"
        );
        assert_eq!(
            resolve_icon(Path::new("/p"), "art/i.png"),
            Some("/p/art/i.png".into())
        );
        assert_eq!(
            resolve_icon(Path::new("/p"), "/x/i.icns"),
            Some("/x/i.icns".into())
        );
        assert_eq!(resolve_icon(Path::new("/p"), "  "), None);
        d.settings.app_name = "  ".into();
        assert!(d.validate(Path::new("/p")).unwrap_err().contains("name"));
        // a console program ignores the bundle fields
        let mut c = dialog(false);
        c.settings.bundle_id = "nodots".into();
        assert!(c.validate(Path::new("/p")).is_ok());
    }
}
