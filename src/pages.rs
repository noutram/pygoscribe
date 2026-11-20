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

//! The pages of what is open: one picture, or every page of a PDF. Each page
//! keeps its own marks, crop and undo history. The canvas works on one page
//! at a time; the others wait here.
//!
//! # Ownership of page state
//!
//! The current page's [`Document`] and [`History`] live on the canvas, where
//! they are edited. Every other page's state is parked in [`Pages`]. Switching
//! page swaps the two ([`Pages::switch`]): the canvas hands over the state it
//! was holding and receives the new page's. This keeps undo strictly per
//! page, and means a page's state is never in two places at once.
//!
//! # Page order
//!
//! Pages can be dragged into a new order in the sidebar. Everything here is
//! indexed by *position* (where the page is now, from 0); `order` maps each
//! position to the page of the source it shows, so poppler is always asked
//! for the right page and Save as PDF writes the pages in their new order.
//! A page's marks and history travel with it, because they are stored by
//! position and moved along with `order` ([`Pages::move_page`]).

use crate::document::{Document, History};
use crate::pdf::Pdf;
use gtk::cairo::{Context, ImageSurface};
use gtk::gdk;
use std::rc::Rc;

/// Where the pages come from.
pub enum Source {
    /// A single raster image (a file or the clipboard).
    Picture(ImageSurface),
    /// A PDF document; pages are rendered on demand.
    Pdf(Pdf),
}

/// Marks and undo history of a page that isn't on the canvas.
#[derive(Default)]
pub struct PageState {
    /// The page's marks, crop and any resized image.
    pub doc: Document,
    /// The page's undo and redo stacks.
    pub history: History,
}

/// How many rendered PDF pages to keep: the current one and the last one, so
/// flicking back is instant without holding a whole document in memory.
const CACHED_PAGES: usize = 2;

/// Everything open in one window: the source and every page's state.
pub struct Pages {
    /// The picture or PDF the pages come from.
    pub source: Source,
    /// The file name, for the title and save names. None for the clipboard.
    pub name: Option<String>,
    /// The file it was opened from, if Save can write back to it.
    pub original: Option<crate::output::Original>,
    /// One per page, by position. The current page's entry is None: its
    /// state is on the canvas.
    states: Vec<Option<PageState>>,
    /// For each position, the source page (from 0) shown there.
    order: Vec<usize>,
    /// The position (from 0) of the page on the canvas.
    current: usize,
    /// Unique to this `Pages` (see [`Pages::id`]).
    id: u64,
    /// Recently rendered PDF pages as (source page, image), most recent
    /// first, at most [`CACHED_PAGES`] long.
    cache: Vec<(usize, ImageSurface)>,
}

impl Pages {
    /// A single picture, which is its own one and only page.
    pub fn picture(image: ImageSurface, name: Option<String>) -> Self {
        Self {
            source: Source::Picture(image),
            name,
            original: None,
            states: vec![None],
            order: vec![0],
            current: 0,
            id: next_id(),
            cache: Vec::new(),
        }
    }

    /// Every page of `pdf`, with page 1 current. `pdf` must have pages.
    pub fn pdf(pdf: Pdf, name: Option<String>) -> Self {
        let n = pdf.n_pages().max(1) as usize;
        let mut states: Vec<Option<PageState>> = (0..n).map(|_| Some(PageState::default())).collect();
        // Page 1 starts on the canvas.
        states[0] = None;
        Self { source: Source::Pdf(pdf), name, original: None, states, order: (0..n).collect(), current: 0, id: next_id(), cache: Vec::new() }
    }

    /// The number of pages (1 for a picture).
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// The index (from 0) of the page on the canvas.
    pub fn current(&self) -> usize {
        self.current
    }

    /// For each position, the source page (from 0) shown there.
    #[cfg(test)]
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The source page (from 0) at position `index`, as poppler numbers it.
    fn source_page(&self, index: usize) -> i32 {
        self.order.get(index).copied().unwrap_or(index) as i32
    }

    /// Moves the page at position `from` to position `to`, the pages between
    /// shifting up or down one. Its marks and undo history go with it, and
    /// the current page stays current wherever it ends up. Returns false
    /// (and changes nothing) if either position is out of range or they
    /// are the same.
    pub fn move_page(&mut self, from: usize, to: usize) -> bool {
        let n = self.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let state = self.states.remove(from);
        self.states.insert(to, state);
        let page = self.order.remove(from);
        self.order.insert(to, page);
        self.current = moved_index(self.current, from, to);
        true
    }

