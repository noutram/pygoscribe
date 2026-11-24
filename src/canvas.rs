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
//!
//! # Structure
//!
//! [`Canvas`] is a cheap-to-clone handle (`Rc<Inner>`) around a
//! `gtk::DrawingArea` with two overlay scrollbars. All mutable state is in
//! [`State`], behind a `RefCell`. GTK calls back into the canvas from signal
//! handlers, which can nest (a redraw while a drag is being handled, a
//! callback into the window that calls the canvas again), so methods take
//! the borrow in a short block, drop it, and only then emit callbacks or
//! queue a redraw. That pattern is used throughout and is what keeps the
//! `RefCell` from panicking.
//!
//! # Coordinates
//!
//! Marks live in image pixels. The view maps them to the widget as
//! `screen = image * scale + offset` (see [`crate::view`]), recomputed on
//! every draw. Pointer events arrive in screen pixels and are turned back
//! into image pixels before hit testing. "100 %" is `scale == unit`: one image
//! pixel per screen pixel for a picture, or a PDF page at its printed size.
//!
//! # Editing and undo
//!
//! Every operation clones the document into `before`, changes it, and calls
//! `History::record(before, &doc)`, which keeps the step only if something
//! changed. A drag keeps its `before` in [`Drag`] until the button is
//! released, so a whole drag is one undo step. Rapid restyles of one mark
//! (dragging the opacity slider) are merged into one step (`restyle`). While
//! a text editor is open, the edit records one step for everything done in it.
//!
//! # Sharp PDF zoom
//!
//! A PDF page is rendered once at 144 dpi for editing. When zoomed in past
//! that, the canvas waits until the view stops moving ([`HIRES_DELAY`]) and
//! then renders just the visible region again with Poppler at the screen's
//! resolution ([`Hires`]), drawing it over the blurry base image.

use crate::document::{self, Document, EndsFor, HAlign, History, Kind, Point, Rect, Rgba, Shape, Style, Tool, VAlign};
use crate::pages::PageState;
use crate::render;
use crate::view::{self, MARGIN};
use gtk::cairo::{self, Context, ImageSurface};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Half the size of a selection handle, in screen pixels.
const HANDLE: f64 = 5.0;
/// How near (in screen pixels) a click must be to count as on a mark.
const HIT_TOL: f64 = 6.0;
/// How far an arrow key scrolls a zoomed-in page, in screen pixels.
const ARROW_STEP: f64 = 40.0;
/// How far (screen pixels, down and right) each paste or duplicate is
/// moved from the marks it copies, so it doesn't hide exactly on top.
const PASTE_STEP: f64 = 20.0;
/// The most of the shown page, across and down, an inserted picture takes
/// at first (a smaller one keeps its own size).
const IMAGE_ROOM: f64 = 0.8;
/// Sharp PDF rendering is skipped past this many device pixels a side.
const HIRES_MAX: i32 = 8192;
/// The largest side Scale Image to Visible Size will make, in pixels.
const SCALE_MAX: i32 = 16384;
/// Wait this long after the view stops changing before re-rendering a PDF
/// page sharply.
const HIRES_DELAY: Duration = Duration::from_millis(120);
/// The selection colour: GNOME's blue accent.
const ACCENT: Rgba = Rgba::new(0.208, 0.518, 0.894, 1.0);
/// Restyles of the same mark less than this far apart share one undo step.
const RESTYLE_MERGE: Duration = Duration::from_millis(1000);
/// Whether a drag with these modifiers should rotate. Alt is the documented
/// key. GNOME moves windows with Super+drag by default, but it can be set to
/// Alt (org.gnome.desktop.wm.preferences mouse-button-modifier), in which case
/// GNOME takes Alt+drag before the app sees it and Super+drag is free, so
/// Super works too.
fn rotate_modifier(mods: gdk::ModifierType) -> bool {
    mods.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK)
}

/// What the current primary-button drag is doing. Variants that change the
/// document carry `before`, the document when the drag began, to record as
/// one undo step when it ends, and `orig`, the mark as it was, so each
/// motion event recomputes from the start (no accumulated error).
enum Drag {
    /// No drag in progress.
    None,
    /// Drawing new mark `idx` (already pushed onto the document), from
    /// `start` in image pixels.
    Create { idx: usize, start: Point, before: Document },
    /// Moving the selected marks, each (index, as it was); `start` is where
    /// the pointer went down.
    Move { group: Vec<(usize, Shape)>, start: Point, before: Document },
    /// Dragging out a marquee from `start` with Select: the marks inside it
    /// are selected, along with `base` (those selected before, when Shift
    /// or Ctrl was held).
    Marquee { start: Point, base: Vec<usize> },
    /// Dragging handle `handle` of mark `idx` (see `Shape::handles`).
    Resize { idx: usize, handle: usize, orig: Shape, before: Document },
    /// Alt+drag: turn the mark about `center` by the pointer's change of
    /// angle since `start`.
    Rotate { idx: usize, orig: Shape, center: Point, start: Point, before: Document },
    /// Choosing a crop rectangle from `start`; the result is `pending_crop`.
    Crop { start: Point },
    /// Space+drag: move the view. `offset` is the view's offset and `at` the
    /// pointer (screen) when the drag began.
    Pan { offset: Point, at: Point },
    /// Open the text editor for `idx` once the button is released. Opening a
    /// popover while the click is still being handled can lose it.
    EditText { idx: usize, before: Document },
}

/// Everything the canvas knows, behind one `RefCell` (see the module docs).
struct State {
    /// The page's image as opened (or rendered from the PDF). `None` while
    /// the window is empty. Scale to Visible Size puts its result in
    /// `doc.image`, which takes precedence (see `current_image`).
    image: Option<ImageSurface>,
    /// The current page's marks and crop.
    doc: Document,
    /// The current page's undo and redo stacks.
    history: History,
    /// The tool chosen in the tool bar.
    tool: Tool,
    /// The style for new marks.
    style: Style,
    /// Index of the selected mark in `doc.shapes`: with several selected,
    /// the main one, whose style the tool bar shows and whose handles are
    /// drawn (handles only when it is the only one).
    selected: Option<usize>,
    /// The other selected marks (Shift/Ctrl+click, a marquee, Ctrl+A),
    /// never including `selected`, and empty when `selected` is None. Set
    /// through [`select_one`] and [`select_many`], which keep it so.
    also: Vec<usize>,
    /// The marquee being dragged out with Select, in image pixels.
    marquee: Option<Rect>,
    /// The last restyle of a selected mark: (mark, when, undo depth after it).
    /// Quick successive restyles of one mark (dragging the opacity slider)
    /// make one undo step.
    restyle: Option<(usize, Instant, usize)>,
    /// The drag in progress.
    drag: Drag,
    /// A crop rectangle drawn but not yet applied (Enter or double-click
    /// applies it, Esc drops it).
    pending_crop: Option<Rect>,
    /// Screen pixels per image pixel, from the last draw.
    scale: f64,
    /// Screen position of image point (0, 0), from the last draw.
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
    /// Where the last context menu was asked for, in image pixels: its
    /// Paste puts the marks there.
    menu_point: Option<Point>,
    /// Draws the page as vectors (PDFs), for sharp zoomed-in views.
    painter: Option<render::Painter>,
    /// The part of a PDF page in view, rendered at the current zoom.
    hires: Option<Hires>,
}

