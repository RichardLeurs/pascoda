//! Real AppKit controls inside the egui window, over the design canvas.
//!
//! Layout (all inside winit's flipped content view):
//!   winit view (egui draws into its Metal layer)
//!     `- CanvasClip  (flipped, click-through, clips to its bounds; frame = the visible canvas)
//!          |- DesignCanvas  (flipped, click-through, paints the window background; frame =
//!          |    |            the form's client area, offset by the scroll)
//!          |    `- one NSControl per component
//!          `- SelectionOverlay (click-through, draws the selection and 8 handles)
//!
//! The clip view keeps the native controls inside the canvas area when the form is larger
//! than it and scrolled: nothing draws over the palette, inspector, tray or other egui panels.
//!
//! Both our views return `nil` from `hitTest:`, so AppKit sends every mouse event to the winit
//! view and egui keeps all input; the native views only draw. See
//! `docs/design/native-embed-spike.md`.

use super::geometry::{Bounds, Guide};
use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezelStyle, NSBezierPath, NSBitmapImageFileType, NSBox, NSBoxType, NSButton, NSButtonType,
    NSColor, NSComboBox, NSControlSize, NSFont, NSPopUpButton, NSProgressIndicator,
    NSProgressIndicatorStyle, NSResponder, NSScroller, NSSlider, NSStepper, NSTabView, NSTextField,
    NSTitlePosition, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSObject, NSPoint, NSRect, NSSize, NSString,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;

/// One control to show: LCL class, text and bounds in form coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    pub class: String,
    pub caption: String,
    pub bounds: Bounds,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements we violate; no Drop impl.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct DesignCanvas;

    impl DesignCanvas {
        /// Top-left origin, like egui points and LCL Left/Top.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// Never takes the mouse: egui handles selection and dragging.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }

        /// The form background: the real, appearance-aware window colour.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::windowBackgroundColor().setFill();
            NSBezierPath::fillRect(self.bounds());
        }
    }
);

define_class!(
    // SAFETY: as above.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct CanvasClip;

    impl CanvasClip {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }
    }
);

impl CanvasClip {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        v.setClipsToBounds(true);
        v
    }
}

impl DesignCanvas {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

/// Handle size of the selection overlay, in points.
pub const HANDLE: f64 = 6.0;

/// What the selection overlay shows, in form coordinates. Also drawn by egui (see `ui`), which
/// is what offscreen renders and windows without the native layer show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    /// The primary selection: outline.
    pub primary: Option<Bounds>,
    /// The other selected controls: outline only.
    pub others: Vec<Bounds>,
    /// Where the 8 resize handles go: the primary, or the union box of a group of siblings.
    pub handles: Option<Bounds>,
    /// Alignment guides shown while a drag snaps to a neighbour.
    pub guides: Vec<Guide>,
    /// The rubber band being dragged.
    pub band: Option<Bounds>,
}

type OverlayIvars = RefCell<Marks>;

/// The overlay reaches this far past the canvas on every side, so handles on the form's edge
/// are drawn whole.
const PAD: f64 = HANDLE;

