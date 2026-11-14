// SPDX-FileCopyrightText: 2026 Nicholas Outram
// SPDX-License-Identifier: GPL-3.0-only
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License version 3 as published by
// the Free Software Foundation.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
// details.

//! The markup model: a list of editable shapes over an image, plus an optional
//! crop. Everything is in image pixel coordinates.

pub type Point = (f64, f64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Pen,
    Highlighter,
    Line,
    Arrow,
    Rect,
    Oval,
    Bubble,
    Text,
    Loupe,
    Crop,
}

pub const DEFAULT_FONT: &str = "Sans";

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub stroke: Rgba,
    /// Whether rectangles, ovals and bubbles draw their outline. Other marks
    /// always draw with the stroke colour.
    pub border: bool,
    pub fill: Option<Rgba>,
    pub width: f64,
    pub font_size: f64,
    /// Pango font description without a size, e.g. "Sans" or "Serif Bold Italic".
    pub font: String,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            stroke: Rgba::new(0.878, 0.106, 0.141, 1.0), // GNOME red 3
            border: true,
            fill: None,
            width: 4.0,
            font_size: 28.0,
            font: DEFAULT_FONT.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Pen(Vec<Point>),
    Highlighter(Vec<Point>),
    Line,
    Arrow,
    Rect,
    Oval,
    Bubble,
    Text(String),
    Loupe { zoom: f64 },
}

impl Kind {
    /// Marks whose outline can be switched off (they have a fill instead).
    pub fn has_optional_border(&self) -> bool {
        matches!(self, Kind::Rect | Kind::Oval | Kind::Bubble)
    }
}

impl Tool {
    /// Whether marks made with this tool can switch their outline off.
    pub fn has_optional_border(self) -> bool {
        matches!(self, Tool::Rect | Tool::Oval | Tool::Bubble)
    }
}

/// Where text sits across a mark: left, centre or right in a box, or along
/// a line or arrow as the text reads (whichever way the line was drawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Where text sits up and down a box. On a line or arrow, `Top` puts it
/// above the line and `Bottom` below (`Middle` counts as above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

impl HAlign {
    pub fn id(self) -> &'static str {
        match self {
            HAlign::Left => "left",
            HAlign::Center => "center",
            HAlign::Right => "right",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [HAlign::Left, HAlign::Center, HAlign::Right].into_iter().find(|a| a.id() == id)
    }
}

impl VAlign {
    pub fn id(self) -> &'static str {
        match self {
            VAlign::Top => "top",
            VAlign::Middle => "middle",
            VAlign::Bottom => "bottom",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [VAlign::Top, VAlign::Middle, VAlign::Bottom].into_iter().find(|a| a.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    /// Start point / first corner (top-left for text).
    pub a: Point,
    /// End point / opposite corner.
    pub b: Point,
    pub style: Style,
    /// Clockwise rotation in radians about the centre of `bounds()`. Only
    /// boxed marks (rectangle, oval, bubble, text) use it: lines and freehand
    /// marks are rotated by moving their points, and a loupe is round.
    pub angle: f64,
    /// Text inside a rectangle, oval or bubble, or along a line or arrow
    /// (a Text mark keeps its text in `Kind::Text`). Empty: none.
    pub label: String,
    /// Where the text sits: `label` for shapes and lines; for a Text mark,
    /// only `halign` counts (how its lines line up).
    pub halign: HAlign,
    pub valign: VAlign,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn from_points(a: Point, b: Point) -> Self {
        Self {
            x: a.0.min(b.0),
            y: a.1.min(b.1),
            w: (a.0 - b.0).abs(),
            h: (a.1 - b.1).abs(),
        }
    }

    pub fn contains(&self, p: Point, tol: f64) -> bool {
        p.0 >= self.x - tol && p.0 <= self.x + self.w + tol && p.1 >= self.y - tol && p.1 <= self.y + self.h + tol
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        (x1 > x0 && y1 > y0).then_some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }

    /// Corners in handle order: top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self) -> [Point; 4] {
        [
            (self.x, self.y),
            (self.x + self.w, self.y),
            (self.x + self.w, self.y + self.h),
            (self.x, self.y + self.h),
        ]
    }
}

/// Rotates `p` by `angle` radians about `c`.
pub fn rotate_point(p: Point, c: Point, angle: f64) -> Point {
    let (sin, cos) = angle.sin_cos();
    let (dx, dy) = (p.0 - c.0, p.1 - c.1);
    (c.0 + dx * cos - dy * sin, c.1 + dx * sin + dy * cos)
}

/// Wraps an angle into (-π, π].
pub fn normalize_angle(a: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let a = a.rem_euclid(TAU);
    if a > PI { a - TAU } else { a }
}

fn dist_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

impl Shape {
    pub fn new(kind: Kind, p: Point, style: Style) -> Self {
        // Text goes in the middle of a box, above the middle of a line, and
        // a Text mark's lines start at the left.
        let (halign, valign) = match kind {
            Kind::Text(_) => (HAlign::Left, VAlign::Top),
            Kind::Line | Kind::Arrow => (HAlign::Center, VAlign::Top),
            _ => (HAlign::Center, VAlign::Middle),
        };
        Self { kind, a: p, b: p, style, angle: 0.0, label: String::new(), halign, valign }
    }

