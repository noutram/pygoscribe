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

//! Opening PDFs: a page is rendered with poppler into an image, which is then
//! marked up like any other picture.
//!
//! # Coordinates
//!
//! PDF pages are measured in points (1/72 inch). The canvas and the marks
//! work in *image pixels* of the rendered page, at [`PIXELS_PER_POINT`]
//! (capped by [`MAX_SIDE`] for huge pages). [`PageSize::scale`] converts
//! points to pixels; [`PageSize::points_per_pixel`] goes back, which is how
//! Save as PDF gives each page its original printed size.
//!
//! # Raster for editing, vector for output
//!
//! Poppler draws a page onto any cairo context. For editing, [`Pdf::render`]
//! draws it once onto an image surface, which the canvas treats like a
//! picture. Everywhere sharpness or file size matters (zoomed-in redraws,
//! thumbnails, the loupe and [`Pdf::save_marked`]), [`Pdf::painter`] draws
//! the page again as vectors at whatever scale the target needs, so text in
//! a saved PDF stays selectable.

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};

/// Pages are rendered at this many pixels per PDF point: 2 is 144 dpi, sharp
/// enough to mark up and zoom into.
pub const PIXELS_PER_POINT: f64 = 2.0;

/// The longest side of a rendered page, in pixels, so huge pages (posters,
/// drawings) don't need gigabytes of memory.
const MAX_SIDE: f64 = 8192.0;

/// Whether `file` is a PDF: by its name, or else by its first bytes.
pub fn is_pdf(file: &gio::File) -> bool {
    let named_pdf = file
        .basename()
        .and_then(|n| n.extension().map(|e| e.eq_ignore_ascii_case("pdf")))
        .unwrap_or(false);
    if named_pdf {
        return true;
    }
    // Every PDF starts with the "%PDF-" magic bytes.
    file.read(gio::Cancellable::NONE)
        .and_then(|s| s.read_bytes(5, gio::Cancellable::NONE))
        .is_ok_and(|b| b.as_ref() == b"%PDF-")
}

/// An open PDF document. Cloning is cheap: it shares the Poppler document.
#[derive(Clone)]
pub struct Pdf(poppler::Document);

/// How a page maps to the image it is marked up on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    /// Image pixels per PDF point.
    pub scale: f64,
    /// The page's image size in pixels.
    pub width: i32,
    /// See `width`.
    pub height: i32,
}

impl PageSize {
    /// PDF points per image pixel, the inverse of `scale`.
    pub fn points_per_pixel(&self) -> f64 {
        1.0 / self.scale
    }
}

impl Pdf {
    /// Opens and parses `file`. Fails for damaged or password-protected PDFs
    /// (no password is passed).
    ///
    /// Opening is serialised through a process-wide lock. Older Poppler
    /// (24.02, in Ubuntu 24.04) sets up its global state lazily on the first
    /// open without any locking, so two threads opening PDFs at the same time
    /// can corrupt the heap ("double free or corruption"). The app opens PDFs
    /// on the main thread only, but the unit tests run on many threads at
    /// once. Rendering and drawing pages afterwards don't need the lock.
    pub fn open(file: &gio::File) -> Result<Self, glib::Error> {
        static OPEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A panic while holding the lock can't leave Poppler half set up in
        // a way the lock protects against, so a poisoned lock is still usable.
        let _guard = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        poppler::Document::from_gfile(file, None, gio::Cancellable::NONE).map(Self)
    }

    /// The number of pages.
    pub fn n_pages(&self) -> i32 {
        self.0.n_pages()
    }

    /// Page `index` (from 0), or an error naming the missing page. Poppler's
    /// own lookup doesn't range-check reliably, so check first.
    fn page(&self, index: i32) -> Result<poppler::Page, String> {
        (0..self.n_pages())
            .contains(&index)
            .then(|| self.0.page(index))
            .flatten()
            .ok_or_else(|| format!("The PDF has no page {}", index + 1))
    }

    /// The image size page `index` is rendered at, without rendering it.
    pub fn page_size(&self, index: i32) -> Result<PageSize, String> {
        let (pw, ph) = self.page(index)?.size();
        if !(pw > 0.0 && ph > 0.0) {
            return Err("The PDF page has no size".into());
        }
        // 144 dpi, unless that would make the longer side exceed MAX_SIDE.
        let scale = PIXELS_PER_POINT.min(MAX_SIDE / pw.max(ph));
        Ok(PageSize { scale, width: (pw * scale).ceil() as i32, height: (ph * scale).ceil() as i32 })
    }