    /// Deletes the page at position `index`, which must not be the current
    /// page (go to another page first) or the only one. Returns what
    /// [`Pages::restore_page`] needs to put it back: its source page and its
    /// marks and history. None (and nothing changes) if it can't be deleted.
    pub fn delete_page(&mut self, index: usize) -> Option<(usize, PageState)> {
        if index >= self.len() || index == self.current || self.len() < 2 {
            return None;
        }
        let state = self.states.remove(index).unwrap_or_default();
        let page = self.order.remove(index);
        if self.current > index {
            self.current -= 1;
        }
        Some((page, state))
    }

    /// Puts a deleted page back at position `index` (or the end, if there
    /// are fewer pages now). Returns the position it went to.
    pub fn restore_page(&mut self, index: usize, page: usize, state: PageState) -> usize {
        let index = index.min(self.len());
        self.states.insert(index, Some(state));
        self.order.insert(index, page);
        if self.current >= index {
            self.current += 1;
        }
        index
    }

    /// A number identifying this set of pages, so a stale Undo for a
    /// document that has since been replaced can be told apart.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Whether the pages come from a PDF.
    pub fn is_pdf(&self) -> bool {
        matches!(self.source, Source::Pdf(_))
    }

    /// The marks of a page that isn't current (None for the current page).
    pub fn doc(&self, index: usize) -> Option<&Document> {
        self.states.get(index)?.as_ref().map(|s| &s.doc)
    }

    /// The full-size image of page `index` to mark up on.
    pub fn image(&mut self, index: usize) -> Result<ImageSurface, String> {
        match &self.source {
            Source::Picture(image) => Ok(image.clone()),
            Source::Pdf(pdf) => {
                let page = self.source_page(index);
                if let Some((_, s)) = self.cache.iter().find(|(i, _)| *i as i32 == page) {
                    return Ok(s.clone());
                }
                // A small most-recently-used cache: newest at the front.
                let (tex, _) = pdf.render(page)?;
                let surface = crate::render::surface_from_texture(&tex).map_err(|e| e.to_string())?;
                self.cache.insert(0, (page as usize, surface.clone()));
                self.cache.truncate(CACHED_PAGES);
                Ok(surface)
            }
        }
    }

    /// Page `index`'s image size in pixels (for a placeholder before its
    /// thumbnail is drawn).
    pub fn page_aspect(&self, index: usize) -> (f64, f64) {
        match &self.source {
            Source::Picture(image) => (f64::from(image.width()), f64::from(image.height())),
            Source::Pdf(pdf) => pdf
                .page_size(self.source_page(index))
                .map(|s| (f64::from(s.width), f64::from(s.height)))
                // A4 proportions if Poppler can't tell us.
                .unwrap_or((210.0, 297.0)),
        }
    }

    /// PDF points per image pixel for page `index`, for Save as PDF.
    pub fn points_per_pixel(&self, index: usize) -> f64 {
        match &self.source {
            Source::Picture(_) => 1.0,
            Source::Pdf(pdf) => pdf.page_size(self.source_page(index)).map(|s| s.points_per_pixel()).unwrap_or(1.0),
        }
    }

    /// Draws page `index` as vectors in image pixels, for a sharp zoomed-in
    /// view. None for pictures, whose image is all there is.
    pub fn painter(&self, index: usize) -> Option<crate::render::Painter> {
        match &self.source {
            Source::Picture(_) => None,
            Source::Pdf(pdf) => pdf.painter(self.source_page(index)).ok().map(|p| Rc::new(p) as crate::render::Painter),
        }
    }

    /// Makes page `to` current. `leaving` is the state the canvas held for
    /// the current page; the returned state is the one to put on the canvas.
    /// Returns None (and changes nothing) if `to` is out of range or already
    /// current.
    pub fn switch(&mut self, to: usize, leaving: PageState) -> Option<PageState> {
        if to >= self.len() || to == self.current {
            return None;
        }
        let arriving = self.states[to].take().unwrap_or_default();
        self.states[self.current] = Some(leaving);
        self.current = to;
        Some(arriving)
    }

