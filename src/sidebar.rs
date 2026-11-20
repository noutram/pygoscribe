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

//! The page sidebar: a vertically scrolling list of page thumbnails on the
//! left of the window, in an `AdwOverlaySplitView` that slides in and out.
//!
//! Rendering a PDF page is slow (tens of milliseconds), so thumbnails are not
//! drawn when the list is built. Each row starts as a correctly sized blank
//! placeholder, and the window draws the queued pages one per idle callback
//! (see `next_pending` and `idle`), current page first, so a long PDF opens
//! at once and the UI stays responsive while the thumbnails fill in.
//!
//! Pages can be dragged to a new place in the list. A line above or below the
//! row under the pointer shows where the page will land, and the list scrolls
//! when the pointer nears its top or bottom edge. The sidebar only moves the
//! rows; the window moves the page itself (`Pages::move_page`) through the
//! callback given to [`Sidebar::connect_page_moved`].

use crate::pages::Pages;
use crate::document::Document;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// Thumbnail width in logical pixels.
const THUMB_WIDTH: i32 = 120;
/// After a change on the canvas, wait this long before redrawing its
/// thumbnail, so dragging a mark doesn't re-render every frame.
const REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// How close (logical pixels) the pointer must come to the top or bottom of
/// the list during a drag before it scrolls.
const SCROLL_EDGE: f64 = 48.0;
/// The fastest the list scrolls during a drag, in pixels per frame.
const SCROLL_STEP: f64 = 14.0;

/// What [`Sidebar::connect_page_moved`] calls: (from, to).
type MoveHandler = Box<dyn Fn(usize, usize)>;
/// What [`Sidebar::connect_delete_requested`] calls: (positions).
type DeleteHandler = dyn Fn(Vec<usize>);

/// The widgets of one page's row that change after it is built.
struct Row {
    /// The row itself, to move it within the list.
    row: gtk::ListBoxRow,
    /// The page number under the thumbnail.
    number: gtk::Label,
    /// The thumbnail image.
    picture: gtk::Picture,
    /// The "marked up" dot beside the page number.
    dot: gtk::Label,
}

/// The sidebar's shared state, behind an `Rc` so signal handlers can hold a
/// weak reference to it.
struct Inner {
    /// The split view: sidebar on the left, `content` (the canvas) on the right.
    split: adw::OverlaySplitView,
    /// One row per page.
    list: gtk::ListBox,
    /// Scrolls `list`; used to keep the selected row in view.
    scroller: gtk::ScrolledWindow,
    /// Row widgets by page index.
    rows: RefCell<Vec<Row>>,
    /// Set while the app selects a row itself, so that isn't seen as a click.
    syncing: Cell<bool>,
    /// Pages whose thumbnails still need drawing, most urgent first.
    queue: RefCell<VecDeque<usize>>,
    /// The window's idle callback that draws queued thumbnails, while it runs.
    idle: RefCell<Option<glib::SourceId>>,
    /// The pending [`Sidebar::refresh_later`] timeout, if any.
    timer: RefCell<Option<glib::SourceId>>,
    /// Called with (from, to) when a page is dropped in a new place.
    on_move: RefCell<Option<MoveHandler>>,
    /// Whether the pages are a PDF's, which can be moved and deleted.
    pdf: Cell<bool>,
    /// Called with the pages' positions when the user asks to delete them.
    on_delete: RefCell<Option<Box<DeleteHandler>>>,
    /// The position of the page being dragged, while one is. Only drags that
    /// start in this sidebar are accepted, so a page can't be dropped into
    /// another window's list.
    dragging: Cell<Option<usize>>,
    /// The row showing the drop line, and whether the line is below it.
    marker: RefCell<Option<(gtk::ListBoxRow, bool)>>,
    /// Scroll speed during a drag near an edge (pixels per frame, + is down).
    scroll_speed: Cell<f64>,
    /// The timer that scrolls during a drag, while it runs.
    scroll_timer: RefCell<Option<glib::SourceId>>,
}

/// A cheap-to-clone handle to the page sidebar.
#[derive(Clone)]
pub struct Sidebar(Rc<Inner>);