/// Part of a PDF page rendered sharply for a zoomed-in view: `region` (image
/// pixels) at `k` device pixels per image pixel.
struct Hires {
    /// Device pixels per image pixel it was rendered at.
    k: f64,
    /// The part of the page it covers, in image pixels.
    region: Rect,
    /// The rendered pixels.
    surface: ImageSurface,
}

/// A callback with no arguments.
type Callback = Box<dyn Fn()>;

/// An open text editor for one text mark.
struct Editor {
    /// The mark being edited.
    idx: usize,
    /// The text when editing began, put back if the edit is cancelled.
    original: String,
    /// The document to record for undo when editing finishes.
    before: Document,
    /// The popover holding the text view and alignment buttons.
    popover: gtk::Popover,
    /// Where the text is typed; cut, copy and paste go to it while open.
    view: gtk::TextView,
}

/// The canvas's widgets, state and callbacks to the window.
struct Inner {
    /// Where the page is drawn and input arrives.
    area: gtk::DrawingArea,
    /// The canvas with its scrollbars over it.
    overlay: gtk::Overlay,
    /// Horizontal scrollbar, shown only when the page overflows.
    hbar: gtk::Scrollbar,
    /// Vertical scrollbar, likewise.
    vbar: gtk::Scrollbar,
    /// Set while the scrollbars are updated from the view, so their change
    /// signals don't feed back.
    syncing: Cell<bool>,
    /// A scrollbar update is already queued for after this draw.
    sync_queued: Cell<bool>,
    /// The pending sharp-PDF render, restarted whenever the view moves.
    hires_timer: RefCell<Option<glib::SourceId>>,
    /// The zoom last reported to `on_zoom`, so it only fires on change.
    last_percent: Cell<f64>,
    /// Told the zoom (percent) when it changes: updates the header label.
    on_zoom: RefCell<Option<Box<dyn Fn(f64)>>>,
    /// See [`State`].
    state: RefCell<State>,
    /// The open text editor, if any.
    editor: RefCell<Option<Editor>>,
    /// Told the selected mark's style (or `None`) when the selection changes,
    /// so the tool bar can show it.
    on_selection: RefCell<Option<Box<dyn Fn(Option<Style>)>>>,
    /// Told when undo/redo availability may have changed.
    on_history: RefCell<Option<Callback>>,
    /// Shows a short hint (a toast) to the user.
    on_hint: RefCell<Option<Box<dyn Fn(&str)>>>,
    /// Opens the context menu (see [`Canvas::connect_context_menu`]).
    on_context: RefCell<Option<Box<dyn Fn(Point, Option<MarkInfo>)>>>,
}

/// The mark a context menu is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkInfo {
    /// How many marks are selected (the menu acts on all of them).
    pub count: usize,
    /// A Text mark (rather than a shape that can hold text).
    pub is_text: bool,
    /// Whether it can hold text: a Text mark, rectangle, oval, bubble,
    /// line or arrow.
    pub holds_text: bool,
    /// Whether it has any text yet.
    pub has_text: bool,
    /// A line or arrow: its text goes above or below.
    pub is_line: bool,
    /// Its text's horizontal alignment.
    pub halign: HAlign,
    /// Its text's vertical alignment (above/below for a line).
    pub valign: VAlign,
    /// Whether it can move up (isn't already the top mark).
    pub can_raise: bool,
    /// Whether it can move down (isn't already the bottom mark).
    pub can_lower: bool,
}

/// A handle to the markup canvas; clones share the same canvas.
#[derive(Clone)]
pub struct Canvas(Rc<Inner>);

impl Canvas {
    /// An empty canvas with the Select tool, ready for `swap_page`.
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
        // Scrollbars float over the canvas (like GTK's overlay scrolling)
        // rather than taking space from it, so showing them doesn't change
        // the viewport and re-fit the page.
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
                also: Vec::new(),
                marquee: None,
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
                menu_point: None,
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