    /// Whether the mark can hold text: a Text mark, or a label in a
    /// rectangle, oval, bubble, line or arrow.
    pub fn holds_text(&self) -> bool {
        matches!(self.kind, Kind::Text(_) | Kind::Rect | Kind::Oval | Kind::Bubble | Kind::Line | Kind::Arrow)
    }

    /// The mark's text: a Text mark's own, or the label of another mark.
    pub fn text(&self) -> &str {
        match &self.kind {
            Kind::Text(t) => t,
            _ => &self.label,
        }
    }

    pub fn set_text(&mut self, text: String) {
        match &mut self.kind {
            Kind::Text(t) => *t = text,
            _ => self.label = text,
        }
    }

    /// Whether the mark can be rotated at all.
    pub fn can_rotate(&self) -> bool {
        !matches!(self.kind, Kind::Loupe { .. })
    }

    /// Whether the mark keeps its rotation in `angle` (rather than in its points).
    fn uses_angle(&self) -> bool {
        matches!(self.kind, Kind::Rect | Kind::Oval | Kind::Bubble | Kind::Text(_))
    }

    pub fn center(&self) -> Point {
        let r = self.bounds();
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    /// The four corners of the mark's box as drawn, i.e. rotated.
    pub fn outline(&self) -> [Point; 4] {
        let c = self.center();
        self.bounds().corners().map(|q| rotate_point(q, c, self.angle))
    }

    /// Writes `orig` turned by `delta` radians about its centre into `self`.
    pub fn rotate_from(&mut self, orig: &Shape, delta: f64) {
        *self = orig.clone();
        if !orig.can_rotate() {
            return;
        }
        if orig.uses_angle() {
            self.angle = normalize_angle(orig.angle + delta);
            return;
        }
        let c = orig.center();
        self.a = rotate_point(orig.a, c, delta);
        self.b = rotate_point(orig.b, c, delta);
        if let Kind::Pen(pts) | Kind::Highlighter(pts) = &mut self.kind {
            for q in pts.iter_mut() {
                *q = rotate_point(*q, c, delta);
            }
        }
    }

    pub fn is_freehand(&self) -> bool {
        matches!(self.kind, Kind::Pen(_) | Kind::Highlighter(_))
    }

    /// Whether the outline is drawn: always, unless this kind can turn it off.
    pub fn draws_border(&self) -> bool {
        self.style.border || !self.kind.has_optional_border()
    }

    pub fn is_segment(&self) -> bool {
        matches!(self.kind, Kind::Line | Kind::Arrow)
    }

    pub fn highlighter_width(&self) -> f64 {
        (self.style.width * 5.0).max(14.0)
    }

    pub fn bounds(&self) -> Rect {
        match &self.kind {
            Kind::Pen(pts) | Kind::Highlighter(pts) if !pts.is_empty() => {
                let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for p in pts {
                    x0 = x0.min(p.0);
                    y0 = y0.min(p.1);
                    x1 = x1.max(p.0);
                    y1 = y1.max(p.1);
                }
                Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
            }
            Kind::Loupe { .. } => {
                let r = Rect::from_points(self.a, self.b);
                let s = r.w.min(r.h);
                Rect { x: r.x, y: r.y, w: s, h: s }
            }
            _ => Rect::from_points(self.a, self.b),
        }
    }

    /// Handle positions that can be dragged to resize.
    pub fn handles(&self) -> Vec<Point> {
        if self.is_segment() { vec![self.a, self.b] } else { self.outline().to_vec() }
    }

    pub fn hit(&self, p: Point, tol: f64) -> bool {
        // Test boxed marks in their own, unrotated frame.
        let p = if self.angle != 0.0 { rotate_point(p, self.center(), -self.angle) } else { p };
        let reach = tol + self.style.width / 2.0;
        match &self.kind {
            Kind::Line | Kind::Arrow => dist_to_segment(p, self.a, self.b) <= reach,
            Kind::Pen(pts) => pts.windows(2).any(|w| dist_to_segment(p, w[0], w[1]) <= reach)
                || pts.first().is_some_and(|q| dist_to_segment(p, *q, *q) <= reach),
            Kind::Highlighter(pts) => {
                let reach = tol + self.highlighter_width() / 2.0;
                pts.windows(2).any(|w| dist_to_segment(p, w[0], w[1]) <= reach)
            }
            _ => self.bounds().contains(p, tol),
        }
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.a = (self.a.0 + dx, self.a.1 + dy);
        self.b = (self.b.0 + dx, self.b.1 + dy);
        if let Kind::Pen(pts) | Kind::Highlighter(pts) = &mut self.kind {
            for q in pts {
                *q = (q.0 + dx, q.1 + dy);
            }
        }
    }

    /// Scales the mark about the image origin by `k`: its points, line
    /// width and text size. Rotation is unchanged.
    pub fn scale(&mut self, k: f64) {
        let sc = |p: Point| (p.0 * k, p.1 * k);
        self.a = sc(self.a);
        self.b = sc(self.b);
        if let Kind::Pen(pts) | Kind::Highlighter(pts) = &mut self.kind {
            for q in pts.iter_mut() {
                *q = sc(*q);
            }
        }
        self.style.width *= k;
        self.style.font_size *= k;
    }

    /// Moves handle `idx` of `orig` to `p`, writing the result into `self`.
    pub fn resize_from(&mut self, orig: &Shape, idx: usize, p: Point) {
        *self = orig.clone();
        if orig.is_segment() {
            if idx == 0 { self.a = p } else { self.b = p }
            return;
        }
        let r = orig.bounds();
        let corners = r.corners();
        let fixed = corners[(idx + 2) % 4];
        // A rotated box is resized in its own frame, keeping the opposite
        // corner where it is on screen.
        let (fixed, p) = if orig.angle != 0.0 {
            let c = orig.center();
            let fixed_world = rotate_point(fixed, c, orig.angle);
            let v = rotate_point(p, fixed_world, -orig.angle);
            let half = ((v.0 - fixed_world.0) / 2.0, (v.1 - fixed_world.1) / 2.0);
            let new_c = rotate_point((fixed_world.0 + half.0, fixed_world.1 + half.1), fixed_world, orig.angle);
            // Express both points around the new centre, unrotated.
            ((new_c.0 - half.0, new_c.1 - half.1), (new_c.0 + half.0, new_c.1 + half.1))
        } else {
            (fixed, p)
        };
        let mut nr = Rect::from_points(fixed, p);
        if matches!(orig.kind, Kind::Loupe { .. }) {
            let s = nr.w.max(nr.h);
            let sx = if p.0 < fixed.0 { -s } else { s };
            let sy = if p.1 < fixed.1 { -s } else { s };
            nr = Rect::from_points(fixed, (fixed.0 + sx, fixed.1 + sy));
        }
        match &mut self.kind {
            Kind::Pen(pts) | Kind::Highlighter(pts) => {
                let sx = if r.w > 0.0 { nr.w / r.w } else { 1.0 };
                let sy = if r.h > 0.0 { nr.h / r.h } else { 1.0 };
                for q in pts.iter_mut() {
                    *q = (nr.x + (q.0 - r.x) * sx, nr.y + (q.1 - r.y) * sy);
                }
                self.a = (nr.x, nr.y);
                self.b = (nr.x + nr.w, nr.y + nr.h);
            }
            Kind::Text(_) => {
                // Resizing text scales the font.
                if r.h > 0.0 {
                    self.style.font_size = (orig.style.font_size * nr.h / r.h).clamp(6.0, 400.0);
                }
                // fit_text then sizes the box to the text, keeping this
                // box's (possibly rotated) top-left corner in place.
                self.a = (nr.x, nr.y);
                self.b = (nr.x + nr.w, nr.y + nr.h);
            }
            _ => {
                self.a = (nr.x, nr.y);
                self.b = (nr.x + nr.w, nr.y + nr.h);
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub shapes: Vec<Shape>,
    pub crop: Option<Rect>,
    /// A resampled image that replaces the page's own (Scale Image to Visible
    /// Size). Kept here so undo puts the old image back with the old marks.
    pub image: Option<Picture>,
}

/// An image held by a document. Snapshots share it, and two compare equal
/// only if they are the same image, so undo steps stay cheap.
#[derive(Clone)]
pub struct Picture(pub gtk::cairo::ImageSurface);

impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_raw_none() == other.0.to_raw_none()
    }
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Picture({}x{})", self.0.width(), self.0.height())
    }
}

impl Document {
    /// Scales every mark and the crop by `k`, for an image resized by `k`.
    /// Text marks need `render::fit_text` afterwards to fit their new size.
    pub fn scale(&mut self, k: f64) {
        for s in &mut self.shapes {
            s.scale(k);
        }
        self.crop = self.crop.map(|r| Rect { x: (r.x * k).round(), y: (r.y * k).round(), w: (r.w * k).round().max(1.0), h: (r.h * k).round().max(1.0) });
    }

