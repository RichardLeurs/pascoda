//! Component geometry in LCL coordinates (points, top-left origin): reading bounds from a form,
//! finding what is under the pointer, and dragging or resizing with grid snap. Pure logic.

use pascoda_forms::{Component, PropValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
}

impl Bounds {
    pub fn right(&self) -> i64 {
        self.left + self.width
    }

    pub fn bottom(&self) -> i64 {
        self.top + self.height
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.left as f64
            && x < self.right() as f64
            && y >= self.top as f64
            && y < self.bottom() as f64
    }

    /// Whether the two overlap or touch (a rubber band touching a control selects it).
    pub fn intersects(&self, o: &Bounds) -> bool {
        self.left <= o.right()
            && o.left <= self.right()
            && self.top <= o.bottom()
            && o.top <= self.bottom()
    }

    /// The rectangle spanned by two points, in any order.
    pub fn spanning(a: (i64, i64), b: (i64, i64)) -> Bounds {
        Bounds {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            width: (a.0 - b.0).abs(),
            height: (a.1 - b.1).abs(),
        }
    }

    pub fn centre_x(&self) -> i64 {
        self.left + self.width / 2
    }

    pub fn centre_y(&self) -> i64 {
        self.top + self.height / 2
    }

    pub fn offset(&self, dx: i64, dy: i64) -> Bounds {
        Bounds {
            left: self.left + dx,
            top: self.top + dy,
            ..*self
        }
    }
}

/// What LCL gives a new component of this class when no size is stored.
pub fn default_size(class: &str) -> (i64, i64) {
    match class.to_ascii_lowercase().as_str() {
        "tbutton" => (75, 25),
        "tlabel" => (36, 17),
        "tedit" => (80, 23),
        "tcheckbox" | "tradiobutton" => (90, 20),
        "tpanel" | "tgroupbox" | "tradiogroup" | "tcheckgroup" => (185, 105),
        "tmemo" => (185, 89),
        "tcombobox" | "tlabelededit" | "tmaskedit" => (100, 23),
        "tlistbox" => (100, 50),
        "ttogglebox" | "tbitbtn" => (75, 25),
        "tspeedbutton" => (23, 22),
        "tscrollbar" => (100, 17),
        "tstatictext" => (100, 17),
        "tspinedit" => (50, 23),
        "tcolorbox" => (145, 22),
        "timage" => (105, 105),
        "tshape" => (65, 65),
        "tbevel" => (50, 50),
        "tsplitter" => (5, 50),
        "tprogressbar" => (100, 20),
        "ttrackbar" => (100, 25),
        "tupdown" => (17, 22),
        "tpagecontrol" => (200, 150),
        "tlistview" => (250, 150),
        "ttreeview" => (100, 100),
        "ttoolbar" => (100, 26),
        "tstatusbar" => (100, 23),
        _ => (80, 25),
    }
}

fn int(c: &Component, name: &str) -> Option<i64> {
    c.prop(name).and_then(PropValue::as_int)
}

/// The component's bounds relative to its parent's client area.
pub fn bounds(c: &Component) -> Bounds {
    let (dw, dh) = default_size(&c.class);
    Bounds {
        left: int(c, "Left").unwrap_or(0),
        top: int(c, "Top").unwrap_or(0),
        width: int(c, "Width").unwrap_or(dw),
        height: int(c, "Height").unwrap_or(dh),
    }
}

/// The form's client size. `ClientWidth/ClientHeight` win when present.
pub fn form_size(form: &Component) -> (i64, i64) {
    (
        int(form, "ClientWidth")
            .or(int(form, "Width"))
            .unwrap_or(320),
        int(form, "ClientHeight")
            .or(int(form, "Height"))
            .unwrap_or(240),
    )
}

/// Bounds of the component at `path` in form coordinates (parents' offsets added). `None` for
/// a non-visual component (or one inside it): it has no place on the form.
pub fn absolute(form: &Component, path: &[usize]) -> Option<Bounds> {
    let (mut c, mut dx, mut dy) = (form, 0, 0);
    let mut b = Bounds {
        left: 0,
        top: 0,
        width: 0,
        height: 0,
    };
    for (n, &i) in path.iter().enumerate() {
        c = c.children.get(i)?;
        if super::is_nonvisual(&c.class) {
            return None;
        }
        b = bounds(c);
        if n + 1 < path.len() {
            dx += b.left;
            dy += b.top;
        }
    }
    Some(if path.is_empty() { b } else { b.offset(dx, dy) })
}