    /// Borrows the state mutably. Keep the borrow short (see module docs).
    fn st(&self) -> std::cell::RefMut<'_, State> {
        self.0.state.borrow_mut()
    }

    /// Called with the selected mark's style, or `None` when nothing is
    /// selected.
    pub fn connect_selection_changed(&self, f: impl Fn(Option<Style>) + 'static) {
        *self.0.on_selection.borrow_mut() = Some(Box::new(f));
    }

    /// Called whenever undo or redo may have become (un)available.
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
        let sel = selection(&st);
        let (n, len) = (sel.len(), st.doc.shapes.len());
        // Text commands apply to one mark at a time.
        let one = n == 1;
        Some(MarkInfo {
            count: n,
            is_text: one && matches!(shape.kind, Kind::Text(_)),
            holds_text: one && shape.holds_text(),
            has_text: one && !shape.text().is_empty(),
            is_line: shape.is_segment(),
            halign: shape.halign,
            valign: shape.valign,
            // Already the top (bottom) marks, in a block: nothing to move.
            can_raise: sel.iter().copied().ne(len - n..len),
            can_lower: sel.iter().copied().ne(0..n),
        })
    }

    /// Moves the selected mark to the top (`front`) or bottom of the stack,
    /// as one undo step.
    pub fn restack_selected(&self, front: bool) {
        self.reorder_selected(|doc, sel| doc.restack_many(sel, front));
    }

    /// Moves the selected marks one place up (`up`, Bring Forward) or down
    /// (Send Backward) the stack, as one undo step.
    pub fn step_selected(&self, up: bool) {
        self.reorder_selected(|doc, sel| doc.step_many(sel, up));
    }

    /// Reorders the selected marks with `f`, which returns their new
    /// indices, as one undo step, keeping them selected.
    fn reorder_selected(&self, f: impl FnOnce(&mut Document, &[usize]) -> Vec<usize>) {
        self.finish_edit(false);
        {
            let mut st = self.st();
            let sel = selection(&st);
            if sel.is_empty() {
                return;
            }
            let before = st.doc.clone();
            let moved = f(&mut st.doc, &sel);
            if st.doc == before {
                return; // nothing moved: no undo step
            }
            select_many(&mut st, moved);
            // Split the borrow: record needs `history` mutably and `doc`
            // immutably at the same time.
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
                    let hit = shape_at(&st, ip);
                    if hit.is_none_or(|i| !selection(&st).contains(&i)) {
                        select_one(&mut st, hit);
                    }
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
        {
            let mut st = self.st();
            st.menu_point = Some(((pos.0 - st.offset.0) / st.scale, (pos.1 - st.offset.1) / st.scale));
        }
        self.emit_selection();
        self.redraw();
        let mark = self.selected_mark();
        if let Some(f) = &*self.0.on_context.borrow() {
            f(pos, mark);
        }
    }

    /// Called with a short message to show the user (as a toast).
    pub fn connect_hint(&self, f: impl Fn(&str) + 'static) {
        *self.0.on_hint.borrow_mut() = Some(Box::new(f));
    }

    /// Tells the window about the selection. The state borrow is dropped
    /// before the callback runs, since it may call back into the canvas.
    fn emit_selection(&self) {
        let style = {
            let st = self.0.state.borrow();
            st.selected.and_then(|i| st.doc.shapes.get(i)).map(|s| s.style.clone())
        };
        if let Some(f) = &*self.0.on_selection.borrow() {
            f(style);
        }
    }

    /// Tells the window undo/redo may have changed.
    fn emit_history(&self) {
        if let Some(f) = &*self.0.on_history.borrow() {
            f();
        }
    }

    /// Shows `msg` through the hint callback.
    fn hint(&self, msg: &str) {
        if let Some(f) = &*self.0.on_hint.borrow() {
            f(msg);
        }
    }

    /// Schedules a redraw for the next frame.
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
            select_one(&mut st, None);
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

    /// Switches tool. Leaving Crop applies a pending crop; any tool but
    /// Select drops the selection.
    pub fn set_tool(&self, tool: Tool) {
        self.finish_edit(false);
        let had_crop = {
            let mut st = self.st();
            st.tool = tool;
            if tool != Tool::Select {
                select_one(&mut st, None);
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

    /// Whether the selected mark is a picture (the Picture opacity control
    /// applies).
    pub fn picture_selected(&self) -> bool {
        let st = self.st();
        st.selected.and_then(|i| st.doc.shapes.get(i)).is_some_and(|s| matches!(s.kind, Kind::Image(_)))
    }

    /// Which kind of mark the Line Start and Line End controls apply to:
    /// the selected mark's, or the current tool's new marks. None when
    /// neither has ends.
    pub fn ends_for(&self) -> Option<EndsFor> {
        let st = self.st();
        match st.selected.and_then(|i| st.doc.shapes.get(i)) {
            Some(shape) => EndsFor::of_kind(&shape.kind),
            None => EndsFor::of_tool(st.tool),
        }
    }

    /// Sets the style for new shapes, and applies it to the selected shape.
    pub fn set_style(&self, style: Style) {
        let editing = self.0.editor.borrow().is_some();
        let changed = {
            let mut st = self.st();
            st.style = style.clone();
            let before = st.doc.clone();
            // fit_text: a new font or size changes a text box's size.
            if let Some(i) = st.selected {
                // The others take only what changed, keeping the rest of
                // their own style.
                let old = st.doc.shapes[i].style.clone();
                for j in st.also.clone() {
                    let shape = &mut st.doc.shapes[j];
                    shape.restyle_from(&old, &style);
                    render::fit_text(shape);
                }
                let shape = &mut st.doc.shapes[i];
                shape.style = style;
                render::fit_text(shape);
            }
            // While editing, the edit's own undo step covers the restyle.
            if !editing {
                // Merge into the previous step if it was a restyle of this
                // same mark, recent, and nothing else has been recorded since.
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
                    // Extend the merge window from this restyle.
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

    /// Undoes the last step on this page. If the step changes the image's
    /// size (Scale to Visible Size), the zoom is adjusted so the page stays
    /// the same size on screen.
    pub fn undo(&self) {
        self.finish_edit(false);
        let ok = {
            let mut st = self.st();
            let width = image_width(&st);
            let State { history, doc, .. } = &mut *st;
            let ok = history.undo(doc);
            select_one(&mut st, None);
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

    /// Redoes the last undone step on this page (see [`Self::undo`]).
    pub fn redo(&self) {
        self.finish_edit(false);
        let ok = {
            let mut st = self.st();
            let width = image_width(&st);
            let State { history, doc, .. } = &mut *st;
            let ok = history.redo(doc);
            select_one(&mut st, None);
            keep_shown_size(&mut st, width);
            ok
        };
        if ok {
            self.emit_selection();
            self.emit_history();
            self.redraw();
        }
    }

    /// Whether this page has anything to undo.
    pub fn can_undo(&self) -> bool {
        self.0.state.borrow().history.can_undo()
    }

    /// Whether this page has anything to redo.
    pub fn can_redo(&self) -> bool {
        self.0.state.borrow().history.can_redo()
    }

    /// Removes the selected marks, as one undo step.
    pub fn delete_selected(&self) {
        {
            let mut st = self.st();
            let sel = selection(&st);
            if sel.is_empty() {
                return;
            }
            select_one(&mut st, None);
            let before = st.doc.clone();
            for &i in sel.iter().rev() {
                st.doc.shapes.remove(i);
            }
            let State { history, doc, .. } = &mut *st;
            history.record(before, doc);
        }
        self.emit_selection();
        self.emit_history();
        self.redraw();
    }

    // ----- cut, copy, paste and duplicate (see `clip`) ----------------------

    /// While the text editor is open, cut, copy and paste act on its text:
    /// runs `clipboard.<op>` on it and returns true. False if it is closed.
    fn editor_clipboard(&self, op: &str) -> bool {
        let Some(view) = self.0.editor.borrow().as_ref().map(|e| e.view.clone()) else { return false };
        let _ = view.activate_action(&format!("clipboard.{op}"), None);
        true
    }

    /// Copies the selected marks to the clipboard (see `clip`), and with
    /// `cut` removes them as one undo step. Returns false, doing nothing, if
    /// no mark is selected (the window then copies the whole image). While
    /// text is being edited this cuts or copies the text instead.
    pub fn copy_selected(&self, cut: bool) -> bool {
        if self.editor_clipboard(if cut { "cut" } else { "copy" }) {
            return true;
        }
        let (shapes, image) = {
            let st = self.0.state.borrow();
            let shapes: Vec<Shape> = selection(&st).into_iter().map(|i| st.doc.shapes[i].clone()).collect();
            if shapes.is_empty() {
                return false;
            }
            (shapes, current_image(&st).cloned())
        };
        let picture = image.and_then(|img| match render::render_marks(&img, &shapes) {
            Ok(p) => p,
            Err(e) => {
                glib::g_warning!("pygoscribe", "could not draw the copied marks: {e}");
                None
            }
        });
        if let Err(e) = crate::clip::put(&self.0.area.clipboard(), shapes, picture, cut) {
            self.hint(&format!("Could not copy: {e}"));
            return true;
        }
        if cut {
            self.delete_selected();
        }
        true
    }

    /// Selects every mark on the page (Ctrl+A; the window switches to the
    /// Select tool first, so they can be moved together).
    pub fn select_all(&self) {
        self.finish_edit(false);
        {
            let mut st = self.st();
            let all = (0..st.doc.shapes.len()).collect();
            select_many(&mut st, all);
        }
        self.emit_selection();
        self.redraw();
    }

    /// Whether the clipboard holds marks copied here, ready to paste.
    pub fn can_paste(&self) -> bool {
        crate::clip::has_marks(&self.0.area.clipboard())
    }

    /// Pastes the copied marks as one undo step and selects them. Each
    /// paste lands a step further down and right of the copied marks (the
    /// first paste after Cut lands where they were), or in the middle of the
    /// view if that would be out of sight; with `at_menu`, centred where the
    /// context menu was opened. Returns false, doing nothing, if the
    /// clipboard holds no marks. While text is being edited this pastes
    /// text into it instead.
    pub fn paste(&self, at_menu: bool) -> bool {
        if self.editor_clipboard("paste") {
            return true;
        }
        if self.0.state.borrow().image.is_none() {
            return false;
        }
        let Some((shapes, n)) = crate::clip::take(&self.0.area.clipboard()) else { return false };
        let placement = {
            let st = self.0.state.borrow();
            match st.menu_point.filter(|_| at_menu) {
                Some(p) => document::Placement::At(p),
                None => document::Placement::Offset { by: f64::from(n) * PASTE_STEP / st.scale, view: shown_rect(&st) },
            }
        };
        self.add_marks(document::place(&shapes, placement));
        true
    }

    /// Adds copies of the selected marks a step down and right of them, as
    /// one undo step, and selects the copies (so repeating it makes a row).
    /// Does nothing while text is being edited.
    pub fn duplicate_selected(&self) {
        if self.0.editor.borrow().is_some() {
            return;
        }
        let copies = {
            let st = self.0.state.borrow();
            let shapes: Vec<Shape> = selection(&st).into_iter().map(|i| st.doc.shapes[i].clone()).collect();
            if shapes.is_empty() {
                return;
            }
            let placement = document::Placement::Offset { by: PASTE_STEP / st.scale, view: shown_rect(&st) };
            document::place(&shapes, placement)
        };
        self.add_marks(copies);
    }

    /// Inserts `textures` as picture marks, as one undo step, and selects
    /// them. The first is centred on `at` (canvas coordinates), or with
    /// `at_menu` where the context menu was opened; otherwise, or if that is
    /// off the page, in the middle of what is shown. Each further one is a
    /// step down and right. Each keeps its own size in pixels, or is made
    /// smaller to fit in most of the view. Returns false if no page is open.
    pub fn insert_images(&self, textures: &[gdk::Texture], at: Option<Point>, at_menu: bool) -> bool {
        let mut surfaces = Vec::new();
        for t in textures {
            match render::surface_from_texture(t) {
                Ok(s) => surfaces.push(s),
                Err(e) => self.hint(&format!("Could not read the picture: {e}")),
            }
        }
        let shapes: Vec<Shape> = {
            let st = self.0.state.borrow();
            if st.image.is_none() {
                return false;
            }
            let shown = shown_rect(&st);
            let middle = (shown.x + shown.w / 2.0, shown.y + shown.h / 2.0);
            let wanted = if at_menu {
                st.menu_point
            } else {
                at.map(|(x, y)| ((x - st.offset.0) / st.scale, (y - st.offset.1) / st.scale))
            };
            let centre = wanted.filter(|p| shown.contains(*p, 0.0)).unwrap_or(middle);
            let room = (shown.w * IMAGE_ROOM, shown.h * IMAGE_ROOM);
            let step = PASTE_STEP / st.scale;
            surfaces
                .into_iter()
                .enumerate()
                .map(|(i, surface)| {
                    let d = step * i as f64;
                    let c = (centre.0 + d, centre.1 + d);
                    Shape::image(crate::document::Picture(surface), c, room, st.style.clone())
                })
                .collect()
        };
        self.add_marks(shapes);
        true
    }

    /// Puts `shapes` on top of the page as one undo step and selects them.
    fn add_marks(&self, shapes: Vec<Shape>) {
        self.finish_edit(false);
        {
            let mut st = self.st();
            let before = st.doc.clone();
            let n = shapes.len();
            let Some(top) = st.doc.add_on_top(shapes) else { return };
            select_many(&mut st, (top + 1 - n..=top).collect());
            let State { history, doc, .. } = &mut *st;
            history.record(before, doc);
        }
        self.emit_selection();
        self.emit_history();
        self.redraw();
    }

    /// Turns the pending crop rectangle into the document's crop (as one
    /// undo step). The rectangle is limited to what is visible now, so a crop
    /// can only make the image smaller; one under 2 × 2 pixels is ignored.
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
            // k is the current zoom as a fraction of actual size: the image
            // is resampled by k so that 100 % afterwards looks like now.
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
                // Best filtering for a good downscale; Source copies alpha
                // as is rather than blending onto the empty surface.
                cr.source().set_filter(cairo::Filter::Best);
                cr.set_operator(cairo::Operator::Source);
                cr.paint().map_err(err)?;
            }
            out.flush();
            let before = st.doc.clone();
            st.doc.scale(k);
            // Re-fit text boxes: font metrics don't scale exactly linearly.
            for shape in &mut st.doc.shapes {
                render::fit_text(shape);
            }
            st.doc.image = Some(crate::document::Picture(out));
            let State { history, doc, .. } = &mut *st;
            history.record(before, doc);
            select_one(&mut st, None);
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
            // The image point under the anchor now, to keep there.
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

    /// Zooms to `percent` of actual size about the middle of the view.
    fn zoom_to_percent(&self, percent: f64) {
        let unit = self.0.state.borrow().unit;
        self.zoom_to(percent / 100.0 * unit, None);
    }

    /// Zooms in to the next step (see [`view::STEPS`]).
    pub fn zoom_in(&self) {
        self.zoom_to_percent(view::step_in(self.zoom_percent()));
    }

    /// Zooms out to the previous step.
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

    /// Device pixels per logical pixel of the window's surface (2 on a
    /// 200 % HiDPI screen; fractional with fractional scaling).
    fn device_scale(&self) -> f64 {
        self.0.area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale())
    }

    /// Schedules [`Self::sync_view`] once, after the current draw. Changing
    /// widgets from inside a draw function isn't allowed, hence the idle.
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
                // (value, lower, upper, step, page step, page size): arrow
                // clicks move 10 % of a page, trough clicks 90 %.
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

        // Keep the text editor's popover pointing at its mark as the view moves.
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
                // Stale unless the existing render is at this zoom and
                // covers everything now in view.
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
        // Debounce: restart the timer, so rendering happens only once the
        // view has been still for HIRES_DELAY (not on every scroll step).
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
            // Render 10 % extra on each side, so small scrolls don't need a
            // new render straight away.
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
        // Very deep zoom on a big window: skip rather than allocate a huge
        // surface; the blurry base image still shows.
        if pw <= 0 || ph <= 0 || pw > HIRES_MAX || ph > HIRES_MAX {
            return;
        }
        // Rgb24: a PDF page on white is opaque, so no alpha is needed.
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

    /// Connects the scrollbars, scroll wheel/touchpad, middle-drag and pinch
    /// gestures that move and zoom the view.
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
            // A mouse wheel reports clicks (Wheel); a touchpad reports
            // smooth pixel deltas (Surface), which need different factors.
            let wheel = ctl.unit() == gdk::ScrollUnit::Wheel;
            if mods.contains(gdk::ModifierType::CONTROL_MASK) {
                // Wheel: 20 % per click. Touchpad: exponential in the
                // distance, so zooming in and back out is symmetric.
                let factor = if wheel { 1.2f64.powf(-dy) } else { (-dy * 0.01).exp() };
                let (scale, pointer) = {
                    let st = c.0.state.borrow();
                    (st.scale, st.pointer)
                };
                c.zoom_to(scale * factor, pointer);
                return glib::Propagation::Stop;
            }
            let (mut dx, mut dy) = if wheel { (dx * 48.0, dy * 48.0) } else { (dx, dy) };
            // Shift+wheel scrolls sideways (mice have no horizontal wheel).
            if wheel && mods.contains(gdk::ModifierType::SHIFT_MASK) && dx == 0.0 {
                std::mem::swap(&mut dx, &mut dy);
            }
            // When the page fits, let the scroll go on to parent widgets.
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

    /// Installs the draw function. It holds a weak reference so the drawing
    /// area doesn't keep the canvas alive in a cycle.
    fn setup_drawing(&self) {
        let weak = Rc::downgrade(&self.0);
        self.0.area.set_draw_func(move |_, cr, w, h| {
            if let Some(inner) = weak.upgrade() {
                Canvas(inner).draw(cr, w as f64, h as f64);
            }
        });
    }

    /// Draws one frame: the page with its marks, then the selection and crop
    /// overlays, which are not part of the document. Also records the
    /// viewport size and re-lays out the view (fit-to-window follows it).
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
                // The sharp render is k device pixels per image pixel;
                // scale by 1/k to put it back in image pixels.
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

        // Overlays, outside the clip so handles at the edge show in full.
        let _ = cr.save();
        cr.translate(offset.0, offset.1);
        cr.scale(scale, scale);
        // Handles only for a single selected mark: several are moved, not
        // resized.
        let sel = selection(&st);
        for &i in &sel {
            draw_selection(cr, &st.doc.shapes[i], scale, sel.len() == 1);
        }
        if let Some(r) = st.marquee {
            draw_marquee(cr, &r, scale);
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

    /// Converts a widget (screen) point to image pixels.
    fn to_image(&self, x: f64, y: f64) -> Point {
        let st = self.0.state.borrow();
        ((x - st.offset.0) / st.scale, (y - st.offset.1) / st.scale)
    }

    /// Connects the pointer, click, keyboard, context-menu and motion
    /// controllers. Primary-button drags drive the tools.
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
                // Claim the press so the primary drag gesture doesn't see it.
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
                    // Key repeat sends Space again and again while held;
                    // only the first press changes the cursor.
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
                        select_one(&mut st, None);
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

    /// The cursor name for widget point (`x`, `y`): what a press there would
    /// do, in the same order of precedence as [`Self::drag_begin`].
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

    /// A primary-button press: decides what the drag will do, in order of
    /// precedence: Space-pan, double-click text edit, Alt-rotate, resizing
    /// the selected mark by a handle, then the current tool.
    fn drag_begin(&self, x: f64, y: f64, mods: gdk::ModifierType) {
        // A press anywhere ends a text edit (clicking away keeps the text).
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
            select_one(&mut st, Some(idx));
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
            select_one(&mut st, Some(idx));
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
                let hit = shape_at(&st, p);
                let sel = selection(&st);
                let adding = mods.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK);
                st.drag = match hit {
                    // Shift or Ctrl+click adds a mark to the selection, or
                    // takes it out.
                    Some(idx) if adding => {
                        let mut sel = sel;
                        match sel.iter().position(|&i| i == idx) {
                            Some(k) => {
                                sel.remove(k);
                            }
                            None => sel.push(idx),
                        }
                        select_many(&mut st, sel);
                        Drag::None
                    }
                    Some(idx) => {
                        // Dragging one of several selected marks moves them
                        // all; any other mark is selected alone.
                        if !sel.contains(&idx) {
                            select_one(&mut st, Some(idx));
                        }
                        let group = selection(&st).into_iter().map(|i| (i, st.doc.shapes[i].clone())).collect();
                        Drag::Move { group, start: p, before }
                    }
                    // On the page itself: drag out a marquee.
                    None => {
                        let base = if adding { sel } else { Vec::new() };
                        select_many(&mut st, base.clone());
                        Drag::Marquee { start: p, base }
                    }
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
                        select_one(&mut st, Some(idx));
                        st.drag = Drag::Move { group: vec![(idx, st.doc.shapes[idx].clone())], start: p, before };
                        drop(st);
                        self.emit_selection();
                        self.redraw();
                    }
                    return;
                }
                // A new, empty text mark, edited straight away when the button
                // is released. If it is left empty, finish_text_edit removes it.
                let mut shape = Shape::new(Kind::Text(String::new()), p, st.style.clone());
                render::fit_text(&mut shape);
                st.doc.shapes.push(shape);
                let idx = st.doc.shapes.len() - 1;
                select_one(&mut st, Some(idx));
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
                // The mark is added now and grows with the drag; drag_end
                // removes it again if it stayed tiny (a stray click).
                let mut style = st.style.clone();
                if tool == Tool::Highlighter {
                    // Highlights start yellow; recolour by selecting one.
                    style.stroke = Rgba::new(0.965, 0.827, 0.176, 1.0);
                }
                st.doc.shapes.push(Shape::new(kind, p, style));
                let idx = st.doc.shapes.len() - 1;
                select_one(&mut st, None);
                st.drag = Drag::Create { idx, start: p, before };
            }
        }
    }

    /// Pointer motion during a primary drag: updates the mark (or crop, or
    /// view) from the drag's starting state to follow the pointer.
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
        // Split the borrow so the drag, the document and the crop can be
        // changed together.
        if let Drag::Marquee { start, base } = &st.drag {
            let r = Rect::from_points(*start, p);
            let mut sel = base.clone();
            sel.extend(st.doc.marks_within(&r));
            st.marquee = Some(r);
            select_many(&mut st, sel);
            drop(st);
            self.redraw();
            return;
        }
        let State { drag, doc, pending_crop, .. } = &mut *st;
        match drag {
            Drag::None | Drag::EditText { .. } | Drag::Pan { .. } | Drag::Marquee { .. } => return,
            Drag::Create { idx, start, .. } => {
                let shape = &mut doc.shapes[*idx];
                match &mut shape.kind {
                    Kind::Pen(pts) | Kind::Highlighter(pts) => pts.push(p),
                    Kind::Loupe { .. } => {
                        // Keep it square, growing toward the pointer.
                        let s = (p.0 - start.0).abs().max((p.1 - start.1).abs());
                        shape.b = (start.0 + s.copysign(p.0 - start.0), start.1 + s.copysign(p.1 - start.1));
                    }
                    // Shift snaps a line or arrow to 15° steps.
                    Kind::Line | Kind::Arrow if mods.contains(gdk::ModifierType::SHIFT_MASK) => {
                        shape.b = document::snap_point(*start, p);
                    }
                    _ => shape.b = p,
                }
            }
            Drag::Move { group, start, .. } => {
                for (idx, orig) in group.iter() {
                    let mut s = orig.clone();
                    s.translate(p.0 - start.0, p.1 - start.1);
                    doc.shapes[*idx] = s;
                }
            }
            Drag::Resize { idx, handle, orig, .. } => {
                let (idx, handle) = (*idx, *handle);
                // Shift snaps a line's dragged end to 15° steps round the
                // other end.
                let p = match handle {
                    0 if orig.is_segment() && mods.contains(gdk::ModifierType::SHIFT_MASK) => {
                        document::snap_point(orig.b, p)
                    }
                    1 if orig.is_segment() && mods.contains(gdk::ModifierType::SHIFT_MASK) => {
                        document::snap_point(orig.a, p)
                    }
                    _ => p,
                };
                doc.shapes[idx].resize_from(orig, handle, p, mods.contains(gdk::ModifierType::SHIFT_MASK));
                render::fit_text(&mut doc.shapes[idx]);
            }
            Drag::Rotate { idx, orig, center, start, .. } => {
                // The rotation is the change in the pointer's angle as seen
                // from the mark's centre.
                let turn = |q: Point| (q.1 - center.1).atan2(q.0 - center.0);
                let mut delta = turn(p) - turn(*start);
                if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    // Snap the resulting angle: a boxed mark's own, a line's
                    // direction (so it can land exactly level or upright),
                    // or, for freehand marks, the turn itself.
                    let base = if orig.is_segment() {
                        (orig.b.1 - orig.a.1).atan2(orig.b.0 - orig.a.0)
                    } else {
                        orig.angle
                    };
                    delta = document::snap_angle(base + delta) - base;
                }
                let shape = &mut doc.shapes[*idx];
                shape.rotate_from(orig, delta);
                if orig.is_segment() && mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    // Clean off rounding so level and upright lines are exact.
                    shape.b = document::snap_point(*center, shape.b);
                    shape.a = (2.0 * center.0 - shape.b.0, 2.0 * center.1 - shape.b.1);
                }
            }
            Drag::Crop { start } => *pending_crop = Some(Rect::from_points(*start, p)),
        }
        drop(st);
        self.redraw();
    }

    /// The button was released: finish the drag and record its undo step.
    fn drag_end(&self, x: f64, y: f64, mods: gdk::ModifierType) {
        // Apply the final position first, as a motion event may not have
        // arrived for it.
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
                // Under 4 screen pixels: a click, not a drag. Throw the mark
                // away (restoring `before` removes it) rather than leave an
                // invisible speck. A pen click is a deliberate dot, so it stays.
                let tiny = !shape.is_freehand() && r.w.max(r.h) * scale < 4.0;
                if tiny {
                    st.doc = before;
                } else {
                    select_one(&mut st, Some(idx));
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
            Drag::Marquee { .. } => st.marquee = None,
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
            let Some(idx) = st.selected.filter(|_| st.also.is_empty()) else { return false };
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
                select_one(&mut st, None);
            } else if let Some(shape) = st.doc.shapes.get_mut(ed.idx) {
                render::fit_text(shape);
            }
            let State { history, doc, .. } = &mut *st;
            history.record(ed.before, doc);
        }
        ed.popover.popdown();
        // Unparent later: doing it inside the popover's own signal handlers
        // (this can run from `closed`) would destroy it mid-emission.
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
        // Select it all, so typing replaces the text and arrows keep it.
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

        // Live preview: every keystroke updates the mark and resizes its box.
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

        // However the popover closes (focus loss, the window), keep the text.
        let this = self.clone();
        popover.connect_closed(move |_| {
            glib::g_debug!("pygoscribe", "text editor closed");
            this.finish_edit(false);
        });

        *self.0.editor.borrow_mut() = Some(Editor { idx, original: text, before, popover: popover.clone(), view: view.clone() });
        glib::g_debug!("pygoscribe", "text editor opening for mark {idx} at {rect:?}");
        popover.popup();
        view.grab_focus();
    }
}