    /// Saves every page, in its current order and with its marks, as a
    /// vector PDF. `current` is the current page's marks (from the canvas).
    /// Only for pages from a PDF.
    pub fn save_pdf(&self, current: &Document, path: &std::path::Path) -> Result<(), String> {
        let Source::Pdf(pdf) = &self.source else { return Err("This isn't a PDF".into()) };
        pdf.save_marked(&self.order, &self.all_docs(current), path)
    }

    /// Every page's marks, by position, using `current` for the current page.
    pub fn all_docs(&self, current: &Document) -> Vec<Document> {
        self.states
            .iter()
            .map(|s| s.as_ref().map(|s| s.doc.clone()).unwrap_or_else(|| current.clone()))
            .collect()
    }

    /// A thumbnail of page `index` with marks `doc`, `width` pixels wide
    /// (before any crop narrows it).
    pub fn thumbnail(&self, index: usize, doc: &Document, width: f64) -> Result<gdk::Texture, String> {
        let err = |e: gtk::cairo::Error| e.to_string();
        match &self.source {
            Source::Picture(image) => {
                // Use the resized image if Scale to Visible Size has run.
                let image = doc.image.as_ref().map_or(image, |p| &p.0);
                let size = (f64::from(image.width()), f64::from(image.height()));
                let paint = |cr: &Context| crate::render::paint_image(cr, image);
                crate::render::render_thumbnail(&paint, size, doc, width / size.0.max(1.0)).map_err(err)
            }
            Source::Pdf(pdf) => {
                // Draw the PDF page as vectors straight at thumbnail size,
                // rather than shrinking the 144 dpi render.
                let page = self.source_page(index);
                let ps = pdf.page_size(page)?;
                let paint = pdf.painter(page)?;
                let size = (f64::from(ps.width), f64::from(ps.height));
                crate::render::render_thumbnail(&paint, size, doc, width / size.0.max(1.0)).map_err(err)
            }
        }
    }
}

/// A new [`Pages::id`].
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Where the item at position `i` ends up when the item at `from` moves to
/// `to` and the ones between close the gap.
pub fn moved_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < i && i <= to {
        i - 1
    } else if to <= i && i < from {
        i + 1
    } else {
        i
    }
}