    /// Moves mark `idx` to the top (`front`) or bottom of the stack.
    /// Returns its new index.
    pub fn restack(&mut self, idx: usize, front: bool) -> usize {
        if idx >= self.shapes.len() {
            return idx;
        }
        let shape = self.shapes.remove(idx);
        if front {
            self.shapes.push(shape);
            self.shapes.len() - 1
        } else {
            self.shapes.insert(0, shape);
            0
        }
    }

    /// The topmost text mark at `p`, if any.
    pub fn text_at(&self, p: Point, tol: f64) -> Option<usize> {
        self.shapes.iter().rposition(|s| matches!(s.kind, Kind::Text(_)) && s.hit(p, tol))
    }

    /// Ends editing of mark `idx`'s text. Cancelling puts back `original`; a
    /// Text mark left with no text is removed (a shape just loses its
    /// label). Returns true if the mark was removed.
    pub fn finish_text_edit(&mut self, idx: usize, original: &str, cancel: bool) -> bool {
        let Some(shape) = self.shapes.get_mut(idx) else { return false };
        if cancel {
            shape.set_text(original.to_string());
        }
        if let Kind::Text(text) = &shape.kind {
            if text.trim().is_empty() {
                self.shapes.remove(idx);
                return true;
            }
        } else if shape.label.trim().is_empty() {
            shape.label.clear();
        }
        false
    }
}

/// Undo/redo by whole-document snapshots; documents are small.
#[derive(Default)]
pub struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
}

impl History {
    /// Records `before` as an undo step if the document changed.
    /// How many steps can be undone.
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    pub fn record(&mut self, before: Document, now: &Document) {
        if &before != now {
            self.undo.push(before);
            self.redo.clear();
        }
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(doc, prev));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(doc, next));
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_rect_from_corner() {
        let mut s = Shape::new(Kind::Rect, (10.0, 10.0), Style::default());
        s.b = (50.0, 30.0);
        let orig = s.clone();
        s.resize_from(&orig, 2, (70.0, 60.0));
        assert_eq!(s.bounds(), Rect { x: 10.0, y: 10.0, w: 60.0, h: 50.0 });
    }