/// The deepest, topmost component under `(x, y)` (form coordinates). Later siblings are
/// drawn on top, so they win. `None` when only the form itself is there. Non-visual
/// components are never hit; they live in the tray.
pub fn hit_test(form: &Component, x: f64, y: f64) -> Option<Vec<usize>> {
    fn walk(c: &Component, x: f64, y: f64, path: &mut Vec<usize>) -> bool {
        for (i, child) in c.children.iter().enumerate().rev() {
            let b = bounds(child);
            if b.contains(x, y) && !super::is_nonvisual(&child.class) {
                path.push(i);
                if !walk(child, x - b.left as f64, y - b.top as f64, path) {
                    // Hit this child, none of its own children.
                }
                return true;
            }
        }
        false
    }
    let mut path = Vec::new();
    walk(form, x, y, &mut path).then_some(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// Which of the 8 resize handles of `b` is at `(x, y)`, with `slop` points of tolerance.
pub fn handle_at(b: Bounds, x: f64, y: f64, slop: f64) -> Option<Handle> {
    let (l, t, r, bo) = (
        b.left as f64,
        b.top as f64,
        b.right() as f64,
        b.bottom() as f64,
    );
    let (xm, ym) = ((l + r) / 2.0, (t + bo) / 2.0);
    let near = |px: f64, py: f64| (x - px).abs() <= slop && (y - py).abs() <= slop;
    [
        (Handle::TopLeft, l, t),
        (Handle::Top, xm, t),
        (Handle::TopRight, r, t),
        (Handle::Left, l, ym),
        (Handle::Right, r, ym),
        (Handle::BottomLeft, l, bo),
        (Handle::Bottom, xm, bo),
        (Handle::BottomRight, r, bo),
    ]
    .into_iter()
    .find(|&(_, px, py)| near(px, py))
    .map(|(h, ..)| h)
}

/// Default designer grid, in points.
pub const GRID: i64 = 8;

/// An alignment guide: a line at `at` (x when `vertical`, else y) from `from` to `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guide {
    pub vertical: bool,
    pub at: i64,
    pub from: i64,
    pub to: i64,
}

impl Guide {
    pub fn offset(self, dx: i64, dy: i64) -> Guide {
        let (along, across) = if self.vertical { (dy, dx) } else { (dx, dy) };
        Guide {
            at: self.at + across,
            from: self.from + along,
            to: self.to + along,
            ..self
        }
    }
}

pub fn snap(v: i64, grid: i64) -> i64 {
    if grid <= 1 {
        v
    } else {
        ((v as f64 / grid as f64).round() as i64) * grid
    }
}

/// Drags `handle` by `(dx, dy)`; the moved vertical edges go where `fx` puts them, the
/// horizontal ones where `fy` does, and the size never drops below 1.
fn resized_with(
    b: Bounds,
    handle: Handle,
    dx: i64,
    dy: i64,
    fx: impl Fn(i64) -> i64,
    fy: impl Fn(i64) -> i64,
) -> Bounds {
    let (mut l, mut t, mut r, mut bo) = (b.left, b.top, b.right(), b.bottom());
    let (west, east) = (
        matches!(handle, Handle::TopLeft | Handle::Left | Handle::BottomLeft),
        matches!(
            handle,
            Handle::TopRight | Handle::Right | Handle::BottomRight
        ),
    );
    let (north, south) = (
        matches!(handle, Handle::TopLeft | Handle::Top | Handle::TopRight),
        matches!(
            handle,
            Handle::BottomLeft | Handle::Bottom | Handle::BottomRight
        ),
    );
    if west {
        l = fx(l + dx).min(r - 1);
    }
    if east {
        r = fx(r + dx).max(l + 1);
    }
    if north {
        t = fy(t + dy).min(bo - 1);
    }
    if south {
        bo = fy(bo + dy).max(t + 1);
    }
    Bounds {
        left: l,
        top: t,
        width: r - l,
        height: bo - t,
    }
}

// ---- alignment guides ----------------------------------------------------------------------

/// How close (in points) an edge or centre must come to a neighbour's to snap to it.
pub const SNAP: i64 = 4;

fn lines_x(b: &Bounds) -> [i64; 3] {
    [b.left, b.centre_x(), b.right()]
}

fn lines_y(b: &Bounds) -> [i64; 3] {
    [b.top, b.centre_y(), b.bottom()]
}

/// The smallest shift, at most `tol` either way, that puts one of `mine` onto one of `theirs`.
fn nearest_shift(mine: &[i64], theirs: &[i64], tol: i64) -> Option<i64> {
    let mut best: Option<i64> = None;
    for m in mine {
        for t in theirs {
            let d = t - m;
            if d.abs() <= tol && best.is_none_or(|b| d.abs() < b.abs()) {
                best = Some(d);
            }
        }
    }
    best
}

/// Moves `start` by `(dx, dy)`. On each axis, when one of its edges or its centre comes within
/// `tol` of a neighbour's edge or centre, it snaps there; otherwise its origin snaps to the
/// grid. Returns the new bounds and the guides to draw (all lines it now shares).
pub fn guided_move(
    start: Bounds,
    dx: i64,
    dy: i64,
    grid: i64,
    others: &[Bounds],
    tol: i64,
) -> (Bounds, Vec<Guide>) {
    let raw = start.offset(dx, dy);
    let xs: Vec<i64> = others.iter().flat_map(lines_x).collect();
    let ys: Vec<i64> = others.iter().flat_map(lines_y).collect();
    let left = nearest_shift(&lines_x(&raw), &xs, tol)
        .map_or_else(|| snap(raw.left, grid), |d| raw.left + d);
    let top = nearest_shift(&lines_y(&raw), &ys, tol)
        .map_or_else(|| snap(raw.top, grid), |d| raw.top + d);
    let b = Bounds { left, top, ..start };
    (b, guides(&b, others))
}

/// Drags `handle` by `(dx, dy)`: a moved edge within `tol` of a neighbour's edge or
/// centre snaps to it instead of to the grid.
pub fn guided_resize(
    start: Bounds,
    handle: Handle,
    (dx, dy): (i64, i64),
    grid: i64,
    others: &[Bounds],
    tol: i64,
) -> (Bounds, Vec<Guide>) {
    let xs: Vec<i64> = others.iter().flat_map(lines_x).collect();
    let ys: Vec<i64> = others.iter().flat_map(lines_y).collect();
    let fx = |v: i64| nearest_shift(&[v], &xs, tol).map_or_else(|| snap(v, grid), |d| v + d);
    let fy = |v: i64| nearest_shift(&[v], &ys, tol).map_or_else(|| snap(v, grid), |d| v + d);
    let b = resized_with(start, handle, dx, dy, fx, fy);
    (b, guides(&b, others))
}

/// Guide lines for every edge or centre `b` shares with a neighbour, each spanning both. Lines
/// at the same place are merged into one.
pub fn guides(b: &Bounds, others: &[Bounds]) -> Vec<Guide> {
    let mut out: Vec<Guide> = Vec::new();
    let mut add = |g: Guide| match out
        .iter_mut()
        .find(|o| o.vertical == g.vertical && o.at == g.at)
    {
        Some(o) => {
            o.from = o.from.min(g.from);
            o.to = o.to.max(g.to);
        }
        None => out.push(g),
    };
    for o in others {
        for x in lines_x(b).into_iter().filter(|x| lines_x(o).contains(x)) {
            add(Guide {
                vertical: true,
                at: x,
                from: b.top.min(o.top),
                to: b.bottom().max(o.bottom()),
            });
        }
        for y in lines_y(b).into_iter().filter(|y| lines_y(o).contains(y)) {
            add(Guide {
                vertical: false,
                at: y,
                from: b.left.min(o.left),
                to: b.right().max(o.right()),
            });
        }
    }
    out
}

// ---- align and space -----------------------------------------------------------------------

/// The designer's align/size/space commands for a multi-selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrange {
    Lefts,
    /// Centres on one vertical line.
    CentresX,
    Rights,
    Tops,
    /// Centres on one horizontal line.
    CentresY,
    Bottoms,
    SameWidth,
    SameHeight,
    /// Equal gaps between neighbours, left to right; the outermost two stay put.
    SpaceX,
    SpaceY,
}

impl Arrange {
    pub const ALIGN: [Arrange; 6] = [
        Arrange::Lefts,
        Arrange::CentresX,
        Arrange::Rights,
        Arrange::Tops,
        Arrange::CentresY,
        Arrange::Bottoms,
    ];
    pub const SIZE_SPACE: [Arrange; 4] = [
        Arrange::SameWidth,
        Arrange::SameHeight,
        Arrange::SpaceX,
        Arrange::SpaceY,
    ];

    /// Short label for the segmented row, and the tooltip.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Arrange::Lefts => ("Left", "Align left edges"),
            Arrange::CentresX => ("Centre", "Align horizontal centres"),
            Arrange::Rights => ("Right", "Align right edges"),
            Arrange::Tops => ("Top", "Align top edges"),
            Arrange::CentresY => ("Middle", "Align vertical centres"),
            Arrange::Bottoms => ("Bottom", "Align bottom edges"),
            Arrange::SameWidth => ("Width", "Make same width"),
            Arrange::SameHeight => ("Height", "Make same height"),
            Arrange::SpaceX => ("Space H", "Space equally, horizontally"),
            Arrange::SpaceY => ("Space V", "Space equally, vertically"),
        }
    }
}