impl Sidebar {
    /// A split view with `content` beside the (still empty) page list.
    pub fn new(content: &impl IsA<gtk::Widget>) -> Self {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        // Ctrl+click, Shift+click and Ctrl+A select several pages to delete
        // at once. A plain click still selects one and goes to it. The list
        // only honours the modifiers when a single click doesn't activate.
        list.set_selection_mode(gtk::SelectionMode::Multiple);
        list.set_activate_on_single_click(false);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&list)
            .vexpand(true)
            .build();
        let split = adw::OverlaySplitView::builder()
            .sidebar(&scroller)
            .content(content)
            .min_sidebar_width(f64::from(THUMB_WIDTH + 40))
            .max_sidebar_width(f64::from(THUMB_WIDTH + 64))
            .show_sidebar(crate::settings::get().sidebar_visible)
            .build();
        split.add_css_class("pygoscribe-pages");

        // The accent-coloured "marked" dot, and a white card with a soft
        // shadow behind each thumbnail so white pages stand out in dark mode.
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".pygoscribe-pages .page-dot { color: @accent_color; font-weight: bold; }\n\
             .pygoscribe-pages .page-thumb { background: white; box-shadow: 0 1px 3px alpha(black, 0.3); }\n\
             .pygoscribe-pages row.page-dragged { opacity: 0.4; }\n\
             .pygoscribe-pages row.drop-above { box-shadow: inset 0 3px @accent_color; }\n\
             .pygoscribe-pages row.drop-below { box-shadow: inset 0 -3px @accent_color; }",
        );
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&split),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let this = Self(Rc::new(Inner {
            split,
            list,
            scroller,
            rows: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            queue: RefCell::new(VecDeque::new()),
            idle: RefCell::new(None),
            timer: RefCell::new(None),
            on_move: RefCell::new(None),
            pdf: Cell::new(false),
            on_delete: RefCell::new(None),
            dragging: Cell::new(None),
            marker: RefCell::new(None),
            scroll_speed: Cell::new(0.0),
            scroll_timer: RefCell::new(None),
        }));
        this.add_drop_target();
        this.add_delete_action();
        this
    }

    /// The `sidebar.delete-page(u)` action that page menus use, and the
    /// Delete key on the selected rows.
    fn add_delete_action(&self) {
        let group = gio::SimpleActionGroup::new();
        let action = gio::SimpleAction::new("delete-page", Some(glib::VariantTy::UINT32));
        let weak = Rc::downgrade(&self.0);
        action.connect_activate(move |_, param| {
            let (Some(inner), Some(index)) = (weak.upgrade(), param.and_then(|p| p.get::<u32>())) else { return };
            request_delete(&inner, index as usize);
        });
        group.add_action(&action);
        self.0.list.insert_action_group("sidebar", Some(&group));

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&self.0);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            if !matches!(key, gdk::Key::Delete | gdk::Key::KP_Delete) || !mods.is_empty() {
                return glib::Propagation::Proceed;
            }
            // Only PDF pages can be deleted.
            let selected = selected_indices(&inner.list);
            if !inner.pdf.get() || selected.is_empty() {
                return glib::Propagation::Proceed;
            }
            if let Some(f) = &*inner.on_delete.borrow() {
                f(selected);
            }
            glib::Propagation::Stop
        });
        self.0.list.add_controller(keys);
    }

    /// Accepts pages dropped on the list: shows where they will land while
    /// the pointer moves, scrolls near the edges, and reports the move.
    fn add_drop_target(&self) {
        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = Rc::downgrade(&self.0);
        target.connect_accept(move |_, _| weak.upgrade().is_some_and(|i| i.dragging.get().is_some()));
        let weak = Rc::downgrade(&self.0);
        target.connect_motion(move |_, x, y| {
            let Some(inner) = weak.upgrade() else { return gdk::DragAction::empty() };
            let gap = drop_gap(&inner.list, y);
            show_marker(&inner, gap);
            autoscroll(&inner, x, y);
            gdk::DragAction::MOVE
        });
        let weak = Rc::downgrade(&self.0);
        target.connect_leave(move |_| {
            if let Some(inner) = weak.upgrade() {
                end_drag_feedback(&inner);
            }
        });
        let weak = Rc::downgrade(&self.0);
        target.connect_drop(move |_, _, _, y| {
            let Some(inner) = weak.upgrade() else { return false };
            end_drag_feedback(&inner);
            let Some(from) = inner.dragging.get() else { return false };
            let gap = drop_gap(&inner.list, y);
            // Taking the page out first closes its gap, so a gap after it
            // is one place earlier.
            let to = if gap > from { gap - 1 } else { gap };
            if to != from
                && let Some(f) = &*inner.on_move.borrow()
            {
                f(from, to);
            }
            true
        });
        self.0.list.add_controller(target);
    }

    /// Lets page rows be dragged (only when there's more than one page).
    fn add_drag_source(&self, row: &gtk::ListBoxRow, picture: &gtk::Picture) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let weak = Rc::downgrade(&self.0);
        let weak_row = row.downgrade();
        source.connect_prepare(move |_, _, _| {
            let (inner, row) = (weak.upgrade()?, weak_row.upgrade()?);
            let index = usize::try_from(row.index()).ok()?;
            inner.dragging.set(Some(index));
            Some(gdk::ContentProvider::for_value(&(index as u32).to_value()))
        });
        let weak_row = row.downgrade();
        let picture = picture.downgrade();
        source.connect_drag_begin(move |source, _| {
            // The thumbnail follows the pointer; the row fades meanwhile.
            if let Some(picture) = picture.upgrade() {
                let icon = gtk::WidgetPaintable::new(Some(&picture));
                source.set_icon(Some(&icon), picture.width() / 2, picture.height() / 2);
            }
            if let Some(row) = weak_row.upgrade() {
                row.add_css_class("page-dragged");
            }
        });
        let weak = Rc::downgrade(&self.0);
        let weak_row = row.downgrade();
        source.connect_drag_end(move |_, _, _| {
            if let Some(row) = weak_row.upgrade() {
                row.remove_css_class("page-dragged");
            }
            if let Some(inner) = weak.upgrade() {
                inner.dragging.set(None);
                end_drag_feedback(&inner);
            }
        });
        row.add_controller(source);
    }

    /// Calls `f(from, to)` when the user drags the page at position `from`
    /// to position `to` (both from 0, `to` counted after it is taken out).
    /// The handler moves the page and then calls [`Sidebar::move_row`].
    pub fn connect_page_moved(&self, f: impl Fn(usize, usize) + 'static) {
        *self.0.on_move.borrow_mut() = Some(Box::new(f));
    }

    /// Moves row `from` to `to`, keeping its thumbnail, and renumbers the
    /// pages. Call [`Sidebar::select`] afterwards for the current page.
    pub fn move_row(&self, from: usize, to: usize) {
        let inner = &self.0;
        let mut rows = inner.rows.borrow_mut();
        if from >= rows.len() || to >= rows.len() || from == to {
            return;
        }
        // Taking out a selected row deselects it; that isn't a page click.
        inner.syncing.set(true);
        let row = rows.remove(from);
        inner.list.remove(&row.row);
        inner.list.insert(&row.row, to as i32);
        rows.insert(to, row);
        inner.syncing.set(false);
        renumber(&rows);
        // Thumbnails still waiting to be drawn follow their pages.
        for i in inner.queue.borrow_mut().iter_mut() {
            *i = crate::pages::moved_index(*i, from, to);
        }
    }

    /// The split view, to put in the window.
    pub fn widget(&self) -> &adw::OverlaySplitView {
        &self.0.split
    }

    /// Calls `f` with the page (from 0) when the user picks one.
    pub fn connect_page_chosen(&self, f: impl Fn(usize) + 'static) {
        let weak = Rc::downgrade(&self.0);
        self.0.list.connect_row_selected(move |_, row| {
            let (Some(inner), Some(row)) = (weak.upgrade(), row) else { return };
            // Ignore selections the app made itself (`select`, `rebuild`).
            // GTK 4.22 also reports a Ctrl+click that adds a row to the
            // selection here; only a row selected on its own is a page click.
            if !inner.syncing.get() && row.index() >= 0 && inner.list.selected_rows().len() == 1 {
                f(row.index() as usize);
            }
        });
    }

    /// Replaces the list with one row per page of `pages`. Thumbnails are
    /// drawn afterwards by `draw_pending`, a few at a time.
    pub fn rebuild(&self, pages: &Pages) {
        let inner = &self.0;
        inner.queue.borrow_mut().clear();
        inner.pdf.set(pages.is_pdf());
        // Removing rows deselects, which must not read as a page click.
        inner.syncing.set(true);
        inner.list.remove_all();
        let mut rows = Vec::with_capacity(pages.len());
        for i in 0..pages.len() {
            let row = self.make_row(pages, i);
            inner.list.append(&row.row);
            rows.push(row);
        }
        *inner.rows.borrow_mut() = rows;
        inner.syncing.set(false);
        // Draw from the current page onwards, then the ones before it.
        let cur = pages.current();
        inner.queue.borrow_mut().extend((cur..pages.len()).chain(0..cur));
        self.select(cur);
    }

    /// Builds the row for page `i` of `pages`, with a blank thumbnail of the
    /// right shape. PDF pages can be dragged and have a context menu.
    fn make_row(&self, pages: &Pages, i: usize) -> Row {
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .halign(gtk::Align::Center)
            .build();
        picture.add_css_class("page-thumb");
        // Size the placeholder to the page's shape now, so the list doesn't
        // jump about as thumbnails arrive.
        let (w, h) = pages.page_aspect(i);
        set_thumb_size(&picture, w, h);
        let number = gtk::Label::new(Some(&(i + 1).to_string()));
        number.add_css_class("caption");
        let dot = gtk::Label::new(Some("●"));
        dot.add_css_class("page-dot");
        dot.add_css_class("caption");
        dot.set_visible(false);
        dot.set_tooltip_text(Some("Marked up"));
        let caption = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        caption.set_halign(gtk::Align::Center);
        caption.append(&number);
        caption.append(&dot);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_margin_top(6);
        body.set_margin_bottom(6);
        body.append(&picture);
        body.append(&caption);
        let row = gtk::ListBoxRow::builder().child(&body).build();
        row.update_property(&[gtk::accessible::Property::Label(&format!("Page {}", i + 1))]);
        if pages.is_pdf() {
            self.add_drag_source(&row, &picture);
            add_context_menu(&row);
            row.set_tooltip_text(Some("Drag to move this page. Right-click for more"));
        }
        Row { row, number, picture, dot }
    }

    /// Calls `f(indices)` when the user asks to delete the pages at those
    /// positions (in ascending order): Delete Page in a row's menu, or the
    /// Delete key on the selected rows. The handler deletes them and then
    /// calls [`Sidebar::remove_row`] for each, last first.
    pub fn connect_delete_requested(&self, f: impl Fn(Vec<usize>) + 'static) {
        *self.0.on_delete.borrow_mut() = Some(Box::new(f));
    }

    /// Takes row `index` out of the list and renumbers the rest.
    pub fn remove_row(&self, index: usize) {
        let inner = &self.0;
        let mut rows = inner.rows.borrow_mut();
        if index >= rows.len() {
            return;
        }
        inner.syncing.set(true);
        let row = rows.remove(index);
        inner.list.remove(&row.row);
        inner.syncing.set(false);
        renumber(&rows);
        // Forget its pending thumbnail; later pages move up one.
        let mut q = inner.queue.borrow_mut();
        q.retain(|&i| i != index);
        for i in q.iter_mut() {
            if *i > index {
                *i -= 1;
            }
        }
    }

    /// Puts a row for page `index` of `pages` back into the list (after an
    /// undone delete) and queues its thumbnail.
    pub fn insert_row(&self, pages: &Pages, index: usize) {
        let inner = &self.0;
        let row = self.make_row(pages, index);
        let mut rows = inner.rows.borrow_mut();
        let index = index.min(rows.len());
        inner.list.insert(&row.row, index as i32);
        rows.insert(index, row);
        renumber(&rows);
        let mut q = inner.queue.borrow_mut();
        for i in q.iter_mut() {
            if *i >= index {
                *i += 1;
            }
        }
        q.push_front(index);
    }

    /// Highlights page `index` alone and scrolls it into view.
    pub fn select(&self, index: usize) {
        let inner = &self.0;
        let Some(row) = inner.list.row_at_index(index as i32) else { return };
        inner.syncing.set(true);
        inner.list.unselect_all();
        inner.list.select_row(Some(&row));
        inner.syncing.set(false);
        // Scroll once the row has a place in the list.
        let weak = Rc::downgrade(&self.0);
        glib::idle_add_local_once(move || {
            let Some(inner) = weak.upgrade() else { return };
            let Some(row) = inner.list.row_at_index(index as i32) else { return };
            let Some(bounds) = row.compute_bounds(&inner.list) else { return };
            let adj = inner.scroller.vadjustment();
            let (top, bottom) = (f64::from(bounds.y()), f64::from(bounds.y() + bounds.height()));
            if top < adj.value() {
                adj.set_value(top);
            } else if bottom > adj.value() + adj.page_size() {
                adj.set_value(bottom - adj.page_size());
            }
        });
    }

    /// Queues page `index`'s thumbnail to be redrawn first.
    pub fn refresh(&self, index: usize) {
        let mut q = self.0.queue.borrow_mut();
        q.retain(|&i| i != index);
        q.push_front(index);
    }

    /// Runs `then` (which redraws the current page's thumbnail) after a short
    /// pause; calling again restarts the pause. This debounces redraws while
    /// the user is dragging or typing.
    pub fn refresh_later(&self, then: impl FnOnce() + 'static) {
        if let Some(id) = self.0.timer.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(REFRESH_DELAY, move || {
            if let Some(inner) = weak.upgrade() {
                inner.timer.borrow_mut().take();
            }
            then();
        });
        *self.0.timer.borrow_mut() = Some(id);
    }

    /// Whether thumbnails are waiting to be drawn.
    pub fn has_pending(&self) -> bool {
        !self.0.queue.borrow().is_empty()
    }

    /// The next page whose thumbnail needs drawing.
    pub fn next_pending(&self) -> Option<usize> {
        self.0.queue.borrow_mut().pop_front()
    }

    /// Whether the idle drawing loop is running, and setting it.
    pub fn idle(&self) -> &RefCell<Option<glib::SourceId>> {
        &self.0.idle
    }

    /// Draws page `index`'s thumbnail with marks `doc`.
    pub fn draw(&self, pages: &Pages, index: usize, doc: &Document) {
        let rows = self.0.rows.borrow();
        let Some(row) = rows.get(index) else { return };
        // Render at device pixels so thumbnails are sharp on HiDPI screens.
        let scale = f64::from(self.0.split.scale_factor().max(1));
        match pages.thumbnail(index, doc, f64::from(THUMB_WIDTH) * scale) {
            Ok(tex) => {
                set_thumb_size(&row.picture, f64::from(tex.width()), f64::from(tex.height()));
                row.picture.set_paintable(Some(&tex));
            }
            Err(e) => glib::g_warning!("pygoscribe", "Could not draw page {} thumbnail: {e}", index + 1),
        }
        row.dot.set_visible(crate::pages::is_marked(doc));
    }
}