    fn close(a: Point, b: Point) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn rotate_rect_keeps_box_and_turns_hit_area() {
        use std::f64::consts::FRAC_PI_2;
        let mut s = Shape::new(Kind::Rect, (0.0, 40.0), Style::default());
        s.b = (100.0, 60.0); // wide and short, centred on (50, 50)
        let orig = s.clone();
        s.rotate_from(&orig, FRAC_PI_2);
        assert_eq!(s.bounds(), orig.bounds());
        assert!((s.angle - FRAC_PI_2).abs() < 1e-9);
        // Now tall and narrow.
        assert!(s.hit((50.0, 5.0), 1.0));
        assert!(!s.hit((5.0, 50.0), 1.0));
        assert!(close(s.handles()[0], (60.0, 0.0)));
    }

    #[test]
    fn rotate_line_moves_points() {
        use std::f64::consts::FRAC_PI_2;
        let mut s = Shape::new(Kind::Line, (0.0, 50.0), Style::default());
        s.b = (100.0, 50.0);
        let orig = s.clone();
        s.rotate_from(&orig, FRAC_PI_2);
        assert_eq!(s.angle, 0.0);
        assert!(close(s.a, (50.0, 0.0)));
        assert!(close(s.b, (50.0, 100.0)));
    }

    #[test]
    fn resize_rotated_rect_keeps_opposite_corner() {
        let mut s = Shape::new(Kind::Rect, (0.0, 0.0), Style::default());
        s.b = (100.0, 50.0);
        let orig0 = s.clone();
        s.rotate_from(&orig0, 0.7);
        let orig = s.clone();
        let fixed = orig.outline()[0];
        let dragged = orig.outline()[2];
        s.resize_from(&orig, 2, (dragged.0 + 30.0, dragged.1 + 10.0));
        assert!(close(s.outline()[0], fixed));
        assert!((s.angle - 0.7).abs() < 1e-9);
    }