/// Applies `op` to `items`. Aligning and sizing follow `items[reference]` (the primary
/// selection, like Delphi's "first selected"); spacing keeps the first and last in place and
/// spreads the rest so the gaps are equal (to within a point).
pub fn arrange(items: &[Bounds], reference: usize, op: Arrange) -> Vec<Bounds> {
    let Some(r) = items.get(reference).copied() else {
        return items.to_vec();
    };
    let each = |f: &dyn Fn(Bounds) -> Bounds| items.iter().map(|b| f(*b)).collect();
    match op {
        Arrange::Lefts => each(&|b| Bounds { left: r.left, ..b }),
        Arrange::CentresX => each(&|b| Bounds {
            left: r.left + (r.width - b.width) / 2,
            ..b
        }),
        Arrange::Rights => each(&|b| Bounds {
            left: r.right() - b.width,
            ..b
        }),
        Arrange::Tops => each(&|b| Bounds { top: r.top, ..b }),
        Arrange::CentresY => each(&|b| Bounds {
            top: r.top + (r.height - b.height) / 2,
            ..b
        }),
        Arrange::Bottoms => each(&|b| Bounds {
            top: r.bottom() - b.height,
            ..b
        }),
        Arrange::SameWidth => each(&|b| Bounds {
            width: r.width,
            ..b
        }),
        Arrange::SameHeight => each(&|b| Bounds {
            height: r.height,
            ..b
        }),
        Arrange::SpaceX => space(items, |b| (b.left, b.width), |b, v| Bounds { left: v, ..b }),
        Arrange::SpaceY => space(items, |b| (b.top, b.height), |b, v| Bounds { top: v, ..b }),
    }
}

/// Equal gaps along one axis: `get` gives (position, extent), `set` a new position.
fn space(
    items: &[Bounds],
    get: impl Fn(&Bounds) -> (i64, i64),
    set: impl Fn(Bounds, i64) -> Bounds,
) -> Vec<Bounds> {
    let mut out = items.to_vec();
    if items.len() < 3 {
        return out;
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| (get(&items[i]).0, i));
    let (first, last) = (get(&items[order[0]]), get(&items[*order.last().unwrap()]));
    let start = first.0;
    let end = last.0 + last.1;
    let total: i64 = items.iter().map(|b| get(b).1).sum();
    let free = end - start - total;
    let gaps = items.len() as i64 - 1;
    let mut pos = start;
    for (k, &i) in order.iter().enumerate() {
        out[i] = set(items[i], pos);
        // Gap k gets its share rounded so the shares add up to `free` exactly.
        let k = k as i64;
        pos += get(&items[i]).1 + (free * (k + 1)).div_euclid(gaps) - (free * k).div_euclid(gaps);
    }
    out
}

/// The smallest rectangle around all of `items`.
pub fn union(items: &[Bounds]) -> Option<Bounds> {
    let first = items.first()?;
    let (mut l, mut t, mut r, mut b) = (first.left, first.top, first.right(), first.bottom());
    for i in items {
        l = l.min(i.left);
        t = t.min(i.top);
        r = r.max(i.right());
        b = b.max(i.bottom());
    }
    Some(Bounds::spanning((l, t), (r, b)))
}

/// `items` (inside the box `from`) scaled so that the box becomes `to`: each edge keeps its
/// relative position in the box, so the controls grow, shrink and spread together. Sizes never
/// drop below 1.
pub fn scale_into(items: &[Bounds], from: Bounds, to: Bounds) -> Vec<Bounds> {
    let map = |v: i64, start: i64, len: i64, new_start: i64, new_len: i64| {
        if len <= 0 {
            new_start + (v - start)
        } else {
            new_start + ((v - start) as f64 * new_len as f64 / len as f64).round() as i64
        }
    };
    items
        .iter()
        .map(|b| {
            let l = map(b.left, from.left, from.width, to.left, to.width);
            let r = map(b.right(), from.left, from.width, to.left, to.width);
            let t = map(b.top, from.top, from.height, to.top, to.height);
            let bo = map(b.bottom(), from.top, from.height, to.top, to.height);
            Bounds {
                left: l,
                top: t,
                width: (r - l).max(1),
                height: (bo - t).max(1),
            }
        })
        .collect()
}

// ---- anchors, alignment, layout ----------------------------------------------------------

/// `Anchors = [akLeft, akTop, ...]`: which parent edges the control keeps its distance from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchors {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl Anchors {
    /// What LCL assumes when the property is absent.
    pub const DEFAULT: Anchors = Anchors {
        left: true,
        top: true,
        right: false,
        bottom: false,
    };

    pub fn from_prop(v: Option<&PropValue>) -> Anchors {
        let Some(PropValue::Set(members)) = v else {
            return Anchors::DEFAULT;
        };
        let has = |name: &str| members.iter().any(|m| m.eq_ignore_ascii_case(name));
        Anchors {
            left: has("akLeft"),
            top: has("akTop"),
            right: has("akRight"),
            bottom: has("akBottom"),
        }
    }

    pub fn to_prop(self) -> PropValue {
        let members = [
            (self.top, "akTop"),
            (self.left, "akLeft"),
            (self.right, "akRight"),
            (self.bottom, "akBottom"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n.to_string())
        .collect();
        PropValue::Set(members)
    }
}

pub fn anchors_of(c: &Component) -> Anchors {
    Anchors::from_prop(c.prop("Anchors"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Top,
    Bottom,
    Left,
    Right,
    Client,
}

impl Align {
    pub const ALL: [Align; 6] = [
        Align::None,
        Align::Top,
        Align::Bottom,
        Align::Left,
        Align::Right,
        Align::Client,
    ];

    pub fn ident(self) -> &'static str {
        match self {
            Align::None => "alNone",
            Align::Top => "alTop",
            Align::Bottom => "alBottom",
            Align::Left => "alLeft",
            Align::Right => "alRight",
            Align::Client => "alClient",
        }
    }

    pub fn from_ident(s: &str) -> Align {
        Align::ALL
            .into_iter()
            .find(|a| a.ident().eq_ignore_ascii_case(s))
            .unwrap_or(Align::None)
    }
}

pub fn align_of(c: &Component) -> Align {
    c.prop("Align")
        .and_then(PropValue::as_ident)
        .map_or(Align::None, Align::from_ident)
}

/// LCL's `TLabel` sizes itself to its text unless told not to; other controls only on request.
pub fn auto_size_of(c: &Component) -> bool {
    c.prop("AutoSize")
        .and_then(PropValue::as_bool)
        .unwrap_or_else(|| c.class.eq_ignore_ascii_case("TLabel"))
}

/// Which reference line of another control (or the parent) an edge is anchored to. LCL's
/// `TAnchorSideReference`: `asrTop` (= `asrLeft`, the default), `asrBottom` (= `asrRight`),
/// `asrCenter`. Only the `asrTop/asrBottom/asrCenter` names are valid in a form file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SideRef {
    /// The reference's left or top edge.
    #[default]
    Start,
    Centre,
    /// The reference's right or bottom edge.
    End,
}

