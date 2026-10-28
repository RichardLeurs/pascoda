//! The visual form designer: the state and the editing operations (no drawing; see `ui`).

pub mod geometry;
pub mod item_editor;
pub mod menu;
pub mod native;
pub mod ui;
pub mod viewport;

use geometry::Bounds;
use pascoda_forms::sync::{self, SyncError};
use pascoda_forms::{Component, EditError, FormDoc, PropValue};
use std::collections::HashMap;
use std::sync::Arc;
use std::path::{Path, PathBuf};

/// One class on the palette.
pub struct Entry {
    pub class: &'static str,
    /// The property that holds its visible text; empty when it has none.
    pub text: &'static str,
    /// The LCL unit that declares the class, which the form's unit must `use`.
    pub unit: &'static str,
    pub page: &'static str,
    /// A non-visual component (timer, dialog, menu): it has no bounds, only a design position
    /// (`Left`/`Top`), and is shown in the component tray instead of on the form.
    pub nonvisual: bool,
}

const fn e(
    page: &'static str,
    class: &'static str,
    text: &'static str,
    unit: &'static str,
) -> Entry {
    Entry {
        class,
        text,
        unit,
        page,
        nonvisual: false,
    }
}

const fn nv(page: &'static str, class: &'static str, unit: &'static str) -> Entry {
    Entry {
        class,
        text: "",
        unit,
        page,
        nonvisual: true,
    }
}

/// Classes the palette offers, in the order of Lazarus's component palette pages.
pub const PALETTE: &[Entry] = &[
    e("Standard", "TButton", "Caption", "StdCtrls"),
    e("Standard", "TLabel", "Caption", "StdCtrls"),
    e("Standard", "TEdit", "Text", "StdCtrls"),
    e("Standard", "TMemo", "", "StdCtrls"),
    e("Standard", "TCheckBox", "Caption", "StdCtrls"),
    e("Standard", "TRadioButton", "Caption", "StdCtrls"),
    e("Standard", "TComboBox", "Text", "StdCtrls"),
    e("Standard", "TListBox", "", "StdCtrls"),
    e("Standard", "TToggleBox", "Caption", "StdCtrls"),
    e("Standard", "TScrollBar", "", "StdCtrls"),
    e("Standard", "TStaticText", "Caption", "StdCtrls"),
    e("Standard", "TGroupBox", "Caption", "StdCtrls"),
    e("Standard", "TPanel", "Caption", "ExtCtrls"),
    e("Additional", "TBitBtn", "Caption", "Buttons"),
    e("Additional", "TSpeedButton", "Caption", "Buttons"),
    e("Additional", "TLabeledEdit", "Text", "ExtCtrls"),
    e("Additional", "TMaskEdit", "Text", "MaskEdit"),
    e("Additional", "TSpinEdit", "", "Spin"),
    e("Additional", "TRadioGroup", "Caption", "ExtCtrls"),
    e("Additional", "TCheckGroup", "Caption", "ExtCtrls"),
    e("Additional", "TColorBox", "", "ColorBox"),
    e("Additional", "TImage", "", "ExtCtrls"),
    e("Additional", "TShape", "", "ExtCtrls"),
    e("Additional", "TBevel", "", "ExtCtrls"),
    e("Additional", "TSplitter", "", "ExtCtrls"),
    e("Common Controls", "TProgressBar", "", "ComCtrls"),
    e("Common Controls", "TTrackBar", "", "ComCtrls"),
    e("Common Controls", "TUpDown", "", "ComCtrls"),
    e("Common Controls", "TPageControl", "", "ComCtrls"),
    e("Common Controls", "TListView", "", "ComCtrls"),
    e("Common Controls", "TTreeView", "", "ComCtrls"),
    e("Common Controls", "TToolBar", "", "ComCtrls"),
    e("Common Controls", "TStatusBar", "", "ComCtrls"),
    nv("System", "TTimer", "ExtCtrls"),
    nv("System", "TOpenDialog", "Dialogs"),
    nv("System", "TSaveDialog", "Dialogs"),
    nv("System", "TSelectDirectoryDialog", "Dialogs"),
    nv("System", "TColorDialog", "Dialogs"),
    nv("System", "TFontDialog", "Dialogs"),
    nv("System", "TActionList", "ActnList"),
    nv("System", "TImageList", "Controls"),
    nv("System", "TMainMenu", "Menus"),
    nv("System", "TPopupMenu", "Menus"),
];

/// Components without a place on the form: the palette's non-visual classes and the items
/// that live inside them (`TMenuItem` in a menu, `TAction` in an action list).
pub fn is_nonvisual(class: &str) -> bool {
    PALETTE
        .iter()
        .find(|en| en.class.eq_ignore_ascii_case(class))
        .map_or_else(
            || {
                ["TMenuItem", "TAction"]
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(class))
            },
            |en| en.nonvisual,
        )
}

/// Where the tray's slots are in the file: Lazarus keeps a non-visual component's icon
/// position on the form as `Left`/`Top`; we store the tray order as a row of such positions.
const TRAY_LEFT: i64 = 16;
const TRAY_TOP: i64 = 16;
const TRAY_STEP: i64 = 40;

/// Project classes (custom components) and the unit each is declared in.
pub type ClassUnits = HashMap<&'static str, &'static str>;

/// Class and unit names found in the project live as long as the program, so they are interned
/// once: the palette and the designer hold them as `&'static str`, like the built-in classes.
fn intern(name: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static POOL: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    let mut pool = POOL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(&known) = pool.get(name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// The project's custom components: classes declared in the project's units, except forms and
/// frames (their base class is a form) and built-in classes. Units are read from the folder and
/// its subfolders, as the file tree shows them.
pub fn custom_classes(root: &Path) -> ClassUnits {
    fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if depth < 6
                    && !crate::files::HIDDEN_DIRS.contains(&name.as_str())
                    && !name.ends_with(".app")
                {
                    collect(&path, depth + 1, out);
                }
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("pas" | "pp")) {
                out.push(path);
            }
        }
    }
    const FORM_BASES: &[&str] = &["tform", "tframe", "tcustomform", "tcustomframe", "tdatamodule"];
    let mut files = Vec::new();
    collect(root, 0, &mut files);
    let mut out = ClassUnits::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(unit) = file.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        for (class, base) in pascoda_forms::sync::declared_classes(&text) {
            let is_form = FORM_BASES.contains(&base.to_ascii_lowercase().as_str());
            if is_form || is_builtin(&class) {
                continue;
            }
            out.insert(intern(&class), intern(unit));
        }
    }
    out
}

/// Whether the class is one of the palette's built-in classes.
fn is_builtin(class: &str) -> bool {
    PALETTE.iter().any(|en| en.class.eq_ignore_ascii_case(class))
}

/// The custom classes a form uses, found in its component tree.
fn custom_used(form: &Component, custom: &ClassUnits) -> Vec<&'static str> {
    fn walk(c: &Component, custom: &ClassUnits, out: &mut Vec<&'static str>) {
        for child in &c.children {
            if let Some((&class, _)) = custom
                .iter()
                .find(|(class, _)| class.eq_ignore_ascii_case(&child.class))
                && !out.contains(&class)
            {
                out.push(class);
            }
            walk(child, custom, out);
        }
    }
    let mut out = Vec::new();
    walk(form, custom, &mut out);
    out
}

/// The units a form needs `uses`d for the classes it contains (Controls/Forms are always there):
/// the palette's units, and the unit of each custom class. The form's own unit is never listed.
pub fn units_for(form: &Component, own_unit: &str, custom: &ClassUnits) -> Vec<&'static str> {
    fn walk(c: &Component, out: &mut Vec<&'static str>) {
        for child in &c.children {
            if let Some(en) = PALETTE
                .iter()
                .find(|en| en.class.eq_ignore_ascii_case(&child.class))
                && !out.contains(&en.unit)
            {
                out.push(en.unit);
            }
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(form, &mut out);
    for class in custom_used(form, custom) {
        let unit = custom[class];
        if !unit.eq_ignore_ascii_case(own_unit) && !out.contains(&unit) {
            out.push(unit);
        }
    }
    out
}

/// Containers whose children are positioned relative to them.
pub fn is_container(class: &str) -> bool {
    ["tpanel", "tgroupbox", "tradiogroup", "tcheckgroup"]
        .contains(&class.to_ascii_lowercase().as_str())
}

fn text_prop(class: &str) -> &'static str {
    PALETTE
        .iter()
        .find(|en| en.class.eq_ignore_ascii_case(class))
        .map_or("Caption", |en| en.text)
}

/// Whether the class can take the keyboard focus, and so has a `TabOrder`: LCL's windowed
/// controls do, graphic controls (labels, speed buttons, images, shapes, bevels) do not.
pub fn takes_focus(class: &str) -> bool {
    !is_nonvisual(class)
        && !["tlabel", "tspeedbutton", "timage", "tshape", "tbevel"]
            .contains(&class.to_ascii_lowercase().as_str())
}

/// The tab sequence for controls with these `TabOrder` values (in file order): by value, those
/// without one last, ties in file order (as LCL does when it reads them). Indexes into `orders`.
pub fn tab_sequence(orders: &[Option<i64>]) -> Vec<usize> {
    let mut seq: Vec<usize> = (0..orders.len()).collect();
    seq.sort_by_key(|&i| (orders[i].unwrap_or(i64::MAX), i));
    seq
}

/// `seq` with the item at `at` moved one place up (`-1`) or down (`1`); unchanged at the ends.
pub fn moved_in_sequence<T: Clone>(seq: &[T], at: usize, by: isize) -> Vec<T> {
    let mut out = seq.to_vec();
    let to = at as isize + by;
    if at < out.len() && to >= 0 && (to as usize) < out.len() {
        out.swap(at, to as usize);
    }
    out
}

/// Sets a property of a component that is not in a document yet.
fn put_prop(c: &mut Component, name: &str, value: PropValue) {
    match c
        .props
        .iter_mut()
        .find(|p| p.name.eq_ignore_ascii_case(name))
    {
        Some(p) => p.value = value,
        None => c.props.push(pascoda_forms::Property {
            name: name.into(),
            value,
            span: 0..0,
            value_span: 0..0,
        }),
    }
}

/// The names of `c` and everything inside it.
fn names_in(c: &Component, out: &mut Vec<String>) {
    if !c.name.is_empty() {
        out.push(c.name.clone());
    }
    for ch in &c.children {
        names_in(ch, out);
    }
}

/// Every handler name some event in the form is bound to.
fn bound_handlers(form: &Component) -> Vec<String> {
    let mut out: Vec<String> = form
        .props
        .iter()
        .filter(|p| p.name.starts_with("On"))
        .filter_map(|p| p.value.as_ident().map(str::to_string))
        .collect();
    for c in &form.children {
        out.extend(bound_handlers(c));
    }
    out
}

/// The event a double-click on this class creates a handler for.
pub fn default_event(class: &str) -> &'static str {
    match class.to_ascii_lowercase().as_str() {
        "tedit" | "tmemo" | "tcombobox" | "tlabelededit" | "tmaskedit" | "tspinedit" => "OnChange",
        "tform" => "OnCreate",
        "ttimer" => "OnTimer",
        "tactionlist" | "taction" => "OnExecute",
        "topendialog"
        | "tsavedialog"
        | "tselectdirectorydialog"
        | "tcolordialog"
        | "tfontdialog" => "OnClose",
        "timagelist" | "tmainmenu" => "OnChange",
        "tpopupmenu" => "OnPopup",
        _ => "OnClick",
    }
}

pub struct Designer {
    pub lfm_path: PathBuf,
    pub pas_path: PathBuf,
    /// The project's custom components, set by the app whenever the project changes.
    pub custom_units: Arc<ClassUnits>,
    pub doc: FormDoc,
    saved: String,
    undo: Vec<String>,
    redo: Vec<String>,
    /// Path of the primary selected component (the one the inspector shows, and the reference
    /// for align commands); `[]` is the form itself. Change it with [`Designer::select`].
    pub selected: Vec<usize>,
    /// The other selected components, in the order they were added. Never holds the primary.
    also: Vec<Vec<usize>>,
    /// Inside [`Designer::batch`]: checkpoints are taken once, by the batch.
    batching: bool,
    /// Field names the unit was last brought in step with, to find deleted components.
    synced_fields: Vec<String>,
    /// Why the unit could not be updated, shown to the user.
    pub sync_error: Option<String>,
    /// The property last edited, so typing in the inspector is one undo step, not one per key.
    last_prop: Option<(Vec<usize>, String)>,
    /// `(caption, AutoSize)` per component name when last sized, so only components whose
    /// text or setting changed get resized; untouched ones keep the size stored in the file.
    autosize_seen: HashMap<String, (String, bool)>,
}

#[derive(Debug)]
pub enum DesignerError {
    Io(std::io::Error),
    Parse(pascoda_forms::ParseError),
    Edit(EditError),
}

impl std::fmt::Display for DesignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DesignerError::Io(e) => write!(f, "{e}"),
            DesignerError::Parse(e) => write!(f, "form file: {e}"),
            DesignerError::Edit(e) => write!(f, "{e}"),
        }
    }
}

impl From<EditError> for DesignerError {
    fn from(e: EditError) -> Self {
        DesignerError::Edit(e)
    }
}

/// Whether `path` is a Pascal unit: `.pas`, or `.pp`, which Free Pascal and Lazarus use too.
pub fn is_unit_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pas") || e.eq_ignore_ascii_case("pp"))
}

/// Whether `path` is a form file the designer can read: a `.lfm`, or a `.dfm` in text form.
/// Binary Delphi forms are not supported.
pub fn is_form_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    if ext.eq_ignore_ascii_case("lfm") {
        true
    } else if ext.eq_ignore_ascii_case("dfm") {
        !is_binary_form(path)
    } else {
        false
    }
}

/// Whether the file at `path` starts like a binary Delphi form (reads its first bytes only).
fn is_binary_form(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    pascoda_forms::lfm::is_binary(&head[..read])
}