/// Passes a delete request from page `index`'s menu to the window, if it
/// listens: for all the selected pages if `index` is one of them, otherwise
/// for that page alone.
fn request_delete(inner: &Inner, index: usize) {
    if let Some(f) = &*inner.on_delete.borrow() {
        f(pages_to_delete(&selected_indices(&inner.list), index));
    }
}

/// The pages a delete from page `index`'s menu applies to, given the
/// `selected` ones: the whole selection if it includes `index`.
fn pages_to_delete(selected: &[usize], index: usize) -> Vec<usize> {
    if selected.contains(&index) { selected.to_vec() } else { vec![index] }
}

/// The positions of the selected rows, in ascending order.
fn selected_indices(list: &gtk::ListBox) -> Vec<usize> {
    let mut v: Vec<usize> = list.selected_rows().iter().filter_map(|r| usize::try_from(r.index()).ok()).collect();
    v.sort_unstable();
    v
}

/// Numbers the rows 1, 2, 3… in list order, on screen and for screen readers.
fn renumber(rows: &[Row]) {
    for (i, r) in rows.iter().enumerate() {
        r.number.set_label(&(i + 1).to_string());
        r.row.update_property(&[gtk::accessible::Property::Label(&format!("Page {}", i + 1))]);
    }
}

/// A right-click menu on a page row with Delete Page, which runs the
/// sidebar's `sidebar.delete-page` action for the row's current position.
/// On a row that is one of several selected, it deletes them all.
fn add_context_menu(row: &gtk::ListBoxRow) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    let weak_row = row.downgrade();
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(row) = weak_row.upgrade() else { return };
        let Ok(index) = u32::try_from(row.index()) else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        // Built on each click, since dragging changes the row's position.
        let menu = gio::Menu::new();
        let n = row
            .parent()
            .and_downcast::<gtk::ListBox>()
            .map_or(1, |list| pages_to_delete(&selected_indices(&list), index as usize).len());
        let label = if n > 1 { format!("_Delete {n} Pages") } else { "_Delete Page".to_owned() };
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_action_and_target_value(Some("sidebar.delete-page"), Some(&index.to_variant()));
        menu.append_item(&item);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&row);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            // Unparent once the menu item's action has run.
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    });
    row.add_controller(click);
}

