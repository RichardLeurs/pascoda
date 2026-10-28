//! A macOS-flavoured look for the IDE shell: system fonts, system-like colours, 13pt text.

use crate::style::Palette;
use eframe::egui::{
    self, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Theme,
    Visuals,
};
use std::sync::Arc;

const SF_PRO: &str = "/System/Library/Fonts/SFNS.ttf";
const SF_MONO: &str = "/System/Library/Fonts/SFNSMono.ttf";
const MENLO: &str = "/System/Library/Fonts/Menlo.ttc";

/// Which colour scheme the IDE uses: follow macOS, or force one. Remembered between runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Match System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        }
    }

    fn file() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(std::path::Path::new(&home).join("Library/Application Support/Pascoda/appearance"))
    }

    pub fn load() -> Self {
        let text = Self::file().and_then(|f| std::fs::read_to_string(f).ok());
        Self::ALL
            .into_iter()
            .find(|a| text.as_deref().map(str::trim) == Some(a.label()))
            .unwrap_or_default()
    }

    fn save(self) {
        if let Some(f) = Self::file() {
            let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
            let _ = std::fs::write(f, self.label());
        }
    }

    /// Switches the window (and with it the native AppKit controls) to this scheme.
    pub fn set(self, ctx: &egui::Context) {
        ctx.set_theme(self.preference());
        self.save();
    }

    /// Applies the remembered choice at startup without rewriting it.
    pub fn restore(ctx: &egui::Context) -> Self {
        let a = Self::load();
        ctx.set_theme(a.preference());
        a
    }
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.all_styles_mut(|style| {
        use TextStyle::*;
        style.text_styles = [
            (Small, FontId::proportional(11.0)),
            (Body, FontId::proportional(13.0)),
            (Button, FontId::proportional(13.0)),
            (Heading, FontId::proportional(17.0)),
            (Monospace, FontId::monospace(12.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 3.0);
        style.spacing.interact_size.y = 22.0;
    });
}

/// Uses San Francisco and SF Mono when the system has them; egui's bundled fonts remain
/// the fallback, so a missing file only costs looks.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, path: &str, family: FontFamily| {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.to_string());
            true
        } else {
            false
        }
    };
    add("SF Pro", SF_PRO, FontFamily::Proportional);
    if !add("SF Mono", SF_MONO, FontFamily::Monospace) {
        add("Menlo", MENLO, FontFamily::Monospace);
    }
    ctx.set_fonts(fonts);
}

fn visuals(dark: bool) -> Visuals {
    let pal = if dark { Palette::DARK } else { Palette::LIGHT };
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = pal.editor;
    v.window_fill = pal.chrome;
    v.extreme_bg_color = pal.editor;
    v.faint_bg_color = pal.sidebar;
    v.window_stroke = Stroke::new(1.0, pal.separator);
    v.hyperlink_color = pal.accent;
    v.selection.bg_fill = pal.accent.gamma_multiply(if dark { 0.45 } else { 0.28 });
    v.selection.stroke = Stroke::new(1.0, pal.accent);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);

    // Quiet by default, like Xcode: no borders on labels and flat buttons that light up on hover.
    v.widgets.noninteractive.bg_fill = pal.chrome;
    v.widgets.noninteractive.weak_bg_fill = pal.chrome;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.separator);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);
    for (w, fill, stroke) in [
        (
            &mut v.widgets.inactive,
            pal.chrome,
            Stroke::new(1.0, pal.separator),
        ),
        (
            &mut v.widgets.hovered,
            pal.row_hover,
            Stroke::new(1.0, pal.separator),
        ),
        (
            &mut v.widgets.active,
            pal.row_selected,
            Stroke::new(1.0, pal.separator),
        ),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = stroke;
        w.corner_radius = CornerRadius::same(6);
        w.fg_stroke = Stroke::new(1.0, pal.text);
    }
    v.widgets.open.corner_radius = CornerRadius::same(6);
    v.widgets.open.weak_bg_fill = pal.row_selected;
    v.text_cursor.stroke = Stroke::new(2.0, pal.accent);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_appearance_selects_its_theme() {
        let mut harness = egui_kittest::Harness::new_ui(|_| {});
        let ctx = harness.ctx.clone();
        apply(&ctx);
        for (a, theme) in [
            (Appearance::Dark, Theme::Dark),
            (Appearance::Light, Theme::Light),
        ] {
            ctx.set_theme(a.preference());
            harness.run();
            assert_eq!(ctx.theme(), theme);
        }
        let labels: Vec<_> = Appearance::ALL.iter().map(|a| a.label()).collect();
        assert_eq!(labels, ["Match System", "Light", "Dark"]);
    }
}