/// The unit (`.pas` or `.pp`) a form file (`.lfm` or `.dfm`) belongs to, if there is one.
pub fn sibling_pas(form: &Path) -> Option<PathBuf> {
    let ext = form.extension().and_then(|e| e.to_str()).unwrap_or_default();
    if !(ext.eq_ignore_ascii_case("lfm") || ext.eq_ignore_ascii_case("dfm")) || !is_form_file(form) {
        return None;
    }
    ["pas", "pp"]
        .iter()
        .map(|ext| form.with_extension(ext))
        .find(|p| p.is_file())
}

/// For either file of a unit/form pair, the unit.
pub fn form_unit(path: &Path) -> Option<PathBuf> {
    if sibling_lfm(path).is_some() {
        Some(path.to_path_buf())
    } else {
        sibling_pas(path)
    }
}

/// The form file next to a unit: its `.lfm`, else its text `.dfm`.
pub fn sibling_lfm(pas: &Path) -> Option<PathBuf> {
    if !is_unit_file(pas) {
        return None;
    }
    ["lfm", "dfm"]
        .iter()
        .map(|ext| pas.with_extension(ext))
        .find(|p| is_form_file(p))
}

impl Designer {
    pub fn open(pas_path: &Path) -> Result<Self, DesignerError> {
        let lfm_path = sibling_lfm(pas_path).ok_or_else(|| {
            DesignerError::Io(std::io::Error::other("no .lfm file next to this unit"))
        })?;
        let text = std::fs::read_to_string(&lfm_path).map_err(DesignerError::Io)?;
        let doc = FormDoc::parse(&text).map_err(DesignerError::Parse)?;
        let synced_fields = sync::wanted(doc.root())
            .fields
            .into_iter()
            .map(|f| f.0)
            .collect();
        let mut d = Self {
            lfm_path,
            pas_path: pas_path.to_path_buf(),
            saved: doc.source().to_string(),
            doc,
            undo: Vec::new(),
            custom_units: Arc::default(),
            redo: Vec::new(),
            selected: Vec::new(),
            also: Vec::new(),
            batching: false,
            synced_fields,
            sync_error: None,
            last_prop: None,
            autosize_seen: HashMap::new(),
        };
        d.snapshot_autosize();
        Ok(d)
    }

    pub fn dirty(&self) -> bool {
        self.doc.source() != self.saved
    }

    /// The form text is what the disk has (after the file was written by someone else and
    /// loaded with `replace_source`).
    pub fn mark_saved(&mut self) {
        self.saved = self.doc.source().to_string();
    }

    /// The form text as the disk last had it (written or loaded by us).
    pub fn saved_source(&self) -> &str {
        &self.saved
    }

    /// The file is gone from the disk: count the form as unsaved so the next save recreates it.
    pub fn mark_unsaved(&mut self) {
        self.saved.clear();
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if self.dirty() {
            std::fs::write(&self.lfm_path, self.doc.source())?;
            self.saved = self.doc.source().to_string();
        }
        Ok(())
    }

    pub fn selected_component(&self) -> Option<&Component> {
        self.doc.component(&self.selected)
    }