    #[test]
    fn border_only_optional_for_boxes() {
        let style = Style { border: false, fill: Some(Rgba::new(1.0, 1.0, 0.0, 0.3)), ..Style::default() };
        for kind in [Kind::Rect, Kind::Oval, Kind::Bubble] {
            assert!(!Shape::new(kind, (0.0, 0.0), style.clone()).draws_border());
        }
        for kind in [Kind::Line, Kind::Arrow, Kind::Pen(vec![]), Kind::Text("x".into()), Kind::Loupe { zoom: 2.0 }] {
            assert!(Shape::new(kind, (0.0, 0.0), style.clone()).draws_border());
        }
        assert!(Style::default().border);
    }

    #[test]
    fn loupe_does_not_rotate() {
        let mut s = Shape::new(Kind::Loupe { zoom: 2.0 }, (0.0, 0.0), Style::default());
        s.b = (50.0, 50.0);
        let orig = s.clone();
        s.rotate_from(&orig, 1.0);
        assert_eq!(s, orig);
    }

    #[test]
    fn hit_line() {
        let mut s = Shape::new(Kind::Line, (0.0, 0.0), Style::default());
        s.b = (100.0, 0.0);
        assert!(s.hit((50.0, 3.0), 2.0));
        assert!(!s.hit((50.0, 20.0), 2.0));
    }

    fn text(s: &str, at: Point) -> Shape {
        let mut t = Shape::new(Kind::Text(s.into()), at, Style::default());
        t.b = (at.0 + 100.0, at.1 + 40.0);
        t
    }

    #[test]
    fn text_at_finds_topmost_text_only() {
        let mut doc = Document::default();
        doc.shapes.push(text("a", (0.0, 0.0)));
        doc.shapes.push(text("b", (50.0, 0.0)));
        let mut r = Shape::new(Kind::Rect, (0.0, 0.0), Style::default());
        r.b = (200.0, 200.0);
        doc.shapes.push(r);
        assert_eq!(doc.text_at((60.0, 10.0), 1.0), Some(1));
        assert_eq!(doc.text_at((10.0, 10.0), 1.0), Some(0));
        assert_eq!(doc.text_at((10.0, 150.0), 1.0), None);
    }

    #[test]
    fn finish_text_edit_keeps_cancels_and_removes() {
        let mut doc = Document::default();
        doc.shapes.push(text("new\nlines", (0.0, 0.0)));
        assert!(!doc.finish_text_edit(0, "old", false));
        assert_eq!(doc.shapes[0].kind, Kind::Text("new\nlines".into()));

        assert!(!doc.finish_text_edit(0, "old", true));
        assert_eq!(doc.shapes[0].kind, Kind::Text("old".into()));

        doc.shapes[0].kind = Kind::Text("  ".into());
        assert!(doc.finish_text_edit(0, "old", false));
        assert!(doc.shapes.is_empty());

        // Cancelling a brand-new mark removes it.
        doc.shapes.push(text("typed", (0.0, 0.0)));
        assert!(doc.finish_text_edit(0, "", true));
        assert!(doc.shapes.is_empty());
    }