/// The gap a page dropped at `y` (list coordinates) goes into: 0 is before
/// the first row, `n` after the last. The upper half of a row means the gap
/// above it, the lower half the gap below.
fn drop_gap(list: &gtk::ListBox, y: f64) -> usize {
    let n = rows_in(list);
    let Some(row) = list.row_at_y(y as i32) else {
        // Past the last row (or above the first, which has no space above).
        return if y < 0.0 { 0 } else { n };
    };
    let index = usize::try_from(row.index()).unwrap_or(n);
    let below = row.compute_bounds(list).is_some_and(|b| y > f64::from(b.y() + b.height() / 2.0));
    if below { index + 1 } else { index }
}

/// The number of rows in `list`.
fn rows_in(list: &gtk::ListBox) -> usize {
    let mut n = 0;
    while list.row_at_index(n as i32).is_some() {
        n += 1;
    }
    n
}

/// Draws the drop line for `gap`: above the row after it, or below the last.
fn show_marker(inner: &Inner, gap: usize) {
    let want = match inner.list.row_at_index(gap as i32) {
        Some(row) => Some((row, false)),
        None => gap.checked_sub(1).and_then(|i| inner.list.row_at_index(i as i32)).map(|r| (r, true)),
    };
    let mut marker = inner.marker.borrow_mut();
    if *marker == want {
        return;
    }
    if let Some((row, _)) = marker.take() {
        row.remove_css_class("drop-above");
        row.remove_css_class("drop-below");
    }
    if let Some((row, below)) = &want {
        row.add_css_class(if *below { "drop-below" } else { "drop-above" });
    }
    *marker = want;
}

