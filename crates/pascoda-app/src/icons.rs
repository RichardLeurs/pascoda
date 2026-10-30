//! Component icons from the Lazarus IDE (`images/components`, the 48 px versions), shown in the
//! palette and the tray. The images are part of Lazarus and under its licence; see
//! `assets/COPYING.modifiedLGPL.txt` and `assets/README.md`.

use eframe::egui;

const ICONS: &[(&str, &[u8])] = &[
    ("tbutton", include_bytes!("../assets/components/tbutton.png")),
    ("tdbgrid", include_bytes!("../assets/components/tdbgrid.png")),
    ("tdbnavigator", include_bytes!("../assets/components/tdbnavigator.png")),
    ("tdbedit", include_bytes!("../assets/components/tdbedit.png")),
    ("tdbtext", include_bytes!("../assets/components/tdbtext.png")),
    ("tdbmemo", include_bytes!("../assets/components/tdbmemo.png")),
    ("tdbcheckbox", include_bytes!("../assets/components/tdbcheckbox.png")),
    ("tdbcombobox", include_bytes!("../assets/components/tdbcombobox.png")),
    ("tdblistbox", include_bytes!("../assets/components/tdblistbox.png")),
    ("tdblookupcombobox", include_bytes!("../assets/components/tdblookupcombobox.png")),
    ("tdblookuplistbox", include_bytes!("../assets/components/tdblookuplistbox.png")),
    ("tdbradiogroup", include_bytes!("../assets/components/tdbradiogroup.png")),
    ("tdbimage", include_bytes!("../assets/components/tdbimage.png")),
    ("tlabel", include_bytes!("../assets/components/tlabel.png")),
    ("tedit", include_bytes!("../assets/components/tedit.png")),
    ("tmemo", include_bytes!("../assets/components/tmemo.png")),
    (
        "tcheckbox",
        include_bytes!("../assets/components/tcheckbox.png"),
    ),
    (
        "tradiobutton",
        include_bytes!("../assets/components/tradiobutton.png"),
    ),
    (
        "tcombobox",
        include_bytes!("../assets/components/tcombobox.png"),
    ),
    (
        "tlistbox",
        include_bytes!("../assets/components/tlistbox.png"),
    ),
    (
        "ttogglebox",
        include_bytes!("../assets/components/ttogglebox.png"),
    ),
    (
        "tscrollbar",
        include_bytes!("../assets/components/tscrollbar.png"),
    ),
    (
        "tstatictext",
        include_bytes!("../assets/components/tstatictext.png"),
    ),
    (
        "tgroupbox",
        include_bytes!("../assets/components/tgroupbox.png"),
    ),
    ("tpanel", include_bytes!("../assets/components/tpanel.png")),
    (
        "tbitbtn",
        include_bytes!("../assets/components/tbitbtn.png"),
    ),
    (
        "tspeedbutton",
        include_bytes!("../assets/components/tspeedbutton.png"),
    ),
    (
        "tlabelededit",
        include_bytes!("../assets/components/tlabelededit.png"),
    ),
    (
        "tmaskedit",
        include_bytes!("../assets/components/tmaskedit.png"),
    ),
    (
        "tspinedit",
        include_bytes!("../assets/components/tspinedit.png"),
    ),
    (
        "tradiogroup",
        include_bytes!("../assets/components/tradiogroup.png"),
    ),
    (
        "tcheckgroup",
        include_bytes!("../assets/components/tcheckgroup.png"),
    ),
    (
        "tcolorbox",
        include_bytes!("../assets/components/tcolorbox.png"),
    ),
    ("timage", include_bytes!("../assets/components/timage.png")),
    ("tshape", include_bytes!("../assets/components/tshape.png")),
    ("tbevel", include_bytes!("../assets/components/tbevel.png")),
    (
        "tsplitter",
        include_bytes!("../assets/components/tsplitter.png"),
    ),
    (
        "tprogressbar",
        include_bytes!("../assets/components/tprogressbar.png"),
    ),
    (
        "ttrackbar",
        include_bytes!("../assets/components/ttrackbar.png"),
    ),
    (
        "tupdown",
        include_bytes!("../assets/components/tupdown.png"),
    ),
    (
        "tpagecontrol",
        include_bytes!("../assets/components/tpagecontrol.png"),
    ),
    (
        "tlistview",
        include_bytes!("../assets/components/tlistview.png"),
    ),
    (
        "ttreeview",
        include_bytes!("../assets/components/ttreeview.png"),
    ),
    (
        "ttoolbar",
        include_bytes!("../assets/components/ttoolbar.png"),
    ),
    (
        "tstatusbar",
        include_bytes!("../assets/components/tstatusbar.png"),
    ),
    ("ttimer", include_bytes!("../assets/components/ttimer.png")),
    (
        "topendialog",
        include_bytes!("../assets/components/topendialog.png"),
    ),
    (
        "tsavedialog",
        include_bytes!("../assets/components/tsavedialog.png"),
    ),
    (
        "tselectdirectorydialog",
        include_bytes!("../assets/components/tselectdirectorydialog.png"),
    ),
    (
        "tcolordialog",
        include_bytes!("../assets/components/tcolordialog.png"),
    ),
    (
        "tfontdialog",
        include_bytes!("../assets/components/tfontdialog.png"),
    ),
    (
        "tactionlist",
        include_bytes!("../assets/components/tactionlist.png"),
    ),
    (
        "timagelist",
        include_bytes!("../assets/components/timagelist.png"),
    ),
    (
        "tmainmenu",
        include_bytes!("../assets/components/tmainmenu.png"),
    ),
    (
        "tpopupmenu",
        include_bytes!("../assets/components/tpopupmenu.png"),
    ),
];
const DEFAULT: &[u8] = include_bytes!("../assets/components/default.png");

