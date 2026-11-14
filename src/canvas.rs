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

//! The interactive markup canvas: draws the document and turns pointer input
//! into shapes, selection, moves, resizes and crops.

use crate::document::{Document, HAlign, History, Kind, Point, Rect, Rgba, Shape, Style, Tool, VAlign};
use crate::pages::PageState;
use crate::render;
use crate::view::{self, MARGIN};
use gtk::cairo::{self, Context, ImageSurface};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const HANDLE: f64 = 5.0; // half-size in screen pixels
const HIT_TOL: f64 = 6.0; // screen pixels
/// How far an arrow key scrolls a zoomed-in page, in screen pixels.
const ARROW_STEP: f64 = 40.0;
/// Sharp PDF rendering is skipped past this many device pixels a side.
const HIRES_MAX: i32 = 8192;
/// The largest side Scale Image to Visible Size will make, in pixels.
const SCALE_MAX: i32 = 16384;
/// Wait this long after the view stops changing before re-rendering a PDF
/// page sharply.
const HIRES_DELAY: Duration = Duration::from_millis(120);
const ACCENT: Rgba = Rgba::new(0.208, 0.518, 0.894, 1.0);
/// Shift+Alt+drag snaps rotation to steps of this size.
const RESTYLE_MERGE: Duration = Duration::from_millis(1000);
const SNAP: f64 = std::f64::consts::PI / 12.0; // 15°

/// Whether a drag with these modifiers should rotate. Alt is the documented
/// key. GNOME moves windows with Super+drag by default, but it can be set to
/// Alt (org.gnome.desktop.wm.preferences mouse-button-modifier), in which case
/// GNOME takes Alt+drag before the app sees it and Super+drag is free, so
/// Super works too.
fn rotate_modifier(mods: gdk::ModifierType) -> bool {
    mods.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK)
}

enum Drag {
    None,
    Create { idx: usize, start: Point, before: Document },
    Move { idx: usize, orig: Shape, start: Point, before: Document },
    Resize { idx: usize, handle: usize, orig: Shape, before: Document },
    /// Alt+drag: turn the mark about `center` by the pointer's change of
    /// angle since `start`.
    Rotate { idx: usize, orig: Shape, center: Point, start: Point, before: Document },
    Crop { start: Point },
    /// Space+drag: move the view. `offset` is the view's offset and `at` the
    /// pointer (screen) when the drag began.
    Pan { offset: Point, at: Point },
    /// Open the text editor for `idx` once the button is released. Opening a
    /// popover while the click is still being handled can lose it.
    EditText { idx: usize, before: Document },
}

struct State {
    image: Option<ImageSurface>,
    doc: Document,
    history: History,
    tool: Tool,
    style: Style,
    selected: Option<usize>,
    /// The last restyle of a selected mark: (mark, when, undo depth after it).
    /// Quick successive restyles of one mark (dragging the opacity slider)
    /// make one undo step.
    restyle: Option<(usize, Instant, usize)>,
    drag: Drag,
    pending_crop: Option<Rect>,
    // View transform from the last draw: screen = image * scale + offset.
    scale: f64,
    offset: Point,
    /// Zoom to fit (the scale follows the window) rather than a set zoom.
    fit: bool,
    /// The scale that counts as 100% (actual size): 1 for pictures, a PDF
    /// page's points per image pixel.
    unit: f64,
    /// The canvas size at the last draw, in screen pixels.
    viewport: (f64, f64),
    /// Where the pointer is over the canvas, for zooming about it.
    pointer: Option<Point>,
    /// Middle-drag panning: the offset when the drag began.
    pan_start: Option<Point>,
    /// Space is held: a primary drag pans instead of using the tool.
    space: bool,
    /// When and where (screen) the last primary press was, to spot a
    /// double-click in `drag_begin` itself.
    last_press: Option<(Instant, Point)>,
    /// Pinch zoom: the scale when the pinch began.
    pinch_start: f64,
    /// Draws the page as vectors (PDFs), for sharp zoomed-in views.
    painter: Option<render::Painter>,
    /// The part of a PDF page in view, rendered at the current zoom.
    hires: Option<Hires>,
}

/// Part of a PDF page rendered sharply for a zoomed-in view: `region` (image
/// pixels) at `k` device pixels per image pixel.
struct Hires {
    k: f64,
    region: Rect,
    surface: ImageSurface,
}

type Callback = Box<dyn Fn()>;

/// An open text editor for one text mark.
struct Editor {
    idx: usize,
    /// The text when editing began, put back if the edit is cancelled.
    original: String,
    /// The document to record for undo when editing finishes.
    before: Document,
    popover: gtk::Popover,
}

struct Inner {
    area: gtk::DrawingArea,
    /// The canvas with its scrollbars over it.
    overlay: gtk::Overlay,
    hbar: gtk::Scrollbar,
    vbar: gtk::Scrollbar,
    /// Set while the scrollbars are updated from the view, so their change
    /// signals don't feed back.
    syncing: Cell<bool>,
    sync_queued: Cell<bool>,
    hires_timer: RefCell<Option<glib::SourceId>>,
    last_percent: Cell<f64>,
    on_zoom: RefCell<Option<Box<dyn Fn(f64)>>>,
    state: RefCell<State>,
    editor: RefCell<Option<Editor>>,
    on_selection: RefCell<Option<Box<dyn Fn(Option<Style>)>>>,
    on_history: RefCell<Option<Callback>>,
    on_hint: RefCell<Option<Box<dyn Fn(&str)>>>,
    on_context: RefCell<Option<Box<dyn Fn(Point, Option<MarkInfo>)>>>,
}

/// The mark a context menu is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkInfo {
    /// A Text mark (rather than a shape that can hold text).
    pub is_text: bool,
    /// Whether it can hold text: a Text mark, rectangle, oval, bubble,
    /// line or arrow.
    pub holds_text: bool,
    /// Whether it has any text yet.
    pub has_text: bool,
    /// A line or arrow: its text goes above or below.
    pub is_line: bool,
    pub halign: HAlign,
    pub valign: VAlign,
    /// Whether it can move up (isn't already the top mark) or down.
    pub can_raise: bool,
    pub can_lower: bool,
}

#[derive(Clone)]
pub struct Canvas(Rc<Inner>);

