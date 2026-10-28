//! The scrollable design canvas: where the form's window sits in the free area, the scroll
//! offset, scroll bars and the mapping between screen points and form coordinates.
//!
//! The content is the form's window (title bar + client area) with a margin around it. A form
//! smaller than the free area is centred horizontally and kept `MARGIN` from the top, as before
//! scrolling existed; a larger one scrolls on that axis. Pure maths, unit-tested; `ui` draws
//! and `native` follows it.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

/// Height of the window mock's title bar above the client area.
pub const TITLE_BAR: f32 = 28.0;
/// Space around the window mock.
pub const MARGIN: f32 = 24.0;
/// Thickness of a scroll bar strip; the canvas shrinks by this on that side.
pub const BAR: f32 = 12.0;
/// Within this distance of the visible area's edge, a drag scrolls the canvas.
pub const AUTO_ZONE: f32 = 24.0;
/// Fastest auto-scroll, points per frame.
pub const AUTO_MAX: f32 = 24.0;

/// Size of everything that scrolls for a form of `form` client size.
pub fn content_size(form: (i64, i64)) -> Vec2 {
    vec2(
        form.0 as f32 + 2.0 * MARGIN,
        form.1 as f32 + TITLE_BAR + 2.0 * MARGIN,
    )
}

/// Splits the free area `avail` into the visible canvas and the strips for a horizontal and a
/// vertical scroll bar (each `None` when that axis fits). A bar on one axis takes space from
/// the other, which may then need a bar too.
pub fn split(avail: Rect, form: (i64, i64)) -> (Rect, Option<Rect>, Option<Rect>) {
    let content = content_size(form);
    let mut need_v = content.y > avail.height();
    let need_h = content.x > avail.width() - if need_v { BAR } else { 0.0 };
    if need_h && !need_v {
        need_v = content.y > avail.height() - BAR;
    }
    let mut area = avail;
    if need_v {
        area.max.x = (area.max.x - BAR).max(area.min.x);
    }
    if need_h {
        area.max.y = (area.max.y - BAR).max(area.min.y);
    }
    let h = need_h
        .then(|| Rect::from_min_max(pos2(area.min.x, area.max.y), pos2(area.max.x, avail.max.y)));
    let v = need_v
        .then(|| Rect::from_min_max(pos2(area.max.x, area.min.y), pos2(avail.max.x, area.max.y)));
    (area, h, v)
}

/// The visible canvas and its scroll offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// The visible part of the canvas, in egui points (scroll bars excluded).
    pub area: Rect,
    /// The form's client size.
    pub form: (i64, i64),
    /// How far the content is scrolled; always within `0..=max_scroll()`.
    pub scroll: Vec2,
}

impl View {
    /// A view with `scroll` clamped to what the content allows.
    pub fn new(area: Rect, form: (i64, i64), scroll: Vec2) -> Self {
        let mut v = Self {
            area,
            form,
            scroll: Vec2::ZERO,
        };
        v.scroll = v.clamp(scroll);
        v
    }

    pub fn content(&self) -> Vec2 {
        content_size(self.form)
    }

    /// The largest scroll offset per axis (0 where the content fits).
    pub fn max_scroll(&self) -> Vec2 {
        (self.content() - self.area.size()).max(Vec2::ZERO)
    }

    pub fn clamp(&self, scroll: Vec2) -> Vec2 {
        let m = self.max_scroll();
        let fix = |v: f32, max: f32| {
            if v.is_finite() {
                v.clamp(0.0, max)
            } else {
                0.0
            }
        };
        vec2(fix(scroll.x, m.x), fix(scroll.y, m.y))
    }

    /// Scrolls by `delta` (positive = towards the end), clamped.
    pub fn scroll_by(&mut self, delta: Vec2) {
        self.scroll = self.clamp(self.scroll + delta);
    }

    /// Top-left corner of the form's client area on screen.
    pub fn origin(&self) -> Pos2 {
        let w = self.form.0 as f32;
        let x = if self.max_scroll().x > 0.0 {
            self.area.left() + MARGIN - self.scroll.x
        } else {
            // Centred, like Xcode; never closer to the left edge than the margin.
            (self.area.center().x - w / 2.0).max(self.area.left() + MARGIN)
        };
        pos2(x, self.area.top() + MARGIN + TITLE_BAR - self.scroll.y)
    }

    /// The form's client area on screen (may reach outside `area`).
    pub fn client(&self) -> Rect {
        Rect::from_min_size(self.origin(), vec2(self.form.0 as f32, self.form.1 as f32))
    }

    /// Client area plus the title bar above it.
    pub fn window(&self) -> Rect {
        let c = self.client();
        Rect::from_min_max(c.min - vec2(0.0, TITLE_BAR), c.max)
    }

    /// A screen point in form coordinates.
    pub fn to_form(self, p: Pos2) -> (f64, f64) {
        let o = self.origin();
        ((p.x - o.x) as f64, (p.y - o.y) as f64)
    }