impl SideRef {
    pub const ALL: [SideRef; 3] = [SideRef::Start, SideRef::Centre, SideRef::End];

    pub fn ident(self) -> &'static str {
        match self {
            SideRef::Start => "asrTop",
            SideRef::Centre => "asrCenter",
            SideRef::End => "asrBottom",
        }
    }

    pub fn from_ident(s: &str) -> SideRef {
        match s.to_ascii_lowercase().as_str() {
            "asrcenter" => SideRef::Centre,
            "asrbottom" | "asrright" => SideRef::End,
            _ => SideRef::Start,
        }
    }

    /// For the inspector: "Left"/"Centre"/"Right" on a horizontal edge, "Top"/.../"Bottom".
    pub fn label(self, horizontal: bool) -> &'static str {
        match (self, horizontal) {
            (SideRef::Start, true) => "Left",
            (SideRef::Start, false) => "Top",
            (SideRef::Centre, _) => "Centre",
            (SideRef::End, true) => "Right",
            (SideRef::End, false) => "Bottom",
        }
    }
}

/// A control edge, as in `akLeft` and `AnchorSideLeft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom];

    pub fn name(self) -> &'static str {
        match self {
            Edge::Left => "Left",
            Edge::Top => "Top",
            Edge::Right => "Right",
            Edge::Bottom => "Bottom",
        }
    }

    pub fn horizontal(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }

    fn index(self) -> usize {
        self as usize
    }

    /// `AnchorSideLeft.Control`, `AnchorSideLeft.Side`.
    pub fn control_prop(self) -> String {
        format!("AnchorSide{}.Control", self.name())
    }

    pub fn side_prop(self) -> String {
        format!("AnchorSide{}.Side", self.name())
    }

    pub fn anchored(self, a: Anchors) -> bool {
        match self {
            Edge::Left => a.left,
            Edge::Top => a.top,
            Edge::Right => a.right,
            Edge::Bottom => a.bottom,
        }
    }

    pub fn set(self, a: &mut Anchors, on: bool) {
        match self {
            Edge::Left => a.left = on,
            Edge::Top => a.top = on,
            Edge::Right => a.right = on,
            Edge::Bottom => a.bottom = on,
        }
    }
}

/// `AnchorSideLeft.Control = Button1` / `AnchorSideLeft.Side = asrBottom`: the edge follows a
/// line of a sibling (or of the parent: its name, or `Owner` for the form).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnchorSide {
    pub control: String,
    pub side: SideRef,
}

pub fn anchor_side(c: &Component, edge: Edge) -> Option<AnchorSide> {
    let control = c.prop(&edge.control_prop())?.as_ident()?.to_string();
    let side = c
        .prop(&edge.side_prop())
        .and_then(PropValue::as_ident)
        .map_or(SideRef::Start, SideRef::from_ident);
    Some(AnchorSide { control, side })
}

/// `BorderSpacing.Left/Top/Right/Bottom/Around`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spacing {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
    pub around: i64,
}