/// Icon files of imported classes (`class`, file), set from the packages (see `designer::imports`).
static ICON_FILES: std::sync::RwLock<Vec<(String, std::path::PathBuf)>> = std::sync::RwLock::new(Vec::new());

/// Replaces the icon files of imported classes.
pub fn set_icon_files(files: Vec<(String, std::path::PathBuf)>) {
    *ICON_FILES.write().unwrap_or_else(|e| e.into_inner()) = files;
}

/// The icon bytes for a class (case-insensitive): the package's icon file when it has one, else the
/// built-in icon, else the generic component icon.
fn bytes(class: &str) -> Vec<u8> {
    let file = ICON_FILES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(class))
        .map(|(_, p)| p.clone());
    if let Some(data) = file.and_then(|p| std::fs::read(p).ok()) {
        return data;
    }
    let class = class.to_ascii_lowercase();
    ICONS
        .iter()
        .find(|(c, _)| *c == class)
        .map_or(DEFAULT, |(_, b)| b)
        .to_vec()
}

/// The icon of a component class as a texture, decoded once per class.
pub fn texture(ctx: &egui::Context, class: &str) -> egui::TextureHandle {
    let class = class.to_ascii_lowercase();
    let id = egui::Id::NULL.with(("component-icon", class.as_str()));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t;
    }
    let img = image::load_from_memory(&bytes(&class)).expect("embedded icons are valid PNGs");
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let t = ctx.load_texture(format!("icon-{class}"), color, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// Paints a class's icon filling `rect`.
pub fn paint(painter: &egui::Painter, rect: egui::Rect, class: &str) {
    let t = texture(painter.ctx(), class);
    painter.image(
        t.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_class_has_its_own_icon_that_decodes() {
        for en in crate::designer::PALETTE {
            let b = bytes(en.class);
            assert!(b != DEFAULT, "{} has no icon", en.class);
            assert!(image::load_from_memory(&b).is_ok(), "{}", en.class);
        }
        assert!(bytes("TNoSuchThing") == DEFAULT);
    }

    #[test]
    fn a_package_icon_file_replaces_the_built_in_for_its_class() {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/pascoda-vlc/tpascodavlcplayer.png");
        set_icon_files(vec![("TPascodaVlcPlayer".to_string(), file.clone())]);
        assert_eq!(bytes("tpascodavlcplayer"), std::fs::read(&file).unwrap(), "the file, case-insensitively");
        assert!(bytes("TPascodaVlcListPlayer") == DEFAULT, "no file: the generic icon");
        set_icon_files(Vec::new());
        assert!(bytes("TPascodaVlcPlayer") == DEFAULT);
    }
}