/// Whether a page has anything on it worth showing as changed.
pub fn is_marked(doc: &Document) -> bool {
    !doc.shapes.is_empty() || doc.crop.is_some() || doc.image.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Kind, Shape, Style};
    use gtk::cairo;

    /// A 10 × 10 rectangle at (`x`, `x`), to tell marks apart.
    fn mark(x: f64) -> Shape {
        let mut s = Shape::new(Kind::Rect, (x, x), Style::default());
        s.b = (x + 10.0, x + 10.0);
        s
    }

    /// Each page keeps its own marks and undo history across switches.
    #[test]
    fn pages_keep_their_own_marks_and_history() {
        let image = ImageSurface::create(cairo::Format::ARgb32, 10, 10).unwrap();
        let mut pages = Pages::picture(image, None);
        pages.states = vec![None, Some(PageState::default()), Some(PageState::default())];

        // Page 1: one mark, with an undo step.
        let mut on_canvas = PageState::default();
        let before = on_canvas.doc.clone();
        on_canvas.doc.shapes.push(mark(1.0));
        on_canvas.history.record(before, &on_canvas.doc);

        // Go to page 3: it's empty with nothing to undo.
        let mut on_canvas = pages.switch(2, on_canvas).unwrap();
        assert_eq!(pages.current(), 2);
        assert!(on_canvas.doc.shapes.is_empty());
        assert!(!on_canvas.history.can_undo());
        on_canvas.doc.shapes.push(mark(5.0));

        // Undoing on page 3 can't reach page 1's mark.
        assert!(!on_canvas.history.undo(&mut on_canvas.doc));
        assert_eq!(pages.doc(0).unwrap().shapes.len(), 1);

        // Back to page 1: its mark and undo step are still there.
        let mut on_canvas = pages.switch(0, on_canvas).unwrap();
        assert_eq!(on_canvas.doc.shapes.len(), 1);
        assert!(on_canvas.history.undo(&mut on_canvas.doc));
        assert!(on_canvas.doc.shapes.is_empty());
        assert_eq!(pages.doc(2).unwrap().shapes.len(), 1, "page 3 untouched");

        // Collecting every page uses the canvas state for the current page.
        let all = pages.all_docs(&on_canvas.doc);
        assert_eq!(all.iter().map(|d| d.shapes.len()).collect::<Vec<_>>(), vec![0, 0, 1]);
        assert!(!is_marked(&all[0]) && is_marked(&all[2]));

        // Switching to the current page or past the end does nothing.
        assert!(pages.switch(0, PageState::default()).is_none());
        assert!(pages.switch(9, PageState::default()).is_none());
    }

    /// Moving a page takes its marks with it, shifts the others, keeps the
    /// current page current and maps positions to the right source pages.
    #[test]
    fn moving_pages_keeps_marks_and_order() {
        let image = ImageSurface::create(cairo::Format::ARgb32, 10, 10).unwrap();
        let mut pages = Pages::picture(image, None);
        // Four pages; page 1 (position 0) is on the canvas.
        pages.states = (0..4).map(|_| Some(PageState::default())).collect();
        pages.states[0] = None;
        pages.order = (0..4).collect();
        for i in 1..4 {
            pages.states[i].as_mut().unwrap().doc.shapes.push(mark(i as f64));
        }

        // Drag page 4 to the front: order 3 0 1 2, current page now second.
        assert!(pages.move_page(3, 0));
        assert_eq!(pages.order(), &[3, 0, 1, 2]);
        assert_eq!(pages.current(), 1);
        assert_eq!(pages.doc(0).unwrap().shapes[0].a, (3.0, 3.0), "marks went with the page");
        assert!(pages.doc(1).is_none(), "current page's state is still on the canvas");

        // Drag the current page to the end: order 3 1 2 0.
        assert!(pages.move_page(1, 3));
        assert_eq!(pages.order(), &[3, 1, 2, 0]);
        assert_eq!(pages.current(), 3);
        assert_eq!(pages.source_page(0), 3);

        // Collecting every page follows the new order.
        let canvas = Document { shapes: vec![mark(9.0)], ..Document::default() };
        let all = pages.all_docs(&canvas);
        let firsts: Vec<f64> = all.iter().map(|d| d.shapes[0].a.0).collect();
        assert_eq!(firsts, vec![3.0, 1.0, 2.0, 9.0]);

        // Out of range or no move: nothing changes.
        assert!(!pages.move_page(0, 0));
        assert!(!pages.move_page(0, 4));
        assert_eq!(pages.order(), &[3, 1, 2, 0]);
    }

    /// Deleting a page drops it from the order and the saved pages; undoing
    /// puts it back with its marks. The current and only pages can't go.
    #[test]
    fn deleting_and_restoring_pages() {
        let image = ImageSurface::create(cairo::Format::ARgb32, 10, 10).unwrap();
        let mut pages = Pages::picture(image, None);
        pages.states = (0..4).map(|_| Some(PageState::default())).collect();
        pages.order = (0..4).collect();
        pages.states[2] = None;
        pages.current = 2;
        pages.states[1].as_mut().unwrap().doc.shapes.push(mark(1.0));

        assert!(pages.delete_page(2).is_none(), "the current page stays");
        let (page, state) = pages.delete_page(1).unwrap();
        assert_eq!((page, state.doc.shapes.len()), (1, 1));
        assert_eq!(pages.order(), &[0, 2, 3]);
        assert_eq!(pages.current(), 1, "current page moved up one");
        assert_eq!(pages.all_docs(&Document::default()).len(), 3);

        assert_eq!(pages.restore_page(1, page, state), 1);
        assert_eq!(pages.order(), &[0, 1, 2, 3]);
        assert_eq!(pages.current(), 2);
        assert_eq!(pages.doc(1).unwrap().shapes.len(), 1, "marks came back");

        // Down to one page: that one can't be deleted.
        pages.delete_page(0).unwrap();
        pages.delete_page(2).unwrap();
        pages.delete_page(0).unwrap();
        assert_eq!(pages.len(), 1);
        assert!(pages.delete_page(0).is_none());
    }

    #[test]
    fn moved_index_shifts_the_pages_between() {
        // Moving 1 → 3 in [a b c d e]: b lands at 3, c and d move up.
        let after: Vec<usize> = (0..5).map(|i| moved_index(i, 1, 3)).collect();
        assert_eq!(after, vec![0, 3, 1, 2, 4]);
        // Moving 3 → 0: d to the front, a b c move down.
        let after: Vec<usize> = (0..5).map(|i| moved_index(i, 3, 0)).collect();
        assert_eq!(after, vec![1, 2, 3, 0, 4]);
    }
}