/// Scrolls the list while a drag hovers near its top or bottom edge, faster
/// the closer it gets. (`x`, `y`) is the pointer in list coordinates.
fn autoscroll(inner: &Rc<Inner>, x: f64, y: f64) {
    let Some(p) = inner.list.compute_point(&inner.scroller, &gtk::graphene::Point::new(x as f32, y as f32)) else {
        return;
    };
    let (y, h) = (f64::from(p.y()), f64::from(inner.scroller.height()));
    let speed = if y < SCROLL_EDGE {
        -SCROLL_STEP * (1.0 - y.max(0.0) / SCROLL_EDGE)
    } else if y > h - SCROLL_EDGE {
        SCROLL_STEP * (1.0 - (h - y).max(0.0) / SCROLL_EDGE)
    } else {
        0.0
    };
    inner.scroll_speed.set(speed);
    if speed == 0.0 || inner.scroll_timer.borrow().is_some() {
        return;
    }
    let weak = Rc::downgrade(inner);
    let id = glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
        let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
        let speed = inner.scroll_speed.get();
        if speed == 0.0 {
            inner.scroll_timer.borrow_mut().take();
            return glib::ControlFlow::Break;
        }
        let adj = inner.scroller.vadjustment();
        adj.set_value((adj.value() + speed).clamp(adj.lower(), adj.upper() - adj.page_size()));
        glib::ControlFlow::Continue
    });
    *inner.scroll_timer.borrow_mut() = Some(id);
}