    /// A point in form coordinates on screen.
    #[cfg(test)]
    pub fn to_screen(self, x: f64, y: f64) -> Pos2 {
        self.origin() + vec2(x as f32, y as f32)
    }

    /// How far to scroll this frame while a drag holds the pointer at `p`: towards an edge the
    /// pointer is within `AUTO_ZONE` of (or past), faster the closer it gets, up to `AUTO_MAX`.
    /// Zero on axes that cannot scroll further that way.
    pub fn auto_scroll(&self, p: Pos2) -> Vec2 {
        let a = self.area;
        let axis = |pos: f32, lo: f32, hi: f32| -> f32 {
            // Small areas: the zones must not overlap.
            let zone = AUTO_ZONE.min((hi - lo) / 4.0).max(1.0);
            let speed = |depth: f32| (depth / zone * AUTO_MAX * 0.5).clamp(1.0, AUTO_MAX);
            if pos < lo + zone {
                -speed(lo + zone - pos)
            } else if pos > hi - zone {
                speed(pos - (hi - zone))
            } else {
                0.0
            }
        };
        let want = vec2(
            axis(p.x, a.left(), a.right()),
            axis(p.y, a.top(), a.bottom()),
        );
        self.clamp(self.scroll + want) - self.scroll
    }

    /// The thumb of the scroll bar in `track` for one axis: `(start, length)` along the track.
    pub fn thumb(&self, track_len: f32, horizontal: bool) -> (f32, f32) {
        let (visible, content, scroll, max) = if horizontal {
            (
                self.area.width(),
                self.content().x,
                self.scroll.x,
                self.max_scroll().x,
            )
        } else {
            (
                self.area.height(),
                self.content().y,
                self.scroll.y,
                self.max_scroll().y,
            )
        };
        let len = (track_len * visible / content.max(1.0)).clamp(track_len.min(20.0), track_len);
        let start = if max > 0.0 {
            (track_len - len) * scroll / max
        } else {
            0.0
        };
        (start, len)
    }

    /// Scroll offset change for dragging a thumb by `delta` points along a track of `track_len`.
    pub fn thumb_drag(&self, track_len: f32, horizontal: bool, delta: f32) -> f32 {
        let (_, len) = self.thumb(track_len, horizontal);
        let max = if horizontal {
            self.max_scroll().x
        } else {
            self.max_scroll().y
        };
        let room = track_len - len;
        if room <= 0.0 { 0.0 } else { delta * max / room }
    }