    /// Draws page `index` with poppler in image-pixel coordinates (vector, no
    /// background). Used for the sidebar, the loupe and Save as PDF.
    pub fn painter(&self, index: i32) -> Result<impl Fn(&cairo::Context) + use<>, String> {
        let page = self.page(index)?;
        let scale = self.page_size(index)?.scale;
        // The closure owns the page, so it can outlive this call and be used
        // for repeated redraws (`use<>` says it borrows nothing from `self`).
        Ok(move |cr: &cairo::Context| {
            let _ = cr.save();
            cr.scale(scale, scale);
            page.render(cr);
            let _ = cr.restore();
        })
    }

    /// Renders page `index` (from 0) on white. Returns the image and the PDF
    /// points per image pixel, so saving as PDF can give the page its
    /// original size.
    pub fn render(&self, index: i32) -> Result<(gdk::Texture, f64), String> {
        let size = self.page_size(index)?;
        let paint = self.painter(index)?;
        let err = |e: cairo::Error| e.to_string();
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size.width, size.height).map_err(err)?;
        // Scoped so the context is dropped (and the surface released) before
        // the surface's pixels are handed to the texture.
        {
            let cr = cairo::Context::new(&surface).map_err(err)?;
            // PDF pages have no background of their own: paper is white.
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().map_err(err)?;
            paint(&cr);
            cr.status().map_err(err)?;
        }
        let texture = crate::render::texture_from_surface(surface).map_err(err)?;
        Ok((texture, size.points_per_pixel()))
    }

    /// Saves pages with their marks as a new PDF. `order` lists the source
    /// pages (from 0) to write, in order (deleted pages are left out), and
    /// `docs` holds each one's marks (in image pixels) in that same order. The pages are
    /// drawn by poppler straight into the PDF, so their text and drawings
    /// stay vector; the marks are vector too. A cropped page gets the size of
    /// its crop.
    pub fn save_marked(&self, order: &[usize], docs: &[Document], path: &std::path::Path) -> Result<(), String> {
        let n = self.n_pages() as usize;
        if docs.len() != order.len() || order.is_empty() {
            return Err("There are no pages to save".into());
        }
        // Each page at most once, and only pages that exist.
        let mut seen = vec![false; n];
        for &p in order {
            if p >= n || std::mem::replace(&mut seen[p], true) {
                return Err("The page order is not valid".into());
            }
        }
        let err = |e: cairo::Error| e.to_string();
        // The initial size doesn't matter: each page sets its own below
        // (set_size applies to the page about to be drawn).
        let surface = cairo::PdfSurface::new(1.0, 1.0, path).map_err(err)?;
        let cr = cairo::Context::new(&surface).map_err(err)?;
        for (&page, doc) in order.iter().zip(docs) {
            let index = page as i32;
            let size = self.page_size(index)?;
            let paint = self.painter(index)?;
            let r = doc.crop.unwrap_or(crate::document::Rect {
                x: 0.0,
                y: 0.0,
                w: f64::from(size.width),
                h: f64::from(size.height),
            });
            // Work in image pixels (like the marks) and let the scale turn
            // them into points; then shift so the crop's corner is the page's
            // origin and clip away everything outside the crop.
            let ppp = size.points_per_pixel();
            surface.set_size(r.w * ppp, r.h * ppp).map_err(err)?;
            cr.save().map_err(err)?;
            cr.scale(ppp, ppp);
            cr.translate(-r.x, -r.y);
            cr.rectangle(r.x, r.y, r.w, r.h);
            cr.clip();
            if doc.shapes.is_empty() {
                // Nothing to blend: copy the page straight across.
                paint(&cr);
            } else {
                // A group, so the highlighter's multiply only blends with the page.
                cr.push_group();
                crate::render::draw_page(&cr, &paint, doc);
                cr.pop_group_to_source().map_err(err)?;
                cr.paint().map_err(err)?;
            }
            cr.restore().map_err(err)?;
            cr.show_page().map_err(err)?;
        }
        // The context holds a reference to the surface; drop it so finish()
        // can flush and close the file.
        drop(cr);
        surface.finish();
        surface.status().map_err(err)
    }
}