/// Clears the drop line and stops any scrolling once a drag leaves or ends.
fn end_drag_feedback(inner: &Inner) {
    if let Some((row, _)) = inner.marker.borrow_mut().take() {
        row.remove_css_class("drop-above");
        row.remove_css_class("drop-below");
    }
    inner.scroll_speed.set(0.0);
    if let Some(id) = inner.scroll_timer.borrow_mut().take() {
        id.remove();
    }
}

/// Sizes a thumbnail for a page of `w` × `h`: a fixed width, height to match.
fn set_thumb_size(picture: &gtk::Picture, w: f64, h: f64) {
    // Fall back to A4 portrait (√2) for an unknown size; clamp so a very
    // tall or very wide page can't make an absurd row.
    let ratio = if w > 0.0 && h > 0.0 { h / w } else { 1.414 };
    let height = (f64::from(THUMB_WIDTH) * ratio).round().clamp(16.0, 4.0 * f64::from(THUMB_WIDTH)) as i32;
    picture.set_size_request(THUMB_WIDTH, height);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::{cairo, gio};

    /// Moving a row keeps its thumbnail widgets, renumbers the pages and
    /// remaps thumbnails still waiting to be drawn. Needs a display:
    /// `cargo test -- --ignored sidebar`.
    #[test]
    #[ignore]
    fn sidebar_moves_rows_and_renumbers() {
        gtk::init().unwrap();
        let dir = std::env::temp_dir().join(format!("pygoscribe-sidebar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("four.pdf");
        {
            let surface = cairo::PdfSurface::new(100.0, 100.0, &src).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            for _ in 0..4 {
                cr.show_page().unwrap();
            }
            drop(cr);
            surface.finish();
        }
        let pdf = crate::pdf::Pdf::open(&gio::File::for_path(&src)).unwrap();
        let pages = Pages::pdf(pdf, None);
        let sidebar = Sidebar::new(&gtk::Box::new(gtk::Orientation::Vertical, 0));
        sidebar.rebuild(&pages);
        let pictures: Vec<gtk::Picture> = sidebar.0.rows.borrow().iter().map(|r| r.picture.clone()).collect();

        // Page 4 to the front.
        sidebar.move_row(3, 0);
        let rows = sidebar.0.rows.borrow();
        assert_eq!(rows[0].picture, pictures[3], "thumbnail moved with the row");
        assert_eq!(rows[1].picture, pictures[0]);
        assert_eq!(sidebar.0.list.row_at_index(0).as_ref(), Some(&rows[0].row), "list order matches");
        let labels: Vec<String> = rows.iter().map(|r| r.number.label().to_string()).collect();
        assert_eq!(labels, ["1", "2", "3", "4"]);
        drop(rows);
        // The queue was 0 1 2 3 (current page first); now the same pages are
        // at 1 2 3 0.
        assert_eq!(sidebar.0.queue.borrow().iter().copied().collect::<Vec<_>>(), [1, 2, 3, 0]);
        assert_eq!(rows_in(&sidebar.0.list), 4);

        // Delete the second row, then put a row back there.
        sidebar.remove_row(1);
        assert_eq!(rows_in(&sidebar.0.list), 3);
        assert_eq!(sidebar.0.rows.borrow()[1].picture, pictures[1]);
        assert_eq!(sidebar.0.queue.borrow().iter().copied().collect::<Vec<_>>(), [1, 2, 0]);
        sidebar.insert_row(&pages, 1);
        let labels: Vec<String> = sidebar.0.rows.borrow().iter().map(|r| r.number.label().to_string()).collect();
        assert_eq!(labels, ["1", "2", "3", "4"]);
        assert_eq!(sidebar.0.queue.borrow().front(), Some(&1), "its thumbnail is drawn next");

        // Several rows can be selected; `select` goes back to one.
        let list = &sidebar.0.list;
        sidebar.select(0);
        list.select_row(list.row_at_index(3).as_ref());
        list.select_row(list.row_at_index(1).as_ref());
        assert_eq!(selected_indices(list), [0, 1, 3]);
        sidebar.select(2);
        assert_eq!(selected_indices(list), [2]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A row's Delete Page deletes the whole selection only if the row is in it.
    #[test]
    fn menu_delete_applies_to_the_selection_it_is_in() {
        assert_eq!(pages_to_delete(&[1, 3, 4], 3), [1, 3, 4]);
        assert_eq!(pages_to_delete(&[1, 3, 4], 2), [2]);
        assert_eq!(pages_to_delete(&[], 0), [0]);
    }
}