define_class!(
    // SAFETY: as above; the ivars are only touched on the main thread.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = OverlayIvars]
    struct SelectionOverlay;

    impl SelectionOverlay {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }

        /// Outlines for every selected control, handles on the primary, guides and the band.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let marks = self.ivars().borrow();
            let rect = |b: &Bounds| {
                NSRect::new(
                    NSPoint::new(b.left as f64 + PAD, b.top as f64 + PAD),
                    NSSize::new(b.width as f64, b.height as f64),
                )
            };
            let accent = NSColor::controlAccentColor();
            if let Some(b) = &marks.band {
                accent.colorWithAlphaComponent(0.12).setFill();
                NSBezierPath::fillRect(rect(b));
                accent.setStroke();
                NSBezierPath::strokeRect(rect(b));
            }
            accent.setStroke();
            for b in marks.others.iter().chain(&marks.primary) {
                let path = NSBezierPath::bezierPathWithRect(rect(b));
                path.setLineWidth(1.0);
                path.stroke();
            }
            if let Some(b) = &marks.handles {
                let sel = rect(b);
                if marks.primary.as_ref() != Some(b) {
                    // A group's union box: a faint frame that the handles belong to.
                    accent.colorWithAlphaComponent(0.45).setStroke();
                    NSBezierPath::strokeRect(sel);
                }
                let h = HANDLE;
                let (x0, y0) = (sel.origin.x, sel.origin.y);
                let (x1, y1) = (x0 + sel.size.width, y0 + sel.size.height);
                let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                for (x, y) in [(x0, y0), (xm, y0), (x1, y0), (x0, ym), (x1, ym), (x0, y1), (xm, y1), (x1, y1)] {
                    let r = NSRect::new(NSPoint::new(x - h / 2.0, y - h / 2.0), NSSize::new(h, h));
                    NSColor::whiteColor().setFill();
                    NSBezierPath::fillRect(r);
                    accent.setStroke();
                    NSBezierPath::strokeRect(r);
                }
            }
            accent.setStroke();
            for g in &marks.guides {
                let (at, from, to) = (g.at as f64 + 0.5, g.from as f64, g.to as f64);
                let (p0, p1) = if g.vertical {
                    (NSPoint::new(at + PAD, from + PAD), NSPoint::new(at + PAD, to + PAD))
                } else {
                    (NSPoint::new(from + PAD, at + PAD), NSPoint::new(to + PAD, at + PAD))
                };
                NSBezierPath::strokeLineFromPoint_toPoint(p0, p1);
            }
        }
    }
);

impl SelectionOverlay {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Marks::default()));
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    /// Covers the canvas at `canvas` (in the clip view) and shows `marks`; redraws only when
    /// they changed.
    fn show(&self, canvas: NSRect, marks: &Marks) {
        let frame = NSRect::new(
            NSPoint::new(canvas.origin.x - PAD, canvas.origin.y - PAD),
            NSSize::new(
                canvas.size.width + 2.0 * PAD,
                canvas.size.height + 2.0 * PAD,
            ),
        );
        if self.frame() != frame {
            self.setFrame(frame);
        }
        if *self.ivars().borrow() != *marks {
            *self.ivars().borrow_mut() = marks.clone();
            self.setNeedsDisplay(true);
        }
    }
}

enum Control {
    Button(Retained<NSButton>),
    Text(Retained<NSTextField>),
    Box(Retained<NSBox>),
    /// Drawn as is; the caption (if any) is not shown.
    Plain,
}

struct Item {
    spec: Spec,
    control: Control,
    view: Retained<NSView>,
    frame: NSRect,
}

pub struct Native {
    mtm: MainThreadMarker,
    clip: Retained<CanvasClip>,
    canvas: Retained<DesignCanvas>,
    overlay: Retained<SelectionOverlay>,
    items: Vec<Item>,
}

/// LCL-Cocoa puts a TButton's `NSButton` frame this far outside its bounds (left, top, right,
/// bottom), so the bezel is drawn around the bounds. Copied from `TCocoaWSButton` so the
/// designer and the running program agree. The values date from Mac OS X 10.6.
const BUTTON_GROW: (f64, f64, f64, f64) = (6.0, 5.0, 6.0, 7.0);

fn native_frame(class: &str, b: Bounds) -> NSRect {
    let (l, t, w, h) = (b.left as f64, b.top as f64, b.width as f64, b.height as f64);
    if ["tbutton", "tbitbtn", "ttogglebox"].contains(&class.to_ascii_lowercase().as_str()) {
        let (gl, gt, gr, gb) = BUTTON_GROW;
        return NSRect::new(
            NSPoint::new(l - gl, t - gt),
            NSSize::new(w + gl + gr, h + gt + gb),
        );
    }
    NSRect::new(NSPoint::new(l, t), NSSize::new(w, h))
}