use crate::document::Document;

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a PDF with pages of the given sizes (in points).
    fn make_pdf(path: &std::path::Path, sizes: &[(f64, f64)]) {
        let surface = cairo::PdfSurface::new(sizes[0].0, sizes[0].1, path).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        for (w, h) in sizes {
            surface.set_size(*w, *h).unwrap();
            cr.set_source_rgb(1.0, 0.0, 0.0);
            cr.rectangle(0.0, 0.0, w / 2.0, h / 2.0);
            cr.fill().unwrap();
            cr.show_page().unwrap();
        }
        drop(cr);
        surface.finish();
    }

    /// Pages open at the right size and content; PDFs are detected by name
    /// or content; broken files fail cleanly.
    #[test]
    fn opens_and_renders_pages() {
        let dir = std::env::temp_dir().join(format!("pygoscribe-pdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two pages.PDF");
        make_pdf(&path, &[(100.0, 50.0), (200.0, 300.0)]);
        let file = gio::File::for_path(&path);
        assert!(is_pdf(&file));

        let pdf = Pdf::open(&file).unwrap();
        assert_eq!(pdf.n_pages(), 2);
        let (tex, pt_per_px) = pdf.render(0).unwrap();
        assert_eq!((tex.width(), tex.height()), (200, 100));
        assert_eq!(pt_per_px, 0.5);
        let (tex, _) = pdf.render(1).unwrap();
        assert_eq!((tex.width(), tex.height()), (400, 600));
        assert!(pdf.render(2).is_err());

        // Top-left quarter is red, the rest white.
        let mut dl = gdk::TextureDownloader::new(&tex);
        dl.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = dl.download_bytes();
        let px = |x: usize, y: usize| bytes[y * stride + x * 4..][..4].to_vec();
        assert_eq!(px(10, 10), [255, 0, 0, 255]);
        assert_eq!(px(390, 590), [255, 255, 255, 255]);

        // A PDF without the extension is recognised by its contents.
        let renamed = dir.join("no-extension");
        std::fs::copy(&path, &renamed).unwrap();
        assert!(is_pdf(&gio::File::for_path(&renamed)));
        let png = dir.join("not.png");
        std::fs::write(&png, b"\x89PNG....").unwrap();
        assert!(!is_pdf(&gio::File::for_path(&png)));

        // A broken PDF gives an error, not a crash.
        let broken = dir.join("broken.pdf");
        std::fs::write(&broken, b"%PDF-1.4 nonsense").unwrap();
        assert!(Pdf::open(&gio::File::for_path(&broken)).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The whole path: open page 2 of a PDF, add a mark, export, save as PDF
    /// and PNG, and read both back.
    #[test]
    fn marks_up_a_page_and_saves_pdf_and_png() {
        use crate::document::{Document, Kind, Rgba, Shape, Style};
        use crate::settings::SaveFormat;
        let dir = std::env::temp_dir().join(format!("pygoscribe-pdf-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("letter.pdf");
        make_pdf(&src, &[(612.0, 792.0), (300.0, 200.0), (612.0, 792.0)]);

        let pdf = Pdf::open(&gio::File::for_path(&src)).unwrap();
        let (tex, pt_per_px) = pdf.render(1).unwrap();
        assert_eq!((tex.width(), tex.height()), (600, 400));

        let image = crate::render::surface_from_texture(&tex).unwrap();
        let style = Style { stroke: Rgba::new(0.0, 0.0, 1.0, 1.0), fill: None, width: 10.0, ..Style::default() };
        let mut rect = Shape::new(Kind::Rect, (400.0, 250.0), style);
        rect.b = (550.0, 350.0);
        let doc = Document { shapes: vec![rect], crop: None, image: None };
        let result = crate::render::export(&image, &doc).unwrap();

        let out_pdf = dir.join("marked.pdf");
        crate::output::save(&result, &out_pdf, SaveFormat::Pdf, pt_per_px).unwrap();
        let out_png = dir.join("marked.png");
        crate::output::save(&result, &out_png, SaveFormat::Png, pt_per_px).unwrap();

        let back = Pdf::open(&gio::File::for_path(&out_pdf)).unwrap();
        assert_eq!(back.n_pages(), 1);
        let (page, _) = back.render(0).unwrap();
        // Same page size as the original page 2.
        assert_eq!((page.width(), page.height()), (600, 400));
        let png = gdk::Texture::from_filename(&out_png).unwrap();
        assert_eq!((png.width(), png.height()), (600, 400));

        for t in [page, png] {
            let mut dl = gdk::TextureDownloader::new(&t);
            dl.set_format(gdk::MemoryFormat::R8g8b8a8);
            let (bytes, stride) = dl.download_bytes();
            let px = |x: usize, y: usize| bytes[y * stride + x * 4..][..3].to_vec();
            assert_eq!(px(10, 10), [255, 0, 0], "page content kept");
            let edge = px(400, 300);
            assert!(edge[2] > 200 && edge[0] < 60, "blue mark drawn: {edge:?}");
            assert_eq!(px(590, 390), [255, 255, 255], "white elsewhere");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Save the whole document: three pages, a mark on page 2 and a crop
    /// on page 3. Pages keep their size (page 3 takes the crop's), the mark
    /// shows, and page 1's text is still text.
    #[test]
    fn saves_every_page_with_marks_as_vector() {
        use crate::document::{Document, Kind, Rect, Rgba, Shape, Style};
        let dir = std::env::temp_dir().join(format!("pygoscribe-pdf-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("three.pdf");
        {
            let surface = cairo::PdfSurface::new(300.0, 200.0, &src).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.set_font_size(20.0);
            cr.move_to(20.0, 50.0);
            cr.show_text("Hello page one").unwrap();
            cr.show_page().unwrap();
            surface.set_size(200.0, 300.0).unwrap();
            cr.show_page().unwrap();
            surface.set_size(300.0, 300.0).unwrap();
            cr.show_page().unwrap();
            drop(cr);
            surface.finish();
        }
        let pdf = Pdf::open(&gio::File::for_path(&src)).unwrap();
        assert_eq!(pdf.n_pages(), 3);
        let style = Style { stroke: Rgba::new(0.0, 0.0, 1.0, 1.0), fill: None, width: 10.0, ..Style::default() };
        let mut rect = Shape::new(Kind::Rect, (100.0, 100.0), style);
        rect.b = (300.0, 300.0);
        let docs = vec![
            Document::default(),
            Document { shapes: vec![rect], crop: None, image: None },
            Document { shapes: vec![], crop: Some(Rect { x: 100.0, y: 100.0, w: 200.0, h: 100.0 }), image: None },
        ];
        let out = dir.join("out.pdf");
        pdf.save_marked(&[0, 1, 2], &docs, &out).unwrap();

        let back = Pdf::open(&gio::File::for_path(&out)).unwrap();
        assert_eq!(back.n_pages(), 3);
        let size = |i| back.0.page(i).unwrap().size();
        assert_eq!(size(0), (300.0, 200.0));
        assert_eq!(size(1), (200.0, 300.0));
        assert_eq!(size(2), (100.0, 50.0), "cropped page is the crop's size in points");
        let text = back.0.page(0).unwrap().text().map(|t| t.to_string()).unwrap_or_default();
        assert!(text.contains("Hello page one"), "text kept as text: {text:?}");

        let (tex, _) = back.render(1).unwrap();
        let mut dl = gdk::TextureDownloader::new(&tex);
        dl.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = dl.download_bytes();
        let px = |x: usize, y: usize| bytes[y * stride + x * 4..][..3].to_vec();
        let edge = px(100, 200);
        assert!(edge[2] > 200 && edge[0] < 60, "blue mark drawn: {edge:?}");
        assert_eq!(px(200, 200), [255, 255, 255]);

        // Marks that don't match the pages are refused instead of writing a
        // bad file, as are no pages, a repeated page and a missing page.
        assert!(pdf.save_marked(&[0, 1], &docs, &dir.join("bad.pdf")).is_err());
        assert!(pdf.save_marked(&[], &[], &dir.join("bad.pdf")).is_err());
        assert!(pdf.save_marked(&[0, 1, 1], &docs, &dir.join("bad.pdf")).is_err());
        assert!(pdf.save_marked(&[0, 1, 3], &docs, &dir.join("bad.pdf")).is_err());

        // With page 2 deleted, only pages 1 and 3 are written.
        let out3 = dir.join("deleted.pdf");
        pdf.save_marked(&[0, 2], &[docs[0].clone(), docs[2].clone()], &out3).unwrap();
        let back = Pdf::open(&gio::File::for_path(&out3)).unwrap();
        assert_eq!(back.n_pages(), 2);
        assert_eq!(back.0.page(1).unwrap().size(), (100.0, 50.0));

        // Reordered: page 3, then 1, then 2, each with its own marks.
        let reordered = vec![docs[2].clone(), docs[0].clone(), docs[1].clone()];
        let out2 = dir.join("reordered.pdf");
        pdf.save_marked(&[2, 0, 1], &reordered, &out2).unwrap();
        let back = Pdf::open(&gio::File::for_path(&out2)).unwrap();
        let size = |i| back.0.page(i).unwrap().size();
        assert_eq!(size(0), (100.0, 50.0), "cropped page 3 now first");
        assert_eq!(size(1), (300.0, 200.0));
        assert_eq!(size(2), (200.0, 300.0));
        let text = back.0.page(1).unwrap().text().map(|t| t.to_string()).unwrap_or_default();
        assert!(text.contains("Hello page one"), "page 1's text moved with it: {text:?}");
        let (tex, _) = back.render(2).unwrap();
        let mut dl = gdk::TextureDownloader::new(&tex);
        dl.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = dl.download_bytes();
        let edge = bytes[200 * stride + 100 * 4..][..3].to_vec();
        assert!(edge[2] > 200 && edge[0] < 60, "page 2's mark moved with it: {edge:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Line ends are saved as vector shapes: the page has no images, and
    /// the heads (translucent too) show where they should.
    #[test]
    fn saves_line_ends_as_vector() {
        use crate::document::{Document, Kind, LineEnd, LineStyle, Rgba, Shape, Style};
        let dir = std::env::temp_dir().join(format!("pygoscribe-pdf-ends-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("blank.pdf");
        {
            let surface = cairo::PdfSurface::new(300.0, 200.0, &src).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            cr.show_page().unwrap();
            drop(cr);
            surface.finish();
        }
        let pdf = Pdf::open(&gio::File::for_path(&src)).unwrap();
        let style = |stroke, start, end| Style { stroke, width: 10.0, line: LineStyle::Dashed, start, end, ..Style::default() };
        let blue = Rgba::new(0.0, 0.0, 1.0, 1.0);
        let mut line = Shape::new(Kind::Line, (100.0, 100.0), style(blue, LineEnd::Dot, LineEnd::Stealth));
        line.b = (500.0, 100.0);
        let mut arrow = Shape::new(Kind::Arrow, (100.0, 300.0), style(Rgba { a: 0.5, ..blue }, LineEnd::None, LineEnd::Arrow));
        arrow.b = (500.0, 300.0);
        let docs = vec![Document { shapes: vec![line, arrow], crop: None, image: None }];
        let out = dir.join("ends.pdf");
        pdf.save_marked(&[0], &docs, &out).unwrap();

        let back = Pdf::open(&gio::File::for_path(&out)).unwrap();
        assert!(back.0.page(0).unwrap().image_mapping().is_empty(), "no raster images");
        let (tex, _) = back.render(0).unwrap();
        let mut dl = gdk::TextureDownloader::new(&tex);
        dl.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = dl.download_bytes();
        let px = |x: usize, y: usize| bytes[y * stride + x * 4..][..3].to_vec();
        // The dot (radius 13) is centred on the start, wider than the line.
        assert!(px(100, 110)[0] < 60, "dot drawn: {:?}", px(100, 110));
        // The swept head spreads across the line near the tip.
        assert!(px(480, 108)[0] < 60, "head drawn: {:?}", px(480, 108));
        assert_eq!(px(510, 100), [255, 255, 255], "nothing past the tip");
        // The translucent head is evenly light blue where the line runs in.
        let (head, inside) = (px(480, 300), px(470, 300));
        assert!((100..160).contains(&head[0]), "translucent head: {head:?}");
        assert_eq!(head, inside);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A stress test for the lock in [`Pdf::open`]: many threads open and render
/// a PDF at the same moment. Without the lock this crashed in about one run
/// in six on Ubuntu 24.04. Run it repeatedly with
/// `cargo test --release concurrent_opens -- --ignored`.
#[cfg(test)]
mod stress {
    use super::*;

    #[test]
    #[ignore]
    fn concurrent_opens() {
        let path = std::env::temp_dir().join(format!("pygoscribe-pdf-race-{}.pdf", std::process::id()));
        {
            let surface = cairo::PdfSurface::new(100.0, 100.0, &path).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            cr.show_page().unwrap();
            drop(cr);
            surface.finish();
        }
        // The barrier lines every thread up so they all open at once.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let (barrier, path) = (barrier.clone(), path.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let pdf = Pdf::open(&gio::File::for_path(&path)).unwrap();
                    pdf.render(0).unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let _ = std::fs::remove_file(path);
    }
}