    #[test]
    fn shapes_keep_text_in_a_label() {
        let mut doc = Document::default();
        let mut r = Shape::new(Kind::Rect, (0.0, 0.0), Style::default());
        r.b = (100.0, 50.0);
        assert!(r.holds_text());
        assert_eq!((r.halign, r.valign), (HAlign::Center, VAlign::Middle));
        doc.shapes.push(r);
        doc.shapes[0].set_text("Hello".into());
        assert_eq!(doc.shapes[0].label, "Hello");
        assert!(!doc.finish_text_edit(0, "", false));
        assert_eq!(doc.shapes[0].text(), "Hello");
        // Emptying a shape's text keeps the shape.
        doc.shapes[0].set_text("  ".into());
        assert!(!doc.finish_text_edit(0, "Hello", false));
        assert_eq!(doc.shapes.len(), 1);
        assert_eq!(doc.shapes[0].label, "");
        // Cancelling puts the old label back.
        doc.shapes[0].set_text("typed".into());
        assert!(!doc.finish_text_edit(0, "Hello", true));
        assert_eq!(doc.shapes[0].label, "Hello");

        let line = Shape::new(Kind::Arrow, (0.0, 0.0), Style::default());
        assert_eq!((line.halign, line.valign), (HAlign::Center, VAlign::Top));
        assert!(!Shape::new(Kind::Pen(vec![]), (0.0, 0.0), Style::default()).holds_text());
        assert_eq!(HAlign::from_id("right"), Some(HAlign::Right));
        assert_eq!(VAlign::from_id(VAlign::Bottom.id()), Some(VAlign::Bottom));
    }

    #[test]
    fn scale_document_scales_marks_and_crop() {
        let mut doc = Document::default();
        let mut r = Shape::new(Kind::Rect, (10.0, 20.0), Style::default());
        r.b = (50.0, 60.0);
        r.angle = 0.5;
        doc.shapes.push(r);
        let mut pen = Shape::new(Kind::Pen(vec![(0.0, 0.0), (100.0, 40.0)]), (0.0, 0.0), Style::default());
        pen.b = (100.0, 40.0);
        doc.shapes.push(pen);
        doc.crop = Some(Rect { x: 10.0, y: 10.0, w: 101.0, h: 51.0 });
        doc.scale(0.5);
        let r = &doc.shapes[0];
        assert_eq!((r.a, r.b, r.angle), ((5.0, 10.0), (25.0, 30.0), 0.5));
        assert_eq!(r.style.width, Style::default().width / 2.0);
        assert_eq!(r.style.font_size, Style::default().font_size / 2.0);
        assert_eq!(doc.shapes[1].kind, Kind::Pen(vec![(0.0, 0.0), (50.0, 20.0)]));
        assert_eq!(doc.crop, Some(Rect { x: 5.0, y: 5.0, w: 51.0, h: 26.0 }));
    }

    #[test]
    fn restack_moves_mark_to_front_or_back() {
        let mut doc = Document::default();
        for x in [1.0, 2.0, 3.0] {
            doc.shapes.push(Shape::new(Kind::Rect, (x, x), Style::default()));
        }
        assert_eq!(doc.restack(0, true), 2);
        assert_eq!(doc.shapes.iter().map(|s| s.a.0).collect::<Vec<_>>(), vec![2.0, 3.0, 1.0]);
        assert_eq!(doc.restack(1, false), 0);
        assert_eq!(doc.shapes.iter().map(|s| s.a.0).collect::<Vec<_>>(), vec![3.0, 2.0, 1.0]);
        assert_eq!(doc.restack(9, true), 9, "out of range does nothing");
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut doc = Document::default();
        let mut h = History::default();
        let before = doc.clone();
        doc.shapes.push(Shape::new(Kind::Rect, (0.0, 0.0), Style::default()));
        h.record(before, &doc);
        assert!(h.undo(&mut doc));
        assert!(doc.shapes.is_empty());
        assert!(h.redo(&mut doc));
        assert_eq!(doc.shapes.len(), 1);
    }
}