impl Canvas {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).focusable(true).build();
        let hbar = gtk::Scrollbar::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::End)
            .visible(false)
            .build();
        let vbar = gtk::Scrollbar::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::End)
            .visible(false)
            .build();
        let overlay = gtk::Overlay::builder().child(&area).build();
        overlay.add_overlay(&hbar);
        overlay.add_overlay(&vbar);
        let inner = Rc::new(Inner {
            area,
            overlay,
            hbar,
            vbar,
            syncing: Cell::new(false),
            sync_queued: Cell::new(false),
            hires_timer: RefCell::new(None),
            last_percent: Cell::new(0.0),
            on_zoom: RefCell::new(None),
            state: RefCell::new(State {
                image: None,
                doc: Document::default(),
                history: History::default(),
                tool: Tool::Select,
                style: Style::default(),
                selected: None,
                restyle: None,
                drag: Drag::None,
                pending_crop: None,
                scale: 1.0,
                offset: (0.0, 0.0),
                fit: true,
                unit: 1.0,
                viewport: (0.0, 0.0),
                pointer: None,
                pan_start: None,
                space: false,
                last_press: None,
                pinch_start: 1.0,
                painter: None,
                hires: None,
            }),
            editor: RefCell::new(None),
            on_selection: RefCell::new(None),
            on_history: RefCell::new(None),
            on_hint: RefCell::new(None),
            on_context: RefCell::new(None),
        });
        let this = Canvas(inner);
        this.setup_drawing();
        this.setup_input();
        this.setup_view();
        this
    }

    /// The drawing area: give it focus and key controllers.
    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.0.area
    }

    /// The canvas and its scrollbars, to put in the window.
    pub fn root(&self) -> &gtk::Overlay {
        &self.0.overlay
    }

    fn st(&self) -> std::cell::RefMut<'_, State> {
        self.0.state.borrow_mut()
    }

    pub fn connect_selection_changed(&self, f: impl Fn(Option<Style>) + 'static) {
        *self.0.on_selection.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_history_changed(&self, f: impl Fn() + 'static) {
        *self.0.on_history.borrow_mut() = Some(Box::new(f));
    }

    /// Called with the zoom in percent of actual size when it changes.
    pub fn connect_zoom_changed(&self, f: impl Fn(f64) + 'static) {
        *self.0.on_zoom.borrow_mut() = Some(Box::new(f));
    }

    /// Called when a context menu is asked for (right-click, long-press,
    /// Shift+F10 or the Menu key), with where to show it (canvas
    /// coordinates) and the mark it is for: the one under the pointer, which
    /// is then selected, or None for the image.
    pub fn connect_context_menu(&self, f: impl Fn(Point, Option<MarkInfo>) + 'static) {
        *self.0.on_context.borrow_mut() = Some(Box::new(f));
    }

    /// The selected mark, as the context menu sees it.
    pub fn selected_mark(&self) -> Option<MarkInfo> {
        let st = self.0.state.borrow();
        let i = st.selected?;
        let shape = st.doc.shapes.get(i)?;
        Some(MarkInfo {
            is_text: matches!(shape.kind, Kind::Text(_)),
            holds_text: shape.holds_text(),
            has_text: !shape.text().is_empty(),
            is_line: shape.is_segment(),
            halign: shape.halign,
            valign: shape.valign,
            can_raise: i + 1 < st.doc.shapes.len(),
            can_lower: i > 0,
        })
    }

    /// Moves the selected mark to the top (`front`) or bottom of the stack,
    /// as one undo step.
    pub fn restack_selected(&self, front: bool) {
        self.finish_edit(false);
        {
            let mut st = self.st();
            let Some(i) = st.selected else { return };
            let before = st.doc.clone();
            let j = st.doc.restack(i, front);
            st.selected = Some(j);
            let State { history, doc, .. } = &mut *st;
            history.record(before, doc);
        }
        self.emit_selection();
        self.emit_history();
        self.redraw();
    }

    /// Sets where the selected mark's text sits (None: leave as is), as one
    /// undo step (or as part of the text edit in progress).
    pub fn align_selected(&self, halign: Option<HAlign>, valign: Option<VAlign>) {
        let editing = self.0.editor.borrow().is_some();
        {
            let mut st = self.st();
            let Some(i) = st.selected else { return };
            let before = st.doc.clone();
            let Some(shape) = st.doc.shapes.get_mut(i) else { return };
            if let Some(h) = halign {
                shape.halign = h;
            }
            if let Some(v) = valign {
                shape.valign = v;
            }
            if !editing {
                let State { history, doc, .. } = &mut *st;
                history.record(before, doc);
            }
        }
        self.emit_selection();
        self.emit_history();
        self.redraw();
    }

    /// Asks for a context menu at canvas point `at` (None: the keyboard,
    /// so at the selected mark, else the pointer or the middle of the view).
    fn context_menu(&self, at: Option<Point>) {
        self.finish_edit(false);
        let pos = {
            let mut st = self.st();
            if st.image.is_none() {
                return;
            }
            match at {
                Some(p) => {
                    // Right-clicking a mark selects it; elsewhere, the image.
                    let ip = ((p.0 - st.offset.0) / st.scale, (p.1 - st.offset.1) / st.scale);
                    st.selected = shape_at(&st, ip);
                    p
                }
                None => match st.selected.and_then(|i| st.doc.shapes.get(i)) {
                    Some(s) => {
                        let c = s.center();
                        (c.0 * st.scale + st.offset.0, c.1 * st.scale + st.offset.1)
                    }
                    None => st.pointer.unwrap_or((st.viewport.0 / 2.0, st.viewport.1 / 2.0)),
                },
            }
        };
        self.emit_selection();
        self.redraw();
        let mark = self.selected_mark();
        if let Some(f) = &*self.0.on_context.borrow() {
            f(pos, mark);
        }
    }

    pub fn connect_hint(&self, f: impl Fn(&str) + 'static) {
        *self.0.on_hint.borrow_mut() = Some(Box::new(f));
    }

    fn emit_selection(&self) {
        let style = {
            let st = self.0.state.borrow();
            st.selected.and_then(|i| st.doc.shapes.get(i)).map(|s| s.style.clone())
        };
        if let Some(f) = &*self.0.on_selection.borrow() {
            f(style);
        }
    }

    fn emit_history(&self) {
        if let Some(f) = &*self.0.on_history.borrow() {
            f();
        }
    }

    fn hint(&self, msg: &str) {
        if let Some(f) = &*self.0.on_hint.borrow() {
            f(msg);
        }
    }

    fn redraw(&self) {
        self.0.area.queue_draw();
    }

    // ----- public API -------------------------------------------------------

    /// Puts another page on the canvas: its image and its own marks and undo
    /// history. Returns the page that was there, with any text edit finished
    /// and a pending crop applied.
    pub fn swap_page(&self, image: ImageSurface, page: PageState) -> PageState {
        self.finish_edit(false);
        self.apply_crop();
        let old = {
            let mut st = self.st();
            st.image = Some(image);
            st.selected = None;
            st.pending_crop = None;
            st.restyle = None;
            st.drag = Drag::None;
            st.painter = None;
            st.hires = None;
            let old = PageState {
                doc: std::mem::replace(&mut st.doc, page.doc),
                history: std::mem::replace(&mut st.history, page.history),
            };
            // Zoomed in, a new page starts at its top.
            if let Some(vis) = visible_rect(&st) {
                st.offset.1 = MARGIN - vis.y * st.scale;
                st.offset.0 = (st.viewport.0 - vis.w * st.scale) / 2.0 - vis.x * st.scale;
                layout(&mut st, &vis);
            }
            old
        };
        self.emit_selection();
        self.emit_history();
        self.redraw();
        old
    }

    /// Finishes a text edit and applies a pending crop, so the document is
    /// final (before saving).
    pub fn commit(&self) {
        self.finish_edit(false);
        self.apply_crop();
    }

    pub fn set_tool(&self, tool: Tool) {
        self.finish_edit(false);
        let had_crop = {
            let mut st = self.st();
            st.tool = tool;
            if tool != Tool::Select {
                st.selected = None;
            }
            st.pending_crop.is_some() && tool != Tool::Crop
        };
        if had_crop {
            self.apply_crop();
        }
        if tool == Tool::Crop {
            self.hint("Drag to choose the area to keep, then press Enter");
        }
        self.emit_selection();
        self.redraw();
    }

    /// Whether the Border toggle means anything for the selected mark, or for
    /// new marks from the current tool.
    pub fn border_is_optional(&self) -> bool {
        let st = self.st();
        match st.selected.and_then(|i| st.doc.shapes.get(i)) {
            Some(shape) => shape.kind.has_optional_border(),
            None => st.tool == Tool::Select || st.tool.has_optional_border(),
        }
    }

    /// Sets the style for new shapes, and applies it to the selected shape.
    pub fn set_style(&self, style: Style) {
        let editing = self.0.editor.borrow().is_some();
        let changed = {
            let mut st = self.st();
            st.style = style.clone();
            let before = st.doc.clone();
            if let Some(i) = st.selected {
                let shape = &mut st.doc.shapes[i];
                shape.style = style;
                render::fit_text(shape);
            }
            // While editing, the edit's own undo step covers the restyle.
            if !editing {
                let merge = match (st.selected, st.restyle) {
                    (Some(i), Some((j, at, depth))) => {
                        i == j && at.elapsed() < RESTYLE_MERGE && st.history.depth() == depth
                    }
                    _ => false,
                };
                if !merge {
                    let depth = st.history.depth();
                    let State { history, doc, .. } = &mut *st;
                    history.record(before, doc);
                    // Only a recorded step can absorb the next restyles.
                    st.restyle = None;
                    if st.history.depth() > depth {
                        st.restyle = st.selected.map(|i| (i, Instant::now(), st.history.depth()));
                    }
                } else if let Some(r) = &mut st.restyle {
                    r.1 = Instant::now();
                }
            }
            true
        };
        if changed {
            self.emit_history();
            self.redraw();
        }
    }

    pub fn undo(&self) {
        self.finish_edit(false);
        let ok = {
            let mut st = self.st();
            let width = image_width(&st);
            let State { history, doc, .. } = &mut *st;
            let ok = history.undo(doc);
            st.selected = None;
            st.pending_crop = None;
            keep_shown_size(&mut st, width);
            ok
        };
        if ok {
            self.emit_selection();
            self.emit_history();
            self.redraw();
        }
    }

    pub fn redo(&self) {
        self.finish_edit(false);
        let ok = {
            let mut st = self.st();
            let width = image_width(&st);
            let State { history, doc, .. } = &mut *st;
            let ok = history.redo(doc);
            st.selected = None;
            keep_shown_size(&mut st, width);
            ok
        };
        if ok {
            self.emit_selection();
            self.emit_history();
            self.redraw();
        }
    }

    pub fn can_undo(&self) -> bool {
        self.0.state.borrow().history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.0.state.borrow().history.can_redo()
    }

    pub fn delete_selected(&self) {
        {
            let mut st = self.st();
            let Some(i) = st.selected.take() else { return };
            let before = st.doc.clone();
            st.doc.shapes.remove(i);
            let State { history, doc, .. } = &mut *st;
            history.record(before, doc);
        }
        self.emit_selection();
        self.emit_history();
        self.redraw();
    }

    pub fn apply_crop(&self) {
        {
            let mut st = self.st();
            let Some(rect) = st.pending_crop.take() else { return };
            let visible = visible_rect(&st);
            if let Some(r) = visible.and_then(|v| v.intersect(&rect))
                && r.w >= 2.0 && r.h >= 2.0 {
                    let before = st.doc.clone();
                    st.doc.crop = Some(Rect { x: r.x.round(), y: r.y.round(), w: r.w.round(), h: r.h.round() });
                    let State { history, doc, .. } = &mut *st;
                    history.record(before, doc);
                }
        }
        self.emit_history();
        self.redraw();
    }

    /// Renders the result at full resolution, applying any pending crop first.
    pub fn export(&self) -> Option<Result<gdk::Texture, cairo::Error>> {
        self.finish_edit(false);
        self.apply_crop();
        let st = self.0.state.borrow();
        let image = current_image(&st)?;
        Some(render::export(image, &st.doc))
    }

    /// Resamples the image (and scales its marks) to the size it is shown
    /// at, so the page looks the same but 100% is now this size. One undo
    /// step. Returns the new size, None if it already is that size.
    pub fn scale_to_visible(&self) -> Result<Option<(i32, i32)>, String> {
        self.finish_edit(false);
        self.apply_crop();
        let size = {
            let mut st = self.st();
            let Some(image) = current_image(&st).cloned() else { return Err("There is no image to scale".into()) };
            let k = st.scale / st.unit;
            let (w0, h0) = (f64::from(image.width()), f64::from(image.height()));
            let (w, h) = ((w0 * k).round().max(1.0), (h0 * k).round().max(1.0));
            if w == w0 && h == h0 {
                return Ok(None);
            }
            if w.max(h) > f64::from(SCALE_MAX) {
                return Err(format!("At this zoom the image would be over {SCALE_MAX} pixels wide or high"));
            }
            let err = |e: cairo::Error| format!("Could not scale the image: {e}");
            let out = ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).map_err(err)?;
            {
                let cr = Context::new(&out).map_err(err)?;
                cr.scale(w / w0, h / h0);
                cr.set_source_surface(&image, 0.0, 0.0).map_err(err)?;
                cr.source().set_filter(cairo::Filter::Best);
                cr.set_operator(cairo::Operator::Source);
                cr.paint().map_err(err)?;
            }
            out.flush();
            let before = st.doc.clone();
            st.doc.scale(k);
            for shape in &mut st.doc.shapes {
                render::fit_text(shape);
            }
            st.doc.image = Some(crate::document::Picture(out));
            let State { history, doc, .. } = &mut *st;
            history.record(before, doc);
            st.selected = None;
            st.pending_crop = None;
            st.restyle = None;
            st.hires = None;
            // Same size on screen: the zoom becomes 100%.
            st.scale /= k;
            relayout(&mut st);
            (w as i32, h as i32)
        };
        self.emit_selection();
        self.emit_history();
        self.redraw();
        Ok(Some(size))
    }

    /// The size of the result in image pixels: the image, or its crop.
    pub fn result_size(&self) -> Option<(f64, f64)> {
        visible_rect(&self.0.state.borrow()).map(|r| (r.w, r.h))
    }

    /// Whether the page is cropped.
    pub fn is_cropped(&self) -> bool {
        self.0.state.borrow().doc.crop.is_some()
    }

    /// The current page's document.
    pub fn document(&self) -> Document {
        self.0.state.borrow().doc.clone()
    }

    // ----- zoom and scrolling -----------------------------------------------

    /// Sets what 100% means for the page on the canvas (`unit`, the scale of
    /// actual size) and, for PDFs, how to draw it sharply when zoomed in.
    pub fn set_page_view(&self, unit: f64, painter: Option<render::Painter>) {
        {
            let mut st = self.st();
            st.unit = unit;
            st.painter = painter;
            st.hires = None;
        }
        self.redraw();
    }

    /// The zoom in percent of actual size.
    pub fn zoom_percent(&self) -> f64 {
        let st = self.0.state.borrow();
        st.scale / st.unit * 100.0
    }

    /// Zooms to `scale` (screen pixels per image pixel), keeping the image
    /// point under `anchor` (screen) still; the middle of the view if None.
    pub fn zoom_to(&self, scale: f64, anchor: Option<Point>) {
        {
            let mut st = self.st();
            let Some(vis) = visible_rect(&st) else { return };
            if self.0.area.width() > 0 {
                st.viewport = (f64::from(self.0.area.width()), f64::from(self.0.area.height()));
            }
            let a = anchor.unwrap_or((st.viewport.0 / 2.0, st.viewport.1 / 2.0));
            let p = ((a.0 - st.offset.0) / st.scale, (a.1 - st.offset.1) / st.scale);
            let unit = st.unit;
            let s = scale.clamp(view::MIN_PERCENT / 100.0 * unit, view::MAX_PERCENT / 100.0 * unit);
            st.fit = false;
            st.scale = s;
            st.offset = (view::offset_about(a.0, p.0)(s), view::offset_about(a.1, p.1)(s));
            layout(&mut st, &vis);
        }
        self.redraw();
    }

    fn zoom_to_percent(&self, percent: f64) {
        let unit = self.0.state.borrow().unit;
        self.zoom_to(percent / 100.0 * unit, None);
    }

    pub fn zoom_in(&self) {
        self.zoom_to_percent(view::step_in(self.zoom_percent()));
    }

    pub fn zoom_out(&self) {
        self.zoom_to_percent(view::step_out(self.zoom_percent()));
    }

    /// Actual size: 1 image pixel per screen pixel, or a PDF page at its
    /// printed size (1 point per screen pixel).
    pub fn zoom_actual(&self) {
        self.zoom_to_percent(100.0);
    }

    /// Fits the page in the window, and keeps it fitted as the window changes.
    pub fn zoom_fit(&self) {
        self.st().fit = true;
        self.redraw();
    }

    /// Scrolls the view by (dx, dy) screen pixels. Returns false if the page
    /// fits, so there is nothing to scroll.
    fn pan_by(&self, dx: f64, dy: f64) -> bool {
        {
            let mut st = self.st();
            let Some(vis) = visible_rect(&st) else { return false };
            let (w, h) = st.viewport;
            if !view::overflows(vis.w, st.scale, w) && !view::overflows(vis.h, st.scale, h) {
                return false;
            }
            st.offset.0 -= dx;
            st.offset.1 -= dy;
            layout(&mut st, &vis);
        }
        self.redraw();
        true
    }

    fn device_scale(&self) -> f64 {
        self.0.area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale())
    }

    fn queue_sync(&self) {
        if self.0.sync_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        glib::idle_add_local_once(move || {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                c.0.sync_queued.set(false);
                c.sync_view();
            }
        });
    }

    /// Brings the scrollbars, the zoom readout, an open text editor and the
    /// sharp PDF rendering up to date with the view.
    fn sync_view(&self) {
        let (percent, bars, editor_rect) = {
            let st = self.0.state.borrow();
            let Some(vis) = visible_rect(&st) else { return };
            let (w, h) = st.viewport;
            let bars = [
                (view::overflows(vis.w, st.scale, w), view::scrollbar(vis.x, vis.w, st.scale, w, st.offset.0)),
                (view::overflows(vis.h, st.scale, h), view::scrollbar(vis.y, vis.h, st.scale, h, st.offset.1)),
            ];
            let editor_rect = self
                .0
                .editor
                .borrow()
                .as_ref()
                .and_then(|e| st.doc.shapes.get(e.idx))
                .map(|s| screen_rect(&st, &outline_bounds(s)));
            (st.scale / st.unit * 100.0, bars, editor_rect)
        };

        self.0.syncing.set(true);
        for (bar, (show, (upper, page, value))) in [(&self.0.hbar, bars[0]), (&self.0.vbar, bars[1])] {
            bar.set_visible(show);
            if show {
                bar.adjustment().configure(value, 0.0, upper, page * 0.1, page * 0.9, page);
            }
        }
        self.0.syncing.set(false);

        if (percent - self.0.last_percent.get()).abs() > 1e-6 {
            self.0.last_percent.set(percent);
            if let Some(f) = &*self.0.on_zoom.borrow() {
                f(percent);
            }
        }

        if let (Some(ed), Some(rect)) = (&*self.0.editor.borrow(), editor_rect)
            && ed.popover.pointing_to().1 != rect
        {
            ed.popover.set_pointing_to(Some(&rect));
        }

        self.update_hires();
    }

    /// Drops the sharp rendering when it isn't needed, or schedules a new one
    /// when it no longer matches the view.
    fn update_hires(&self) {
        let k = self.device_scale() * self.0.state.borrow().scale;
        let stale = {
            let mut st = self.st();
            let Some(vis) = visible_rect(&st) else { return };
            if st.painter.is_none() || k <= 1.01 {
                // The raster is sharp enough at this zoom.
                st.hires = None;
                false
            } else {
                match view_rect(&st).intersect(&vis) {
                    None => false,
                    Some(wanted) => !st
                        .hires
                        .as_ref()
                        .is_some_and(|h| (h.k - k).abs() <= 1e-6 * k && covers(&h.region, &wanted)),
                }
            }
        };
        if !stale {
            return;
        }
        if let Some(id) = self.0.hires_timer.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(HIRES_DELAY, move || {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                c.0.hires_timer.borrow_mut().take();
                c.render_hires();
            }
        });
        *self.0.hires_timer.borrow_mut() = Some(id);
    }

    /// Renders the part of the PDF page in view (and a little around it) at
    /// the current zoom.
    fn render_hires(&self) {
        let dev = self.device_scale();
        let (painter, region, k) = {
            let st = self.0.state.borrow();
            let Some(painter) = st.painter.clone() else { return };
            let Some(vis) = visible_rect(&st) else { return };
            let k = st.scale * dev;
            if k <= 1.01 {
                return;
            }
            let v = view_rect(&st);
            let pad = 0.1;
            let padded = Rect { x: v.x - v.w * pad, y: v.y - v.h * pad, w: v.w * (1.0 + 2.0 * pad), h: v.h * (1.0 + 2.0 * pad) };
            let Some(mut r) = padded.intersect(&vis) else { return };
            // Start on a whole device pixel, so it lines up with the screen.
            let snap = |x: f64, off: f64| ((x * st.scale + off) * dev).floor() / dev;
            let (x0, y0) = ((snap(r.x, st.offset.0) - st.offset.0) / st.scale, (snap(r.y, st.offset.1) - st.offset.1) / st.scale);
            r.w += r.x - x0;
            r.h += r.y - y0;
            r.x = x0;
            r.y = y0;
            (painter, r, k)
        };
        let (pw, ph) = ((region.w * k).ceil() as i32, (region.h * k).ceil() as i32);
        if pw <= 0 || ph <= 0 || pw > HIRES_MAX || ph > HIRES_MAX {
            return;
        }
        let Ok(surface) = ImageSurface::create(cairo::Format::Rgb24, pw, ph) else { return };
        {
            let Ok(cr) = Context::new(&surface) else { return };
            cr.scale(k, k);
            cr.translate(-region.x, -region.y);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let _ = cr.paint();
            painter(&cr);
        }
        surface.flush();
        self.st().hires = Some(Hires { k, region, surface });
        self.redraw();
    }

    fn setup_view(&self) {
        // Scrollbars move the view.
        for horizontal in [true, false] {
            let bar = if horizontal { &self.0.hbar } else { &self.0.vbar };
            let weak = Rc::downgrade(&self.0);
            bar.adjustment().connect_value_changed(move |adj| {
                let Some(i) = weak.upgrade() else { return };
                let c = Canvas(i);
                if c.0.syncing.get() {
                    return;
                }
                {
                    let mut st = c.st();
                    let Some(vis) = visible_rect(&st) else { return };
                    let scale = st.scale;
                    if horizontal {
                        st.offset.0 = view::offset_for_scroll(vis.x, scale, adj.value());
                    } else {
                        st.offset.1 = view::offset_for_scroll(vis.y, scale, adj.value());
                    }
                    relayout(&mut st);
                }
                c.redraw();
            });
        }

        // Scrolling pans; Ctrl+scroll zooms about the pointer.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = Rc::downgrade(&self.0);
        scroll.connect_scroll(move |ctl, dx, dy| {
            let Some(i) = weak.upgrade() else { return glib::Propagation::Proceed };
            let c = Canvas(i);
            let mods = ctl.current_event_state();
            let wheel = ctl.unit() == gdk::ScrollUnit::Wheel;
            if mods.contains(gdk::ModifierType::CONTROL_MASK) {
                let factor = if wheel { 1.2f64.powf(-dy) } else { (-dy * 0.01).exp() };
                let (scale, pointer) = {
                    let st = c.0.state.borrow();
                    (st.scale, st.pointer)
                };
                c.zoom_to(scale * factor, pointer);
                return glib::Propagation::Stop;
            }
            let (mut dx, mut dy) = if wheel { (dx * 48.0, dy * 48.0) } else { (dx, dy) };
            if wheel && mods.contains(gdk::ModifierType::SHIFT_MASK) && dx == 0.0 {
                std::mem::swap(&mut dx, &mut dy);
            }
            if c.pan_by(dx, dy) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.0.area.add_controller(scroll);

        // Middle-drag pans.
        let pan = gtk::GestureDrag::new();
        pan.set_button(gdk::BUTTON_MIDDLE);
        let weak = Rc::downgrade(&self.0);
        pan.connect_drag_begin(move |_, _, _| {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                let offset = c.0.state.borrow().offset;
                c.st().pan_start = Some(offset);
                c.0.area.set_cursor_from_name(Some("grabbing"));
            }
        });
        let weak = Rc::downgrade(&self.0);
        pan.connect_drag_update(move |_, dx, dy| {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                {
                    let mut st = c.st();
                    let Some(start) = st.pan_start else { return };
                    st.offset = (start.0 + dx, start.1 + dy);
                    relayout(&mut st);
                }
                c.redraw();
            }
        });
        let weak = Rc::downgrade(&self.0);
        pan.connect_drag_end(move |_, _, _| {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                c.st().pan_start = None;
                c.0.area.set_cursor_from_name(Some("default"));
            }
        });
        self.0.area.add_controller(pan);

        // Pinch zooms (touchpad or touchscreen).
        let pinch = gtk::GestureZoom::new();
        let weak = Rc::downgrade(&self.0);
        pinch.connect_begin(move |_, _| {
            if let Some(i) = weak.upgrade() {
                let mut st = i.state.borrow_mut();
                st.pinch_start = st.scale;
            }
        });
        let weak = Rc::downgrade(&self.0);
        pinch.connect_scale_changed(move |g, s| {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                let start = c.0.state.borrow().pinch_start;
                c.zoom_to(start * s, g.bounding_box_center());
            }
        });
        self.0.area.add_controller(pinch);
    }

    // ----- drawing ----------------------------------------------------------

    fn setup_drawing(&self) {
        let weak = Rc::downgrade(&self.0);
        self.0.area.set_draw_func(move |_, cr, w, h| {
            if let Some(inner) = weak.upgrade() {
                Canvas(inner).draw(cr, w as f64, h as f64);
            }
        });
    }

    fn draw(&self, cr: &Context, w: f64, h: f64) {
        let mut st = self.st();
        let Some(image) = current_image(&st).cloned() else { return };
        let Some(vis) = visible_rect(&st) else { return };
        st.viewport = (w, h);
        layout(&mut st, &vis);
        let (scale, offset) = (st.scale, st.offset);

        // Drop shadow behind the page.
        let (sx, sy) = (vis.x * scale + offset.0, vis.y * scale + offset.1);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.15);
        cr.rectangle(sx + 1.0, sy + 2.0, vis.w * scale, vis.h * scale);
        let _ = cr.fill();

        let _ = cr.save();
        cr.translate(offset.0, offset.1);
        cr.scale(scale, scale);
        cr.rectangle(vis.x, vis.y, vis.w, vis.h);
        cr.clip();
        // Zoomed into a PDF, the part in view is drawn sharp over the raster.
        let hires = st.hires.as_ref().map(|h| (h.surface.clone(), h.region, h.k));
        let background = move |cr: &Context| {
            render::paint_image(cr, &image);
            if let Some((surface, r, k)) = &hires {
                let _ = cr.save();
                cr.rectangle(r.x, r.y, r.w, r.h);
                cr.clip();
                cr.translate(r.x, r.y);
                cr.scale(1.0 / k, 1.0 / k);
                let _ = cr.set_source_surface(surface, 0.0, 0.0);
                cr.source().set_filter(cairo::Filter::Good);
                let _ = cr.paint();
                let _ = cr.restore();
            }
        };
        // Draw into a group so the highlighter's multiply only affects the image.
        cr.push_group();
        render::draw_page(cr, &background, &st.doc);
        let _ = cr.pop_group_to_source();
        let _ = cr.paint();
        let _ = cr.restore();

        let _ = cr.save();
        cr.translate(offset.0, offset.1);
        cr.scale(scale, scale);
        if let Some(sel) = st.selected.and_then(|i| st.doc.shapes.get(i)) {
            draw_selection(cr, sel, scale);
        }
        if let Some(crop) = st.pending_crop {
            draw_crop_overlay(cr, &vis, &crop, scale);
        }
        let _ = cr.restore();
        drop(st);
        // Scrollbars, zoom readout and so on follow the view once drawn.
        self.queue_sync();
    }

    // ----- input ------------------------------------------------------------

    fn to_image(&self, x: f64, y: f64) -> Point {
        let st = self.0.state.borrow();
        ((x - st.offset.0) / st.scale, (y - st.offset.1) / st.scale)
    }

    fn setup_input(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(&self.0);
        drag.connect_drag_begin(move |g, x, y| {
            if let Some(i) = weak.upgrade() {
                Canvas(i).drag_begin(x, y, g.current_event_state());
            }
        });
        let weak = Rc::downgrade(&self.0);
        drag.connect_drag_update(move |g, dx, dy| {
            if let (Some(i), Some((sx, sy))) = (weak.upgrade(), g.start_point()) {
                Canvas(i).drag_update(sx + dx, sy + dy, g.current_event_state());
            }
        });
        let weak = Rc::downgrade(&self.0);
        drag.connect_drag_end(move |g, dx, dy| {
            if let (Some(i), Some((sx, sy))) = (weak.upgrade(), g.start_point()) {
                Canvas(i).drag_end(sx + dx, sy + dy, g.current_event_state());
            }
        });
        self.0.area.add_controller(drag);

        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&self.0);
        click.connect_pressed(move |_, n, x, y| {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                // Don't pull focus away from an open text editor.
                if c.0.editor.borrow().is_none() {
                    c.0.area.grab_focus();
                }
                if n == 2 {
                    c.double_click(x, y);
                }
            }
        });
        self.0.area.add_controller(click);

        // Context menu: right-click, or a long press on a touch screen.
        let menu_click = gtk::GestureClick::new();
        menu_click.set_button(gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(&self.0);
        menu_click.connect_pressed(move |g, _, x, y| {
            if let Some(i) = weak.upgrade() {
                g.set_state(gtk::EventSequenceState::Claimed);
                let c = Canvas(i);
                c.0.area.grab_focus();
                c.context_menu(Some((x, y)));
            }
        });
        self.0.area.add_controller(menu_click);
        let long_press = gtk::GestureLongPress::new();
        long_press.set_touch_only(true);
        let weak = Rc::downgrade(&self.0);
        long_press.connect_pressed(move |g, x, y| {
            if let Some(i) = weak.upgrade() {
                g.set_state(gtk::EventSequenceState::Claimed);
                Canvas(i).context_menu(Some((x, y)));
            }
        });
        self.0.area.add_controller(long_press);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&self.0);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(i) = weak.upgrade() else { return glib::Propagation::Proceed };
            let c = Canvas(i);
            match key {
                gdk::Key::Menu => c.context_menu(None),
                gdk::Key::space if mods.is_empty() => {
                    let fresh = {
                        let mut st = c.st();
                        let fresh = !st.space && st.image.is_some();
                        st.space |= fresh;
                        fresh && st.pan_start.is_none()
                    };
                    if fresh {
                        c.0.area.set_cursor_from_name(Some("grab"));
                    }
                }
                gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down
                | gdk::Key::KP_Left | gdk::Key::KP_Right | gdk::Key::KP_Up | gdk::Key::KP_Down
                    if mods.is_empty() || mods == gdk::ModifierType::SHIFT_MASK =>
                {
                    // Arrow keys scroll a zoomed-in page; Shift scrolls further.
                    let step = if mods.is_empty() { ARROW_STEP } else { ARROW_STEP * 4.0 };
                    let (dx, dy) = match key {
                        gdk::Key::Left | gdk::Key::KP_Left => (-step, 0.0),
                        gdk::Key::Right | gdk::Key::KP_Right => (step, 0.0),
                        gdk::Key::Up | gdk::Key::KP_Up => (0.0, -step),
                        _ => (0.0, step),
                    };
                    if !c.pan_by(dx, dy) {
                        return glib::Propagation::Proceed;
                    }
                    c.redraw();
                }
                gdk::Key::F10 if mods.contains(gdk::ModifierType::SHIFT_MASK) => c.context_menu(None),
                gdk::Key::Delete | gdk::Key::BackSpace => c.delete_selected(),
                gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::F2 => {
                    // Enter applies a waiting crop; otherwise Enter or F2
                    // edits the selected text mark.
                    let crop = key != gdk::Key::F2 && c.0.state.borrow().pending_crop.is_some();
                    if crop {
                        c.apply_crop();
                    } else if !c.edit_selected_text() {
                        return glib::Propagation::Proceed;
                    }
                }
                gdk::Key::Escape => {
                    {
                        let mut st = c.st();
                        st.pending_crop = None;
                        st.selected = None;
                    }
                    c.emit_selection();
                    c.redraw();
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&self.0);
        keys.connect_key_released(move |_, key, _, _| {
            if key == gdk::Key::space
                && let Some(i) = weak.upgrade()
            {
                Canvas(i).end_space();
            }
        });
        self.0.area.add_controller(keys);
        // Space may be let go after focus has moved elsewhere.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&self.0);
        focus.connect_leave(move |_| {
            if let Some(i) = weak.upgrade() {
                Canvas(i).end_space();
            }
        });
        self.0.area.add_controller(focus);

        // Show a move cursor over shapes when selecting.
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&self.0);
        motion.connect_motion(move |m, x, y| {
            if let Some(i) = weak.upgrade() {
                let c = Canvas(i);
                c.st().pointer = Some((x, y));
                if c.0.state.borrow().pan_start.is_some() {
                    return;
                }
                let name = c.cursor_at(x, y, m.current_event_state());
                c.0.area.set_cursor_from_name(Some(name));
            }
        });
        let weak = Rc::downgrade(&self.0);
        motion.connect_leave(move |_| {
            if let Some(i) = weak.upgrade() {
                Canvas(i).st().pointer = None;
            }
        });
        self.0.area.add_controller(motion);
    }

    /// Space was let go: back to the tool's cursor, unless still dragging
    /// (the drag ends the pan when the button is released).
    fn end_space(&self) {
        let pointer = {
            let mut st = self.st();
            if !st.space {
                return;
            }
            st.space = false;
            if st.pan_start.is_some() { None } else { st.pointer }
        };
        let name = match pointer {
            Some((x, y)) => self.cursor_at(x, y, gdk::ModifierType::empty()),
            None if self.0.state.borrow().pan_start.is_some() => return,
            None => "default",
        };
        self.0.area.set_cursor_from_name(Some(name));
    }

    fn cursor_at(&self, x: f64, y: f64, mods: gdk::ModifierType) -> &'static str {
        let p = self.to_image(x, y);
        let st = self.0.state.borrow();
        if st.image.is_none() {
            return "default";
        }
        if st.space {
            return "grab";
        }
        if rotate_modifier(mods) && rotate_target(&st, p).is_some() {
            return "grab";
        }
        if handle_at(&st, p).is_some() {
            return "crosshair";
        }
        match st.tool {
            Tool::Select if shape_at(&st, p).is_some() => "move",
            Tool::Select => "default",
            Tool::Text => "text",
            _ => "crosshair",
        }
    }

    fn drag_begin(&self, x: f64, y: f64, mods: gdk::ModifierType) {
        self.finish_edit(false);
        let p = self.to_image(x, y);
        let mut st = self.st();
        if st.image.is_none() {
            return;
        }
        if st.space {
            st.drag = Drag::Pan { offset: st.offset, at: (x, y) };
            st.pan_start = Some(st.offset);
            drop(st);
            self.0.area.set_cursor_from_name(Some("grabbing"));
            return;
        }
        let before = st.doc.clone();

        // A double-click on a mark that holds text edits it, whatever the
        // tool. Spotted here rather than by the click gesture, whose order
        // against this drag gesture isn't reliable: either way round, the
        // first press's new mark (an empty text mark, a tiny shape) is gone by
        // now, and the editor opens once the button is released.
        let now = Instant::now();
        let double = st.last_press.is_some_and(|(at, q)| is_double_click(at, q, now, (x, y)));
        st.last_press = if double { None } else { Some((now, (x, y))) };
        if double
            && st.tool != Tool::Crop
            && st.pending_crop.is_none()
            && !rotate_modifier(mods)
            && let Some(idx) = shape_at(&st, p).filter(|&i| st.doc.shapes[i].holds_text())
        {
            st.selected = Some(idx);
            st.drag = Drag::EditText { idx, before };
            drop(st);
            self.emit_selection();
            self.redraw();
            return;
        }

        // Alt+drag on a mark (or anywhere, with a mark selected) rotates it.
        if rotate_modifier(mods)
            && let Some(idx) = rotate_target(&st, p)
        {
            let orig = st.doc.shapes[idx].clone();
            let center = orig.center();
            st.selected = Some(idx);
            st.drag = Drag::Rotate { idx, orig, center, start: p, before };
            drop(st);
            self.0.area.set_cursor_from_name(Some("grabbing"));
            self.emit_selection();
            self.redraw();
            return;
        }

        // Grabbing a handle of the selected shape works with any tool.
        if let (Some(idx), Some(handle)) = (st.selected, handle_at(&st, p)) {
            let orig = st.doc.shapes[idx].clone();
            st.drag = Drag::Resize { idx, handle, orig, before };
            return;
        }

        let tool = st.tool;
        match tool {
            Tool::Select => {
                st.selected = shape_at(&st, p);
                st.drag = match st.selected {
                    Some(idx) => Drag::Move { idx, orig: st.doc.shapes[idx].clone(), start: p, before },
                    None => Drag::None,
                };
                drop(st);
                self.emit_selection();
                self.redraw();
            }
            Tool::Crop => {
                st.pending_crop = Some(Rect::from_points(p, p));
                st.drag = Drag::Crop { start: p };
            }
            Tool::Text => {
                // Clicking existing text selects it (and can move it); clicking
                // it again, or double-clicking, edits it. Opening the editor on
                // the first click would let the second click of a double-click
                // dismiss it again.
                let tol = HIT_TOL / st.scale;
                if let Some(idx) = st.doc.text_at(p, tol) {
                    if st.selected == Some(idx) {
                        st.drag = Drag::EditText { idx, before };
                    } else {
                        st.selected = Some(idx);
                        st.drag = Drag::Move { idx, orig: st.doc.shapes[idx].clone(), start: p, before };
                        drop(st);
                        self.emit_selection();
                        self.redraw();
                    }
                    return;
                }
                let mut shape = Shape::new(Kind::Text(String::new()), p, st.style.clone());
                render::fit_text(&mut shape);
                st.doc.shapes.push(shape);
                let idx = st.doc.shapes.len() - 1;
                st.selected = Some(idx);
                st.drag = Drag::EditText { idx, before };
                drop(st);
                self.redraw();
            }
            _ => {
                let kind = match tool {
                    Tool::Pen => Kind::Pen(vec![p]),
                    Tool::Highlighter => Kind::Highlighter(vec![p]),
                    Tool::Line => Kind::Line,
                    Tool::Arrow => Kind::Arrow,
                    Tool::Rect => Kind::Rect,
                    Tool::Oval => Kind::Oval,
                    Tool::Bubble => Kind::Bubble,
                    Tool::Loupe => Kind::Loupe { zoom: 2.0 },
                    _ => unreachable!(),
                };
                let mut style = st.style.clone();
                if tool == Tool::Highlighter {
                    // Highlights start yellow; recolour by selecting one.
                    style.stroke = Rgba::new(0.965, 0.827, 0.176, 1.0);
                }
                st.doc.shapes.push(Shape::new(kind, p, style));
                let idx = st.doc.shapes.len() - 1;
                st.selected = None;
                st.drag = Drag::Create { idx, start: p, before };
            }
        }
    }

    fn drag_update(&self, x: f64, y: f64, mods: gdk::ModifierType) {
        let p = self.to_image(x, y);
        let mut st = self.st();
        if let Drag::Pan { offset, at } = st.drag {
            st.offset = (offset.0 + x - at.0, offset.1 + y - at.1);
            relayout(&mut st);
            drop(st);
            self.redraw();
            return;
        }
        let State { drag, doc, pending_crop, .. } = &mut *st;
        match drag {
            Drag::None | Drag::EditText { .. } | Drag::Pan { .. } => return,
            Drag::Create { idx, start, .. } => {
                let shape = &mut doc.shapes[*idx];
                match &mut shape.kind {
                    Kind::Pen(pts) | Kind::Highlighter(pts) => pts.push(p),
                    Kind::Loupe { .. } => {
                        let s = (p.0 - start.0).abs().max((p.1 - start.1).abs());
                        shape.b = (start.0 + s.copysign(p.0 - start.0), start.1 + s.copysign(p.1 - start.1));
                    }
                    _ => shape.b = p,
                }
            }
            Drag::Move { idx, orig, start, .. } => {
                let mut s = orig.clone();
                s.translate(p.0 - start.0, p.1 - start.1);
                doc.shapes[*idx] = s;
            }
            Drag::Resize { idx, handle, orig, .. } => {
                let (idx, handle) = (*idx, *handle);
                doc.shapes[idx].resize_from(orig, handle, p);
                render::fit_text(&mut doc.shapes[idx]);
            }
            Drag::Rotate { idx, orig, center, start, .. } => {
                let turn = |q: Point| (q.1 - center.1).atan2(q.0 - center.0);
                let mut delta = turn(p) - turn(*start);
                if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    // Snap the resulting angle (or, for marks rotated by
                    // moving their points, the turn itself).
                    let base = orig.angle;
                    delta = ((base + delta) / SNAP).round() * SNAP - base;
                }
                doc.shapes[*idx].rotate_from(orig, delta);
            }
            Drag::Crop { start } => *pending_crop = Some(Rect::from_points(*start, p)),
        }
        drop(st);
        self.redraw();
    }

    fn drag_end(&self, x: f64, y: f64, mods: gdk::ModifierType) {
        self.drag_update(x, y, mods);
        if matches!(self.0.state.borrow().drag, Drag::Pan { .. }) {
            let space = {
                let mut st = self.st();
                st.drag = Drag::None;
                st.pan_start = None;
                st.space
            };
            self.0.area.set_cursor_from_name(Some(if space { "grab" } else { "default" }));
            return;
        }
        let mut st = self.st();
        let drag = std::mem::replace(&mut st.drag, Drag::None);
        let scale = st.scale;
        match drag {
            Drag::Create { idx, before, .. } => {
                let shape = &st.doc.shapes[idx];
                let r = shape.bounds();
                let tiny = !shape.is_freehand() && r.w.max(r.h) * scale < 4.0;
                if tiny {
                    st.doc = before;
                } else {
                    st.selected = Some(idx);
                    let State { history, doc, .. } = &mut *st;
                    history.record(before, doc);
                }
            }
            Drag::Move { before, .. } | Drag::Resize { before, .. } | Drag::Rotate { before, .. } => {
                let State { history, doc, .. } = &mut *st;
                history.record(before, doc);
            }
            Drag::Crop { .. } => {
                if st.pending_crop.is_some_and(|r| r.w * scale < 4.0 || r.h * scale < 4.0) {
                    st.pending_crop = None;
                }
            }
            Drag::None | Drag::Pan { .. } => {}
            Drag::EditText { idx, before } => {
                drop(st);
                self.emit_selection();
                self.redraw();
                self.edit_text(idx, before);
                return;
            }
        }
        drop(st);
        self.emit_selection();
        self.emit_history();
        self.redraw();
    }

    /// Double-clicking applies a waiting crop. (Double-clicking a mark to
    /// edit its text is handled in `drag_begin`.)
    fn double_click(&self, _x: f64, _y: f64) {
        if self.0.state.borrow().pending_crop.is_some() {
            self.apply_crop();
        }
    }

    /// Starts editing the selected mark's text, if it can hold text.
    /// Returns false otherwise.
    pub fn edit_selected_text(&self) -> bool {
        let (idx, before) = {
            let st = self.0.state.borrow();
            let Some(idx) = st.selected else { return false };
            if !st.doc.shapes.get(idx).is_some_and(Shape::holds_text) {
                return false;
            }
            (idx, st.doc.clone())
        };
        self.edit_text(idx, before);
        true
    }

    /// Closes the text editor, if open. Cancelling restores the text it had
    /// when editing began. One undo step covers the whole edit.
    fn finish_edit(&self, cancel: bool) {
        // Take it first: popping down fires `closed`, which calls back here.
        let Some(ed) = self.0.editor.borrow_mut().take() else { return };
        glib::g_debug!("pygoscribe", "text edit finished: mark {}, cancel {cancel}", ed.idx);
        {
            let mut st = self.st();
            if st.doc.finish_text_edit(ed.idx, &ed.original, cancel) {
                st.selected = None;
            } else if let Some(shape) = st.doc.shapes.get_mut(ed.idx) {
                render::fit_text(shape);
            }
            let State { history, doc, .. } = &mut *st;
            history.record(ed.before, doc);
        }
        ed.popover.popdown();
        let pop = ed.popover.clone();
        glib::idle_add_local_once(move || pop.unparent());
        self.emit_selection();
        self.emit_history();
        self.redraw();
        self.0.area.grab_focus();
    }

    /// Opens an editor for the text of mark `idx` (a Text mark, or the
    /// label of a shape or line). `before` is the document to
    /// record for undo when editing finishes. Enter finishes, Shift+Enter
    /// starts a new line, Esc puts the old text back, clicking away keeps it.
    fn edit_text(&self, idx: usize, before: Document) {
        if self.0.editor.borrow().as_ref().is_some_and(|e| e.idx == idx) {
            return; // already open (e.g. double-click then release)
        }
        self.finish_edit(false);
        let (text, rect, mark) = {
            let st = self.0.state.borrow();
            let Some(shape) = st.doc.shapes.get(idx).filter(|s| s.holds_text()) else { return };
            let mark = (matches!(shape.kind, Kind::Text(_)), shape.is_segment(), shape.halign, shape.valign);
            (shape.text().to_string(), screen_rect(&st, &outline_bounds(shape)), mark)
        };

        let view = gtk::TextView::builder()
            .accepts_tab(false)
            .wrap_mode(gtk::WrapMode::None)
            .top_margin(4)
            .bottom_margin(4)
            .left_margin(6)
            .right_margin(6)
            .build();
        view.buffer().set_text(&text);
        let (start, end) = view.buffer().bounds();
        view.buffer().select_range(&start, &end);

        let hint = gtk::Label::builder()
            .label("Enter to finish · Shift+Enter for a new line · Esc to cancel")
            .css_classes(["dim-label", "caption"])
            .xalign(0.0)
            .build();
        // A fixed-size scrolling area: GTK closes a popover at once if its
        // minimum size grows past the popup's current size (as a TextView's
        // does when it is first laid out, or gains a line), so the popover's
        // size must not depend on the text.
        let frame = gtk::ScrolledWindow::builder()
            .child(&view)
            .has_frame(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(280)
            .min_content_height(84)
            .max_content_width(280)
            .max_content_height(84)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&frame);
        content.append(&self.alignment_buttons(mark));
        content.append(&hint);

        // Not autohide, so the editor stays open while the style controls are
        // used (restyling the text as you type) and takes no popup grab.
        // Clicks elsewhere on the canvas, tool changes, undo and so on finish
        // the edit instead (see finish_edit callers).
        let popover = gtk::Popover::builder()
            .child(&content)
            .position(gtk::PositionType::Bottom)
            .autohide(false)
            .build();
        popover.set_parent(&self.0.area);
        popover.set_pointing_to(Some(&rect));

        let this = self.clone();
        view.buffer().connect_changed(move |b| {
            let text = b.text(&b.start_iter(), &b.end_iter(), false).to_string();
            {
                let mut st = this.st();
                if let Some(shape) = st.doc.shapes.get_mut(idx) {
                    shape.set_text(text);
                    render::fit_text(shape);
                }
            }
            this.redraw();
        });

        // Capture phase, so Enter and Esc reach us before the text view and
        // the popover's own Esc handling.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = self.clone();
        let buffer = view.buffer();
        keys.connect_key_pressed(move |_, key, _, mods| match key {
            gdk::Key::Escape => {
                this.finish_edit(true);
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    // Insert the line break ourselves: GtkTextView doesn't
                    // reliably treat Shift+Enter as a new line.
                    buffer.delete_selection(true, true);
                    buffer.insert_at_cursor("\n");
                } else {
                    this.finish_edit(false);
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        popover.add_controller(keys);

        let this = self.clone();
        popover.connect_closed(move |_| {
            glib::g_debug!("pygoscribe", "text editor closed");
            this.finish_edit(false);
        });

        *self.0.editor.borrow_mut() = Some(Editor { idx, original: text, before, popover: popover.clone() });
        glib::g_debug!("pygoscribe", "text editor opening for mark {idx} at {rect:?}");
        popover.popup();
        view.grab_focus();
    }
}

impl Canvas {
    /// The text editor's alignment buttons for a mark described by
    /// (is a Text mark, is a line, halign, valign): left/centre/right, and
    /// top/middle/bottom for a box or above/below for a line. A Text mark
    /// only lines up its lines left, centre or right.
    fn alignment_buttons(&self, (is_text, is_line, halign, valign): (bool, bool, HAlign, VAlign)) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let group = |items: Vec<(&str, &str, bool, Option<HAlign>, Option<VAlign>)>| {
            let linked = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            linked.add_css_class("linked");
            let mut first: Option<gtk::ToggleButton> = None;
            for (icon, tip, active, h, v) in items {
                // Clicking keeps the focus in the text, so typing carries on.
                let b = gtk::ToggleButton::builder().tooltip_text(tip).active(active).focus_on_click(false).build();
                if icon.ends_with("-symbolic") {
                    b.set_icon_name(icon);
                } else {
                    b.set_label(icon);
                }
                b.set_group(first.as_ref());
                first.get_or_insert_with(|| b.clone());
                let this = self.clone();
                b.connect_toggled(move |b| {
                    if b.is_active() {
                        this.align_selected(h, v);
                    }
                });
                linked.append(&b);
            }
            linked
        };
        let horizontal: Vec<_> = [
            ("format-justify-left-symbolic", "Align Left", HAlign::Left),
            ("format-justify-center-symbolic", "Centre", HAlign::Center),
            ("format-justify-right-symbolic", "Align Right", HAlign::Right),
        ]
        .into_iter()
        .map(|(icon, tip, a)| (icon, tip, halign == a, Some(a), None))
        .collect();
        row.append(&group(horizontal));
        if is_line {
            let below = valign == VAlign::Bottom;
            row.append(&group(vec![
                ("Above", "Text Above the Line", !below, None, Some(VAlign::Top)),
                ("Below", "Text Below the Line", below, None, Some(VAlign::Bottom)),
            ]));
        } else if !is_text {
            row.append(&group(vec![
                ("Top", "Align Top", valign == VAlign::Top, None, Some(VAlign::Top)),
                ("Middle", "Align Middle", valign == VAlign::Middle, None, Some(VAlign::Middle)),
                ("Bottom", "Align Bottom", valign == VAlign::Bottom, None, Some(VAlign::Bottom)),
            ]));
        }
        row
    }
}

/// Whether a press at `p` (screen) at `now` makes a double-click with the
/// one at `q` at `at`, by the desktop's double-click time and distance.
fn is_double_click(at: Instant, q: Point, now: Instant, p: Point) -> bool {
    let (time, dist) = gtk::Settings::default()
        .map(|s| (s.gtk_double_click_time(), s.gtk_double_click_distance()))
        .unwrap_or((400, 5));
    now.duration_since(at) <= Duration::from_millis(time.max(0) as u64)
        && (p.0 - q.0).abs() <= f64::from(dist)
        && (p.1 - q.1).abs() <= f64::from(dist)
}

/// After undo or redo swapped the image for one of another size, zooms so
/// the page stays the size it was on screen (`width` is the old image's).
fn keep_shown_size(st: &mut State, width: Option<f64>) {
    if let (Some(old), Some(new)) = (width, image_width(st))
        && old != new
        && new > 0.0
    {
        st.scale *= old / new;
        relayout(st);
    }
}

/// Fits the view to the window (zoom to fit) or keeps the page in view.
fn layout(st: &mut State, vis: &Rect) {
    let (w, h) = st.viewport;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    if st.fit {
        st.scale = view::fit_scale((vis.w, vis.h), (w, h));
    }
    st.offset.0 = view::clamp_offset(vis.x, vis.w, st.scale, w, st.offset.0);
    st.offset.1 = view::clamp_offset(vis.y, vis.h, st.scale, h, st.offset.1);
}

fn relayout(st: &mut State) {
    if let Some(vis) = visible_rect(st) {
        layout(st, &vis);
    }
}

/// An image-pixel rectangle on screen.
fn screen_rect(st: &State, r: &Rect) -> gdk::Rectangle {
    gdk::Rectangle::new(
        (r.x * st.scale + st.offset.0) as i32,
        (r.y * st.scale + st.offset.1) as i32,
        (r.w * st.scale).max(1.0) as i32,
        (r.h * st.scale).max(1.0) as i32,
    )
}

/// The part of the image in view, in image pixels.
fn view_rect(st: &State) -> Rect {
    Rect {
        x: -st.offset.0 / st.scale,
        y: -st.offset.1 / st.scale,
        w: st.viewport.0 / st.scale,
        h: st.viewport.1 / st.scale,
    }
}

fn covers(outer: &Rect, inner: &Rect) -> bool {
    const EPS: f64 = 0.5;
    inner.x >= outer.x - EPS
        && inner.y >= outer.y - EPS
        && inner.x + inner.w <= outer.x + outer.w + EPS
        && inner.y + inner.h <= outer.y + outer.h + EPS
}

/// The image being marked up: the page's own, or the resampled one that
/// replaced it.
fn current_image(st: &State) -> Option<&ImageSurface> {
    st.doc.image.as_ref().map(|p| &p.0).or(st.image.as_ref())
}

fn image_width(st: &State) -> Option<f64> {
    current_image(st).map(|i| f64::from(i.width()))
}

fn visible_rect(st: &State) -> Option<Rect> {
    let img = current_image(st)?;
    Some(st.doc.crop.unwrap_or(Rect { x: 0.0, y: 0.0, w: img.width() as f64, h: img.height() as f64 }))
}

fn shape_at(st: &State, p: Point) -> Option<usize> {
    let tol = HIT_TOL / st.scale;
    st.doc.shapes.iter().rposition(|s| s.hit(p, tol) || render::hits_line_label(s, p, tol))
}

/// The mark Alt+drag at `p` would rotate: the one under the pointer, else
/// the selected one.
fn rotate_target(st: &State, p: Point) -> Option<usize> {
    shape_at(st, p).or(st.selected).filter(|&i| st.doc.shapes.get(i).is_some_and(Shape::can_rotate))
}

/// Upright box around a mark as drawn (rotation included).
fn outline_bounds(shape: &Shape) -> Rect {
    let pts = shape.outline();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

fn handle_at(st: &State, p: Point) -> Option<usize> {
    let shape = st.doc.shapes.get(st.selected?)?;
    let tol = (HANDLE + 3.0) / st.scale;
    shape.handles().iter().position(|h| (h.0 - p.0).abs() <= tol && (h.1 - p.1).abs() <= tol)
}

fn draw_selection(cr: &Context, shape: &Shape, scale: f64) {
    let px = 1.0 / scale;
    cr.set_source_rgba(ACCENT.r, ACCENT.g, ACCENT.b, 1.0);
    cr.set_line_width(px);
    if !shape.is_segment() {
        let [q0, q1, q2, q3] = shape.outline();
        cr.set_dash(&[4.0 * px, 3.0 * px], 0.0);
        cr.move_to(q0.0, q0.1);
        cr.line_to(q1.0, q1.1);
        cr.line_to(q2.0, q2.1);
        cr.line_to(q3.0, q3.1);
        cr.close_path();
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
    } else if let Some([q0, q1, q2, q3]) = render::line_label_outline(shape) {
        // A line's text, faintly, so it's clear it belongs to the line.
        cr.set_dash(&[2.0 * px, 3.0 * px], 0.0);
        cr.move_to(q0.0, q0.1);
        cr.line_to(q1.0, q1.1);
        cr.line_to(q2.0, q2.1);
        cr.line_to(q3.0, q3.1);
        cr.close_path();
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
    }
    for (hx, hy) in shape.handles() {
        let s = HANDLE * px;
        cr.rectangle(hx - s, hy - s, 2.0 * s, 2.0 * s);
        cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(ACCENT.r, ACCENT.g, ACCENT.b, 1.0);
        cr.set_line_width(1.5 * px);
        let _ = cr.stroke();
    }
}

fn draw_crop_overlay(cr: &Context, vis: &Rect, crop: &Rect, scale: f64) {
    let px = 1.0 / scale;
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.rectangle(vis.x, vis.y, vis.w, vis.h);
    cr.rectangle(crop.x, crop.y, crop.w, crop.h);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
    let _ = cr.fill();
    cr.set_fill_rule(cairo::FillRule::Winding);
    cr.rectangle(crop.x, crop.y, crop.w, crop.h);
    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    cr.set_line_width(1.5 * px);
    cr.set_dash(&[6.0 * px, 4.0 * px], 0.0);
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Pdf;

    fn pump(ms: u64) {
        let ctx = glib::MainContext::default();
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Draws the canvas as a 700×500 view into a PNG.
    fn save_view(c: &Canvas, path: &std::path::Path) {
        let surface = ImageSurface::create(cairo::Format::ARgb32, 700, 500).unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            cr.set_source_rgb(0.92, 0.92, 0.92);
            cr.paint().unwrap();
            c.draw(&cr, 700.0, 500.0);
        }
        render::texture_from_surface(surface).unwrap().save_to_png(path).unwrap();
    }

    /// Zooms a PDF page and writes what the canvas shows to PNGs for a look,
    /// and checks a mark stays under the same image point. GTK needs a
    /// display: `GDK_BACKEND=broadway` with `gtk4-broadwayd` works headless.
    /// Run with `cargo test zoomed_view -- --ignored`.
    #[test]
    #[ignore]
    fn zoomed_view() {
        gtk::init().unwrap();
        let dir = std::env::temp_dir().join(format!("pygoscribe-zoom-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("text.pdf");
        {
            let surface = cairo::PdfSurface::new(400.0, 300.0, &path).unwrap();
            let cr = Context::new(&surface).unwrap();
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.set_font_size(14.0);
            for i in 0..12 {
                cr.move_to(20.0, 30.0 + f64::from(i) * 20.0);
                cr.show_text("The quick brown fox jumps over the lazy dog 0123456789").unwrap();
            }
            drop(cr);
            surface.finish();
        }
        let pdf = Pdf::open(&gtk::gio::File::for_path(&path)).unwrap();
        let (tex, ppp) = pdf.render(0).unwrap();
        let image = render::surface_from_texture(&tex).unwrap();
        let painter: render::Painter = Rc::new(pdf.painter(0).unwrap());

        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        c.set_page_view(ppp, Some(painter));
        {
            let mut st = c.st();
            let mut mark = Shape::new(Kind::Rect, (40.0, 40.0), Style::default());
            mark.b = (200.0, 120.0);
            st.doc.shapes.push(mark);
            st.selected = Some(0);
        }
        pump(300);
        save_view(&c, &dir.join("1-fit.png"));
        pump(100);
        let fit = c.zoom_percent();

        // Zoom to 400% about a screen point: that image point stays put.
        let anchor = (300.0, 200.0);
        let before = c.to_image(anchor.0, anchor.1);
        c.zoom_to(4.0 * ppp, Some(anchor));
        // Without a browser attached, Broadway doesn't draw by itself.
        save_view(&c, &dir.join("2-zoom400-raster.png"));
        pump(400);
        assert!((c.zoom_percent() - 400.0).abs() < 1e-6);
        let after = c.to_image(anchor.0, anchor.1);
        assert!((before.0 - after.0).abs() < 1e-6 && (before.1 - after.1).abs() < 1e-6);
        assert!(c.0.state.borrow().hires.is_some(), "PDF re-rendered sharply");
        assert!(c.0.hbar.is_visible() && c.0.vbar.is_visible());
        save_view(&c, &dir.join("2-zoom400.png"));

        // The mark is still hit where it is drawn: its corner on screen.
        let (sx, sy) = {
            let st = c.0.state.borrow();
            (40.0 * st.scale + st.offset.0, 40.0 * st.scale + st.offset.1)
        };
        let p = c.to_image(sx, sy);
        assert_eq!(handle_at(&c.0.state.borrow(), p), Some(0));

        // Scrolling moves the view but not the marks.
        assert!(c.pan_by(50.0, 30.0));
        pump(300);
        save_view(&c, &dir.join("3-panned.png"));

        c.zoom_fit();
        save_view(&c, &dir.join("4-fit.png"));
        pump(300);
        assert!((c.zoom_percent() - fit).abs() < 1e-6);
        assert!(!c.0.hbar.is_visible());
        eprintln!("snapshots in {}", dir.display());
    }

    /// Scale Image to Visible Size resamples the picture and its marks to
    /// the zoomed size, keeps it the same size on screen, and undoes in one
    /// step. Needs a display, like `zoomed_view`.
    #[test]
    #[ignore]
    fn scale_to_visible_size() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 200).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        {
            let mut st = c.st();
            let mut mark = Shape::new(Kind::Rect, (40.0, 20.0), Style::default());
            mark.b = (200.0, 120.0);
            st.doc.shapes.push(mark);
        }
        c.zoom_to(0.5, None);
        let shown = |c: &Canvas| {
            let st = c.0.state.borrow();
            (st.doc.shapes[0].a.0 * st.scale + st.offset.0, st.doc.shapes[0].b.0 * st.scale + st.offset.0)
        };
        let before = shown(&c);
        assert_eq!(c.scale_to_visible(), Ok(Some((200, 100))));
        assert!((c.zoom_percent() - 100.0).abs() < 1e-9);
        assert_eq!(shown(&c), before, "same place on screen");
        assert_eq!(c.document().shapes[0].b, (100.0, 60.0));
        let tex = c.export().unwrap().unwrap();
        assert_eq!((tex.width(), tex.height()), (200, 100));
        assert_eq!(c.scale_to_visible(), Ok(None), "already that size");

        c.undo();
        let tex = c.export().unwrap().unwrap();
        assert_eq!((tex.width(), tex.height()), (400, 200));
        assert!((c.zoom_percent() - 50.0).abs() < 1e-9, "undo keeps the size on screen");
        assert_eq!(c.document().shapes[0].b, (200.0, 120.0));
        c.redo();
        assert_eq!(c.export().unwrap().unwrap().width(), 200);
    }

    /// Right-clicking a mark selects it and offers its menu; elsewhere, the
    /// image's. Bring to Front and Send to Back are one undo step each.
    #[test]
    #[ignore]
    fn context_menu_targets() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 200).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        {
            let mut st = c.st();
            for x in [10.0, 200.0] {
                let mut mark = Shape::new(Kind::Rect, (x, 10.0), Style::default());
                mark.b = (x + 100.0, 100.0);
                st.doc.shapes.push(mark);
            }
        }
        c.zoom_to(1.0, None);
        let got = Rc::new(RefCell::new(None));
        let g = got.clone();
        c.connect_context_menu(move |_, m| *g.borrow_mut() = Some(m));
        let screen = |p: Point| {
            let st = c.0.state.borrow();
            (p.0 * st.scale + st.offset.0, p.1 * st.scale + st.offset.1)
        };
        let rect = MarkInfo {
            is_text: false,
            holds_text: true,
            has_text: false,
            is_line: false,
            halign: HAlign::Center,
            valign: VAlign::Middle,
            can_raise: true,
            can_lower: false,
        };
        c.context_menu(Some(screen((10.0, 50.0))));
        assert_eq!(*got.borrow(), Some(Some(rect)));
        c.restack_selected(true);
        assert_eq!(c.document().shapes[1].a, (10.0, 10.0));
        assert_eq!(c.selected_mark(), Some(MarkInfo { can_raise: false, can_lower: true, ..rect }));
        // Aligning the text is one undo step.
        c.align_selected(Some(HAlign::Left), Some(VAlign::Bottom));
        assert_eq!(c.selected_mark().map(|m| (m.halign, m.valign)), Some((HAlign::Left, VAlign::Bottom)));
        c.undo();
        assert_eq!(c.document().shapes[1].halign, HAlign::Center);
        c.undo();
        c.undo();
        assert_eq!(c.document().shapes[0].a, (10.0, 10.0));
        c.context_menu(Some(screen((150.0, 150.0))));
        assert_eq!(*got.borrow(), Some(None), "empty spot: the image menu");
        assert_eq!(c.selected_mark(), None);
    }

    /// With Space held, a primary drag moves the view and leaves the marks
    /// alone; arrow keys scroll too.
    #[test]
    #[ignore]
    fn space_drag_pans() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 2000, 2000).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        c.set_tool(Tool::Rect);
        c.st().viewport = (600.0, 400.0);
        c.zoom_to(1.0, None);
        let start = c.0.state.borrow().offset;
        c.st().space = true;
        let none = gdk::ModifierType::empty();
        c.drag_begin(300.0, 200.0, none);
        c.drag_update(250.0, 170.0, none);
        c.drag_end(250.0, 170.0, none);
        let st = c.0.state.borrow();
        assert_eq!(st.offset, (start.0 - 50.0, start.1 - 30.0));
        assert!(st.doc.shapes.is_empty(), "no rectangle drawn");
        assert!(st.pan_start.is_none() && matches!(st.drag, Drag::None));
        drop(st);
        c.end_space();
        assert!(c.pan_by(0.0, ARROW_STEP));
        assert_eq!(c.0.state.borrow().offset.1, start.1 - 30.0 - ARROW_STEP);
    }

    /// Double-clicking a rectangle or line edits its text with any tool,
    /// including Text (which would otherwise start a new text mark).
    #[test]
    #[ignore]
    fn double_click_edits_shape_text() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 200).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        {
            let mut st = c.st();
            let mut rect = Shape::new(Kind::Rect, (10.0, 10.0), Style::default());
            rect.b = (150.0, 100.0);
            st.doc.shapes.push(rect);
            let mut line = Shape::new(Kind::Line, (200.0, 150.0), Style::default());
            line.b = (380.0, 150.0);
            st.doc.shapes.push(line);
        }
        c.zoom_to(1.0, None);
        let screen = |p: Point| {
            let st = c.0.state.borrow();
            (p.0 * st.scale + st.offset.0, p.1 * st.scale + st.offset.1)
        };
        let none = gdk::ModifierType::empty();
        for tool in [Tool::Select, Tool::Rect, Tool::Line, Tool::Text] {
            for (idx, at) in [(0, (60.0, 50.0)), (1, (300.0, 150.0))] {
                c.set_tool(tool);
                let (x, y) = screen(at);
                for _ in 0..2 {
                    c.drag_begin(x, y, none);
                    c.drag_end(x, y, none);
                }
                let editing = c.0.editor.borrow().as_ref().map(|e| e.idx);
                assert_eq!(editing, Some(idx), "{tool:?} on mark {idx}");
                c.finish_edit(true);
                assert_eq!(c.document().shapes.len(), 2, "{tool:?}: no stray marks");
                c.st().last_press = None;
            }
        }
    }
}
