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

use crate::document::{Document, History};
use crate::pdf::Pdf;
use gtk::cairo::{Context, ImageSurface};
use gtk::gdk;
use std::rc::Rc;

/// Where the pages come from.
pub enum Source {
    Picture(ImageSurface),
    Pdf(Pdf),
}

/// Marks and undo history of a page that isn't on the canvas.
#[derive(Default)]
pub struct PageState {
    pub doc: Document,
    pub history: History,
}

/// How many rendered PDF pages to keep: the current one and the last one, so
/// flicking back is instant without holding a whole document in memory.
const CACHED_PAGES: usize = 2;

pub struct Pages {
    pub source: Source,
    /// The file name, for the title and save names. None for the clipboard.
    pub name: Option<String>,
    /// The file it was opened from, if Save can write back to it.
    pub original: Option<crate::output::Original>,
    /// One per page. The current page's entry is None: its state is on the
    /// canvas.
    states: Vec<Option<PageState>>,
    current: usize,
    cache: Vec<(usize, ImageSurface)>,
}

impl Pages {
    pub fn picture(image: ImageSurface, name: Option<String>) -> Self {
        Self { source: Source::Picture(image), name, original: None, states: vec![None], current: 0, cache: Vec::new() }
    }

    /// Every page of `pdf`, with page 1 current. `pdf` must have pages.
    pub fn pdf(pdf: Pdf, name: Option<String>) -> Self {
        let n = pdf.n_pages().max(1) as usize;
        let mut states: Vec<Option<PageState>> = (0..n).map(|_| Some(PageState::default())).collect();
        states[0] = None;
        Self { source: Source::Pdf(pdf), name, original: None, states, current: 0, cache: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn current(&self) -> usize {
        self.current
    }

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
                if let Some((_, s)) = self.cache.iter().find(|(i, _)| *i == index) {
                    return Ok(s.clone());
                }
                let (tex, _) = pdf.render(index as i32)?;
                let surface = crate::render::surface_from_texture(&tex).map_err(|e| e.to_string())?;
                self.cache.insert(0, (index, surface.clone()));
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
                .page_size(index as i32)
                .map(|s| (f64::from(s.width), f64::from(s.height)))
                .unwrap_or((210.0, 297.0)),
        }
    }

    /// PDF points per image pixel for page `index`, for Save as PDF.
    pub fn points_per_pixel(&self, index: usize) -> f64 {
        match &self.source {
            Source::Picture(_) => 1.0,
            Source::Pdf(pdf) => pdf.page_size(index as i32).map(|s| s.points_per_pixel()).unwrap_or(1.0),
        }
    }

    /// Draws page `index` as vectors in image pixels, for a sharp zoomed-in
    /// view. None for pictures, whose image is all there is.
    pub fn painter(&self, index: usize) -> Option<crate::render::Painter> {
        match &self.source {
            Source::Picture(_) => None,
            Source::Pdf(pdf) => pdf.painter(index as i32).ok().map(|p| Rc::new(p) as crate::render::Painter),
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

    /// Every page's marks, using `current` for the current page.
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
                let image = doc.image.as_ref().map_or(image, |p| &p.0);
                let size = (f64::from(image.width()), f64::from(image.height()));
                let paint = |cr: &Context| crate::render::paint_image(cr, image);
                crate::render::render_thumbnail(&paint, size, doc, width / size.0.max(1.0)).map_err(err)
            }
            Source::Pdf(pdf) => {
                let ps = pdf.page_size(index as i32)?;
                let paint = pdf.painter(index as i32)?;
                let size = (f64::from(ps.width), f64::from(ps.height));
                crate::render::render_thumbnail(&paint, size, doc, width / size.0.max(1.0)).map_err(err)
            }
        }
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
}
