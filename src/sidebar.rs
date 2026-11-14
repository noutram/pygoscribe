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

use crate::pages::Pages;
use crate::document::Document;
use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// Thumbnail width in logical pixels.
const THUMB_WIDTH: i32 = 120;
/// After a change on the canvas, wait this long before redrawing its
/// thumbnail, so dragging a mark doesn't re-render every frame.
const REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

struct Row {
    picture: gtk::Picture,
    dot: gtk::Label,
}

struct Inner {
    split: adw::OverlaySplitView,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    rows: RefCell<Vec<Row>>,
    /// Set while the app selects a row itself, so that isn't seen as a click.
    syncing: Cell<bool>,
    /// Pages whose thumbnails still need drawing, most urgent first.
    queue: RefCell<VecDeque<usize>>,
    idle: RefCell<Option<glib::SourceId>>,
    timer: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct Sidebar(Rc<Inner>);

impl Sidebar {
    /// A split view with `content` beside the (still empty) page list.
    pub fn new(content: &impl IsA<gtk::Widget>) -> Self {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
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

        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".pygoscribe-pages .page-dot { color: @accent_color; font-weight: bold; }\n\
             .pygoscribe-pages .page-thumb { background: white; box-shadow: 0 1px 3px alpha(black, 0.3); }",
        );
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&split),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        Self(Rc::new(Inner {
            split,
            list,
            scroller,
            rows: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            queue: RefCell::new(VecDeque::new()),
            idle: RefCell::new(None),
            timer: RefCell::new(None),
        }))
    }

    pub fn widget(&self) -> &adw::OverlaySplitView {
        &self.0.split
    }

    /// Calls `f` with the page (from 0) when the user picks one.
    pub fn connect_page_chosen(&self, f: impl Fn(usize) + 'static) {
        let weak = Rc::downgrade(&self.0);
        self.0.list.connect_row_selected(move |_, row| {
            let (Some(inner), Some(row)) = (weak.upgrade(), row) else { return };
            if !inner.syncing.get() && row.index() >= 0 {
                f(row.index() as usize);
            }
        });
    }

    /// Replaces the list with one row per page of `pages`. Thumbnails are
    /// drawn afterwards by `draw_pending`, a few at a time.
    pub fn rebuild(&self, pages: &Pages) {
        let inner = &self.0;
        inner.queue.borrow_mut().clear();
        inner.syncing.set(true);
        inner.list.remove_all();
        let mut rows = Vec::with_capacity(pages.len());
        for i in 0..pages.len() {
            let picture = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .halign(gtk::Align::Center)
                .build();
            picture.add_css_class("page-thumb");
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
            inner.list.append(&row);
            rows.push(Row { picture, dot });
        }
        *inner.rows.borrow_mut() = rows;
        inner.syncing.set(false);
        // Draw from the current page onwards, then the ones before it.
        let cur = pages.current();
        inner.queue.borrow_mut().extend((cur..pages.len()).chain(0..cur));
        self.select(cur);
    }

    /// Highlights page `index` and scrolls it into view.
    pub fn select(&self, index: usize) {
        let inner = &self.0;
        let Some(row) = inner.list.row_at_index(index as i32) else { return };
        inner.syncing.set(true);
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

    /// Redraws `index`'s thumbnail after a short pause; calling again
    /// restarts the pause. `then` runs when the pause ends.
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

/// Sizes a thumbnail for a page of `w` × `h`: a fixed width, height to match.
fn set_thumb_size(picture: &gtk::Picture, w: f64, h: f64) {
    let ratio = if w > 0.0 && h > 0.0 { h / w } else { 1.414 };
    let height = (f64::from(THUMB_WIDTH) * ratio).round().clamp(16.0, 4.0 * f64::from(THUMB_WIDTH)) as i32;
    picture.set_size_request(THUMB_WIDTH, height);
}