fn make_control(mtm: MainThreadMarker, class: &str, caption: &str) -> (Control, Retained<NSView>) {
    let title = NSString::from_str(caption);
    match class.to_ascii_lowercase().as_str() {
        "tbutton" | "tbitbtn" | "ttogglebox" | "tspeedbutton" => {
            // Same recipe as LCL-Cocoa's TCocoaWSButtonUtil.allocButton.
            let b = NSButton::initWithFrame(NSButton::alloc(mtm), NSRect::ZERO);
            b.setBezelStyle(NSBezelStyle::Push);
            b.setButtonType(NSButtonType::MomentaryLight);
            b.setControlSize(NSControlSize::Regular);
            b.setFont(Some(&NSFont::systemFontOfSize(
                NSFont::systemFontSizeForControlSize(NSControlSize::Regular),
            )));
            b.setTitle(&title);
            let v = Retained::into_super(Retained::into_super(b.clone()));
            (Control::Button(b), v)
        }
        // LCL-Cocoa builds check boxes and radio buttons like push buttons (alloc/init, title,
        // button type) rather than with AppKit's convenience constructors; measurements show
        // the two differ in size, so follow LCL (see `TCocoaWSButtonUtil.allocButton`).
        "tcheckbox" | "tradiobutton" => {
            let b = NSButton::initWithFrame(NSButton::alloc(mtm), NSRect::ZERO);
            b.setTitle(&title);
            b.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            b.setButtonType(if class.eq_ignore_ascii_case("TCheckBox") {
                NSButtonType::Switch
            } else {
                NSButtonType::Radio
            });
            let v = Retained::into_super(Retained::into_super(b.clone()));
            (Control::Button(b), v)
        }
        "tedit" => {
            let t = NSTextField::textFieldWithString(&title, mtm);
            let v = Retained::into_super(Retained::into_super(t.clone()));
            (Control::Text(t), v)
        }
        "tlabel" => {
            let t = NSTextField::labelWithString(&title, mtm);
            let v = Retained::into_super(Retained::into_super(t.clone()));
            (Control::Text(t), v)
        }
        "tcombobox" | "tlabelededit" | "tmaskedit" | "tspinedit" => {
            let t = if class.eq_ignore_ascii_case("TComboBox") {
                Retained::into_super(NSComboBox::initWithFrame(
                    NSComboBox::alloc(mtm),
                    NSRect::ZERO,
                ))
            } else {
                NSTextField::textFieldWithString(&title, mtm)
            };
            if class.eq_ignore_ascii_case("TSpinEdit") {
                t.setStringValue(&NSString::from_str("0"));
            } else {
                t.setStringValue(&title);
            }
            let v = Retained::into_super(Retained::into_super(t.clone()));
            (Control::Text(t), v)
        }
        "tstatictext" => {
            let t = NSTextField::labelWithString(&title, mtm);
            let v = Retained::into_super(Retained::into_super(t.clone()));
            (Control::Text(t), v)
        }
        // A titled frame, as Mac apps draw group boxes.
        "tgroupbox" | "tradiogroup" | "tcheckgroup" => {
            let b = NSBox::new(mtm);
            b.setTitle(&title);
            let v = Retained::into_super(b.clone());
            (Control::Box(b), v)
        }
        // Surfaces: panels, text areas, lists, images and the like are an outlined plain box.
        "tpanel" | "tmemo" | "tlistbox" | "tlistview" | "ttreeview" | "timage" | "tshape"
        | "tbevel" | "tsplitter" | "ttoolbar" | "tstatusbar" => {
            let b = NSBox::new(mtm);
            b.setBoxType(NSBoxType::Custom);
            b.setTitlePosition(NSTitlePosition::NoTitle);
            b.setBorderWidth(1.0);
            b.setCornerRadius(match class.to_ascii_lowercase().as_str() {
                "tpanel" | "tshape" => 0.0,
                _ => 3.0,
            });
            b.setBorderColor(&NSColor::separatorColor());
            b.setFillColor(&*match class.to_ascii_lowercase().as_str() {
                "tmemo" | "tlistbox" | "tlistview" | "ttreeview" => NSColor::textBackgroundColor(),
                "ttoolbar" | "tstatusbar" | "tsplitter" => NSColor::windowBackgroundColor(),
                _ => NSColor::clearColor(),
            });
            let v = Retained::into_super(b.clone());
            (Control::Box(b), v)
        }
        "tprogressbar" => {
            let p = NSProgressIndicator::new(mtm);
            p.setStyle(NSProgressIndicatorStyle::Bar);
            p.setIndeterminate(false);
            p.setDoubleValue(40.0);
            (Control::Plain, Retained::into_super(p))
        }
        "ttrackbar" => {
            let sl = NSSlider::new(mtm);
            sl.setDoubleValue(50.0);
            (
                Control::Plain,
                Retained::into_super(Retained::into_super(sl)),
            )
        }
        "tscrollbar" => {
            let sc = NSScroller::initWithFrame(
                NSScroller::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 17.0)),
            );
            (
                Control::Plain,
                Retained::into_super(Retained::into_super(sc)),
            )
        }
        "tupdown" => {
            let st = NSStepper::new(mtm);
            (
                Control::Plain,
                Retained::into_super(Retained::into_super(st)),
            )
        }
        "tpagecontrol" => {
            let tv = NSTabView::new(mtm);
            (Control::Plain, Retained::into_super(tv))
        }
        "tcolorbox" => {
            let pb = NSPopUpButton::new(mtm);
            (
                Control::Plain,
                Retained::into_super(Retained::into_super(Retained::into_super(pb))),
            )
        }
        _ => {
            // No native counterpart yet: show the class name so nothing is silently missing.
            let t = NSTextField::labelWithString(&NSString::from_str(&format!("[{class}]")), mtm);
            t.setTextColor(Some(&NSColor::secondaryLabelColor()));
            let v = Retained::into_super(Retained::into_super(t.clone()));
            (Control::Text(t), v)
        }
    }
}