    /// Scroll offset that brings form rectangle `b` (left, top, width, height) into view with a
    /// small margin, moving as little as possible.
    pub fn reveal(&self, b: (f64, f64, f64, f64)) -> Vec2 {
        let pad = 12.0;
        let o = self.origin() + self.scroll; // origin at scroll 0
        let (l, t) = (o.x + b.0 as f32 - pad, o.y + b.1 as f32 - pad);
        let (r, bo) = (
            o.x + (b.0 + b.2) as f32 + pad,
            o.y + (b.1 + b.3) as f32 + pad,
        );
        let a = self.area;
        let axis = |s: f32, lo: f32, hi: f32, vlo: f32, vhi: f32| {
            if lo - s < vlo {
                lo - vlo
            } else if hi - s > vhi {
                (hi - vhi).min(lo - vlo)
            } else {
                s
            }
        };
        self.clamp(vec2(
            axis(self.scroll.x, l, r, a.left(), a.right()),
            axis(self.scroll.y, t, bo, a.top(), a.bottom()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(w, h))
    }

    #[test]
    fn a_form_that_fits_is_centred_and_does_not_scroll() {
        let v = View::new(area(800.0, 600.0), (320, 240), vec2(50.0, 70.0));
        assert_eq!(v.scroll, Vec2::ZERO, "nothing to scroll");
        assert_eq!(v.max_scroll(), Vec2::ZERO);
        // Centred: 100 + 400 - 160; MARGIN + TITLE_BAR below the top.
        assert_eq!(v.origin(), pos2(340.0, 50.0 + MARGIN + TITLE_BAR));
        assert_eq!(v.window().top(), 50.0 + MARGIN);
    }

    #[test]
    fn scroll_is_clamped_to_the_content() {
        // Content 640+48 x 508+28+48 = 688 x 584 in a 500 x 400 area.
        let v = View::new(area(500.0, 400.0), (640, 508), vec2(-5.0, 9999.0));
        assert_eq!(v.max_scroll(), vec2(188.0, 184.0));
        assert_eq!(v.scroll, vec2(0.0, 184.0));
        let v = View::new(area(500.0, 400.0), (640, 508), vec2(f32::NAN, 10.0));
        assert_eq!(v.scroll, vec2(0.0, 10.0), "NaN resets");
        let mut v = View::new(area(500.0, 400.0), (640, 508), vec2(100.0, 100.0));
        v.scroll_by(vec2(500.0, -500.0));
        assert_eq!(v.scroll, vec2(188.0, 0.0));
    }

    #[test]
    fn screen_and_form_coordinates_follow_the_scroll() {
        let mut v = View::new(area(500.0, 400.0), (640, 508), Vec2::ZERO);
        let o = pos2(100.0 + MARGIN, 50.0 + MARGIN + TITLE_BAR);
        assert_eq!(v.origin(), o);
        assert_eq!(v.to_form(o + vec2(10.0, 20.0)), (10.0, 20.0));
        v.scroll_by(vec2(30.0, 100.0));
        assert_eq!(v.origin(), o - vec2(30.0, 100.0));
        // The same screen point is further into the form.
        assert_eq!(v.to_form(o + vec2(10.0, 20.0)), (40.0, 120.0));
        assert_eq!(v.to_screen(40.0, 120.0), o + vec2(10.0, 20.0));
        // Fully scrolled, the form's bottom-right sits MARGIN inside the area's corner.
        v.scroll_by(vec2(1e6, 1e6));
        let c = v.client();
        assert_eq!(c.max, v.area.max - vec2(MARGIN, MARGIN));
    }

    #[test]
    fn one_axis_can_scroll_while_the_other_stays_centred() {
        let v = View::new(area(1000.0, 300.0), (400, 600), vec2(80.0, 80.0));
        assert_eq!(v.scroll, vec2(0.0, 80.0));
        assert_eq!(v.origin().x, 100.0 + 500.0 - 200.0);
        assert_eq!(v.origin().y, 50.0 + MARGIN + TITLE_BAR - 80.0);
    }

    #[test]
    fn scroll_bars_take_space_only_when_needed() {
        let avail = area(800.0, 600.0);
        let (a, h, v) = split(avail, (320, 240));
        assert_eq!((a, h, v), (avail, None, None));
        // Too tall only: a vertical bar on the right.
        let (a, h, v) = split(avail, (320, 900));
        assert!(h.is_none());
        assert_eq!(v.unwrap().width(), BAR);
        assert_eq!(a.width(), 800.0 - BAR);
        // Fits in width only without the vertical bar: the bar makes it need a horizontal one.
        let w = (800.0 - 2.0 * MARGIN) as i64 - 4;
        let (a, h, v) = split(avail, (w, 900));
        assert!(h.is_some() && v.is_some());
        assert_eq!(a.size(), vec2(800.0 - BAR, 600.0 - BAR));
        // Exactly fits: no bars.
        let (_, h, v) = split(
            avail,
            (
                (800.0 - 2.0 * MARGIN) as i64,
                (600.0 - 2.0 * MARGIN - TITLE_BAR) as i64,
            ),
        );
        assert!(h.is_none() && v.is_none());
    }

    #[test]
    fn thumbs_track_the_scroll() {
        let mut v = View::new(area(500.0, 400.0), (640, 508), Vec2::ZERO);
        let (s, len) = v.thumb(500.0, true);
        assert_eq!(s, 0.0);
        assert!((len - 500.0 * 500.0 / 688.0).abs() < 0.01);
        v.scroll_by(vec2(1e6, 0.0));
        let (s, len) = v.thumb(500.0, true);
        assert!((s + len - 500.0).abs() < 0.01, "thumb at the end");
        // Dragging the thumb over its whole room scrolls over the whole range.
        assert!((v.thumb_drag(500.0, true, 500.0 - len) - 188.0).abs() < 0.01);
    }

    #[test]
    fn dragging_near_an_edge_scrolls_that_way() {
        let v = View::new(area(500.0, 400.0), (640, 508), vec2(50.0, 50.0));
        let mid = v.area.center();
        assert_eq!(v.auto_scroll(mid), Vec2::ZERO);
        let d = v.auto_scroll(pos2(v.area.right() - 2.0, mid.y));
        assert!(d.x > 0.0 && d.y == 0.0);
        let d = v.auto_scroll(pos2(mid.x, v.area.top() - 100.0));
        assert_eq!(d, vec2(0.0, -AUTO_MAX), "past the edge: full speed");
        // At the start already: nothing to scroll towards.
        let v = View::new(area(500.0, 400.0), (640, 508), Vec2::ZERO);
        assert_eq!(v.auto_scroll(pos2(v.area.left(), v.area.top())), Vec2::ZERO);
    }

    #[test]
    fn reveal_scrolls_a_hidden_control_into_view() {
        let v = View::new(area(500.0, 400.0), (640, 508), Vec2::ZERO);
        // Already visible: no change.
        assert_eq!(v.reveal((10.0, 10.0, 50.0, 20.0)), Vec2::ZERO);
        // Near the bottom-right: scrolled just enough.
        let s = v.reveal((560.0, 470.0, 75.0, 25.0));
        let v2 = View::new(v.area, v.form, s);
        let br = v2.to_screen(635.0, 495.0);
        assert!(v2.area.contains(br), "{br:?} in {:?}", v2.area);
        assert!(s.x > 0.0 && s.y > 0.0);
    }
}