    /// The controls to draw, in z-order, with form-relative bounds.
    pub fn specs(&self) -> Vec<native::Spec> {
        fn walk(
            c: &Component,
            path: &mut Vec<usize>,
            form: &Component,
            out: &mut Vec<native::Spec>,
        ) {
            for (i, child) in c.children.iter().enumerate() {
                path.push(i);
                if let Some(bounds) = geometry::absolute(form, path) {
                    let caption = child
                        .prop(text_prop(&child.class))
                        .and_then(PropValue::as_str)
                        .unwrap_or_default()
                        .to_string();
                    out.push(native::Spec {
                        name: child.name.clone(),
                        class: child.class.clone(),
                        caption,
                        bounds,
                    });
                }
                walk(child, path, form, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(self.doc.root(), &mut Vec::new(), self.doc.root(), &mut out);
        out
    }

    // ---- history ----------------------------------------------------------------------

    /// Remembers the current state so the next change can be undone.
    pub fn checkpoint(&mut self) {
        if self.batching {
            return;
        }
        self.undo.push(self.doc.source().to_string());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last_prop = None;
    }

    /// Runs `f` as one undo step: edits inside it do not checkpoint on their own. With a `key`,
    /// consecutive batches with the same key coalesce (typing in a multi-selection field). A
    /// batch that changes nothing leaves no undo step.
    fn batch<R>(&mut self, key: Option<(Vec<usize>, String)>, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.batching {
            return f(self);
        }
        let coalesce = key.is_some() && self.last_prop == key;
        let before = self.undo.len();
        if !coalesce {
            self.checkpoint();
        }
        self.batching = true;
        let r = f(self);
        self.batching = false;
        let unchanged = self.undo.last().map(String::as_str) == Some(self.doc.source());
        if !coalesce && self.undo.len() > before && unchanged {
            self.undo.pop();
        } else {
            self.last_prop = key;
        }
        r
    }

    fn restore(&mut self, source: &str) {
        if let Ok(doc) = FormDoc::parse(source) {
            self.doc = doc;
            let mut kept: Vec<Vec<usize>> = self
                .selection()
                .into_iter()
                .filter(|p| self.doc.component(p).is_some())
                .collect();
            if kept.is_empty() {
                self.select(Vec::new());
            } else {
                self.selected = kept.remove(0);
                self.also = kept;
            }
        }
    }

    /// Replaces the whole form by `text` (e.g. an edit made outside the designer) as one undo
    /// step. The selection is kept where the components still exist.
    pub fn replace_source(&mut self, text: &str) -> Result<(), DesignerError> {
        FormDoc::parse(text).map_err(DesignerError::Parse)?;
        self.checkpoint();
        self.restore(text);
        self.sync_error = None;
        Ok(())
    }

    // ---- selection --------------------------------------------------------------------

    /// Every selected component, the primary first. Empty when the form itself is selected.
    pub fn selection(&self) -> Vec<Vec<usize>> {
        if self.selected.is_empty() {
            return Vec::new();
        }
        std::iter::once(self.selected.clone())
            .chain(self.also.iter().cloned())
            .collect()
    }

    pub fn is_selected(&self, path: &[usize]) -> bool {
        self.selected == path || self.also.iter().any(|p| p == path)
    }

    /// Makes `path` the only selected component (`[]`: the form).
    pub fn select(&mut self, path: Vec<usize>) {
        self.selected = path;
        self.also.clear();
    }

    /// Selects `paths`, the first one primary. Empty selects the form.
    pub fn select_paths(&mut self, mut paths: Vec<Vec<usize>>) {
        paths.retain(|p| !p.is_empty());
        paths.dedup();
        if paths.is_empty() {
            self.select(Vec::new());
        } else {
            self.selected = paths.remove(0);
            self.also = paths;
        }
    }

    /// Shift/Cmd-click: adds `path` to the selection or takes it out. The selection may span
    /// containers: moving, deleting and copying apply to all; aligning and union resizing use
    /// only the controls in the primary's container.
    pub fn toggle(&mut self, path: Vec<usize>) {
        if path.is_empty() || self.selected.is_empty() {
            self.select(path);
        } else if self.selected == path {
            self.selected = if self.also.is_empty() {
                Vec::new()
            } else {
                self.also.remove(0)
            };
        } else if let Some(i) = self.also.iter().position(|p| *p == path) {
            self.also.remove(i);
        } else {
            self.also.push(path);
        }
    }

    /// The container whose children the selection is made of (the form when nothing is).
    pub fn current_container(&self) -> Vec<usize> {
        self.selected[..self.selected.len().saturating_sub(1)].to_vec()
    }

    /// Cmd+A: every control in the current container. Non-visual components are left out, as
    /// they cannot be moved or aligned.
    pub fn select_all(&mut self) {
        let parent = self.current_container();
        let Some(c) = self.doc.component(&parent) else {
            return;
        };
        let paths = (0..c.children.len())
            .filter(|&i| !is_nonvisual(&c.children[i].class))
            .map(|i| [parent.as_slice(), &[i]].concat())
            .collect();
        self.select_paths(paths);
    }

    /// Rubber band: selects the controls in `container` that intersect `rect` (in the
    /// container's coordinates). With `add`, they join the current selection.
    pub fn select_in_rect(&mut self, container: &[usize], rect: Bounds, add: bool) {
        let Some(c) = self.doc.component(container) else {
            return;
        };
        let hits: Vec<Vec<usize>> = c
            .children
            .iter()
            .enumerate()
            .filter(|(_, ch)| !is_nonvisual(&ch.class) && geometry::bounds(ch).intersects(&rect))
            .map(|(i, _)| [container, &[i]].concat())
            .collect();
        let mut paths = if add { self.selection() } else { Vec::new() };
        for h in hits {
            if !paths.contains(&h) {
                paths.push(h);
            }
        }
        self.select_paths(paths);
    }

    /// The selected components with no selected ancestor, in document order.
    fn selection_roots(&self) -> Vec<Vec<usize>> {
        let all = self.selection();
        let mut roots: Vec<Vec<usize>> = all
            .iter()
            .filter(|p| !all.iter().any(|q| q.len() < p.len() && p.starts_with(q)))
            .cloned()
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.doc.source().to_string());
        self.restore(&prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.doc.source().to_string());
        self.restore(&next);
        true
    }

    // ---- editing ----------------------------------------------------------------------

    fn name_taken(&self, name: &str) -> bool {
        fn walk(c: &Component, name: &str) -> bool {
            c.name.eq_ignore_ascii_case(name) || c.children.iter().any(|ch| walk(ch, name))
        }
        walk(self.doc.root(), name)
    }

    /// `Button1`, `Button2`, ... the first name not used anywhere in the form.
    pub fn free_name(&self, class: &str) -> String {
        self.free_name_besides(class, &[])
    }

    /// [`Designer::free_name`], also avoiding `reserved` (names handed out but not yet added).
    fn free_name_besides(&self, class: &str, reserved: &[String]) -> String {
        let base = class
            .strip_prefix('T')
            .or(class.strip_prefix('t'))
            .unwrap_or(class);
        (1..)
            .map(|n| format!("{base}{n}"))
            .find(|n| !self.name_taken(n) && !reserved.iter().any(|r| r.eq_ignore_ascii_case(n)))
            .expect("a free name exists")
    }

    // ---- clipboard ----------------------------------------------------------------------

    /// The selected components (with everything inside them) as `.lfm` text, the way Lazarus
    /// and Delphi put components on the clipboard. `None` when only the form is selected.
    pub fn copy_selection(&self) -> Option<String> {
        let text: Vec<String> = self
            .selection_roots()
            .iter()
            .filter_map(|p| self.doc.component(p))
            .map(|c| pascoda_forms::lfm::write_component(c, "", "\n"))
            .collect();
        (!text.is_empty()).then(|| text.join("\n") + "\n")
    }

    /// Pastes components copied as `.lfm` text, one undo step, and selects them. They go into
    /// the selected container, or next to the selected control, offset by one grid step.
    /// Non-visual ones go into the form's tray. Names already used get the next free name for
    /// the class (children too), and references between the pasted components follow the new
    /// names. Event bindings are kept only when the handler is already bound in this form
    /// (as Lazarus does: the method exists); others are dropped, so no stubs appear. `TabOrder`
    /// is dropped so pasted controls come last. Text that is not components pastes nothing.
    pub fn paste(&mut self, text: &str) -> Result<Vec<Vec<usize>>, DesignerError> {
        let wrapped = format!("object Clipboard: TClipboard\n{}\nend\n", text.trim());
        let Ok(holder) = pascoda_forms::lfm::parse(&wrapped) else {
            return Ok(Vec::new());
        };
        let target = match self.selected_component() {
            Some(c) if self.also.is_empty() && is_container(&c.class) => self.selected.clone(),
            _ => self.current_container(),
        };
        // Old name -> new name for every named component pasted, in all levels.
        let mut renames: Vec<(String, String)> = Vec::new();
        fn plan(d: &Designer, c: &Component, renames: &mut Vec<(String, String)>) {
            if !c.name.is_empty() {
                let reserved: Vec<String> = renames.iter().map(|r| r.1.clone()).collect();
                let taken = d.name_taken(&c.name)
                    || reserved.iter().any(|r| r.eq_ignore_ascii_case(&c.name));
                let new = if taken {
                    d.free_name_besides(&c.class, &reserved)
                } else {
                    c.name.clone()
                };
                renames.push((c.name.clone(), new));
            }
            for ch in &c.children {
                plan(d, ch, renames);
            }
        }
        for c in &holder.children {
            plan(self, c, &mut renames);
        }
        let handlers = bound_handlers(self.doc.root());
        // Names are handed out in the same (pre-)order as `plan` made them, so two pasted
        // components with the same name still get different ones.
        fn rewrite(
            c: &mut Component,
            next: &mut std::slice::Iter<(String, String)>,
            renames: &[(String, String)],
            handlers: &[String],
        ) {
            if !c.name.is_empty()
                && let Some((_, new)) = next.next()
            {
                c.name = new.clone();
            }
            c.props.retain(|p| {
                !p.name.starts_with("On")
                    || p.value
                        .as_ident()
                        .is_some_and(|h| handlers.iter().any(|b| b.eq_ignore_ascii_case(h)))
            });
            for p in &mut c.props {
                if let PropValue::Ident(v) = &mut p.value
                    && let Some((_, new)) = renames.iter().find(|r| r.0.eq_ignore_ascii_case(v))
                {
                    *v = new.clone();
                }
            }
            for ch in &mut c.children {
                rewrite(ch, next, renames, handlers);
            }
        }
        let mut next = renames.iter();
        let tray: Vec<(i64, i64)> = self
            .tray()
            .iter()
            .filter_map(|p| self.doc.component(p))
            .map(|c| {
                let b = geometry::bounds(c);
                (b.left, b.top)
            })
            .collect();
        let mut used_slots = tray;
        let mut pasted = Vec::new();
        // Menu items and actions go into the selected menu or action list, after the selected
        // item; anywhere else they are not pasted.
        let mut item_slot: Option<(Vec<usize>, usize, &str)> =
            self.item_owner(&self.selected).and_then(|owner| {
                let class = menu::item_class(&self.doc.component(&owner)?.class);
                Some(if self.selected == owner {
                    let n = self.doc.component(&owner)?.children.len();
                    (owner, n, class)
                } else {
                    let (last, parent) = self.selected.split_last()?;
                    (parent.to_vec(), last + 1, class)
                })
            });
        self.batch(None, |d| -> Result<(), DesignerError> {
            for mut c in holder.children.clone() {
                rewrite(&mut c, &mut next, &renames, &handlers);
                if ["TMenuItem", "TAction"]
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(&c.class))
                {
                    match &mut item_slot {
                        Some((parent, at, class)) if class.eq_ignore_ascii_case(&c.class) => {
                            d.doc.insert_child(parent, *at, c)?;
                            pasted.push([parent.as_slice(), &[*at]].concat());
                            *at += 1;
                        }
                        _ => {}
                    }
                    continue;
                }
                let parent = if is_nonvisual(&c.class) {
                    let left = (0..)
                        .map(|k| TRAY_LEFT + TRAY_STEP * k)
                        .find(|l| !used_slots.contains(&(*l, TRAY_TOP)))
                        .expect("a free slot exists");
                    used_slots.push((left, TRAY_TOP));
                    put_prop(&mut c, "Left", PropValue::Int(left));
                    put_prop(&mut c, "Top", PropValue::Int(TRAY_TOP));
                    Vec::new()
                } else {
                    let b = geometry::bounds(&c);
                    put_prop(&mut c, "Left", PropValue::Int(b.left + geometry::GRID));
                    put_prop(&mut c, "Top", PropValue::Int(b.top + geometry::GRID));
                    c.props.retain(|p| !p.name.eq_ignore_ascii_case("TabOrder"));
                    target.clone()
                };
                d.doc.add_child(&parent, c)?;
                let n = d.doc.component(&parent).map_or(0, |p| p.children.len());
                pasted.push([parent.as_slice(), &[n - 1]].concat());
            }
            Ok(())
        })?;
        self.select_paths(pasted.clone());
        Ok(pasted)
    }

    /// Cmd+D: copies and pastes the selection in one go, without the clipboard.
    pub fn duplicate(&mut self) -> Result<Vec<Vec<usize>>, DesignerError> {
        let Some(text) = self.copy_selection() else {
            return Ok(Vec::new());
        };
        // Next to the originals, not into a selected container.
        self.select(self.current_container());
        self.paste(&text)
    }

    /// Adds a component of `class` into the container at `parent`, with its top-left corner at
    /// `(x, y)` relative to that container (snapped to the grid). Selects it and returns its path.
    /// A non-visual class goes onto the form at the next free tray slot instead.
    pub fn add_component(
        &mut self,
        class: &str,
        parent: &[usize],
        x: i64,
        y: i64,
    ) -> Result<Vec<usize>, DesignerError> {
        if is_nonvisual(class) {
            return self.add_nonvisual(class);
        }
        let name = self.free_name(class);
        let (w, h) = geometry::default_size(class);
        let comp = Component::new(&name, class)
            .with_prop("Left", PropValue::Int(geometry::snap(x, geometry::GRID)))
            .with_prop("Height", PropValue::Int(h))
            .with_prop("Top", PropValue::Int(geometry::snap(y, geometry::GRID)))
            .with_prop("Width", PropValue::Int(w));
        // Only built-in classes are known to have the text property (a custom class may not).
        let text = if is_builtin(class) { text_prop(class) } else { "" };
        let comp = match text {
            "" => comp,
            text => comp.with_prop(text, PropValue::Str(name.clone())),
        };
        self.checkpoint();
        self.doc.add_child(parent, comp)?;
        let n = self.doc.component(parent).map_or(0, |p| p.children.len());
        let mut path = parent.to_vec();
        path.push(n - 1);
        self.select(path.clone());
        Ok(path)
    }

    /// Adds a non-visual component to the form, the Lazarus way: only `Left`/`Top`.
    fn add_nonvisual(&mut self, class: &str) -> Result<Vec<usize>, DesignerError> {
        let used: Vec<(i64, i64)> = self
            .tray()
            .iter()
            .filter_map(|p| self.doc.component(p))
            .map(|c| {
                let b = geometry::bounds(c);
                (b.left, b.top)
            })
            .collect();
        let left = (0..)
            .map(|k| TRAY_LEFT + TRAY_STEP * k)
            .find(|l| !used.contains(&(*l, TRAY_TOP)))
            .expect("a free slot exists");
        let comp = Component::new(&self.free_name(class), class)
            .with_prop("Left", PropValue::Int(left))
            .with_prop("Top", PropValue::Int(TRAY_TOP));
        self.checkpoint();
        self.doc.add_child(&[], comp)?;
        let path = vec![self.doc.root().children.len() - 1];
        self.select(path.clone());
        Ok(path)
    }

    /// The non-visual components (not those inside another one, such as menu items), in tray
    /// order: by their stored `Top`, then `Left`.
    pub fn tray(&self) -> Vec<Vec<usize>> {
        fn walk(c: &Component, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            for (i, child) in c.children.iter().enumerate() {
                path.push(i);
                if is_nonvisual(&child.class) {
                    out.push(path.clone());
                } else {
                    walk(child, path, out);
                }
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(self.doc.root(), &mut Vec::new(), &mut out);
        out.sort_by_key(|p| {
            let b = self.doc.component(p).map(geometry::bounds);
            b.map(|b| (b.top, b.left))
        });
        out
    }

    /// Sets position and size (relative to the parent). Does not checkpoint: a drag calls
    /// [`Designer::checkpoint`] once when it starts.
    pub fn set_bounds(&mut self, path: &[usize], b: Bounds) -> Result<(), DesignerError> {
        let Some(cur) = self.doc.component(path).map(geometry::bounds) else {
            return Err(EditError::NoSuchComponent.into());
        };
        for (name, v, old) in [
            ("Left", b.left, cur.left),
            ("Height", b.height, cur.height),
            ("Top", b.top, cur.top),
            ("Width", b.width, cur.width),
        ] {
            if v != old {
                self.doc.set_prop(path, name, PropValue::Int(v))?;
            }
        }
        Ok(())
    }

    /// Moves or resizes a component. When it is a container whose size changes, its children
    /// follow their anchors and docking. For drags; the caller checkpoints once at the start.
    pub fn place(&mut self, path: &[usize], b: Bounds) -> Result<(), DesignerError> {
        let old = self.client_size(path);
        self.set_bounds(path, b)?;
        let is_box = self
            .doc
            .component(path)
            .is_some_and(|c| is_container(&c.class));
        if let Some(old) = old
            && is_box
            && old != (b.width, b.height)
        {
            self.relayout(path, old, (b.width, b.height))?;
        }
        // Controls anchored to this one (or this one to them) follow, as they will at run time.
        let parent = &path[..path.len().saturating_sub(1)];
        if !path.is_empty() && self.has_anchor_sides(parent) {
            let size = self.client_size(parent).ok_or(EditError::NoSuchComponent)?;
            self.relayout(parent, size, size)?;
        }
        Ok(())
    }

    /// Sets the size of the form or a container from the inspector. Consecutive edits are one
    /// undo step.
    pub fn set_container_size(
        &mut self,
        path: &[usize],
        width: i64,
        height: i64,
    ) -> Result<(), DesignerError> {
        let key = (path.to_vec(), "size".to_string());
        if self.last_prop.as_ref() != Some(&key) {
            self.checkpoint();
        }
        self.resize_container(path, width, height)?;
        self.last_prop = Some(key);
        Ok(())
    }

    pub fn set_prop(
        &mut self,
        path: &[usize],
        name: &str,
        value: PropValue,
    ) -> Result<(), DesignerError> {
        if self.doc.component(path).and_then(|c| c.prop(name)) != Some(&value) {
            let key = (path.to_vec(), name.to_string());
            if self.last_prop.as_ref() != Some(&key) {
                self.checkpoint();
            }
            self.doc.set_prop(path, name, value)?;
            self.last_prop = Some(key);
        }
        Ok(())
    }

    /// Deletes every selected component (one undo step) and selects the primary's parent.
    pub fn delete_selected(&mut self) -> Result<(), DesignerError> {
        if self.selected.is_empty() {
            return Ok(());
        }
        let mut roots = self.selection_roots();
        // Last first: removing a component only shifts the indexes of later siblings.
        roots.reverse();
        let mut gone = Vec::new();
        for p in &roots {
            if let Some(c) = self.doc.component(p) {
                names_in(c, &mut gone);
            }
        }
        self.batch(None, |d| -> Result<(), DesignerError> {
            roots.iter().try_for_each(|p| d.doc.remove_component(p))?;
            // References to them (`AnchorSideLeft.Control = Button1`, `PopupMenu = ...`) would
            // stop the form from loading.
            for (path, prop) in d.references(&gone) {
                if let Some(side) = prop
                    .strip_suffix(".Control")
                    .filter(|p| p.starts_with("AnchorSide"))
                {
                    let side = format!("{side}.Side");
                    if d.doc.component(&path).and_then(|c| c.prop(&side)).is_some() {
                        d.doc.remove_prop(&path, &side)?;
                    }
                }
                d.doc.remove_prop(&path, &prop)?;
            }
            Ok(())
        })?;
        let mut parent = self.current_container();
        if self.doc.component(&parent).is_none() {
            parent.clear();
        }
        self.select(parent);
        Ok(())
    }

    /// The selected controls (not non-visual components) with their current bounds, which a
    /// drag moves together.
    pub fn movable_selection(&self) -> Vec<(Vec<usize>, Bounds)> {
        self.selection_roots()
            .into_iter()
            .filter_map(|p| {
                let c = self.doc.component(&p).filter(|c| !is_nonvisual(&c.class))?;
                Some((p, geometry::bounds(c)))
            })
            .collect()
    }

    /// The selected controls that share the primary's container, when there are two or more:
    /// that container and their union box (in its coordinates). The resize handles go on this
    /// box, and dragging them scales every control in it.
    #[allow(clippy::type_complexity)]
    pub fn resize_group(&self) -> Option<(Vec<usize>, Vec<(Vec<usize>, Bounds)>, Bounds)> {
        let parent = self.current_container();
        let items: Vec<(Vec<usize>, Bounds)> = self
            .movable_selection()
            .into_iter()
            .filter(|(p, _)| p[..p.len() - 1] == *parent)
            .collect();
        if items.len() < 2 {
            return None;
        }
        let all: Vec<Bounds> = items.iter().map(|(_, b)| *b).collect();
        let union = geometry::union(&all)?;
        Some((parent, items, union))
    }

    /// Resizes a group (see [`Designer::resize_group`]) whose union box was `from` to the box
    /// `to`: every control keeps its relative place and size in the box. For drags; the caller
    /// checkpoints when the drag starts.
    pub fn scale_all(
        &mut self,
        starts: &[(Vec<usize>, Bounds)],
        from: Bounds,
        to: Bounds,
    ) -> Result<(), DesignerError> {
        let bounds: Vec<Bounds> = starts.iter().map(|(_, b)| *b).collect();
        let placed = geometry::scale_into(&bounds, from, to);
        starts
            .iter()
            .zip(placed)
            .try_for_each(|((p, _), b)| self.place(p, b))
    }

    /// The origin of the container `path` in form coordinates.
    pub fn origin_of(&self, path: &[usize]) -> (i64, i64) {
        geometry::absolute(self.doc.root(), path).map_or((0, 0), |b| (b.left, b.top))
    }

    /// For moving `starts` (bounds relative to their own parents, which may differ): their
    /// union and what it snaps to (the primary's container and the other controls in it), both
    /// in the primary's container's coordinates, and that container's origin in the form.
    #[allow(clippy::type_complexity)]
    pub fn move_context(
        &self,
        starts: &[(Vec<usize>, Bounds)],
    ) -> Option<(Bounds, Vec<Bounds>, (i64, i64))> {
        let parent = self.current_container();
        let o = self.origin_of(&parent);
        let local: Vec<Bounds> = starts
            .iter()
            .map(|(p, b)| {
                let po = self.origin_of(&p[..p.len() - 1]);
                b.offset(po.0 - o.0, po.1 - o.1)
            })
            .collect();
        let union = geometry::union(&local)?;
        let paths: Vec<Vec<usize>> = starts.iter().map(|(p, _)| p.clone()).collect();
        Some((union, self.neighbours(&parent, &paths), o))
    }

    /// Puts each of `starts` at its start bounds moved by `(dx, dy)`. For drags; the caller
    /// checkpoints once when the drag starts.
    pub fn offset_all(
        &mut self,
        starts: &[(Vec<usize>, Bounds)],
        dx: i64,
        dy: i64,
    ) -> Result<(), DesignerError> {
        starts
            .iter()
            .try_for_each(|(p, b)| self.place(p, b.offset(dx, dy)))
    }

    /// What a drag in `parent` snaps to: the parent's client area and the controls in it that
    /// are not being dragged (`moving`), in the parent's coordinates.
    pub fn neighbours(&self, parent: &[usize], moving: &[Vec<usize>]) -> Vec<Bounds> {
        let Some(c) = self.doc.component(parent) else {
            return Vec::new();
        };
        let (w, h) = self.client_size(parent).unwrap_or((0, 0));
        let client = Bounds {
            left: 0,
            top: 0,
            width: w,
            height: h,
        };
        std::iter::once(client)
            .chain(
                c.children
                    .iter()
                    .enumerate()
                    .filter(|(i, ch)| {
                        !is_nonvisual(&ch.class)
                            && !moving
                                .iter()
                                .any(|m| m[..m.len() - 1] == *parent && m[m.len() - 1] == *i)
                    })
                    .map(|(_, ch)| geometry::bounds(ch)),
            )
            .collect()
    }

    /// Arrow keys: moves the selected controls by `(dx, dy)`, one undo step.
    pub fn nudge_selection(&mut self, dx: i64, dy: i64) -> Result<(), DesignerError> {
        let starts = self.movable_selection();
        self.batch(None, |d| d.offset_all(&starts, dx, dy))
    }

    /// The inspector with several components selected: sets `Left`, `Top`, `Width` or `Height`
    /// of all selected controls to `v`. Consecutive edits of one field are one undo step.
    pub fn set_selection_bound(&mut self, field: &str, v: i64) -> Result<(), DesignerError> {
        let items = self.movable_selection();
        let key = (Vec::new(), format!("selection.{field}"));
        self.batch(Some(key), |d| {
            items.iter().try_for_each(|(p, b)| {
                let mut b = *b;
                match field {
                    "Left" => b.left = v,
                    "Top" => b.top = v,
                    "Width" => b.width = v.max(1),
                    "Height" => b.height = v.max(1),
                    _ => return Ok(()),
                }
                d.place(p, b)
            })
        })
    }

    /// Aligns, sizes or spaces the selected controls (see [`geometry::arrange`]) relative to
    /// the primary one, as one undo step. Only controls in the primary's container take part.
    pub fn arrange(&mut self, op: geometry::Arrange) -> Result<(), DesignerError> {
        let parent = self.current_container();
        let items: Vec<(Vec<usize>, Bounds)> = self
            .movable_selection()
            .into_iter()
            .filter(|(p, _)| p[..p.len() - 1] == *parent)
            .collect();
        let reference = items
            .iter()
            .position(|(p, _)| *p == self.selected)
            .unwrap_or(0);
        let bounds: Vec<Bounds> = items.iter().map(|(_, b)| *b).collect();
        let placed = geometry::arrange(&bounds, reference, op);
        self.batch(None, |d| {
            items
                .iter()
                .zip(placed)
                .try_for_each(|((p, _), b)| d.place(p, b))
        })
    }

    /// The focusable controls directly in `container`, in tab order.
    pub fn tab_order(&self, container: &[usize]) -> Vec<Vec<usize>> {
        let Some(c) = self.doc.component(container) else {
            return Vec::new();
        };
        let focusable: Vec<usize> = (0..c.children.len())
            .filter(|&i| takes_focus(&c.children[i].class))
            .collect();
        let orders: Vec<Option<i64>> = focusable
            .iter()
            .map(|&i| c.children[i].prop("TabOrder").and_then(PropValue::as_int))
            .collect();
        tab_sequence(&orders)
            .into_iter()
            .map(|k| [container, &[focusable[k]]].concat())
            .collect()
    }

    /// Writes `TabOrder` 0, 1, 2, ... for `order` (paths of focusable children of one
    /// container), leaving values that are already right alone. `TabStop` is not touched.
    /// One undo step.
    pub fn set_tab_order(&mut self, order: &[Vec<usize>]) -> Result<(), DesignerError> {
        self.batch(None, |d| {
            order.iter().enumerate().try_for_each(|(n, p)| {
                let want = PropValue::Int(n as i64);
                match d.doc.component(p) {
                    Some(c) if c.prop("TabOrder") != Some(&want) => d
                        .doc
                        .set_prop(p, "TabOrder", want)
                        .map_err(DesignerError::from),
                    _ => Ok(()),
                }
            })
        })
    }

    /// Moves the control at position `at` of `container`'s tab order up (`-1`) or down (`1`)
    /// and renumbers the whole sequence.
    pub fn move_in_tab_order(
        &mut self,
        container: &[usize],
        at: usize,
        by: isize,
    ) -> Result<(), DesignerError> {
        let order = moved_in_sequence(&self.tab_order(container), at, by);
        self.set_tab_order(&order)
    }

    /// Sets the anchors of all selected controls, one undo step.
    pub fn set_selection_anchors(&mut self, a: geometry::Anchors) -> Result<(), DesignerError> {
        let items = self.movable_selection();
        self.batch(None, |d| {
            items.iter().try_for_each(|(p, _)| d.set_anchors(p, a))
        })
    }

    /// Docks all selected controls, one undo step.
    pub fn set_selection_align(&mut self, align: geometry::Align) -> Result<(), DesignerError> {
        let items = self.movable_selection();
        self.batch(None, |d| {
            items.iter().try_for_each(|(p, _)| d.set_align(p, align))
        })
    }

    /// Renames the selected component and the handlers that still follow the `<Name><Event>`
    /// pattern (`Button1Click` becomes `OkButtonClick`). Returns the `(old, new)` handler
    /// names so the unit can be renamed too.
    pub fn rename_selected(
        &mut self,
        new_name: &str,
    ) -> Result<Vec<(String, String)>, DesignerError> {
        let Some(c) = self.selected_component() else {
            return Ok(Vec::new());
        };
        let old = c.name.clone();
        if old == new_name || !pascoda_project::is_valid_name(new_name) || self.name_taken(new_name)
        {
            return Ok(Vec::new());
        }
        let renames: Vec<(String, String, String)> = c
            .props
            .iter()
            .filter(|p| p.name.starts_with("On"))
            .filter_map(|p| {
                let h = p.value.as_ident()?;
                let suffix = h.strip_prefix(&old)?;
                Some((p.name.clone(), h.to_string(), format!("{new_name}{suffix}")))
            })
            .collect();
        self.checkpoint();
        let path = self.selected.clone();
        self.doc.rename_component(&path, new_name)?;
        for (event, _, new_handler) in &renames {
            self.doc
                .set_prop(&path, event, PropValue::Ident(new_handler.clone()))?;
        }
        // Other components' references to it follow (anchor sides, PopupMenu, Action, ...).
        for (p, prop) in self.references(std::slice::from_ref(&old)) {
            self.doc
                .set_prop(&p, &prop, PropValue::Ident(new_name.to_string()))?;
        }
        Ok(renames
            .into_iter()
            .map(|(_, old_h, new_h)| (old_h, new_h))
            .collect())
    }

    /// Every `(component path, property)` whose value names one of `names` (a reference to a
    /// component, such as `AnchorSideLeft.Control = Button1`). Event bindings are not
    /// references.
    fn references(&self, names: &[String]) -> Vec<(Vec<usize>, String)> {
        fn walk(
            c: &Component,
            path: &mut Vec<usize>,
            names: &[String],
            out: &mut Vec<(Vec<usize>, String)>,
        ) {
            for p in &c.props {
                if !p.name.starts_with("On")
                    && let Some(v) = p.value.as_ident()
                    && names.iter().any(|n| n.eq_ignore_ascii_case(v))
                {
                    out.push((path.clone(), p.name.clone()));
                }
            }
            for (i, ch) in c.children.iter().enumerate() {
                path.push(i);
                walk(ch, path, names, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(self.doc.root(), &mut Vec::new(), names, &mut out);
        out
    }

    /// Makes sure the selected component has a handler for `event`; returns its name.
    pub fn ensure_handler(&mut self, event: &str) -> Result<Option<String>, DesignerError> {
        let Some(c) = self.selected_component() else {
            return Ok(None);
        };
        if let Some(h) = c.prop(event).and_then(PropValue::as_ident) {
            return Ok(Some(h.to_string()));
        }
        let suffix = event.strip_prefix("On").unwrap_or(event);
        // Lazarus calls the form's handlers FormCreate, FormClose, ... not Form1Create.
        let owner = if self.selected.is_empty() {
            "Form"
        } else {
            c.name.as_str()
        };
        let name = format!("{owner}{suffix}");
        self.set_prop(
            &self.selected.clone(),
            event,
            PropValue::Ident(name.clone()),
        )?;
        Ok(Some(name))
    }

    // ---- layout: anchors, align, auto-size ----------------------------------------------

    /// The client area that the children of `path` are laid out in.
    pub fn client_size(&self, path: &[usize]) -> Option<(i64, i64)> {
        let c = self.doc.component(path)?;
        Some(if path.is_empty() {
            geometry::form_size(c)
        } else {
            let b = geometry::bounds(c);
            (b.width, b.height)
        })
    }

    /// Resizes the form or a container; its children follow their anchors and docking, and
    /// containers among them pass the change on. Does not checkpoint (a drag does, once).
    pub fn resize_container(
        &mut self,
        path: &[usize],
        width: i64,
        height: i64,
    ) -> Result<(), DesignerError> {
        let old = self.client_size(path).ok_or(EditError::NoSuchComponent)?;
        let new = (width.max(1), height.max(1));
        if old == new {
            return Ok(());
        }
        if path.is_empty() {
            // Keep ClientWidth/Height and Width/Height in step, whichever the file uses.
            for (names, v) in [
                (["ClientWidth", "Width"], new.0),
                (["ClientHeight", "Height"], new.1),
            ] {
                let present: Vec<&str> = names
                    .into_iter()
                    .filter(|n| self.doc.root().prop(n).is_some())
                    .collect();
                for n in if present.is_empty() {
                    vec![names[1]]
                } else {
                    present
                } {
                    self.doc.set_prop(path, n, PropValue::Int(v))?;
                }
            }
        } else if let Some(b) = self.doc.component(path).map(geometry::bounds) {
            self.set_bounds(
                path,
                Bounds {
                    width: new.0,
                    height: new.1,
                    ..b
                },
            )?;
        }
        self.relayout(path, old, new)
    }

    /// Moves and resizes the children of `path` for a client area change from `old` to `new`.
    fn relayout(
        &mut self,
        path: &[usize],
        old: (i64, i64),
        new: (i64, i64),
    ) -> Result<(), DesignerError> {
        let Some(parent) = self.doc.component(path) else {
            return Ok(());
        };
        // Non-visual components have no bounds to lay out.
        let controls: Vec<(usize, &Component)> = parent
            .children
            .iter()
            .enumerate()
            .filter(|(_, c)| !is_nonvisual(&c.class))
            .collect();
        let items: Vec<geometry::Item> = controls
            .iter()
            .map(|(_, c)| geometry::Item::of(c))
            .collect();
        let children: Vec<(usize, bool)> = controls
            .iter()
            .map(|(i, c)| (*i, is_container(&c.class)))
            .collect();
        let placed = geometry::layout_in(old, new, &items, &self.parent_names(path));
        for (n, b) in placed.into_iter().enumerate() {
            let (i, is_box) = children[n];
            let mut child = path.to_vec();
            child.push(i);
            self.set_bounds(&child, b)?;
            let (was, now) = (
                (items[n].bounds.width, items[n].bounds.height),
                (b.width, b.height),
            );
            if is_box && was != now {
                self.relayout(&child, was, now)?;
            }
        }
        Ok(())
    }

    /// The names by which children of `path` refer to it in anchor sides: `Owner` (what
    /// Lazarus writes) and the form's name for the form, the component's name otherwise.
    fn parent_names(&self, path: &[usize]) -> Vec<String> {
        let name = self
            .doc
            .component(path)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        if path.is_empty() {
            vec!["Owner".into(), name]
        } else {
            vec![name]
        }
    }

    /// Whether any control directly in `parent` has an anchor side, so moving or resizing one
    /// of its children may move others.
    fn has_anchor_sides(&self, parent: &[usize]) -> bool {
        self.doc.component(parent).is_some_and(|c| {
            c.children.iter().any(|ch| {
                geometry::Edge::ALL
                    .iter()
                    .any(|e| geometry::anchor_side(ch, *e).is_some())
            })
        })
    }

    /// What an edge of the control at `path` can be anchored to: the parent first (`Owner`
    /// for the form), then the named sibling controls.
    pub fn anchor_targets(&self, path: &[usize]) -> Vec<String> {
        let Some((&me, parent)) = path.split_last() else {
            return Vec::new();
        };
        let Some(p) = self.doc.component(parent) else {
            return Vec::new();
        };
        let own = if parent.is_empty() {
            "Owner".to_string()
        } else {
            p.name.clone()
        };
        std::iter::once(own)
            .chain(
                p.children
                    .iter()
                    .enumerate()
                    .filter(|(i, c)| *i != me && !c.name.is_empty() && !is_nonvisual(&c.class))
                    .map(|(_, c)| c.name.clone()),
            )
            .collect()
    }

    /// Anchors `edge` of the control at `path` to a line of a sibling or the parent (or frees
    /// it with `None`), adds the edge to `Anchors` as Lazarus does, and lays out the parent
    /// again. One undo step.
    pub fn set_anchor_side(
        &mut self,
        path: &[usize],
        edge: geometry::Edge,
        to: Option<geometry::AnchorSide>,
    ) -> Result<(), DesignerError> {
        let c = self.doc.component(path).ok_or(EditError::NoSuchComponent)?;
        if geometry::anchor_side(c, edge) == to {
            return Ok(());
        }
        let (control, side) = (edge.control_prop(), edge.side_prop());
        let mut anchors = geometry::anchors_of(c);
        self.batch(None, |d| -> Result<(), DesignerError> {
            let has = |d: &Self, n: &str| d.doc.component(path).and_then(|c| c.prop(n)).is_some();
            match to {
                Some(s) => {
                    d.doc
                        .set_prop(path, &control, PropValue::Ident(s.control.clone()))?;
                    if s.side == geometry::SideRef::Start {
                        if has(d, &side) {
                            d.doc.remove_prop(path, &side)?;
                        }
                    } else {
                        d.doc
                            .set_prop(path, &side, PropValue::Ident(s.side.ident().into()))?;
                    }
                    edge.set(&mut anchors, true);
                    d.set_anchors(path, anchors)?;
                }
                None => {
                    for n in [&control, &side] {
                        if has(d, n) {
                            d.doc.remove_prop(path, n)?;
                        }
                    }
                }
            }
            let parent = &path[..path.len() - 1];
            let size = d.client_size(parent).ok_or(EditError::NoSuchComponent)?;
            d.relayout(parent, size, size)
        })
    }

    /// Sets `BorderSpacing.<field>` (`Left`, `Top`, `Right`, `Bottom`, `Around`); 0 removes it.
    /// Docked and side-anchored controls move with it. Typing is one undo step.
    pub fn set_border_spacing(
        &mut self,
        path: &[usize],
        field: &str,
        v: i64,
    ) -> Result<(), DesignerError> {
        let name = format!("BorderSpacing.{field}");
        let key = (path.to_vec(), name.clone());
        self.batch(Some(key), |d| -> Result<(), DesignerError> {
            let has = d
                .doc
                .component(path)
                .ok_or(EditError::NoSuchComponent)?
                .prop(&name)
                .is_some();
            if v == 0 {
                if has {
                    d.doc.remove_prop(path, &name)?;
                }
            } else {
                d.doc.set_prop(path, &name, PropValue::Int(v))?;
            }
            let parent = &path[..path.len().saturating_sub(1)];
            let size = d.client_size(parent).ok_or(EditError::NoSuchComponent)?;
            d.relayout(parent, size, size)
        })
    }

    pub fn set_anchors(
        &mut self,
        path: &[usize],
        a: geometry::Anchors,
    ) -> Result<(), DesignerError> {
        let has = self
            .doc
            .component(path)
            .ok_or(EditError::NoSuchComponent)?
            .prop("Anchors")
            .is_some();
        if a == geometry::Anchors::DEFAULT {
            if has {
                self.checkpoint();
                self.doc.remove_prop(path, "Anchors")?;
            }
            return Ok(());
        }
        self.set_prop(path, "Anchors", a.to_prop())
    }

    /// Docks the component (or frees it with `Align::None`) and re-lays out its parent.
    pub fn set_align(
        &mut self,
        path: &[usize],
        align: geometry::Align,
    ) -> Result<(), DesignerError> {
        let has = self
            .doc
            .component(path)
            .ok_or(EditError::NoSuchComponent)?
            .prop("Align")
            .is_some();
        if align == geometry::Align::None {
            if has {
                self.checkpoint();
                self.doc.remove_prop(path, "Align")?;
            }
        } else {
            self.set_prop(path, "Align", PropValue::Ident(align.ident().into()))?;
        }
        let parent = &path[..path.len().saturating_sub(1)];
        let size = self.client_size(parent).ok_or(EditError::NoSuchComponent)?;
        self.relayout(parent, size, size)
    }

    /// `AutoSize` is only written when it differs from the class default (LCL does the same).
    pub fn set_auto_size(&mut self, path: &[usize], on: bool) -> Result<(), DesignerError> {
        let c = self.doc.component(path).ok_or(EditError::NoSuchComponent)?;
        let default = c.class.eq_ignore_ascii_case("TLabel");
        let has = c.prop("AutoSize").is_some();
        if on == default {
            if has {
                self.checkpoint();
                self.doc.remove_prop(path, "AutoSize")?;
            }
            return Ok(());
        }
        self.set_prop(path, "AutoSize", PropValue::bool(on))
    }

    fn caption_of(c: &Component) -> String {
        c.prop(text_prop(&c.class))
            .and_then(PropValue::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn all_paths(&self) -> Vec<Vec<usize>> {
        fn walk(c: &Component, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            for (i, child) in c.children.iter().enumerate() {
                path.push(i);
                out.push(path.clone());
                walk(child, path, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(self.doc.root(), &mut Vec::new(), &mut out);
        out
    }

    /// Records every component's text and `AutoSize` as already sized (used when a form is
    /// opened: sizes stored in the file are respected until the user changes something).
    fn snapshot_autosize(&mut self) {
        let seen = self
            .all_paths()
            .iter()
            .filter_map(|p| self.doc.component(p))
            .map(|c| {
                (
                    c.name.clone(),
                    (Self::caption_of(c), geometry::auto_size_of(c)),
                )
            })
            .collect();
        self.autosize_seen = seen;
    }

    /// Gives auto-sized components the size their text calls for, but only those whose text
    /// or `AutoSize` changed since they were last sized. `preferred(class, text)` is the
    /// control's natural `(width, height)`. A `TEdit` only takes the height. Returns whether
    /// anything changed. Does not checkpoint; the edit that triggered it already did.
    pub fn apply_autosize(&mut self, preferred: impl Fn(&str, &str) -> Option<(i64, i64)>) -> bool {
        let mut changed = false;
        let mut parents: Vec<Vec<usize>> = Vec::new();
        for path in self.all_paths() {
            let Some(c) = self.doc.component(&path) else {
                continue;
            };
            if is_nonvisual(&c.class) {
                continue;
            }
            let state = (Self::caption_of(c), geometry::auto_size_of(c));
            if self.autosize_seen.get(&c.name) == Some(&state) {
                continue;
            }
            let (name, class, cur) = (c.name.clone(), c.class.clone(), geometry::bounds(c));
            self.autosize_seen.insert(name, state.clone());
            if !state.1 {
                continue;
            }
            let Some((w, h)) = preferred(&class, &state.0) else {
                continue;
            };
            let target = if class.eq_ignore_ascii_case("TEdit") {
                Bounds { height: h, ..cur }
            } else {
                Bounds {
                    width: w,
                    height: h,
                    ..cur
                }
            };
            if target != cur && self.set_bounds(&path, target).is_ok() {
                changed = true;
                parents.push(path[..path.len() - 1].to_vec());
            }
        }
        parents.dedup();
        for p in parents {
            if let Some(size) = self.client_size(&p) {
                let _ = self.relayout(&p, size, size);
            }
        }
        changed
    }

    // ---- keeping the unit in step ------------------------------------------------------

    /// Brings `unit` in step with the form: removes fields of deleted components, then adds
    /// missing fields, declarations and stubs. On failure the unit is left alone and the reason
    /// is kept in `sync_error`.
    pub fn sync_unit(&mut self, unit: &mut String) {
        match self.try_sync(unit) {
            Ok(()) => self.sync_error = None,
            Err(e) => self.sync_error = Some(e.to_string()),
        }
    }

    fn try_sync(&mut self, unit: &mut String) -> Result<(), SyncError> {
        let class = self.doc.root().class.clone();
        let wanted = sync::wanted(self.doc.root());
        let current: Vec<String> = wanted.fields.iter().map(|f| f.0.clone()).collect();
        let removed: Vec<String> = self
            .synced_fields
            .iter()
            .filter(|f| !current.iter().any(|c| c.eq_ignore_ascii_case(f)))
            .cloned()
            .collect();
        let mut text = unit.clone();
        if !removed.is_empty() {
            let plan = sync::plan_remove_fields(&text, &class, &removed)?;
            text = sync::apply_edits(&text, &plan.edits);
        }
        let own_unit = self
            .pas_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let custom = self.custom_units.clone();
        let units = units_for(self.doc.root(), &own_unit, &custom);
        let units: Vec<&str> = units.into_iter().collect();
        let plan = sync::plan_uses(&text, &units)?;
        text = sync::apply_edits(&text, &plan.edits);
        // A custom class must be registered before a form that names it in a `.lfm` loads.
        let used = custom_used(self.doc.root(), &custom);
        let plan = sync::plan_register(&text, &used)?;
        text = sync::apply_edits(&text, &plan.edits);
        let plan = sync::plan(&text, &class, &wanted)?;
        text = sync::apply_edits(&text, &plan.edits);
        *unit = text;
        self.synced_fields = current;
        Ok(())
    }

    /// Renames handler methods in the unit after [`Designer::rename_selected`], and the field.
    pub fn sync_rename(
        &mut self,
        unit: &mut String,
        old: &str,
        new: &str,
        handlers: &[(String, String)],
    ) {
        let class = self.doc.root().class.clone();
        let mut text = unit.clone();
        let result: Result<(), SyncError> = (|| {
            let p = sync::plan_rename_field(&text, &class, old, new)?;
            text = sync::apply_edits(&text, &p.edits);
            for (o, n) in handlers {
                let p = sync::plan_rename_method(&text, &class, o, n)?;
                text = sync::apply_edits(&text, &p.edits);
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                *unit = text;
                for f in &mut self.synced_fields {
                    if f.eq_ignore_ascii_case(old) {
                        *f = new.to_string();
                    }
                }
                self.sync_error = None;
            }
            Err(e) => self.sync_error = Some(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const LFM: &str = "object Form1: TForm1\n  Left = 300\n  Height = 300\n  Top = 200\n  Width = 400\n  Caption = 'Form1'\nend\n";
    pub(super) const PAS: &str = "unit Unit1;\n{$mode objfpc}{$H+}\ninterface\nuses Forms;\ntype\n  TForm1 = class(TForm)\n  end;\nvar\n  Form1: TForm1;\nimplementation\n{$R *.lfm}\nend.\n";

    pub(super) fn designer() -> Designer {
        let doc = FormDoc::parse(LFM).unwrap();
        Designer {
            lfm_path: PathBuf::from("unit1.lfm"),
            pas_path: PathBuf::from("unit1.pas"),
            saved: LFM.to_string(),
            doc,
            undo: vec![],
            custom_units: Arc::default(),
            redo: vec![],
            selected: vec![],
            also: vec![],
            batching: false,
            synced_fields: vec![],
            sync_error: None,
            last_prop: None,
            autosize_seen: HashMap::new(),
        }
    }

    #[test]
    fn adds_components_with_unique_names_and_lazarus_defaults() {
        let mut d = designer();
        let p = d.add_component("TButton", &[], 21, 30).unwrap();
        assert_eq!(p, vec![0]);
        let c = d.doc.component(&p).unwrap();
        assert_eq!(c.name, "Button1");
        assert_eq!(
            geometry::bounds(c),
            Bounds {
                left: 24,
                top: 32,
                width: 75,
                height: 25
            }
        ); // snapped
        assert_eq!(
            c.prop("Caption").and_then(PropValue::as_str),
            Some("Button1")
        );
        assert_eq!(d.add_component("TButton", &[], 0, 0).unwrap(), vec![1]);
        assert_eq!(d.doc.component(&[1]).unwrap().name, "Button2");
        d.add_component("TEdit", &[], 0, 0).unwrap();
        assert_eq!(
            d.doc
                .component(&[2])
                .unwrap()
                .prop("Text")
                .and_then(PropValue::as_str),
            Some("Edit1")
        );
        assert!(d.dirty());
    }

    #[test]
    fn undo_and_redo_restore_the_exact_text() {
        let mut d = designer();
        d.add_component("TLabel", &[], 8, 8).unwrap();
        let with = d.doc.source().to_string();
        assert!(d.undo());
        assert_eq!(d.doc.source(), LFM);
        assert!(!d.dirty());
        assert!(d.redo());
        assert_eq!(d.doc.source(), with);
        assert!(!d.undo() || !d.undo(), "history runs out");
    }

    #[test]
    fn moving_and_resizing_write_the_geometry() {
        let mut d = designer();
        let p = d.add_component("TButton", &[], 8, 8).unwrap();
        d.checkpoint();
        d.set_bounds(
            &p,
            Bounds {
                left: 40,
                top: 48,
                width: 100,
                height: 30,
            },
        )
        .unwrap();
        assert_eq!(
            geometry::bounds(d.doc.component(&p).unwrap()),
            Bounds {
                left: 40,
                top: 48,
                width: 100,
                height: 30
            }
        );
        assert!(d.undo());
        assert_eq!(geometry::bounds(d.doc.component(&p).unwrap()).left, 8);
    }

    #[test]
    fn typing_in_one_property_is_a_single_undo_step() {
        let mut d = designer();
        let p = d.add_component("TButton", &[], 8, 8).unwrap();
        for text in ["O", "Ok", "OK!"] {
            d.set_prop(&p, "Caption", PropValue::Str(text.into()))
                .unwrap();
        }
        assert!(d.undo());
        assert_eq!(
            d.doc
                .component(&p)
                .unwrap()
                .prop("Caption")
                .and_then(PropValue::as_str),
            Some("Button1")
        );
    }

    #[test]
    fn the_form_gets_lazarus_style_handler_names() {
        let mut d = designer();
        assert_eq!(
            d.ensure_handler("OnCreate").unwrap().as_deref(),
            Some("FormCreate")
        );
        assert_eq!(
            d.doc.root().prop("OnCreate").and_then(PropValue::as_ident),
            Some("FormCreate")
        );
    }

    #[test]
    fn deleting_selects_the_parent_again() {
        let mut d = designer();
        d.add_component("TLabel", &[], 0, 0).unwrap();
        d.delete_selected().unwrap();
        assert!(d.selected.is_empty());
        assert_eq!(d.doc.source(), LFM);
    }

    #[test]
    fn panels_receive_children_relative_to_themselves() {
        let mut d = designer();
        d.add_component("TPanel", &[], 16, 16).unwrap(); // not in the palette, but is a container
        let p = d.add_component("TButton", &[0], 8, 8).unwrap();
        assert_eq!(p, vec![0, 0]);
        let abs = geometry::absolute(d.doc.root(), &p).unwrap();
        assert_eq!((abs.left, abs.top), (24, 24));
    }

    #[test]
    fn sync_adds_fields_handlers_and_stubs_then_removes_fields() {
        let mut d = designer();
        let mut unit = PAS.to_string();
        d.add_component("TButton", &[], 8, 8).unwrap();
        d.ensure_handler("OnClick").unwrap();
        d.sync_unit(&mut unit);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        assert!(unit.contains("    Button1: TButton;"));
        assert!(unit.contains("    procedure Button1Click(Sender: TObject);"));
        assert!(unit.contains("procedure TForm1.Button1Click(Sender: TObject);\nbegin\nend;"));
        let again = unit.clone();
        d.sync_unit(&mut unit);
        assert_eq!(unit, again, "syncing twice changes nothing");

        d.delete_selected().unwrap();
        d.sync_unit(&mut unit);
        assert!(
            !unit.contains("    Button1: TButton;"),
            "the field goes with the component"
        );
        assert!(
            unit.contains("procedure TForm1.Button1Click"),
            "user code is never removed"
        );
    }

    #[test]
    fn renaming_updates_the_form_the_handlers_and_the_unit() {
        let mut d = designer();
        let mut unit = PAS.to_string();
        d.add_component("TButton", &[], 8, 8).unwrap();
        d.ensure_handler("OnClick").unwrap();
        d.sync_unit(&mut unit);
        let handlers = d.rename_selected("OkButton").unwrap();
        assert_eq!(
            handlers,
            vec![("Button1Click".to_string(), "OkButtonClick".to_string())]
        );
        d.sync_rename(&mut unit, "Button1", "OkButton", &handlers);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        assert!(
            d.doc.source().contains("object OkButton: TButton")
                && d.doc.source().contains("OnClick = OkButtonClick")
        );
        assert!(
            unit.contains("OkButton: TButton;")
                && unit.contains("TForm1.OkButtonClick")
                && !unit.contains("Button1")
        );
        assert!(
            d.rename_selected("bad name").unwrap().is_empty()
                && d.selected_component().unwrap().name == "OkButton"
        );
    }

    #[test]
    fn a_failed_sync_leaves_the_unit_alone() {
        let mut d = designer();
        d.add_component("TButton", &[], 0, 0).unwrap();
        let mut unit = "unit u;\ninterface\nimplementation\nend.\n".to_string();
        d.sync_unit(&mut unit);
        assert_eq!(unit, "unit u;\ninterface\nimplementation\nend.\n");
        assert!(d.sync_error.is_some());
    }

    #[test]
    fn specs_list_controls_in_z_order_with_absolute_bounds() {
        let mut d = designer();
        d.add_component("TPanel", &[], 16, 16).unwrap();
        d.add_component("TButton", &[0], 8, 8).unwrap();
        d.add_component("TLabel", &[], 100, 100).unwrap();
        let specs = d.specs();
        let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Panel1", "Button1", "Label1"]);
        assert_eq!((specs[1].bounds.left, specs[1].bounds.top), (24, 24));
    }

    #[test]
    fn resizing_the_form_moves_anchored_controls() {
        let mut d = designer();
        let b = d.add_component("TButton", &[], 16, 16).unwrap();
        d.set_anchors(
            &b,
            geometry::Anchors {
                left: false,
                top: true,
                right: true,
                bottom: false,
            },
        )
        .unwrap();
        let e = d.add_component("TEdit", &[], 16, 64).unwrap();
        d.set_anchors(
            &e,
            geometry::Anchors {
                left: true,
                top: true,
                right: true,
                bottom: true,
            },
        )
        .unwrap();
        d.resize_container(&[], 500, 350).unwrap();
        assert_eq!(d.client_size(&[]), Some((500, 350)));
        assert_eq!(geometry::bounds(d.doc.component(&b).unwrap()).left, 116); // followed the right edge
        let eb = geometry::bounds(d.doc.component(&e).unwrap());
        assert_eq!((eb.width, eb.height), (180, 73)); // stretched both ways
        assert!(d.doc.source().contains("Anchors = [akTop, akRight]"));
        assert!(d.undo(), "the last edit (the anchors) can be undone");
    }

    #[test]
    fn the_form_size_is_written_where_the_file_keeps_it() {
        let mut d = designer(); // Width/Height only
        d.resize_container(&[], 410, 310).unwrap();
        assert!(d.doc.source().contains("Width = 410") && !d.doc.source().contains("ClientWidth"));
        let lfm = "object F: TF\n  ClientHeight = 100\n  ClientWidth = 200\n  Height = 130\n  Width = 200\nend\n";
        let mut d = designer();
        d.doc = FormDoc::parse(lfm).unwrap();
        d.resize_container(&[], 300, 120).unwrap();
        let r = d.doc.root();
        assert_eq!(r.prop("ClientWidth"), Some(&PropValue::Int(300)));
        assert_eq!(r.prop("Width"), Some(&PropValue::Int(300)));
        assert_eq!(r.prop("ClientHeight"), Some(&PropValue::Int(120)));
    }

    #[test]
    fn docking_follows_the_form_when_it_is_resized() {
        let mut d = designer();
        let p = d.add_component("TPanel", &[], 0, 0).unwrap();
        d.set_align(&p, geometry::Align::Top).unwrap();
        assert_eq!(geometry::bounds(d.doc.component(&p).unwrap()).width, 400);
        d.resize_container(&[], 640, 300).unwrap();
        assert_eq!(geometry::bounds(d.doc.component(&p).unwrap()).width, 640);
        assert!(d.doc.source().contains("Align = alTop"));
        d.set_align(&p, geometry::Align::None).unwrap();
        assert!(!d.doc.source().contains("Align"));
    }

    #[test]
    fn containers_pass_a_resize_on_to_their_children() {
        let mut d = designer();
        let panel = d.add_component("TPanel", &[], 16, 16).unwrap(); // 185x105
        let b = d.add_component("TButton", &panel, 96, 8).unwrap();
        d.set_anchors(
            &b,
            geometry::Anchors {
                left: false,
                top: true,
                right: true,
                bottom: false,
            },
        )
        .unwrap();
        d.resize_container(&panel, 285, 105).unwrap();
        assert_eq!(geometry::bounds(d.doc.component(&b).unwrap()).left, 196);
    }

    #[test]
    fn default_anchors_and_align_are_not_written() {
        let mut d = designer();
        let b = d.add_component("TButton", &[], 8, 8).unwrap();
        d.set_anchors(
            &b,
            geometry::Anchors {
                left: true,
                top: true,
                right: true,
                bottom: false,
            },
        )
        .unwrap();
        assert!(
            d.doc
                .source()
                .contains("Anchors = [akTop, akLeft, akRight]")
        );
        d.set_anchors(&b, geometry::Anchors::DEFAULT).unwrap();
        assert!(!d.doc.source().contains("Anchors"));
        d.set_auto_size(&b, true).unwrap();
        assert!(d.doc.source().contains("AutoSize = True"));
        d.set_auto_size(&b, false).unwrap();
        assert!(!d.doc.source().contains("AutoSize"));
    }

    #[test]
    fn auto_sized_components_follow_their_text_and_untouched_ones_are_left_alone() {
        let preferred = |_: &str, text: &str| Some((10 + 7 * text.chars().count() as i64, 17));
        let lfm = "object F: TF\n  object Old: TLabel\n    Left = 1\n    Height = 99\n    Top = 1\n    Width = 99\n    Caption = 'kept'\n  end\nend\n";
        let mut d = designer();
        d.doc = FormDoc::parse(lfm).unwrap();
        d.snapshot_autosize(); // as when a form is opened
        assert!(
            !d.apply_autosize(preferred),
            "stored sizes are respected on open"
        );

        let l = d.add_component("TLabel", &[], 8, 8).unwrap(); // a TLabel auto-sizes by default
        assert!(d.apply_autosize(preferred));
        let b = geometry::bounds(d.doc.component(&l).unwrap());
        assert_eq!((b.width, b.height), (10 + 7 * 6, 17)); // "Label1"
        assert_eq!(
            geometry::bounds(d.doc.component(&[0]).unwrap()).width,
            99,
            "untouched label unchanged"
        );
        assert!(!d.apply_autosize(preferred), "nothing left to do");

        d.set_prop(&l, "Caption", PropValue::Str("Longer text".into()))
            .unwrap();
        assert!(d.apply_autosize(preferred));
        assert_eq!(
            geometry::bounds(d.doc.component(&l).unwrap()).width,
            10 + 7 * 11
        );

        let btn = d.add_component("TButton", &[], 8, 64).unwrap(); // buttons do not, until asked
        assert!(!d.apply_autosize(preferred));
        d.set_auto_size(&btn, true).unwrap();
        assert!(d.apply_autosize(preferred));
        assert_eq!(
            geometry::bounds(d.doc.component(&btn).unwrap()).width,
            10 + 7 * 7
        );

        let edit = d.add_component("TEdit", &[], 8, 100).unwrap();
        d.set_auto_size(&edit, true).unwrap();
        d.apply_autosize(preferred);
        let eb = geometry::bounds(d.doc.component(&edit).unwrap());
        assert_eq!(
            (eb.width, eb.height),
            (80, 17),
            "an edit only takes the height"
        );
    }

    #[test]
    fn non_visual_components_live_in_the_tray_not_on_the_form() {
        let mut d = designer();
        let mut unit = PAS.to_string();
        d.add_component("TButton", &[], 8, 8).unwrap();
        let t = d.add_component("TTimer", &[], 200, 200).unwrap();
        assert!(
            d.doc
                .source()
                .contains("  object Timer1: TTimer\n    Left = 16\n    Top = 16\n  end\n"),
            "only the design position, like Lazarus:\n{}",
            d.doc.source()
        );
        let o = d.add_component("TOpenDialog", &[0], 0, 0).unwrap();
        assert_eq!(o, vec![2], "always on the form, never in a container");
        let od = d.doc.component(&o).unwrap();
        assert_eq!(
            (od.name.as_str(), geometry::bounds(od).left),
            ("OpenDialog1", 56)
        );
        assert_eq!(d.tray(), vec![t.clone(), o.clone()]);

        let names: Vec<_> = d.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            ["Button1"],
            "no native views for non-visual components"
        );
        assert_eq!(geometry::hit_test(d.doc.root(), 20.0, 20.0), Some(vec![0]));
        assert_eq!(geometry::hit_test(d.doc.root(), 60.0, 20.0), Some(vec![0])); // button, not the dialog
        d.resize_container(&[], 600, 400).unwrap();
        assert!(
            !d.doc.source().contains("Width = 80"),
            "layout leaves them alone"
        );

        d.selected = t.clone();
        assert_eq!(
            d.ensure_handler(default_event("TTimer"))
                .unwrap()
                .as_deref(),
            Some("Timer1Timer")
        );
        d.sync_unit(&mut unit);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        assert!(
            unit.contains("uses Forms, StdCtrls, ExtCtrls, Dialogs;"),
            "{unit}"
        );
        assert!(
            unit.contains("    Timer1: TTimer;") && unit.contains("    OpenDialog1: TOpenDialog;")
        );
        assert!(unit.contains("procedure TForm1.Timer1Timer(Sender: TObject);"));

        d.selected = t;
        d.delete_selected().unwrap();
        d.sync_unit(&mut unit);
        assert!(
            !unit.contains("Timer1: TTimer;"),
            "the field goes with the component"
        );
        assert_eq!(d.tray(), vec![vec![1]]);
        assert_eq!(
            d.add_component("TTimer", &[], 0, 0).unwrap(),
            vec![2],
            "the freed slot is reused"
        );
        assert_eq!(geometry::bounds(d.doc.component(&[2]).unwrap()).left, 16);
        assert_eq!(d.tray(), vec![vec![2], vec![1]]);
    }

    fn left(d: &Designer, p: &[usize]) -> i64 {
        geometry::bounds(d.doc.component(p).unwrap()).left
    }

    /// Button1 at 8, Button2 at 96, a panel at 200 with Edit1 inside, and Timer1.
    fn four_controls() -> Designer {
        let mut d = designer();
        d.add_component("TButton", &[], 8, 8).unwrap();
        d.add_component("TButton", &[], 96, 8).unwrap();
        let panel = d.add_component("TPanel", &[], 200, 8).unwrap();
        d.add_component("TEdit", &panel, 8, 8).unwrap();
        d.add_component("TTimer", &[], 0, 0).unwrap();
        d.select(Vec::new());
        d
    }

    #[test]
    fn a_selection_can_span_containers_for_moving_deleting_and_copying() {
        let mut d = four_controls();
        d.select(vec![0]);
        d.toggle(vec![2, 0]); // Edit1 inside the panel
        assert_eq!(d.selection(), vec![vec![0], vec![2, 0]]);
        let before = d.doc.source().to_string();
        d.nudge_selection(8, 0).unwrap();
        assert_eq!((left(&d, &[0]), left(&d, &[2, 0])), (16, 16));
        assert!(d.undo());
        let (union, near, origin) = d.move_context(&d.movable_selection()).unwrap();
        assert_eq!(origin, (0, 0), "the primary is on the form");
        assert_eq!(
            (union.left, union.top),
            (8, 8),
            "in the primary's container"
        );
        assert_eq!(
            union.right(),
            200 + 8 + 80,
            "the edit's right edge, absolute"
        );
        assert!(near.iter().any(|b| b.left == 96), "Button2 is a neighbour");
        assert!(
            d.resize_group().is_none(),
            "one control per container: no group handles"
        );
        let text = d.copy_selection().unwrap();
        assert!(text.contains("object Button1") && text.contains("object Edit1"));
        // The arrange commands only use the primary's container.
        d.arrange(geometry::Arrange::Tops).unwrap();
        assert_eq!(d.doc.source(), before);
        d.delete_selected().unwrap();
        let names: Vec<_> = d
            .all_paths()
            .iter()
            .map(|p| d.doc.component(p).unwrap().name.clone())
            .collect();
        assert_eq!(names, ["Button2", "Panel1", "Timer1"]);
        assert!(d.undo());
        assert_eq!(d.doc.source(), before);
        // A band with Shift adds to a selection in another container.
        d.select(vec![2, 0]);
        d.select_in_rect(&[], Bounds::spanning((0, 0), (10, 10)), true);
        assert_eq!(d.selection(), vec![vec![2, 0], vec![0]]);
    }

    #[test]
    fn group_handles_scale_the_selected_siblings() {
        let mut d = four_controls();
        d.select_paths(vec![vec![0], vec![1]]); // 8..83 and 96..171, both 25 high at top 8
        let (parent, starts, union) = d.resize_group().unwrap();
        assert!(parent.is_empty());
        assert_eq!(
            union,
            Bounds {
                left: 8,
                top: 8,
                width: 163,
                height: 25
            }
        );
        d.checkpoint();
        let to = Bounds {
            width: 326,
            height: 50,
            ..union
        };
        d.scale_all(&starts, union, to).unwrap();
        let b = |d: &Designer, p: &[usize]| geometry::bounds(d.doc.component(p).unwrap());
        assert_eq!(
            b(&d, &[0]),
            Bounds {
                left: 8,
                top: 8,
                width: 150,
                height: 50
            }
        );
        assert_eq!(
            b(&d, &[1]),
            Bounds {
                left: 184,
                top: 8,
                width: 150,
                height: 50
            }
        );
        assert!(d.undo());
        assert_eq!(b(&d, &[1]).width, 75);
    }

    #[test]
    fn shift_click_toggles_across_containers() {
        let mut d = four_controls();
        d.toggle(vec![0]);
        assert_eq!(
            d.selection(),
            vec![vec![0]],
            "toggling from the form selects"
        );
        d.toggle(vec![1]);
        d.toggle(vec![3]); // the timer, from the tray
        assert_eq!(d.selection(), vec![vec![0], vec![1], vec![3]]);
        assert_eq!(d.selected, vec![0], "the first one stays primary");
        d.toggle(vec![0]);
        assert_eq!(
            d.selection(),
            vec![vec![1], vec![3]],
            "the next one becomes primary"
        );
        d.toggle(vec![2, 0]);
        assert_eq!(
            d.selection(),
            vec![vec![1], vec![3], vec![2, 0]],
            "inside the panel: added too"
        );
        for p in [vec![1], vec![3], vec![2, 0]] {
            d.toggle(p);
        }
        assert!(
            d.selection().is_empty() && d.selected.is_empty(),
            "back to the form"
        );
    }

    #[test]
    fn select_all_and_the_rubber_band_stay_in_one_container() {
        let mut d = four_controls();
        d.select_all();
        assert_eq!(
            d.selection(),
            vec![vec![0], vec![1], vec![2]],
            "the form's controls, not the timer"
        );
        d.select(vec![2, 0]);
        d.select_all();
        assert_eq!(d.selection(), vec![vec![2, 0]]);

        // A band over both buttons (8..83 and 96..171, top 8) but not the panel at 200.
        let band = Bounds::spanning((50, 0), (120, 20));
        d.select_in_rect(&[], band, false);
        assert_eq!(d.selection(), vec![vec![0], vec![1]]);
        d.select(vec![2]);
        d.select_in_rect(&[], Bounds::spanning((0, 0), (10, 10)), true);
        assert_eq!(d.selection(), vec![vec![2], vec![0]], "Shift adds to it");
        d.select_in_rect(&[], Bounds::spanning((0, 200), (10, 210)), false);
        assert!(d.selection().is_empty(), "an empty band selects the form");
    }

    #[test]
    fn moving_and_deleting_a_selection_is_one_undo_step() {
        let mut d = four_controls();
        let before = d.doc.source().to_string();
        d.select_paths(vec![vec![0], vec![1], vec![3]]);
        d.nudge_selection(8, 0).unwrap();
        assert_eq!((left(&d, &[0]), left(&d, &[1])), (16, 104));
        assert_eq!(left(&d, &[3]), 16, "non-visual components are not moved");
        assert!(d.undo());
        assert_eq!(d.doc.source(), before);
        assert_eq!(d.selection().len(), 3, "undo keeps the selection");

        // A drag: start bounds once, then offsets from them.
        let starts = d.movable_selection();
        d.checkpoint();
        d.offset_all(&starts, 5, 0).unwrap();
        d.offset_all(&starts, 16, 8).unwrap();
        assert_eq!((left(&d, &[0]), left(&d, &[1])), (24, 112));
        assert!(d.undo());
        assert_eq!(d.doc.source(), before);

        d.delete_selected().unwrap();
        let names: Vec<_> = d
            .doc
            .root()
            .children
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["Panel1"]);
        assert!(d.selected.is_empty());
        assert!(d.undo());
        assert_eq!(d.doc.source(), before, "one step brings all three back");
    }

    #[test]
    fn nothing_to_move_leaves_no_undo_step() {
        let mut d = four_controls();
        d.select(vec![3]); // the timer
        d.nudge_selection(1, 0).unwrap();
        let after_add = d.doc.source().to_string();
        assert!(d.undo());
        assert_ne!(
            d.doc.source(),
            after_add,
            "the undo was the timer being added"
        );
    }

    #[test]
    fn the_multi_inspector_edits_all_controls_as_one_step() {
        let mut d = four_controls();
        d.select_paths(vec![vec![0], vec![2]]);
        for v in [1, 12, 120] {
            d.set_selection_bound("Top", v).unwrap();
        }
        let top = |d: &Designer, p: &[usize]| geometry::bounds(d.doc.component(p).unwrap()).top;
        assert_eq!((top(&d, &[0]), top(&d, &[2])), (120, 120));
        d.set_selection_anchors(geometry::Anchors {
            left: true,
            top: true,
            right: true,
            bottom: false,
        })
        .unwrap();
        assert_eq!(d.doc.source().matches("akRight").count(), 2);
        assert!(d.undo());
        assert!(!d.doc.source().contains("akRight"));
        assert!(d.undo());
        assert_eq!(top(&d, &[0]), 8, "the typing was one step");
    }

    #[test]
    fn arrange_aligns_to_the_primary_in_one_undo_step() {
        let mut d = four_controls();
        d.set_bounds(
            &[1],
            Bounds {
                left: 96,
                top: 40,
                width: 75,
                height: 25,
            },
        )
        .unwrap();
        let before = d.doc.source().to_string();
        d.select_paths(vec![vec![1], vec![0], vec![2], vec![3]]); // Button2 primary; a timer too
        d.arrange(geometry::Arrange::Tops).unwrap();
        let top = |d: &Designer, p: &[usize]| geometry::bounds(d.doc.component(p).unwrap()).top;
        assert_eq!((top(&d, &[0]), top(&d, &[1]), top(&d, &[2])), (40, 40, 40));
        assert_eq!(top(&d, &[3]), 16, "the timer stays in the tray");
        d.arrange(geometry::Arrange::SameWidth).unwrap();
        assert_eq!(
            geometry::bounds(d.doc.component(&[2]).unwrap()).width,
            75,
            "the panel takes Button2's width"
        );
        assert!(d.undo() && d.undo());
        assert_eq!(d.doc.source(), before);
    }

    #[test]
    fn tab_sequence_orders_by_value_then_file_order() {
        assert_eq!(
            tab_sequence(&[Some(2), None, Some(0), Some(0)]),
            [2, 3, 0, 1]
        );
        assert_eq!(moved_in_sequence(&['a', 'b', 'c'], 2, -1), ['a', 'c', 'b']);
        assert_eq!(moved_in_sequence(&['a', 'b', 'c'], 0, -1), ['a', 'b', 'c']);
        assert_eq!(moved_in_sequence(&['a', 'b', 'c'], 2, 1), ['a', 'b', 'c']);
        assert!(takes_focus("TEdit") && takes_focus("TPanel"));
        assert!(!takes_focus("TLabel") && !takes_focus("TTimer"));
    }

    #[test]
    fn tab_order_lists_focusable_children_and_renumbers_them() {
        let mut d = four_controls();
        d.add_component("TLabel", &[], 8, 200).unwrap(); // [4], no focus
        assert_eq!(d.tab_order(&[]), vec![vec![0], vec![1], vec![2]]);
        assert_eq!(d.tab_order(&[2]), vec![vec![2, 0]]);
        d.doc
            .set_prop(&[0], "TabStop", PropValue::bool(false))
            .unwrap();
        let before = d.doc.source().to_string();

        d.move_in_tab_order(&[], 2, -1).unwrap(); // the panel before Button2
        assert_eq!(d.tab_order(&[]), vec![vec![0], vec![2], vec![1]]);
        let order =
            |d: &Designer, p: &[usize]| d.doc.component(p).unwrap().prop("TabOrder").cloned();
        assert_eq!(
            (order(&d, &[0]), order(&d, &[2]), order(&d, &[1])),
            (
                Some(PropValue::Int(0)),
                Some(PropValue::Int(1)),
                Some(PropValue::Int(2))
            )
        );
        assert_eq!(order(&d, &[4]), None, "labels get none");
        assert!(
            d.doc.source().contains("TabStop = False"),
            "TabStop is left alone"
        );

        d.move_in_tab_order(&[], 0, 1).unwrap();
        assert_eq!(d.tab_order(&[]), vec![vec![2], vec![0], vec![1]]);
        let lfm = d.doc.source().to_string();
        d.set_tab_order(&d.tab_order(&[])).unwrap();
        assert_eq!(
            d.doc.source(),
            lfm,
            "renumbering a numbered sequence changes nothing"
        );
        assert!(d.undo() && d.undo());
        assert_eq!(d.doc.source(), before, "each move is one undo step");
    }

    #[test]
    fn copy_and_paste_renames_offsets_and_keeps_the_text() {
        let mut d = four_controls();
        let mut unit = PAS.to_string();
        d.select(vec![0]);
        d.ensure_handler("OnClick").unwrap();
        d.doc
            .set_prop(&[2, 0], "PopupMenu", PropValue::Ident("Panel1".into()))
            .unwrap(); // a reference to a pasted component
        d.select_paths(vec![vec![0], vec![2]]);
        let text = d.copy_selection().unwrap();
        assert!(
            text.starts_with("object Button1: TButton\n  Left = 8\n"),
            "{text}"
        );
        assert!(
            text.contains("\nobject Panel1: TPanel\n") && text.contains("  object Edit1: TEdit\n")
        );
        // As if copied from another form, whose handler this form does not have.
        let text = text.replace(
            "  object Edit1: TEdit\n",
            "  object Edit1: TEdit\n    OnChange = SomewhereElse\n",
        );

        let before = d.doc.source().to_string();
        let pasted = d.paste(&text).unwrap();
        assert_eq!(pasted, vec![vec![4], vec![5]]);
        assert_eq!(d.selection(), pasted);
        let b = d.doc.component(&[4]).unwrap();
        assert_eq!((b.name.as_str(), left(&d, &[4])), ("Button3", 16));
        assert_eq!(
            b.prop("OnClick").and_then(PropValue::as_ident),
            Some("Button1Click"),
            "a handler that exists in the form stays bound"
        );
        let p = d.doc.component(&[5]).unwrap();
        assert_eq!(
            (p.name.as_str(), p.children[0].name.as_str()),
            ("Panel2", "Edit2")
        );
        assert_eq!(
            left(&d, &[5, 0]),
            8,
            "children keep their place in the container"
        );
        assert_eq!(
            p.children[0].prop("OnChange"),
            None,
            "a handler that is not bound in this form is dropped"
        );
        assert_eq!(
            p.children[0]
                .prop("PopupMenu")
                .and_then(PropValue::as_ident),
            Some("Panel2"),
            "references follow the renames"
        );
        assert_eq!(
            p.children[0].prop("Caption"),
            None,
            "nothing else is touched (an edit has Text, not Caption)"
        );
        d.sync_unit(&mut unit);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        assert!(unit.contains("    Button3: TButton;") && unit.contains("    Edit2: TEdit;"));
        assert_eq!(unit.matches("procedure TForm1.Button1Click").count(), 1);

        assert!(d.undo());
        assert_eq!(d.doc.source(), before, "a paste is one undo step");
        assert!(d.paste("not a component").unwrap().is_empty());
        assert_eq!(d.doc.source(), before);
    }

    #[test]
    fn paste_goes_into_the_selected_container_and_timers_into_the_tray() {
        let mut d = four_controls();
        d.select_paths(vec![vec![0], vec![3]]);
        let text = d.copy_selection().unwrap();
        d.select(vec![2]); // the panel
        let pasted = d.paste(&text).unwrap();
        assert_eq!(pasted, vec![vec![2, 1], vec![4]]);
        assert_eq!(d.doc.component(&[2, 1]).unwrap().name, "Button3");
        let t = d.doc.component(&[4]).unwrap();
        assert_eq!((t.name.as_str(), geometry::bounds(t).left), ("Timer2", 56));

        // Cut and paste keeps the names: they are free again.
        d.select(vec![1]);
        let text = d.copy_selection().unwrap();
        d.delete_selected().unwrap();
        let pasted = d.paste(&text).unwrap();
        assert_eq!(d.doc.component(&pasted[0]).unwrap().name, "Button2");
    }

    #[test]
    fn duplicate_puts_the_copy_next_to_the_original() {
        let mut d = four_controls();
        d.select(vec![2, 0]); // Edit1 in the panel
        let p = d.duplicate().unwrap();
        assert_eq!(p, vec![vec![2, 1]]);
        assert_eq!(d.doc.component(&p[0]).unwrap().name, "Edit2");
        assert_eq!(
            geometry::bounds(d.doc.component(&p[0]).unwrap()).top,
            16,
            "one grid step down"
        );
        d.select(vec![2]);
        assert_eq!(
            d.duplicate().unwrap(),
            vec![vec![4]],
            "a panel duplicates beside itself"
        );
        assert_eq!(d.doc.component(&[4]).unwrap().children.len(), 2);
    }

    #[test]
    fn anchor_sides_are_written_like_lazarus_and_followed() {
        use geometry::{AnchorSide, Edge, SideRef};
        let mut d = designer();
        let e = d.add_component("TEdit", &[], 16, 16).unwrap(); // 80x23
        let l = d.add_component("TLabel", &[], 200, 100).unwrap();
        assert_eq!(d.anchor_targets(&l), ["Owner", "Edit1"]);
        let before = d.doc.source().to_string();
        d.set_anchor_side(
            &l,
            Edge::Left,
            Some(AnchorSide {
                control: "Edit1".into(),
                side: SideRef::End,
            }),
        )
        .unwrap();
        d.set_border_spacing(&l, "Left", 3).unwrap();
        d.set_border_spacing(&l, "Left", 6).unwrap(); // typing: one step with the 3
        d.set_anchor_side(
            &l,
            Edge::Top,
            Some(AnchorSide {
                control: "Edit1".into(),
                side: SideRef::Start,
            }),
        )
        .unwrap();
        let src = d.doc.source().to_string();
        for line in [
            "    AnchorSideLeft.Control = Edit1\n    AnchorSideLeft.Side = asrBottom\n",
            "    AnchorSideTop.Control = Edit1\n",
            "    BorderSpacing.Left = 6\n",
        ] {
            assert!(src.contains(line), "{line}{src}");
        }
        assert!(
            !src.contains("AnchorSideTop.Side"),
            "the default side is not written"
        );
        assert!(
            !src.contains("Anchors ="),
            "Left and Top are the default anchors"
        );
        let lb = geometry::bounds(d.doc.component(&l).unwrap());
        assert_eq!((lb.left, lb.top), (16 + 80 + 6, 16));

        // Moving the edit takes the label along.
        d.place(
            &e,
            Bounds {
                left: 40,
                top: 48,
                width: 80,
                height: 23,
            },
        )
        .unwrap();
        let lb = geometry::bounds(d.doc.component(&l).unwrap());
        assert_eq!((lb.left, lb.top), (126, 48));

        // Renaming the edit updates the reference; deleting it removes the reference.
        d.select(e.clone());
        d.rename_selected("NameEdit").unwrap();
        assert!(d.doc.source().contains("AnchorSideLeft.Control = NameEdit"));
        d.delete_selected().unwrap();
        assert!(!d.doc.source().contains("AnchorSide"), "{}", d.doc.source());
        assert!(d.doc.source().contains("BorderSpacing.Left = 6"));

        for _ in 0..5 {
            assert!(d.undo());
        }
        assert_eq!(
            d.doc.source(),
            before,
            "five steps (the move had no checkpoint)"
        );
        // Freeing an edge removes both lines.
        d.redo();
        d.set_anchor_side(&l, Edge::Left, None).unwrap();
        assert!(!d.doc.source().contains("AnchorSideLeft"));
    }

    #[test]
    fn anchor_sides_to_the_parent_stretch_with_it() {
        use geometry::{AnchorSide, Edge, SideRef};
        let mut d = designer(); // 400x300
        let b = d.add_component("TButton", &[], 16, 16).unwrap();
        d.set_border_spacing(&b, "Around", 8).unwrap();
        d.set_anchor_side(
            &b,
            Edge::Right,
            Some(AnchorSide {
                control: "Owner".into(),
                side: SideRef::End,
            }),
        )
        .unwrap();
        assert!(
            d.doc
                .source()
                .contains("Anchors = [akTop, akLeft, akRight]")
        );
        let bb = geometry::bounds(d.doc.component(&b).unwrap());
        assert_eq!((bb.left, bb.right()), (16, 400 - 8));
        d.resize_container(&[], 500, 300).unwrap();
        assert_eq!(
            geometry::bounds(d.doc.component(&b).unwrap()).right(),
            500 - 8
        );
    }

    #[test]
    fn border_spacing_insets_docked_controls() {
        let mut d = designer();
        let p = d.add_component("TPanel", &[], 0, 0).unwrap();
        d.set_align(&p, geometry::Align::Top).unwrap();
        d.set_border_spacing(&p, "Around", 6).unwrap();
        let pb = geometry::bounds(d.doc.component(&p).unwrap());
        assert_eq!((pb.left, pb.top, pb.width), (6, 6, 400 - 12));
        d.set_border_spacing(&p, "Around", 0).unwrap();
        assert!(!d.doc.source().contains("BorderSpacing"));
    }

    /// A copy of the GUI fixture in the scratch dir, and the LCL (`None`: not vendored).
    fn lcl_scratch(name: &str) -> Option<(PathBuf, pascoda_fpc::lcl::Lcl)> {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gui");
        let dir = crate::target_tmp().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["hello.lpr", "unit1.pas", "unit1.lfm"] {
            std::fs::copy(fixtures.join(f), dir.join(f)).unwrap();
        }
        let dir = dir.canonicalize().unwrap();
        let Some(lcl) = pascoda_fpc::lcl::Lcl::find(&dir) else {
            eprintln!("skipped: vendor/lazarus not found");
            return None;
        };
        Some((dir, lcl))
    }

    /// Saves the form and unit, builds with the LCL and checks the program stays up (a form
    /// that fails to load ends it at startup).
    fn build_and_start(d: &mut Designer, unit: &str, dir: &Path, lcl: &pascoda_fpc::lcl::Lcl) {
        d.save().unwrap();
        std::fs::write(dir.join("unit1.pas"), unit).unwrap();
        let built =
            pascoda_fpc::lcl::build_gui(&dir.join("hello.lpr"), pascoda_fpc::Mode::ObjFpc, lcl)
                .unwrap();
        assert!(
            built.success,
            "{}\n--- unit ---\n{unit}\n--- form ---\n{}",
            built.raw_output,
            d.doc.source()
        );
        let mut app = std::process::Command::new(dir.join("hello"))
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        let alive = app.try_wait().unwrap().is_none();
        let _ = app.kill();
        let _ = app.wait();
        assert!(
            alive,
            "the program exited at startup: the form failed to load\n{}",
            d.doc.source()
        );
    }

    #[test]
    fn menus_actions_and_anchor_sides_build_and_load_with_the_lcl() {
        use geometry::{AnchorSide, Edge, SideRef};
        use menu::NewItem;
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dir, lcl)) = lcl_scratch("menu-gui") else {
            return;
        };
        let mut d = Designer::open(&dir.join("unit1.pas")).unwrap();
        let mut unit = std::fs::read_to_string(dir.join("unit1.pas")).unwrap();

        // Main menu: File (Open Cmd+O with a handler, a separator, Recent > two checked
        // items, Quit disabled), Edit.
        let main = d.add_component("TMainMenu", &[], 0, 0).unwrap();
        let file = d.add_item(&main, NewItem::Item).unwrap();
        d.set_prop(&file, "Caption", PropValue::Str("&File".into()))
            .unwrap();
        let open = d.add_item(&file, NewItem::SubItem).unwrap();
        d.set_prop(&open, "Caption", PropValue::Str("Open\u{2026}".into()))
            .unwrap();
        assert!(d.set_shortcut(&open, "Cmd+O").unwrap());
        d.select(open.clone());
        d.ensure_handler("OnClick").unwrap();
        let sep = d.add_item(&open, NewItem::Separator).unwrap();
        let recent = d.add_item(&sep, NewItem::Item).unwrap();
        let r1 = d.add_item(&recent, NewItem::SubItem).unwrap();
        d.set_flag(&r1, "Checked", true, false).unwrap();
        let r2 = d.add_item(&r1, NewItem::Item).unwrap();
        d.select(r2);
        d.ensure_handler("OnClick").unwrap();
        let quit = d.add_item(&recent, NewItem::Item).unwrap();
        d.set_flag(&quit, "Enabled", false, true).unwrap();
        assert!(d.set_shortcut(&quit, "Cmd+Q").unwrap());
        d.add_item(&file, NewItem::Item).unwrap();
        // Popup menu with one item, bound to the button.
        let popup = d.add_component("TPopupMenu", &[], 0, 0).unwrap();
        let pi = d.add_item(&popup, NewItem::Item).unwrap();
        d.select(pi);
        d.ensure_handler("OnClick").unwrap();
        d.doc
            .set_prop(&[0], "PopupMenu", PropValue::Ident("PopupMenu1".into()))
            .unwrap();
        // Action list with two actions, one linked to a menu item.
        let list = d.add_component("TActionList", &[], 0, 0).unwrap();
        let a1 = d.add_item(&list, NewItem::Item).unwrap();
        d.select(a1);
        d.ensure_handler(default_event("TAction")).unwrap();
        d.add_item(&list, NewItem::Item).unwrap();
        d.doc
            .set_prop(&quit, "Action", PropValue::Ident("Action2".into()))
            .unwrap();
        // Reorder: Edit before File, then back; the tray reordered.
        let edit = [main.as_slice(), &[1]].concat();
        let edit = d.move_item(&edit, -1).unwrap();
        d.move_item(&edit, 1).unwrap();
        d.move_in_tray(2, 0).unwrap();

        // Anchor sides and border spacing on the existing button and label.
        d.set_anchor_side(
            &[1],
            Edge::Top,
            Some(AnchorSide {
                control: "Button1".into(),
                side: SideRef::End,
            }),
        )
        .unwrap();
        d.set_border_spacing(&[1], "Top", 8).unwrap();
        d.set_anchor_side(
            &[0],
            Edge::Right,
            Some(AnchorSide {
                control: "Owner".into(),
                side: SideRef::End,
            }),
        )
        .unwrap();
        d.set_border_spacing(&[0], "Around", 16).unwrap();

        d.sync_unit(&mut unit);
        assert_eq!(d.sync_error, None);
        for line in [
            "    MenuItem4: TMenuItem;",
            "    Action1: TAction;",
            "    Separator1: TMenuItem;",
        ] {
            assert!(unit.contains(line), "{line}\n{unit}");
        }
        build_and_start(&mut d, &unit, &dir, &lcl);
    }

    #[test]
    fn every_palette_class_builds_and_loads_with_the_lcl() {
        let _fpc = crate::FPC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dir, lcl)) = lcl_scratch("palette-gui") else {
            return;
        };
        let mut d = Designer::open(&dir.join("unit1.pas")).unwrap();
        let mut unit = std::fs::read_to_string(dir.join("unit1.pas")).unwrap();
        for (i, en) in PALETTE.iter().enumerate() {
            d.add_component(
                en.class,
                &[],
                8 + (i as i64 % 4) * 100,
                8 + (i as i64 / 4) * 40,
            )
            .unwrap();
            // Non-visual events have their own signatures; the build checks them.
            if en.nonvisual {
                d.ensure_handler(default_event(en.class)).unwrap();
            }
        }
        d.sync_unit(&mut unit);
        assert_eq!(d.sync_error, None);
        build_and_start(&mut d, &unit, &dir, &lcl);
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;

    /// `.pp` units pair with their `.lfm` the same way `.pas` units do.
    /// A text `.dfm` pairs with its unit; a binary one does not, and is not read as a form.
    #[test]
    fn a_dfm_pairs_only_in_text_form() {
        let d = std::env::temp_dir().join(format!("pascoda-dfm-pair-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (pas, dfm) = (d.join("main.pas"), d.join("main.dfm"));
        std::fs::write(&pas, "unit main;\n").unwrap();
        std::fs::write(&dfm, "object Form1: TForm1\nend\n").unwrap();
        assert_eq!(sibling_lfm(&pas).as_deref(), Some(dfm.as_path()));
        assert_eq!(sibling_pas(&dfm).as_deref(), Some(pas.as_path()));
        assert!(is_form_file(&dfm));
        // The designer opens the text form and saves back to the same `.dfm`.
        let form = Designer::open(&pas).expect("a text .dfm opens in the designer");
        assert_eq!(form.lfm_path, dfm);

        std::fs::write(&dfm, b"\xff\x0a\x00TPF0\x06TForm").unwrap();
        assert_eq!(sibling_lfm(&pas), None, "binary forms do not pair");
        assert!(!is_form_file(&dfm));
        assert_eq!(sibling_pas(&dfm), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pp_unit_pairs_with_its_form() {
        let d = std::env::temp_dir().join(format!("pascoda-pp-pair-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (pp, lfm) = (d.join("unit1.pp"), d.join("unit1.lfm"));
        std::fs::write(&pp, "unit unit1;\n").unwrap();
        std::fs::write(&lfm, "object Form1: TForm1\nend\n").unwrap();

        assert!(is_unit_file(&pp) && !is_unit_file(&lfm));
        assert_eq!(sibling_lfm(&pp).as_deref(), Some(lfm.as_path()));
        assert_eq!(sibling_pas(&lfm).as_deref(), Some(pp.as_path()));
        assert_eq!(form_unit(&lfm).as_deref(), Some(pp.as_path()));
        assert_eq!(form_unit(&pp).as_deref(), Some(pp.as_path()));

        // A `.pas` next to the form wins over a `.pp` (no pairing is lost either way).
        let pas = d.join("unit1.pas");
        std::fs::write(&pas, "unit unit1;\n").unwrap();
        assert_eq!(sibling_pas(&lfm).as_deref(), Some(pas.as_path()));
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod custom_tests {
    use super::*;

    /// A project with the gui fixture's form and a custom component in its own unit.
    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pascoda-custom-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["unit1.pas", "unit1.lfm"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/gui")
                    .join(f),
                dir.join(f),
            )
            .unwrap();
        }
        std::fs::write(
            dir.join("mylabel.pas"),
            "unit mylabel;\n\n{$mode objfpc}{$H+}\n\ninterface\n\nuses\n  Classes, Controls, StdCtrls;\n\ntype\n  TMyLabel = class(TLabel)\n  end;\n\nimplementation\n\nend.\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn the_scan_finds_custom_components_but_not_forms() {
        let dir = project("scan");
        let found = custom_classes(&dir);
        assert_eq!(found.get("TMyLabel"), Some(&"mylabel"));
        assert!(!found.contains_key("TForm1"), "a form is not a component");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_custom_component_is_placed_without_a_caption_and_synced_with_its_unit() {
        let dir = project("place");
        let mut d = Designer::open(&dir.join("unit1.pas")).unwrap();
        let mut custom = ClassUnits::new();
        custom.insert("TMyLabel", "mylabel");
        d.custom_units = Arc::new(custom);

        let path = d.add_component("TMyLabel", &[], 16, 16).unwrap();
        assert_eq!(d.doc.component(&path).unwrap().class, "TMyLabel");
        assert!(
            !d.doc.source().contains("Caption = 'MyLabel"),
            "the class may have no Caption: {}",
            d.doc.source()
        );

        let mut unit = std::fs::read_to_string(dir.join("unit1.pas")).unwrap();
        d.sync_unit(&mut unit);
        assert!(d.sync_error.is_none(), "{:?}", d.sync_error);
        assert!(unit.contains("mylabel"), "the unit is used:\n{unit}");
        assert!(unit.contains("RegisterClass(TMyLabel)"), "and registered:\n{unit}");
        let once = unit.clone();
        d.sync_unit(&mut unit);
        assert_eq!(unit, once, "syncing again changes nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