fn snapshot_view(view: &NSView, b: NSRect, path: &std::path::Path) -> Result<(), String> {
    let rep = view
        .bitmapImageRepForCachingDisplayInRect(b)
        .ok_or("no bitmap")?;
    view.cacheDisplayInRect_toBitmapImageRep(b, &rep);
    let props = NSDictionary::<NSString, AnyObject>::new();
    let data =
        unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }
            .ok_or("PNG encoding failed")?;
    std::fs::write(path, data.to_vec()).map_err(|e| e.to_string())
}

fn set_caption(control: &Control, caption: &str) {
    let s = NSString::from_str(caption);
    match control {
        Control::Button(b) => b.setTitle(&s),
        Control::Text(t) => t.setStringValue(&s),
        Control::Box(b) => {
            if b.titlePosition() != NSTitlePosition::NoTitle {
                b.setTitle(&s)
            }
        }
        Control::Plain => {}
    }
}

impl Native {
    /// Adds the canvas to the eframe window's content view.
    pub fn attach(frame: &eframe::Frame) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
        let handle = frame.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return Err("not an AppKit window".into());
        };
        // SAFETY: winit guarantees `ns_view` is a live NSView for the window's lifetime.
        let winit_view: Retained<NSView> =
            unsafe { Retained::retain(h.ns_view.as_ptr().cast()) }.ok_or("null view")?;
        let clip = CanvasClip::new(mtm);
        clip.setHidden(true);
        winit_view.addSubview(&clip);
        let canvas = DesignCanvas::new(mtm);
        clip.addSubview(&canvas);
        let overlay = SelectionOverlay::new(mtm);
        overlay.setHidden(true);
        clip.addSubview(&overlay); // after the canvas: drawn above the controls
        Ok(Self {
            mtm,
            clip,
            canvas,
            overlay,
            items: Vec::new(),
        })
    }

    pub fn hide(&self) {
        self.clip.setHidden(true);
    }

    /// Shows `specs` on a form canvas whose top-left corner is at `origin` in egui points,
    /// visible only inside `area` (the canvas part of the window, scroll bars excluded).
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        area: egui::Rect,
        origin: egui::Pos2,
        size: (i64, i64),
        specs: &[Spec],
        marks: &Marks,
    ) {
        let z = ctx.zoom_factor() as f64; // egui point -> native point
        if area.width() < 1.0 || area.height() < 1.0 {
            self.hide();
            return;
        }
        let clip_frame = NSRect::new(
            NSPoint::new(area.left() as f64 * z, area.top() as f64 * z),
            NSSize::new(area.width() as f64 * z, area.height() as f64 * z),
        );
        if self.clip.frame() != clip_frame {
            self.clip.setFrame(clip_frame);
        }
        // In the clip view's coordinates: the scroll moves the canvas, the clip view stays.
        let canvas_frame = NSRect::new(
            NSPoint::new(
                (origin.x - area.left()) as f64 * z,
                (origin.y - area.top()) as f64 * z,
            ),
            NSSize::new(size.0 as f64 * z, size.1 as f64 * z),
        );
        if self.canvas.frame() != canvas_frame {
            self.canvas.setFrame(canvas_frame);
        }
        self.clip.setHidden(false);

        let same_shape = self.items.len() == specs.len()
            && self
                .items
                .iter()
                .zip(specs)
                .all(|(i, s)| i.spec.name == s.name && i.spec.class == s.class);
        if !same_shape {
            for item in self.items.drain(..) {
                item.view.removeFromSuperview();
            }
            for s in specs {
                let (control, view) = make_control(self.mtm, &s.class, &s.caption);
                self.canvas.addSubview(&view);
                self.items.push(Item {
                    spec: s.clone(),
                    control,
                    view,
                    frame: NSRect::ZERO,
                });
            }
            for item in &mut self.items {
                item.spec.caption.clear();
                item.spec.bounds = Bounds {
                    left: i64::MIN,
                    top: 0,
                    width: 0,
                    height: 0,
                };
            }
        }
        for (item, s) in self.items.iter_mut().zip(specs) {
            if item.spec.caption != s.caption {
                set_caption(&item.control, &s.caption);
                item.spec.caption = s.caption.clone();
            }
            if item.spec.bounds != s.bounds {
                item.frame = native_frame(&s.class, s.bounds);
                item.view.setFrame(item.frame);
                item.spec.bounds = s.bounds;
            }
        }

        self.overlay.setHidden(*marks == Marks::default());
        self.overlay.show(canvas_frame, marks);
    }

    /// Renders the visible canvas (the clip view: controls and selection overlay, clipped and
    /// scrolled as on screen) through AppKit's own drawing path and saves it as a PNG.
    /// In-process, so it needs no screen-recording permission; it shows the native views only,
    /// not egui's pixels (transparent where no native view draws).
    pub fn snapshot(&self, path: &std::path::Path) -> Result<(), String> {
        snapshot_view(&self.clip, self.clip.bounds(), path)
    }

    /// Renders the whole window content view (egui's Metal layer comes out empty, so this shows
    /// where the native views draw within the window: only inside the canvas area).
    pub fn snapshot_window(&self, path: &std::path::Path) -> Result<(), String> {
        let root = unsafe { self.clip.superview() }.ok_or("not in a window")?;
        snapshot_view(&root, root.bounds(), path)
    }

    /// The visible canvas area in the window, in native points (for checking the clip).
    pub fn clip_frame(&self) -> (f64, f64, f64, f64) {
        let f = self.clip.frame();
        (f.origin.x, f.origin.y, f.size.width, f.size.height)
    }

    /// The size AppKit says a control of `class` with this text wants, before any LCL
    /// adjustment (`fittingSize`).
    pub fn fitting_size(&self, class: &str, caption: &str) -> (f64, f64) {
        let (control, view) = make_control(self.mtm, class, caption);
        // LCL asks check boxes and radio buttons for `sizeToFit()`, the rest for `fittingSize`.
        if let (Control::Button(b), "tcheckbox" | "tradiobutton") =
            (&control, class.to_ascii_lowercase().as_str())
        {
            b.sizeToFit();
            let s = b.frame().size;
            return (s.width, s.height);
        }
        let s = view.fittingSize();
        (s.width, s.height)
    }

    /// The size LCL gives an auto-sized control of `class` with this text, as measured on the
    /// real LCL (see `crates/pascoda-forms/tests/autosize_truth.rs`): AppKit's own preferred
    /// size plus the fixed offsets LCL's widgetset adds. Fitted on three texts per class;
    /// labels can be one point off on long text. `None` for classes that cannot auto-size.
    pub fn preferred_size(&self, class: &str, caption: &str) -> Option<(i64, i64)> {
        let (fw, fh) = self.fitting_size(class, caption);
        let (w, h) = match class.to_ascii_lowercase().as_str() {
            "tbutton" => (fw - 2.0, fh + 2.0),
            "tlabel" => (fw - 5.0, fh),
            // These two draw their text one point smaller than the control's default.
            "tcheckbox" | "tradiobutton" => ((fw + 4.5).floor(), fh + 2.0),
            "tedit" => (fw, fh - 4.0), // only the height is applied
            _ => return None,
        };
        Some((w.round() as i64, h.round() as i64))
    }

    pub fn control_count(&self) -> usize {
        self.items.len()
    }
}