/// Helpers for the text editor.
impl Canvas {
    /// The text editor's alignment buttons for a mark described by
    /// (is a Text mark, is a line, halign, valign): left/centre/right, and
    /// top/middle/bottom for a box or above/below for a line. A Text mark
    /// only lines up its lines left, centre or right.
    fn alignment_buttons(&self, (is_text, is_line, halign, valign): (bool, bool, HAlign, VAlign)) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        // A linked row of radio-style toggle buttons. Each item is (icon name
        // or label, tooltip, initially active, alignment to set).
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
                // Grouping makes the toggles exclusive, like radio buttons.
                b.set_group(first.as_ref());
                first.get_or_insert_with(|| b.clone());
                let this = self.clone();
                b.connect_toggled(move |b| {
                    // Only the newly active button acts (the one turned off
                    // also gets `toggled`).
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
        // GTK's own defaults, if there are no settings (no display).
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
        // An image half as wide needs twice the scale to look the same.
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

/// [`layout`] for the current visible rectangle, if there is an image.
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

/// Whether `outer` contains `inner`, give or take half an image pixel (so
/// rounding at the edge doesn't trigger a needless re-render).
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

/// The width of [`current_image`], to detect a change of image size.
fn image_width(st: &State) -> Option<f64> {
    current_image(st).map(|i| f64::from(i.width()))
}

/// The part of the image that is the result: the crop, or the whole image.
/// `None` when there is no image. The view lays out and scrolls this.
fn visible_rect(st: &State) -> Option<Rect> {
    let img = current_image(st)?;
    Some(st.doc.crop.unwrap_or(Rect { x: 0.0, y: 0.0, w: img.width() as f64, h: img.height() as f64 }))
}

/// Selects mark `i` alone, or nothing.
fn select_one(st: &mut State, i: Option<usize>) {
    st.selected = i;
    st.also.clear();
}

/// Selects the marks at `indices` (any order, repeats ignored); the
/// topmost becomes the main one ([`State::selected`]).
fn select_many(st: &mut State, mut indices: Vec<usize>) {
    indices.retain(|&i| i < st.doc.shapes.len());
    indices.sort_unstable();
    indices.dedup();
    st.selected = indices.pop();
    st.also = indices;
}

/// Every selected mark, bottom to top.
fn selection(st: &State) -> Vec<usize> {
    let mut v: Vec<usize> =
        st.selected.into_iter().chain(st.also.iter().copied()).filter(|&i| i < st.doc.shapes.len()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The part of the result on screen, in image pixels (the whole result
/// before the first draw, or if none of it is in view).
fn shown_rect(st: &State) -> Rect {
    let vis = visible_rect(st).unwrap_or(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
    if st.viewport.0 <= 0.0 || st.viewport.1 <= 0.0 {
        return vis;
    }
    view_rect(st).intersect(&vis).unwrap_or(vis)
}

/// The topmost mark at image point `p` (its body or a line's text), with a
/// tolerance of [`HIT_TOL`] screen pixels at the current zoom.
fn shape_at(st: &State, p: Point) -> Option<usize> {
    let tol = HIT_TOL / st.scale;
    // rposition: search from the top of the stack down.
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

/// The handle of the selected mark at image point `p`, if any. The grab
/// area is a little bigger than the drawn handle.
fn handle_at(st: &State, p: Point) -> Option<usize> {
    if !st.also.is_empty() {
        return None; // several selected: no handles
    }
    let shape = st.doc.shapes.get(st.selected?)?;
    let tol = (HANDLE + 3.0) / st.scale;
    shape.handles().iter().position(|h| (h.0 - p.0).abs() <= tol && (h.1 - p.1).abs() <= tol)
}

/// Draws the selection: a dashed outline (rotated with the mark) and square
/// handles. `cr` is in image pixels; sizes are divided by `scale` so they
/// stay the same on screen at any zoom.
fn draw_selection(cr: &Context, shape: &Shape, scale: f64, handles: bool) {
    // One screen pixel, in image pixels.
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
    if !handles {
        if shape.is_segment() {
            // A line has no box: a thin accent line along it instead.
            cr.move_to(shape.a.0, shape.a.1);
            cr.line_to(shape.b.0, shape.b.1);
            let _ = cr.stroke();
        }
        return;
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

/// The marquee being dragged out: a faint accent box with a thin edge.
fn draw_marquee(cr: &Context, r: &Rect, scale: f64) {
    let px = 1.0 / scale;
    cr.rectangle(r.x, r.y, r.w, r.h);
    cr.set_source_rgba(ACCENT.r, ACCENT.g, ACCENT.b, 0.12);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(ACCENT.r, ACCENT.g, ACCENT.b, 0.8);
    cr.set_line_width(px);
    let _ = cr.stroke();
}

/// Darkens everything outside the pending crop and outlines the crop.
fn draw_crop_overlay(cr: &Context, vis: &Rect, crop: &Rect, scale: f64) {
    let px = 1.0 / scale;
    // Even-odd fill of the page plus the crop rectangle paints only the
    // area between them: a frame with a hole.
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

    /// Runs the GLib main loop for `ms` milliseconds, so idles and timeouts
    /// (scrollbar sync, sharp PDF rendering) get to run.
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
            select_one(&mut st, Some(0));
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
            count: 1,
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

    /// Copy, paste, cut and duplicate: each paste a step further on, the
    /// first paste after Cut in place, each one undo step; copying anything
    /// else to the clipboard means Paste has no marks.
    #[test]
    #[ignore]
    fn cut_copy_paste_duplicate() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 200).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        let mut mark = Shape::new(Kind::Rect, (10.0, 10.0), Style::default());
        mark.b = (60.0, 50.0);
        c.st().doc.shapes.push(mark.clone());
        c.zoom_to(2.0, None);
        let step = PASTE_STEP / 2.0;
        let at = |i: usize| c.document().shapes[i].a;

        assert!(!c.copy_selected(false), "nothing selected: the window copies the image");
        c.st().selected = Some(0);
        assert!(c.copy_selected(false));
        assert!(c.can_paste());
        assert!(c.paste(false));
        assert!(c.paste(false));
        assert_eq!((at(1), at(2)), ((10.0 + step, 10.0 + step), (10.0 + 2.0 * step, 10.0 + 2.0 * step)));
        assert_eq!(c.st().selected, Some(2), "the paste is selected");
        c.undo();
        assert_eq!(c.document().shapes.len(), 2, "one undo step per paste");

        c.st().selected = Some(1);
        c.duplicate_selected();
        assert_eq!(at(2), (10.0 + 2.0 * step, 10.0 + 2.0 * step));
        assert_eq!(c.st().selected, Some(2));

        assert!(c.copy_selected(true));
        assert_eq!(c.document().shapes.len(), 2, "cut removes the mark");
        assert!(c.paste(false));
        assert_eq!(at(2), (10.0 + 2.0 * step, 10.0 + 2.0 * step), "the first paste after Cut lands in place");

        c.st().menu_point = Some((300.0, 100.0));
        assert!(c.paste(true));
        assert_eq!(c.document().shapes[3].center(), (300.0, 100.0));

        c.widget().clipboard().set_text("something else");
        assert!(!c.can_paste());
        assert!(!c.paste(false));
        assert_eq!(c.document().shapes.len(), 4);
    }

    /// Inserting pictures: centred in view (or at a point, or the menu's),
    /// shrunk to fit, cascaded, selected, one undo step; then copied and
    /// pasted like any mark. Run alone under Broadway (see the module docs).
    #[test]
    #[ignore]
    fn insert_images() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 200).unwrap();
        let c = Canvas::new();
        let texture = |w: i32, h: i32| {
            render::texture_from_surface(ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap()).unwrap()
        };
        assert!(!c.insert_images(&[texture(10, 10)], None, false), "no page open");
        c.swap_page(image, PageState::default());
        assert!(c.insert_images(&[texture(40, 20), texture(1000, 100)], None, false));
        let shapes = c.document().shapes;
        assert_eq!(shapes.len(), 2);
        assert_eq!(shapes[0].bounds(), Rect { x: 180.0, y: 90.0, w: 40.0, h: 20.0 }, "own size, centred");
        // 80 % of the 400 px page across, proportions kept, one step on.
        let r = shapes[1].bounds();
        assert_eq!((r.w, r.h), (320.0, 32.0));
        assert_eq!(shapes[1].center(), (200.0 + PASTE_STEP, 100.0 + PASTE_STEP));
        assert_eq!(selection(&c.st()), vec![0, 1], "both selected");
        c.undo();
        assert!(c.document().shapes.is_empty(), "one undo step");

        // At a point on the canvas, or where the context menu was opened.
        c.zoom_to(1.0, None);
        let (ox, oy) = c.st().offset;
        assert!(c.insert_images(&[texture(10, 10)], Some((50.0 + ox, 60.0 + oy)), false));
        assert_eq!(c.document().shapes[0].center(), (50.0, 60.0));
        c.st().menu_point = Some((300.0, 150.0));
        assert!(c.insert_images(&[texture(10, 10)], None, true));
        assert_eq!(c.document().shapes[1].center(), (300.0, 150.0));

        // Copy and paste keep it a picture.
        c.st().selected = Some(1);
        c.st().also.clear();
        assert!(c.copy_selected(false));
        assert!(c.paste(false));
        let doc = c.document();
        assert!(matches!(doc.shapes[2].kind, Kind::Image(_)));
        assert_eq!(doc.shapes[2].kind, doc.shapes[1].kind, "the same picture");
    }

    /// Several marks: Shift+click adds and removes, a marquee takes what
    /// it covers, dragging one moves them all, and copy, duplicate, restyle,
    /// restack and delete act on all of them as one undo step each.
    #[test]
    #[ignore]
    fn multi_select() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 200).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        for x in [10.0, 100.0, 200.0] {
            let mut mark = Shape::new(Kind::Rect, (x, 10.0), Style::default());
            mark.b = (x + 50.0, 50.0);
            c.st().doc.shapes.push(mark);
        }
        c.zoom_to(1.0, None);
        let screen = |p: Point| {
            let st = c.0.state.borrow();
            (p.0 * st.scale + st.offset.0, p.1 * st.scale + st.offset.1)
        };
        let shift = gdk::ModifierType::SHIFT_MASK;
        let none = gdk::ModifierType::empty();
        let click = |p: Point, mods| {
            let (x, y) = screen(p);
            c.0.state.borrow_mut().last_press = None;
            c.drag_begin(x, y, mods);
            c.drag_end(x, y, mods);
        };
        let selected = || selection(&c.0.state.borrow());

        click((20.0, 20.0), none);
        click((110.0, 20.0), shift);
        assert_eq!(selected(), vec![0, 1]);
        click((20.0, 20.0), shift);
        assert_eq!(selected(), vec![1]);

        // A marquee round the last two.
        let (x0, y0) = screen((90.0, 0.0));
        let (x1, y1) = screen((300.0, 100.0));
        c.drag_begin(x0, y0, none);
        c.drag_update(x1, y1, none);
        c.drag_end(x1, y1, none);
        assert_eq!(selected(), vec![1, 2]);
        assert_eq!(c.selected_mark().map(|m| m.count), Some(2));

        // Dragging one moves both, as one undo step.
        let (x0, y0) = screen((110.0, 20.0));
        let (x1, y1) = screen((120.0, 40.0));
        c.drag_begin(x0, y0, none);
        c.drag_end(x1, y1, none);
        let doc = c.document();
        assert_eq!((doc.shapes[0].a, doc.shapes[1].a, doc.shapes[2].a), ((10.0, 10.0), (110.0, 30.0), (210.0, 30.0)));
        c.undo();
        assert_eq!(c.document().shapes[2].a, (200.0, 10.0));

        // Restyling changes only what changed, on both.
        c.select_all();
        assert_eq!(selected(), vec![0, 1, 2]);
        c.st().doc.shapes[0].style.width = 1.0;
        let mut style = c.document().shapes[2].style.clone();
        style.stroke = Rgba::new(0.0, 0.0, 1.0, 1.0);
        c.set_style(style);
        let doc = c.document();
        assert!(doc.shapes.iter().all(|s| s.style.stroke == Rgba::new(0.0, 0.0, 1.0, 1.0)));
        assert_eq!(doc.shapes[0].style.width, 1.0, "width wasn't changed, so it stays");

        // Duplicate copies all three; Send to Back keeps them together.
        c.duplicate_selected();
        assert_eq!(c.document().shapes.len(), 6);
        assert_eq!(selected(), vec![3, 4, 5]);
        c.restack_selected(false);
        assert_eq!(selected(), vec![0, 1, 2]);
        assert!(c.copy_selected(true));
        assert_eq!(c.document().shapes.len(), 3, "cut takes all three");
        assert!(c.paste(false));
        assert_eq!(selected(), vec![3, 4, 5]);
        c.delete_selected();
        assert_eq!(c.document().shapes.len(), 3);
        c.undo();
        assert_eq!(c.document().shapes.len(), 6);
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

    /// Shift snaps a line to 15° steps while it is drawn, while an end is
    /// dragged, and while Alt+drag turns it.
    #[test]
    #[ignore]
    fn shift_snaps_line_angles() {
        gtk::init().unwrap();
        let image = ImageSurface::create(cairo::Format::ARgb32, 400, 400).unwrap();
        let c = Canvas::new();
        c.swap_page(image, PageState::default());
        c.zoom_to(1.0, None);
        let screen = |p: Point| {
            let st = c.0.state.borrow();
            (p.0 * st.scale + st.offset.0, p.1 * st.scale + st.offset.1)
        };
        let shift = gdk::ModifierType::SHIFT_MASK;
        // Drawing: a nearly level line comes out level.
        c.set_tool(Tool::Arrow);
        let (x0, y0) = screen((100.0, 100.0));
        let (x1, y1) = screen((300.0, 108.0));
        c.drag_begin(x0, y0, shift);
        c.drag_update(x1, y1, shift);
        c.drag_end(x1, y1, shift);
        let line = c.document().shapes[0].clone();
        assert_eq!(line.a, (100.0, 100.0));
        assert_eq!(line.b.1, 100.0);
        // Without Shift it follows the pointer.
        c.set_tool(Tool::Line);
        c.drag_begin(x0, y0 + 100.0, gdk::ModifierType::empty());
        c.drag_end(x1, y1 + 100.0, gdk::ModifierType::empty());
        assert_eq!(c.document().shapes[1].b, (300.0, 208.0));
        // Dragging the end of the selected line: nearly upright snaps upright.
        c.set_tool(Tool::Select);
        c.st().selected = Some(1);
        let (hx, hy) = screen((300.0, 208.0));
        let (tx, ty) = screen((96.0, 380.0));
        c.drag_begin(hx, hy, shift);
        c.drag_end(tx, ty, shift);
        let s = c.document().shapes[1].clone();
        assert_eq!(s.b.0, 100.0, "{s:?}");
        // Shift+Alt+drag turns the first line to exactly upright.
        c.st().selected = Some(0);
        let (cx, cy) = screen((300.0, 100.0));
        let (ux, uy) = screen((205.0, 290.0));
        let both = shift | gdk::ModifierType::ALT_MASK;
        c.drag_begin(cx, cy, both);
        c.drag_end(ux, uy, both);
        let s = c.document().shapes[0].clone();
        assert_eq!(s.a.0, s.b.0, "{s:?}");
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