impl Spacing {
    pub const FIELDS: [&'static str; 5] = ["Left", "Top", "Right", "Bottom", "Around"];

    pub fn of(c: &Component) -> Spacing {
        let v = |f: &str| int(c, &format!("BorderSpacing.{f}")).unwrap_or(0);
        Spacing {
            left: v("Left"),
            top: v("Top"),
            right: v("Right"),
            bottom: v("Bottom"),
            around: v("Around"),
        }
    }

    pub fn get(&self, field: &str) -> i64 {
        match field {
            "Left" => self.left,
            "Top" => self.top,
            "Right" => self.right,
            "Bottom" => self.bottom,
            _ => self.around,
        }
    }

    /// LCL's `GetSideSpace`: the side's own spacing plus `Around`.
    pub fn side(&self, edge: Edge) -> i64 {
        self.around
            + match edge {
                Edge::Left => self.left,
                Edge::Top => self.top,
                Edge::Right => self.right,
                Edge::Bottom => self.bottom,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub bounds: Bounds,
    pub anchors: Anchors,
    pub align: Align,
    /// The component's name, for anchor sides that refer to it.
    pub name: String,
    /// Per [`Edge`]: the anchor side, if the file sets one.
    pub sides: [Option<AnchorSide>; 4],
    pub spacing: Spacing,
}

impl Item {
    pub fn of(c: &Component) -> Item {
        Item {
            bounds: bounds(c),
            anchors: anchors_of(c),
            align: align_of(c),
            name: c.name.clone(),
            sides: Edge::ALL.map(|e| anchor_side(c, e)),
            spacing: Spacing::of(c),
        }
    }

    #[cfg(test)]
    pub fn plain(bounds: Bounds, anchors: Anchors, align: Align) -> Item {
        Item {
            bounds,
            anchors,
            align,
            name: String::new(),
            sides: Default::default(),
            spacing: Spacing::default(),
        }
    }
}

/// Where each child goes when its parent's client area changes from `old` to `new` size.
///
/// Docked children (`Align` other than none) are laid out in passes top, bottom, left, right,
/// client, each taking its strip from what the earlier ones left. The others follow their
/// anchors: an anchored edge keeps its distance from the parent's edge, so anchoring both
/// sides stretches the control, and anchoring neither keeps its centre proportional.
#[cfg(test)]
pub fn layout(old: (i64, i64), new: (i64, i64), items: &[Item]) -> Vec<Bounds> {
    layout_in(old, new, items, &[])
}

/// [`layout`], then the anchor sides: an anchored edge with an `AnchorSide` follows the line it
/// refers to in a sibling, or in the parent when the reference is one of `parent_names` (its
/// name, `Owner` for the form). The rules are LCL's (`TAnchorSide.CheckSidePosition`): the
/// edge sits the control's own `BorderSpacing` (side + Around) away from the line; anchored to
/// the opposite side of a sibling, the larger of both controls' spacings; centred, no spacing.
/// Both edges of an axis anchored this way stretch the control; one moves it. Chains are
/// followed by repeating; a cycle stops after as many passes as there are controls. References
/// that resolve to nothing leave the edge as plain anchoring placed it. Docked controls ignore
/// anchor sides.
pub fn layout_in(
    old: (i64, i64),
    new: (i64, i64),
    items: &[Item],
    parent_names: &[String],
) -> Vec<Bounds> {
    let mut out = layout_plain(old, new, items);
    let parent = Bounds {
        left: 0,
        top: 0,
        width: new.0,
        height: new.1,
    };
    for _ in 0..=items.len() {
        let mut changed = false;
        for (i, it) in items.iter().enumerate() {
            if it.align != Align::None {
                continue;
            }
            let mut b = out[i];
            // An anchored edge without (a resolvable) anchor side keeps where plain anchoring
            // put it; an edge that is not anchored is free.
            let pos = |edge: Edge, b: &Bounds| -> Option<i64> {
                if !edge.anchored(it.anchors) {
                    return None;
                }
                let here = match edge {
                    Edge::Left => b.left,
                    Edge::Top => b.top,
                    Edge::Right => b.right(),
                    Edge::Bottom => b.bottom(),
                };
                let Some(s) = it.sides[edge.index()].as_ref() else {
                    return Some(here);
                };
                let (r, r_space, is_parent) = if parent_names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(&s.control))
                {
                    (parent, Spacing::default(), true)
                } else {
                    let Some(j) = items
                        .iter()
                        .position(|o| !o.name.is_empty() && o.name.eq_ignore_ascii_case(&s.control))
                        .filter(|&j| j != i)
                    else {
                        return Some(here);
                    };
                    (out[j], items[j].spacing, false)
                };
                let own = it.spacing.side(edge);
                let h = edge.horizontal();
                let (start, end, size, my_size) = if h {
                    (r.left, r.right(), r.width, b.width)
                } else {
                    (r.top, r.bottom(), r.height, b.height)
                };
                let leading = matches!(edge, Edge::Left | Edge::Top);
                let opposite = |e: Edge| match e {
                    Edge::Left => Edge::Right,
                    Edge::Right => Edge::Left,
                    Edge::Top => Edge::Bottom,
                    Edge::Bottom => Edge::Top,
                };
                Some(match s.side {
                    SideRef::Centre => {
                        let c = start + size / 2;
                        if leading {
                            c - my_size / 2
                        } else {
                            c + my_size / 2
                        }
                    }
                    side => {
                        let line = if side == SideRef::Start { start } else { end };
                        // Facing the reference's other side: the wider spacing wins.
                        let facing = !is_parent && (side == SideRef::End) == leading;
                        let space = if facing {
                            own.max(r_space.side(opposite(edge)))
                        } else {
                            own
                        };
                        if leading { line + space } else { line - space }
                    }
                })
            };
            let (l, r) = (pos(Edge::Left, &b), pos(Edge::Right, &b));
            match (l, r) {
                (Some(l), Some(r)) => {
                    b.left = l;
                    b.width = (r - l).max(1);
                }
                (Some(l), None) => b.left = l,
                (None, Some(r)) => b.left = r - b.width,
                (None, None) => {}
            }
            let (t, bo) = (pos(Edge::Top, &b), pos(Edge::Bottom, &b));
            match (t, bo) {
                (Some(t), Some(bo)) => {
                    b.top = t;
                    b.height = (bo - t).max(1);
                }
                (Some(t), None) => b.top = t,
                (None, Some(bo)) => b.top = bo - b.height,
                (None, None) => {}
            }
            if b != out[i] {
                out[i] = b;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// Anchors and docking only.
fn layout_plain(old: (i64, i64), new: (i64, i64), items: &[Item]) -> Vec<Bounds> {
    let (dw, dh) = (new.0 - old.0, new.1 - old.1);
    let mut out: Vec<Bounds> = items
        .iter()
        .map(|it| {
            let mut b = it.bounds;
            let a = it.anchors;
            match (a.left, a.right) {
                (true, true) => b.width += dw,
                (false, true) => b.left += dw,
                (true, false) => {}
                (false, false) if old.0 > 0 => {
                    let centre =
                        (b.left as f64 + b.width as f64 / 2.0) * new.0 as f64 / old.0 as f64;
                    b.left = (centre - b.width as f64 / 2.0).round() as i64;
                }
                (false, false) => {}
            }
            match (a.top, a.bottom) {
                (true, true) => b.height += dh,
                (false, true) => b.top += dh,
                (true, false) => {}
                (false, false) if old.1 > 0 => {
                    let centre =
                        (b.top as f64 + b.height as f64 / 2.0) * new.1 as f64 / old.1 as f64;
                    b.top = (centre - b.height as f64 / 2.0).round() as i64;
                }
                (false, false) => {}
            }
            b.width = b.width.max(1);
            b.height = b.height.max(1);
            b
        })
        .collect();

    let mut rest = Bounds {
        left: 0,
        top: 0,
        width: new.0,
        height: new.1,
    };
    for pass in [
        Align::Top,
        Align::Bottom,
        Align::Left,
        Align::Right,
        Align::Client,
    ] {
        for (i, it) in items.iter().enumerate().filter(|(_, it)| it.align == pass) {
            let (w, h) = (it.bounds.width, it.bounds.height);
            // BorderSpacing keeps a docked control off the strip's edges.
            let sp = it.spacing;
            let (sl, st, sr, sb) = (
                sp.side(Edge::Left),
                sp.side(Edge::Top),
                sp.side(Edge::Right),
                sp.side(Edge::Bottom),
            );
            out[i] = match pass {
                Align::Top => {
                    let b = Bounds {
                        left: rest.left + sl,
                        top: rest.top + st,
                        width: rest.width - sl - sr,
                        height: h,
                    };
                    rest.top += h + st + sb;
                    rest.height -= h + st + sb;
                    b
                }
                Align::Bottom => {
                    let b = Bounds {
                        left: rest.left + sl,
                        top: rest.bottom() - h - sb,
                        width: rest.width - sl - sr,
                        height: h,
                    };
                    rest.height -= h + st + sb;
                    b
                }
                Align::Left => {
                    let b = Bounds {
                        left: rest.left + sl,
                        top: rest.top + st,
                        width: w,
                        height: rest.height - st - sb,
                    };
                    rest.left += w + sl + sr;
                    rest.width -= w + sl + sr;
                    b
                }
                Align::Right => {
                    let b = Bounds {
                        left: rest.right() - w - sr,
                        top: rest.top + st,
                        width: w,
                        height: rest.height - st - sb,
                    };
                    rest.width -= w + sl + sr;
                    b
                }
                Align::Client | Align::None => Bounds {
                    left: rest.left + sl,
                    top: rest.top + st,
                    width: rest.width - sl - sr,
                    height: rest.height - st - sb,
                },
            };
            out[i].width = out[i].width.max(1);
            out[i].height = out[i].height.max(1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pascoda_forms::lfm::parse;

    const FORM: &str = "object F: TF\n  ClientWidth = 400\n  ClientHeight = 300\n  object P: TPanel\n    Left = 100\n    Top = 50\n    Width = 200\n    Height = 150\n    object B: TButton\n      Left = 10\n      Top = 20\n    end\n  end\n  object L: TLabel\n    Left = 150\n    Top = 60\n  end\nend\n";

    #[test]
    fn reads_bounds_with_lcl_defaults() {
        let f = parse(FORM).unwrap();
        assert_eq!(form_size(&f), (400, 300));
        let b = bounds(&f.children[0].children[0]);
        assert_eq!((b.left, b.top, b.width, b.height), (10, 20, 75, 25)); // TButton default size
        assert_eq!(
            form_size(&parse("object F: TF\n  Width = 50\nend").unwrap()),
            (50, 240)
        );
    }

    #[test]
    fn child_bounds_are_relative_to_the_parent() {
        let f = parse(FORM).unwrap();
        let abs = absolute(&f, &[0, 0]).unwrap();
        assert_eq!((abs.left, abs.top), (110, 70));
        assert_eq!(absolute(&f, &[1]).unwrap().left, 150);
        assert_eq!(absolute(&f, &[9]), None);
    }

    #[test]
    fn hit_test_finds_the_deepest_topmost_component() {
        let f = parse(FORM).unwrap();
        assert_eq!(hit_test(&f, 115.0, 75.0), Some(vec![0, 0])); // the button inside the panel
        assert_eq!(hit_test(&f, 250.0, 150.0), Some(vec![0])); // panel only
        assert_eq!(hit_test(&f, 160.0, 65.0), Some(vec![1])); // label drawn after the panel wins
        assert_eq!(hit_test(&f, 5.0, 5.0), None); // just the form
        let t = parse("object F: TF\n  object T: TTimer\n    Left = 0\n    Top = 0\n  end\nend")
            .unwrap();
        assert_eq!(
            hit_test(&t, 5.0, 5.0),
            None,
            "non-visual components are not on the form"
        );
        assert_eq!(absolute(&t, &[0]), None);
    }

    #[test]
    fn finds_resize_handles() {
        let b = Bounds {
            left: 10,
            top: 10,
            width: 100,
            height: 40,
        };
        assert_eq!(handle_at(b, 10.0, 10.0, 4.0), Some(Handle::TopLeft));
        assert_eq!(handle_at(b, 60.0, 50.0, 4.0), Some(Handle::Bottom));
        assert_eq!(handle_at(b, 110.0, 30.0, 4.0), Some(Handle::Right));
        assert_eq!(handle_at(b, 60.0, 30.0, 4.0), None);
    }

    #[test]
    fn snaps_to_the_grid() {
        assert_eq!(
            (snap(13, 8), snap(12, 8), snap(-5, 8), snap(7, 1)),
            (16, 16, -8, 7)
        );
        let b = Bounds {
            left: 16,
            top: 16,
            width: 75,
            height: 25,
        };
        let (m, _) = guided_move(b, 5, 3, 8, &[], SNAP);
        assert_eq!((m.left, m.top, m.width, m.height), (24, 16, 75, 25)); // size untouched
    }

    #[test]
    fn resizing_moves_only_the_dragged_edges() {
        let b = Bounds {
            left: 16,
            top: 16,
            width: 80,
            height: 24,
        };
        let r = guided_resize(b, Handle::BottomRight, (20, 9), 8, &[], SNAP).0;
        assert_eq!((r.left, r.top, r.right(), r.bottom()), (16, 16, 120, 48));
        let r = guided_resize(b, Handle::TopLeft, (-8, -8), 8, &[], SNAP).0;
        assert_eq!((r.left, r.top, r.right(), r.bottom()), (8, 8, 96, 40));
        let r = guided_resize(b, Handle::Right, (-500, 0), 8, &[], SNAP).0;
        assert_eq!(r.width, 1, "never collapses below one point");
        assert_eq!((r.top, r.height), (16, 24));
    }

    fn rect(l: i64, t: i64, w: i64, h: i64) -> Bounds {
        Bounds {
            left: l,
            top: t,
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_move_snaps_to_a_neighbours_edges_and_centre_before_the_grid() {
        let other = rect(100, 100, 80, 20); // edges 100/180, centre 140
        let me = rect(0, 0, 40, 20);
        // Left edge dragged to 98: within 4 of 100, so it lands on 100, not the grid's 96.
        let (b, g) = guided_move(me, 98, 50, 8, &[other], SNAP);
        assert_eq!((b.left, b.top), (100, 48));
        // Its left edge meets the other's, and its right edge (140) the other's centre.
        assert_eq!(
            g,
            vec![
                Guide {
                    vertical: true,
                    at: 100,
                    from: 48,
                    to: 120
                },
                Guide {
                    vertical: true,
                    at: 140,
                    from: 48,
                    to: 120
                }
            ]
        );
        // Centre (left + 20) to 142: snaps to the centre 140.
        let (b, _) = guided_move(me, 122, 0, 8, &[other], SNAP);
        assert_eq!(b.centre_x(), 140);
        // Right edge 40 + 143 = 183 -> 180; same top as the other: a horizontal guide too.
        let (b, g) = guided_move(me, 143, 101, 8, &[other], SNAP);
        assert_eq!((b.right(), b.top), (180, 100));
        assert!(g.contains(&Guide {
            vertical: false,
            at: 100,
            from: 100,
            to: 180
        }));
        // Too far: the grid wins and there are no guides.
        let (b, g) = guided_move(me, 90, 50, 8, &[other], SNAP);
        assert_eq!((b.left, b.top, g.len()), (88, 48, 0));
    }

    #[test]
    fn the_nearest_line_wins_and_size_is_kept() {
        let others = [rect(50, 0, 10, 10), rect(53, 0, 10, 10)];
        let (b, _) = guided_move(rect(0, 200, 30, 30), 52, 0, 8, &others, SNAP);
        assert_eq!((b.left, b.width, b.height), (53, 30, 30));
    }

    #[test]
    fn a_resize_snaps_only_the_dragged_edge() {
        let other = rect(100, 0, 50, 20); // edges 100/150, centre 125
        let me = rect(0, 40, 60, 20);
        let (b, g) = guided_resize(me, Handle::Right, (63, 0), 8, &[other], SNAP);
        assert_eq!((b.left, b.right()), (0, 125), "the centre is a target too");
        assert!(g.iter().any(|g| g.vertical && g.at == 125));
        let (b, _) = guided_resize(me, Handle::Right, (30, 0), 8, &[other], SNAP);
        assert_eq!(b.right(), 88, "nothing near: grid");
        let (b, _) = guided_resize(me, Handle::Left, (97, 3), 8, &[other], SNAP);
        assert_eq!(
            (b.left, b.right(), b.top),
            (59, 60, 40),
            "never past the other edge"
        );
    }

    #[test]
    fn align_and_size_follow_the_reference() {
        let items = [
            rect(10, 10, 40, 20),
            rect(30, 50, 80, 30),
            rect(0, 90, 20, 10),
        ];
        let lefts: Vec<i64> = arrange(&items, 1, Arrange::Lefts)
            .iter()
            .map(|b| b.left)
            .collect();
        assert_eq!(lefts, [30, 30, 30]);
        let r = arrange(&items, 1, Arrange::Rights);
        assert!(r.iter().all(|b| b.right() == 110));
        let r = arrange(&items, 1, Arrange::CentresX);
        assert_eq!((r[0].left, r[2].left), (50, 60)); // centre 70
        let r = arrange(&items, 0, Arrange::Tops);
        assert!(
            r.iter().all(|b| b.top == 10) && r[1].left == 30,
            "only the top changes"
        );
        let r = arrange(&items, 0, Arrange::Bottoms);
        assert!(r.iter().all(|b| b.bottom() == 30));
        let r = arrange(&items, 0, Arrange::CentresY);
        assert_eq!((r[1].top, r[2].top), (5, 15)); // centre 20
        let r = arrange(&items, 2, Arrange::SameWidth);
        assert!(r.iter().all(|b| b.width == 20) && r[1].height == 30);
        let r = arrange(&items, 2, Arrange::SameHeight);
        assert!(r.iter().all(|b| b.height == 10));
        assert_eq!(
            arrange(&items, 9, Arrange::Lefts),
            items.to_vec(),
            "bad reference"
        );
    }

    #[test]
    fn spacing_makes_the_gaps_equal_and_keeps_the_ends() {
        // Out of order on purpose: spacing goes by position, not selection order.
        let items = [
            rect(100, 0, 10, 10),
            rect(0, 0, 20, 10),
            rect(30, 0, 30, 10),
            rect(200, 0, 21, 10),
        ];
        let r = arrange(&items, 0, Arrange::SpaceX);
        assert_eq!(r[1].left, 0, "first stays");
        assert_eq!(r[3].right(), 221, "last stays");
        // 221 - 81 of widths = 140 free over 3 gaps: 46, 47, 47 (rounded so they add up).
        let gaps = [
            r[2].left - r[1].right(),
            r[0].left - r[2].right(),
            r[3].left - r[0].right(),
        ];
        assert_eq!(gaps.iter().sum::<i64>(), 140);
        assert!(gaps.iter().all(|g| (46..=47).contains(g)), "{gaps:?}");
        assert!(
            r.iter()
                .zip(&items)
                .all(|(a, b)| a.width == b.width && a.top == b.top)
        );
        let col = [
            rect(0, 0, 10, 10),
            rect(0, 15, 10, 10),
            rect(0, 100, 10, 10),
        ];
        let r = arrange(&col, 0, Arrange::SpaceY);
        assert_eq!(r[1].top, 50);
        let two = [rect(0, 0, 10, 10), rect(50, 0, 10, 10)];
        assert_eq!(arrange(&two, 0, Arrange::SpaceX), two.to_vec());
    }

    #[test]
    fn guides_at_the_same_place_merge() {
        let g = guides(
            &rect(0, 50, 10, 10),
            &[rect(0, 0, 10, 10), rect(0, 100, 10, 10)],
        );
        let left: Vec<_> = g.iter().filter(|g| g.vertical && g.at == 0).collect();
        assert_eq!(left.len(), 1);
        assert_eq!((left[0].from, left[0].to), (0, 110));
        assert_eq!(
            union(&[rect(0, 0, 10, 10), rect(20, 30, 5, 5)]),
            Some(rect(0, 0, 25, 35))
        );
        assert!(rect(0, 0, 10, 10).intersects(&rect(10, 10, 5, 5)));
        assert!(!rect(0, 0, 10, 10).intersects(&rect(11, 0, 5, 5)));
    }

    fn item(l: i64, t: i64, w: i64, h: i64, a: Anchors, align: Align) -> Item {
        Item::plain(
            Bounds {
                left: l,
                top: t,
                width: w,
                height: h,
            },
            a,
            align,
        )
    }

    const LT: Anchors = Anchors::DEFAULT;

    fn anchors(l: bool, t: bool, r: bool, b: bool) -> Anchors {
        Anchors {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn reads_and_writes_anchors_the_way_lazarus_does() {
        let f = parse("object F: TF\n  object B: TButton\n    Anchors = [akTop, akRight]\n  end\n  object C: TButton\n  end\nend").unwrap();
        assert_eq!(
            anchors_of(&f.children[0]),
            anchors(false, true, true, false)
        );
        assert_eq!(anchors_of(&f.children[1]), Anchors::DEFAULT);
        assert_eq!(
            anchors(true, true, true, false).to_prop(),
            PropValue::Set(vec!["akTop".into(), "akLeft".into(), "akRight".into()])
        );
        assert_eq!(
            Anchors::from_prop(Some(&PropValue::Set(vec![]))),
            anchors(false, false, false, false)
        );
    }

    #[test]
    fn align_and_autosize_defaults() {
        let f = parse("object F: TF\n  object L: TLabel\n  end\n  object B: TButton\n    Align = alTop\n    AutoSize = True\n  end\n  object C: TLabel\n    AutoSize = False\n  end\nend").unwrap();
        assert!(
            auto_size_of(&f.children[0]),
            "a TLabel sizes itself unless told not to"
        );
        assert_eq!(
            (align_of(&f.children[1]), auto_size_of(&f.children[1])),
            (Align::Top, true)
        );
        assert!(!auto_size_of(&f.children[2]));
        assert_eq!(
            (align_of(&f.children[0]), Align::from_ident("alclient")),
            (Align::None, Align::Client)
        );
    }

    #[test]
    fn anchored_edges_keep_their_distance() {
        let items = [
            item(10, 10, 100, 20, LT, Align::None), // left+top: stays put
            item(
                10,
                40,
                100,
                20,
                anchors(true, true, true, false),
                Align::None,
            ), // stretches
            item(
                10,
                70,
                60,
                20,
                anchors(false, true, true, false),
                Align::None,
            ), // follows the right edge
            item(
                10,
                100,
                60,
                20,
                anchors(true, false, false, true),
                Align::None,
            ), // follows the bottom
            item(
                10,
                130,
                60,
                20,
                anchors(true, true, true, true),
                Align::None,
            ), // stretches both ways
        ];
        let r = layout((300, 200), (400, 260), &items);
        assert_eq!(r[0], items[0].bounds);
        assert_eq!((r[1].left, r[1].width), (10, 200));
        assert_eq!((r[2].left, r[2].width), (110, 60));
        assert_eq!((r[3].top, r[3].height), (160, 20));
        assert_eq!((r[4].width, r[4].height), (160, 80));
    }

    #[test]
    fn controls_without_horizontal_anchors_keep_a_proportional_centre() {
        let items = [item(
            100,
            10,
            100,
            20,
            anchors(false, true, false, false),
            Align::None,
        )];
        // centre was at 150 of 300; at width 600 it is at 300.
        let r = layout((300, 200), (600, 200), &items);
        assert_eq!((r[0].left, r[0].width), (250, 100));
    }

    #[test]
    fn docked_controls_take_their_strips_in_order() {
        let items = [
            item(0, 0, 50, 30, LT, Align::Top),
            item(0, 0, 80, 10, LT, Align::Left),
            item(0, 0, 10, 24, LT, Align::Bottom),
            item(0, 0, 10, 10, LT, Align::Client),
            item(5, 5, 20, 20, LT, Align::None),
        ];
        let r = layout((300, 200), (400, 300), &items);
        assert_eq!(
            r[0],
            Bounds {
                left: 0,
                top: 0,
                width: 400,
                height: 30
            }
        ); // top strip, full width
        assert_eq!(
            r[2],
            Bounds {
                left: 0,
                top: 276,
                width: 400,
                height: 24
            }
        ); // bottom strip
        assert_eq!(
            r[1],
            Bounds {
                left: 0,
                top: 30,
                width: 80,
                height: 246
            }
        ); // left, between them
        assert_eq!(
            r[3],
            Bounds {
                left: 80,
                top: 30,
                width: 320,
                height: 246
            }
        ); // client fills the rest
        assert_eq!(r[4], items[4].bounds, "undocked controls are left alone");
    }

    #[test]
    fn layout_is_stable_when_nothing_changes() {
        let items = [
            item(0, 0, 50, 30, LT, Align::Top),
            item(0, 0, 10, 10, LT, Align::Client),
            item(
                7,
                9,
                33,
                21,
                anchors(false, false, false, false),
                Align::None,
            ),
        ];
        let once = layout((300, 200), (300, 200), &items);
        let again: Vec<Item> = items
            .iter()
            .zip(&once)
            .map(|(it, b)| Item {
                bounds: *b,
                ..it.clone()
            })
            .collect();
        assert_eq!(layout((300, 200), (300, 200), &again), once);
        assert_eq!(once[2], items[2].bounds);
    }

    #[test]
    fn controls_never_collapse_below_one_point() {
        let items = [item(
            10,
            10,
            20,
            20,
            anchors(true, true, true, true),
            Align::None,
        )];
        let r = layout((300, 200), (100, 50), &items);
        assert_eq!((r[0].width, r[0].height), (1, 1));
    }

    fn named(name: &str, b: Bounds, a: Anchors) -> Item {
        Item {
            name: name.into(),
            ..Item::plain(b, a, Align::None)
        }
    }

    fn side(control: &str, side: SideRef) -> Option<AnchorSide> {
        Some(AnchorSide {
            control: control.into(),
            side,
        })
    }

    #[test]
    fn reads_anchor_sides_and_border_spacing() {
        let f = parse("object F: TF\n  object L: TLabel\n    AnchorSideLeft.Control = E\n    AnchorSideLeft.Side = asrBottom\n    AnchorSideTop.Control = E\n    AnchorSideTop.Side = asrCenter\n    BorderSpacing.Left = 6\n    BorderSpacing.Around = 2\n  end\nend").unwrap();
        let l = &f.children[0];
        assert_eq!(anchor_side(l, Edge::Left), side("E", SideRef::End));
        assert_eq!(anchor_side(l, Edge::Top).unwrap().side, SideRef::Centre);
        assert_eq!(anchor_side(l, Edge::Right), None);
        let sp = Spacing::of(l);
        assert_eq!((sp.side(Edge::Left), sp.side(Edge::Top)), (8, 2));
        assert_eq!(
            SideRef::from_ident("asrRight"),
            SideRef::End,
            "the Pascal alias is accepted"
        );
        assert_eq!(
            SideRef::End.ident(),
            "asrBottom",
            "but the enum name is written"
        );
    }

    #[test]
    fn anchor_sides_follow_siblings_and_the_parent_like_lcl() {
        let lt = Anchors::DEFAULT;
        // An edit, a label to its right (centred vertically), a button below the edit that
        // stretches to the parent's right edge.
        let edit = named("Edit1", rect(10, 10, 100, 24), lt);
        let mut label = named("Label1", rect(0, 0, 40, 16), lt);
        label.sides[Edge::Left as usize] = side("Edit1", SideRef::End);
        label.sides[Edge::Top as usize] = side("Edit1", SideRef::Centre);
        label.spacing.left = 6;
        let mut button = named(
            "Button1",
            rect(0, 0, 75, 25),
            anchors(true, true, true, false),
        );
        button.sides[Edge::Left as usize] = side("Edit1", SideRef::Start);
        button.sides[Edge::Top as usize] = side("Edit1", SideRef::End);
        button.sides[Edge::Right as usize] = side("Owner", SideRef::End);
        button.spacing.around = 4;
        let parent = ["Owner".to_string(), "Form1".to_string()];
        let r = layout_in(
            (300, 200),
            (300, 200),
            &[edit.clone(), label.clone(), button.clone()],
            &parent,
        );
        assert_eq!(
            r[1],
            rect(116, 14, 40, 16),
            "right of the edit plus 6, centred on it"
        );
        assert_eq!(
            r[2],
            rect(14, 38, 300 - 4 - 14, 25),
            "left with the edit, below it, to the parent's right"
        );

        // The wider of the two spacings wins between facing sides.
        let mut wide = edit.clone();
        wide.spacing.right = 12;
        let r = layout_in((300, 200), (300, 200), &[wide, label.clone()], &parent);
        assert_eq!(r[1].left, 110 + 12);

        // A chain resolves even when listed backwards; the form grows and they follow.
        let r = layout_in((300, 200), (400, 200), &[button, label, edit], &parent);
        assert_eq!(r[0].right(), 400 - 4);
        assert_eq!(r[1].left, 116);

        // Unknown references and edges not in Anchors are left to plain anchoring.
        let mut stray = named("X", rect(5, 5, 10, 10), lt);
        stray.sides[Edge::Left as usize] = side("Nobody", SideRef::End);
        stray.sides[Edge::Right as usize] = side("Owner", SideRef::End);
        assert_eq!(
            layout_in((300, 200), (300, 200), &[stray], &parent)[0],
            rect(5, 5, 10, 10)
        );

        // A cycle stops.
        let mut a = named("A", rect(0, 0, 10, 10), lt);
        let mut b = named("B", rect(0, 0, 10, 10), lt);
        a.sides[0] = side("B", SideRef::End);
        b.sides[0] = side("A", SideRef::End);
        let r = layout_in((300, 200), (300, 200), &[a, b], &parent);
        assert!(r[0].left > 0 && r[1].left > 0);
    }

    #[test]
    fn docked_controls_keep_their_border_spacing() {
        let mut top = item(0, 0, 50, 30, Anchors::DEFAULT, Align::Top);
        top.spacing.around = 4;
        let mut client = item(0, 0, 10, 10, Anchors::DEFAULT, Align::Client);
        client.spacing.left = 8;
        let r = layout((300, 200), (300, 200), &[top, client]);
        assert_eq!(r[0], rect(4, 4, 292, 30));
        assert_eq!(r[1], rect(8, 38, 292, 162));
    }

    #[test]
    fn scaling_into_a_new_box_keeps_relative_places() {
        let a = rect(0, 0, 50, 20);
        let b = rect(100, 40, 100, 20);
        let from = union(&[a, b]).unwrap(); // 0,0 200x60
        let r = scale_into(&[a, b], from, rect(10, 10, 400, 60));
        assert_eq!(r, vec![rect(10, 10, 100, 20), rect(210, 50, 200, 20)]);
        let r = scale_into(&[a, b], from, rect(0, 0, 2, 2));
        assert!(r.iter().all(|b| b.width >= 1 && b.height >= 1));
    }
}
