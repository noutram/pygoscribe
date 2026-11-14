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

//! Cairo drawing of the image and its markup, in image coordinates. Shared by
//! the on-screen canvas and (in step 3) export.

use crate::document::{rotate_point, Document, HAlign, Kind, Point, Rect, Rgba, Shape, VAlign};
use gtk::cairo::{self, Context, ImageSurface};
use gtk::gdk;
use gtk::pango;
use gtk::prelude::*;
use std::f64::consts::PI;

const LINE_SPACING: f64 = 1.25;

/// Converts a GDK texture into a cairo surface we can paint and sample.
pub fn surface_from_texture(texture: &gdk::Texture) -> Result<ImageSurface, cairo::Error> {
    let mut dl = gdk::TextureDownloader::new(texture);
    dl.set_format(gdk::MemoryFormat::B8g8r8a8Premultiplied);
    let (bytes, stride) = dl.download_bytes();
    ImageSurface::create_for_data(
        bytes.to_vec(),
        cairo::Format::ARgb32,
        texture.width(),
        texture.height(),
        stride as i32,
    )
}

fn set_color(cr: &Context, c: Rgba) {
    cr.set_source_rgba(c.r, c.g, c.b, c.a);
}

/// A Pango layout for one line of text in a mark's font, drawing on `cr`.
/// Hinting is off so text has the same proportions at every zoom and in the
/// full-resolution export.
fn text_layout(cr: &Context, font: &str, font_size: f64) -> (pango::Layout, f64) {
    let layout = pangocairo::functions::create_layout(cr);
    if let Ok(mut opts) = cairo::FontOptions::new() {
        opts.set_hint_style(cairo::HintStyle::None);
        opts.set_hint_metrics(cairo::HintMetrics::Off);
        pangocairo::functions::context_set_font_options(&layout.context(), Some(&opts));
        layout.context_changed();
    }
    let mut desc = pango::FontDescription::from_string(font);
    // An absolute size is in user units (image pixels), like cairo's font size.
    desc.set_absolute_size(font_size * f64::from(pango::SCALE));
    layout.set_font_description(Some(&desc));
    let ascent = layout.context().metrics(Some(&desc), None).ascent();
    (layout, f64::from(ascent) / f64::from(pango::SCALE))
}

/// Size of a text block in a font at a font size.
pub fn text_size(text: &str, font: &str, font_size: f64) -> (f64, f64) {
    let surf = ImageSurface::create(cairo::Format::ARgb32, 1, 1).expect("scratch surface");
    let cr = Context::new(&surf).expect("scratch context");
    let (layout, _) = text_layout(&cr, font, font_size);
    let lines: Vec<&str> = if text.is_empty() { vec![""] } else { text.split('\n').collect() };
    let w = lines
        .iter()
        .map(|l| {
            layout.set_text(l);
            f64::from(layout.extents().1.width()) / f64::from(pango::SCALE)
        })
        .fold(font_size * 0.5, f64::max);
    (w, lines.len() as f64 * font_size * LINE_SPACING)
}

/// Draws a block of text with its top-left corner at (`x`, `y`), each line
/// placed across `width` by `halign`, in the mark's font and border colour.
fn draw_text_block(cr: &Context, shape: &Shape, text: &str, (x, y): Point, width: f64) {
    let st = &shape.style;
    set_color(cr, st.stroke);
    let (layout, ascent) = text_layout(cr, &st.font, st.font_size);
    for (i, line) in text.split('\n').enumerate() {
        layout.set_text(line);
        let w = f64::from(layout.extents().1.width()) / f64::from(pango::SCALE);
        let dx = match shape.halign {
            HAlign::Left => 0.0,
            HAlign::Center => (width - w) / 2.0,
            HAlign::Right => width - w,
        };
        // Line up each line's baseline; show_layout draws from the top.
        let baseline = y + ascent + i as f64 * st.font_size * LINE_SPACING;
        let top = f64::from(layout.baseline()) / f64::from(pango::SCALE);
        cr.move_to(x + dx, baseline - top);
        pangocairo::functions::show_layout(cr, &layout);
    }
    // Drop the current point, or the next mark's path (an oval's arc) would
    // start with a line from here.
    cr.new_path();
}

/// The area text sits in inside a rectangle, oval or bubble, in the mark's
/// own (unrotated) frame: inside the border, the oval's inscribed box, or the
/// bubble's body, with some padding.
fn label_area(shape: &Shape) -> Rect {
    let r = shape.bounds();
    let pad = shape.style.font_size * 0.2;
    let border = if shape.draws_border() { shape.style.width / 2.0 } else { 0.0 };
    let (r, inset) = match shape.kind {
        Kind::Oval => {
            let k = (1.0 - std::f64::consts::FRAC_1_SQRT_2) / 2.0;
            let inner = Rect { x: r.x + r.w * k, y: r.y + r.h * k, w: r.w * (1.0 - 2.0 * k), h: r.h * (1.0 - 2.0 * k) };
            (inner, pad)
        }
        Kind::Bubble => (Rect { h: r.h * 0.78, ..r }, border + pad),
        _ => (r, border + pad),
    };
    Rect { x: r.x + inset, y: r.y + inset, w: (r.w - 2.0 * inset).max(0.0), h: (r.h - 2.0 * inset).max(0.0) }
}

/// Where a line's or arrow's text goes (left, centre or right as read): the transform from the text's frame
/// (x along the line as read, y down) to the image, and the text block's
/// top-left corner and size in that frame. The frame is turned so the text
/// never reads upside down.
struct LineLabel {
    origin: Point,
    angle: f64,
    pos: Point,
    size: (f64, f64),
}

fn line_label(shape: &Shape) -> Option<LineLabel> {
    if !shape.is_segment() || shape.label.is_empty() {
        return None;
    }
    let st = &shape.style;
    let (a, b) = (shape.a, shape.b);
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    if len < 0.5 {
        return None;
    }
    let mut angle = (b.1 - a.1).atan2(b.0 - a.0);
    // Read left to right: start from whichever end is on the left (an
    // upright line reads bottom to top, like a chart's axis label).
    let flipped = angle.cos() < -1e-9 || (angle.cos().abs() <= 1e-9 && angle.sin() > 0.0);
    let origin = if flipped {
        angle += PI;
        b
    } else {
        a
    };
    let (tw, th) = text_size(&shape.label, &st.font, st.font_size);
    let pad = st.font_size * 0.2;
    // Keep text off an arrow's head.
    let head = if matches!(shape.kind, Kind::Arrow) { (st.width * 3.0 + 8.0).min(len * 0.6) } else { 0.0 };
    let (head_start, head_end) = if flipped { (head, 0.0) } else { (0.0, head) };
    let x = match shape.halign {
        HAlign::Left => head_start + pad,
        HAlign::Center => (len - tw) / 2.0,
        HAlign::Right => len - head_end - pad - tw,
    };
    // Clear the line and, near the tip, the head's spread.
    let gap = st.width / 2.0 + pad;
    let y = match shape.valign {
        VAlign::Bottom => gap,
        _ => -gap - th,
    };
    Some(LineLabel { origin, angle, pos: (x, y), size: (tw, th) })
}

/// The corners of a line's or arrow's text, in image coordinates, for
/// hit-testing and the selection outline. None for other marks or no text.
pub fn line_label_outline(shape: &Shape) -> Option<[Point; 4]> {
    let l = line_label(shape)?;
    let (x, y, w, h) = (l.pos.0, l.pos.1, l.size.0, l.size.1);
    Some([(x, y), (x + w, y), (x + w, y + h), (x, y + h)].map(|q| {
        let p = rotate_point(q, (0.0, 0.0), l.angle);
        (p.0 + l.origin.0, p.1 + l.origin.1)
    }))
}

/// Whether `p` is on a line's or arrow's text.
pub fn hits_line_label(shape: &Shape, p: Point, tol: f64) -> bool {
    let Some(l) = line_label(shape) else { return false };
    let q = rotate_point((p.0 - l.origin.0, p.1 - l.origin.1), (0.0, 0.0), -l.angle);
    Rect { x: l.pos.0, y: l.pos.1, w: l.size.0, h: l.size.1 }.contains(q, tol)
}

/// Draws the text of a rectangle, oval, bubble, line or arrow. `cr` is
/// already turned by the mark's angle.
fn draw_label(cr: &Context, shape: &Shape) {
    if shape.label.is_empty() {
        return;
    }
    if shape.is_segment() {
        if let Some(l) = line_label(shape) {
            let _ = cr.save();
            cr.translate(l.origin.0, l.origin.1);
            cr.rotate(l.angle);
            draw_text_block(cr, shape, &shape.label, l.pos, l.size.0);
            let _ = cr.restore();
        }
        return;
    }
    let st = &shape.style;
    let area = label_area(shape);
    let (tw, th) = text_size(&shape.label, &st.font, st.font_size);
    let x = match shape.halign {
        HAlign::Left => area.x,
        HAlign::Center => area.x + (area.w - tw) / 2.0,
        HAlign::Right => area.x + area.w - tw,
    };
    let y = match shape.valign {
        VAlign::Top => area.y,
        VAlign::Middle => area.y + (area.h - th) / 2.0,
        VAlign::Bottom => area.y + area.h - th,
    };
    draw_text_block(cr, shape, &shape.label, (x, y), tw);
}

/// Updates a text shape's far corner to fit its content.
pub fn fit_text(shape: &mut Shape) {
    if let Kind::Text(t) = &shape.kind {
        let (w, h) = text_size(t, &shape.style.font, shape.style.font_size);
        let pad = shape.style.font_size * 0.2;
        let (w, h) = (w + 2.0 * pad, h + 2.0 * pad);
        if shape.angle == 0.0 {
            shape.b = (shape.a.0 + w, shape.a.1 + h);
        } else {
            // Rotation is about the centre, so a size change would slide the
            // text; keep its top-left corner where it is on screen instead.
            let corner = shape.outline()[0];
            let half = rotate_point((w / 2.0, h / 2.0), (0.0, 0.0), shape.angle);
            let c = (corner.0 + half.0, corner.1 + half.1);
            shape.a = (c.0 - w / 2.0, c.1 - h / 2.0);
            shape.b = (c.0 + w / 2.0, c.1 + h / 2.0);
        }
    }
}

pub fn draw_document(cr: &Context, image: &ImageSurface, doc: &Document) {
    draw_page(cr, &|cr: &Context| paint_image(cr, image), doc);
}

/// Paints the page under the marks, in image coordinates: a picture, or a
/// PDF page drawn by poppler (vector). The loupe paints it again, magnified.
pub type Background<'a> = &'a dyn Fn(&Context);

/// A shared page painter (a PDF page drawn by poppler, in image pixels).
pub type Painter = std::rc::Rc<dyn Fn(&Context)>;

/// Draws the page background and every mark on it.
pub fn draw_page(cr: &Context, background: Background, doc: &Document) {
    background(cr);
    for shape in &doc.shapes {
        draw_shape_on(cr, background, shape);
    }
}

pub fn paint_image(cr: &Context, image: &ImageSurface) {
    let _ = cr.set_source_surface(image, 0.0, 0.0);
    cr.source().set_filter(cairo::Filter::Good);
    let _ = cr.paint();
}

fn polyline(cr: &Context, pts: &[Point]) {
    if let Some(first) = pts.first() {
        cr.move_to(first.0, first.1);
        if pts.len() == 1 {
            cr.line_to(first.0 + 0.01, first.1);
        }
        for p in &pts[1..] {
            cr.line_to(p.0, p.1);
        }
    }
}

fn rounded_rect(cr: &Context, r: &Rect, radius: f64) {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0);
    cr.new_sub_path();
    cr.arc(r.x + r.w - rad, r.y + rad, rad, -PI / 2.0, 0.0);
    cr.arc(r.x + r.w - rad, r.y + r.h - rad, rad, 0.0, PI / 2.0);
    cr.arc(r.x + rad, r.y + r.h - rad, rad, PI / 2.0, PI);
    cr.arc(r.x + rad, r.y + rad, rad, PI, 1.5 * PI);
    cr.close_path();
}

/// A speech bubble as one outline (rounded body with the tail cut into its
/// bottom edge), so a translucent fill or border is painted only once.
fn bubble_path(cr: &Context, r: &Rect) {
    let body = Rect { h: r.h * 0.78, ..*r };
    let rad = (body.w.min(body.h) * 0.25).min(24.0).min(body.w / 2.0).min(body.h / 2.0);
    let (left, right, top, base) = (body.x, body.x + body.w, body.y, body.y + body.h);
    // Keep the tail's base on the straight part of the bottom edge.
    let tx = (left + body.w * 0.22).max(left + rad);
    let tw = (body.w * 0.14).max(6.0).min(right - rad - tx);
    cr.new_sub_path();
    cr.arc(right - rad, top + rad, rad, -PI / 2.0, 0.0);
    cr.arc(right - rad, base - rad, rad, 0.0, PI / 2.0);
    if tw > 0.0 {
        cr.line_to(tx + tw, base);
        cr.line_to(tx - tw * 0.6, r.y + r.h);
        cr.line_to(tx, base);
    }
    cr.arc(left + rad, base - rad, rad, PI / 2.0, PI);
    cr.arc(left + rad, top + rad, rad, PI, 1.5 * PI);
    cr.close_path();
}

/// Fills and outlines the current path, leaving out whichever is off.
fn fill_and_stroke(cr: &Context, shape: &Shape) {
    if let Some(fill) = shape.style.fill {
        set_color(cr, fill);
        let _ = cr.fill_preserve();
    }
    if shape.draws_border() {
        set_color(cr, shape.style.stroke);
        cr.set_line_width(shape.style.width);
        let _ = cr.stroke();
    } else {
        cr.new_path();
    }
}

/// Runs `draw` with the source colour's alpha applied once to everything it
/// paints, so overlapping parts (an arrow's shaft and head) don't show darker.
/// `draw` should paint with `alpha` 1.
fn with_alpha(cr: &Context, alpha: f64, draw: impl FnOnce()) {
    if alpha >= 1.0 {
        draw();
        return;
    }
    cr.push_group();
    draw();
    let _ = cr.pop_group_to_source();
    let _ = cr.paint_with_alpha(alpha);
}

#[cfg(test)]
pub fn draw_shape(cr: &Context, image: &ImageSurface, shape: &Shape) {
    draw_shape_on(cr, &|cr: &Context| paint_image(cr, image), shape);
}

/// Draws one mark. `background` is only used by the loupe.
pub fn draw_shape_on(cr: &Context, background: Background, shape: &Shape) {
    let _ = cr.save();
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    if shape.angle != 0.0 {
        let (cx, cy) = shape.center();
        cr.translate(cx, cy);
        cr.rotate(shape.angle);
        cr.translate(-cx, -cy);
    }
    let st = &shape.style;
    match &shape.kind {
        Kind::Pen(pts) => {
            polyline(cr, pts);
            set_color(cr, st.stroke);
            cr.set_line_width(st.width);
            let _ = cr.stroke();
        }
        Kind::Highlighter(pts) => {
            polyline(cr, pts);
            set_color(cr, Rgba { a: 0.4 * st.stroke.a, ..st.stroke });
            cr.set_line_width(shape.highlighter_width());
            cr.set_line_cap(cairo::LineCap::Square);
            cr.set_operator(cairo::Operator::Multiply);
            let _ = cr.stroke();
        }
        Kind::Line => {
            cr.move_to(shape.a.0, shape.a.1);
            cr.line_to(shape.b.0, shape.b.1);
            set_color(cr, st.stroke);
            cr.set_line_width(st.width);
            let _ = cr.stroke();
            draw_label(cr, shape);
        }
        Kind::Arrow => {
            draw_arrow(cr, shape);
            draw_label(cr, shape);
        }
        Kind::Rect => {
            let r = shape.bounds();
            cr.rectangle(r.x, r.y, r.w, r.h);
            fill_and_stroke(cr, shape);
            draw_label(cr, shape);
        }
        Kind::Oval => {
            let r = shape.bounds();
            if r.w > 0.0 && r.h > 0.0 {
                let _ = cr.save();
                cr.translate(r.x + r.w / 2.0, r.y + r.h / 2.0);
                cr.scale(r.w / 2.0, r.h / 2.0);
                cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
                let _ = cr.restore();
                fill_and_stroke(cr, shape);
                draw_label(cr, shape);
            }
        }
        Kind::Bubble => {
            bubble_path(cr, &shape.bounds());
            fill_and_stroke(cr, shape);
            draw_label(cr, shape);
        }
        Kind::Text(text) => {
            let r = shape.bounds();
            if let Some(fill) = st.fill {
                set_color(cr, fill);
                rounded_rect(cr, &r, st.font_size * 0.2);
                let _ = cr.fill();
            }
            let pad = st.font_size * 0.2;
            draw_text_block(cr, shape, text, (r.x + pad, r.y + pad), r.w - 2.0 * pad);
        }
        Kind::Loupe { zoom } => {
            let r = shape.bounds();
            if r.w > 1.0 {
                let (cx, cy, rad) = (r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0);
                let _ = cr.save();
                cr.arc(cx, cy, rad, 0.0, 2.0 * PI);
                cr.clip();
                cr.translate(cx, cy);
                cr.scale(*zoom, *zoom);
                cr.translate(-cx, -cy);
                set_color(cr, Rgba::new(1.0, 1.0, 1.0, 1.0));
                let _ = cr.paint();
                background(cr);
                let _ = cr.restore();
                cr.arc(cx, cy, rad, 0.0, 2.0 * PI);
                set_color(cr, st.stroke);
                cr.set_line_width(st.width.max(2.0));
                let _ = cr.stroke();
            }
        }
    }
    let _ = cr.restore();
}

fn draw_arrow(cr: &Context, shape: &Shape) {
    let st = &shape.style;
    let (ax, ay) = shape.a;
    let (bx, by) = shape.b;
    let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
    if len < 0.5 {
        return;
    }
    let head = (st.width * 3.0 + 8.0).min(len * 0.6);
    let angle = (by - ay).atan2(bx - ax);
    let spread = 0.45;
    let (ux, uy) = (angle.cos(), angle.sin());
    with_alpha(cr, st.stroke.a, || {
        set_color(cr, Rgba { a: 1.0, ..st.stroke });
        // Shaft stops inside the head so its round cap doesn't poke out of the tip.
        cr.move_to(ax, ay);
        cr.line_to(bx - ux * head * 0.7, by - uy * head * 0.7);
        cr.set_line_width(st.width);
        let _ = cr.stroke();
        cr.move_to(bx, by);
        cr.line_to(bx - head * (angle - spread).cos(), by - head * (angle - spread).sin());
        cr.line_to(bx - head * (angle + spread).cos(), by - head * (angle + spread).sin());
        cr.close_path();
        let _ = cr.fill();
    });
}

/// Renders the marked-up image at full resolution, cropped.
pub fn export(image: &ImageSurface, doc: &Document) -> Result<gdk::Texture, cairo::Error> {
    let r = doc.crop.unwrap_or(Rect { x: 0.0, y: 0.0, w: image.width() as f64, h: image.height() as f64 });
    let (w, h) = (r.w.round() as i32, r.h.round() as i32);
    let out = ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    {
        let cr = Context::new(&out)?;
        cr.translate(-r.x, -r.y);
        cr.push_group();
        draw_document(&cr, image, doc);
        cr.pop_group_to_source()?;
        cr.paint()?;
    }
    texture_from_surface(out)
}

/// A small picture of a marked-up page for the page sidebar: the page
/// (`size` in image pixels) with its marks and crop, scaled by `scale`, on
/// white.
pub fn render_thumbnail(
    background: Background,
    size: (f64, f64),
    doc: &Document,
    scale: f64,
) -> Result<gdk::Texture, cairo::Error> {
    let r = doc.crop.unwrap_or(Rect { x: 0.0, y: 0.0, w: size.0, h: size.1 });
    let (w, h) = (((r.w * scale).round() as i32).max(1), ((r.h * scale).round() as i32).max(1));
    let out = ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    {
        let cr = Context::new(&out)?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint()?;
        cr.scale(scale, scale);
        cr.translate(-r.x, -r.y);
        cr.push_group();
        draw_page(&cr, background, doc);
        cr.pop_group_to_source()?;
        cr.paint()?;
    }
    texture_from_surface(out)
}

/// Turns a finished ARGB32 surface into a texture.
pub fn texture_from_surface(out: ImageSurface) -> Result<gdk::Texture, cairo::Error> {
    out.flush();
    let (w, h) = (out.width(), out.height());
    let stride = out.stride() as usize;
    let mut bytes = None;
    out.with_data(|d| bytes = Some(gtk::glib::Bytes::from(d)))
        .map_err(|_| cairo::Error::SurfaceFinished)?;
    let bytes = bytes.ok_or(cairo::Error::SurfaceFinished)?;
    Ok(gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride).upcast())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Style;

    /// Draws `shape` on white and returns a pixel reader (r, g, b as 0–255).
    fn draw_on_white(shape: &Shape) -> impl Fn(i32, i32) -> (u8, u8, u8) {
        let mut surface = ImageSurface::create(cairo::Format::Rgb24, 200, 100).unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().unwrap();
            draw_shape(&cr, &surface.clone(), shape);
        }
        surface.flush();
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap().to_vec();
        move |x, y| {
            let i = y as usize * stride + x as usize * 4;
            (data[i + 2], data[i + 1], data[i])
        }
    }

    #[test]
    fn translucent_arrow_has_no_dark_overlap() {
        let style = Style { stroke: Rgba::new(0.0, 0.0, 0.0, 0.5), width: 4.0, ..Style::default() };
        let mut arrow = Shape::new(Kind::Arrow, (10.0, 50.0), style);
        arrow.b = (190.0, 50.0);
        let px = draw_on_white(&arrow);
        let shaft = px(100, 50);
        // Where the shaft runs into the head.
        let overlap = px(173, 50);
        assert!((shaft.0 as i32 - 128).abs() <= 2, "shaft {shaft:?}");
        assert_eq!(shaft, overlap);
    }

    #[test]
    fn borderless_rect_draws_only_its_fill() {
        let style = Style {
            stroke: Rgba::new(0.0, 0.0, 0.0, 1.0),
            border: false,
            fill: Some(Rgba::new(1.0, 0.0, 0.0, 0.5)),
            width: 6.0,
            ..Style::default()
        };
        let mut rect = Shape::new(Kind::Rect, (50.0, 20.0), style);
        rect.b = (150.0, 80.0);
        let px = draw_on_white(&rect);
        assert_eq!(px(48, 50), (255, 255, 255)); // just outside: no border
        let inside = px(100, 50);
        assert_eq!(inside.0, 255);
        assert!((inside.1 as i32 - 128).abs() <= 2, "inside {inside:?}");
    }

    #[test]
    fn bubble_without_fill_is_see_through() {
        let style = Style { fill: None, width: 2.0, ..Style::default() };
        let mut bubble = Shape::new(Kind::Bubble, (20.0, 10.0), style);
        bubble.b = (180.0, 90.0);
        let px = draw_on_white(&bubble);
        assert_eq!(px(100, 40), (255, 255, 255));
    }

    #[test]
    fn text_uses_its_font() {
        let sans = text_size("iiii", "Sans", 20.0).0;
        let mono = text_size("iiii", "Monospace", 20.0).0;
        assert!(mono > sans * 1.2, "sans {sans}, mono {mono}");
        // Twice the size is twice as wide: no hinting differences with scale.
        let big = text_size("iiii", "Monospace", 40.0).0;
        assert!((big - 2.0 * mono).abs() < 0.5, "mono {mono}, big {big}");
    }

    #[test]
    fn text_draws_ink_inside_its_box() {
        let style = Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), font: "Serif Bold".into(), ..Style::default() };
        let mut text = Shape::new(Kind::Text("HH".into()), (20.0, 20.0), style);
        fit_text(&mut text);
        let r = text.bounds();
        let px = draw_on_white(&text);
        let mut dark = 0;
        for y in r.y as u32..(r.y + r.h) as u32 {
            for x in r.x as u32..(r.x + r.w) as u32 {
                if px(x as i32, y as i32).0 < 100 {
                    dark += 1;
                }
            }
        }
        assert!(dark > 50, "only {dark} dark pixels");
        assert_eq!(px((r.x + r.w + 3.0) as i32, (r.y + r.h / 2.0) as i32), (255, 255, 255));
    }

    /// The bounding box of dark pixels drawn by `shape` on white, 200×100.
    fn ink(shape: &Shape) -> Option<Rect> {
        let px = draw_on_white(shape);
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in 0..100 {
            for x in 0..200 {
                if px(x, y).0 < 100 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        (x0 <= x1).then(|| Rect { x: x0.into(), y: y0.into(), w: (x1 - x0).into(), h: (y1 - y0).into() })
    }

    fn black() -> Style {
        Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), border: false, fill: Some(Rgba::new(1.0, 1.0, 1.0, 0.0)), font_size: 16.0, ..Style::default() }
    }

    #[test]
    fn rect_text_follows_its_alignment() {
        let mut rect = Shape::new(Kind::Rect, (10.0, 10.0), black());
        rect.b = (190.0, 90.0);
        assert_eq!(ink(&rect), None, "no text, no border: nothing drawn");
        rect.label = "Hi".into();
        let mid = ink(&rect).unwrap();
        assert!((mid.x + mid.w / 2.0 - 100.0).abs() < 3.0, "centred across: {mid:?}");
        assert!((mid.y + mid.h / 2.0 - 50.0).abs() < 4.0, "centred down: {mid:?}");
        rect.halign = HAlign::Left;
        rect.valign = VAlign::Top;
        let tl = ink(&rect).unwrap();
        assert!(tl.x < 25.0 && tl.y < 25.0, "top left: {tl:?}");
        rect.halign = HAlign::Right;
        rect.valign = VAlign::Bottom;
        let br = ink(&rect).unwrap();
        assert!(br.x + br.w > 175.0 && br.y + br.h > 75.0, "bottom right: {br:?}");
    }

    #[test]
    fn line_text_goes_above_or_below_and_reads_upright() {
        let mut line = Shape::new(Kind::Line, (20.0, 50.0), black());
        line.style.width = 2.0;
        line.b = (180.0, 50.0);
        line.label = "Ab".into();
        let above = line_label_outline(&line).unwrap();
        assert!(above.iter().all(|q| q.1 < 50.0), "above: {above:?}");
        assert!(hits_line_label(&line, ((above[0].0 + above[2].0) / 2.0, (above[0].1 + above[2].1) / 2.0), 0.0));
        line.valign = VAlign::Bottom;
        let below = line_label_outline(&line).unwrap();
        assert!(below.iter().all(|q| q.1 > 50.0), "below: {below:?}");
        // Drawn right to left, the text still reads left to right, above.
        line.valign = VAlign::Top;
        (line.a, line.b) = (line.b, line.a);
        let flipped = line_label_outline(&line).unwrap();
        for (p, q) in flipped.iter().zip(above.iter()) {
            assert!((p.0 - q.0).abs() < 1e-6 && (p.1 - q.1).abs() < 1e-6, "{flipped:?} vs {above:?}");
        }
        // Left sits at the left end.
        line.halign = HAlign::Left;
        assert!(line_label_outline(&line).unwrap()[0].0 < 30.0);
    }

    /// Draws a sheet of labelled marks to the temp folder for a visual check:
    /// `cargo test label_sheet -- --ignored`.
    #[test]
    #[ignore]
    fn label_sheet() {
        let surface = ImageSurface::create(cairo::Format::ARgb32, 900, 600).unwrap();
        let cr = Context::new(&surface).unwrap();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().unwrap();
        let style = Style { fill: Some(Rgba::new(1.0, 0.95, 0.6, 1.0)), font_size: 20.0, ..Style::default() };
        let add = |kind: Kind, a: Point, b: Point, label: &str, h: HAlign, v: VAlign, angle: f64| {
            let mut s = Shape::new(kind, a, style.clone());
            s.b = b;
            s.label = label.into();
            s.halign = h;
            s.valign = v;
            s.angle = angle;
            draw_shape_on(&cr, &|_: &Context| {}, &s);
        };
        add(Kind::Rect, (20.0, 20.0), (280.0, 160.0), "Centre\ncentre", HAlign::Center, VAlign::Middle, 0.0);
        add(Kind::Rect, (320.0, 20.0), (580.0, 160.0), "Top left", HAlign::Left, VAlign::Top, 0.0);
        add(Kind::Oval, (620.0, 20.0), (880.0, 160.0), "Bottom right", HAlign::Right, VAlign::Bottom, 0.0);
        add(Kind::Bubble, (20.0, 200.0), (280.0, 360.0), "Hello!", HAlign::Center, VAlign::Middle, 0.0);
        add(Kind::Rect, (340.0, 220.0), (560.0, 340.0), "Rotated", HAlign::Center, VAlign::Middle, 0.4);
        add(Kind::Line, (620.0, 280.0), (880.0, 220.0), "Above", HAlign::Center, VAlign::Top, 0.0);
        add(Kind::Arrow, (880.0, 360.0), (620.0, 300.0), "Below, left", HAlign::Left, VAlign::Bottom, 0.0);
        add(Kind::Arrow, (40.0, 560.0), (400.0, 420.0), "Right, above", HAlign::Right, VAlign::Top, 0.0);
        add(Kind::Line, (500.0, 420.0), (500.0, 580.0), "Vertical", HAlign::Center, VAlign::Top, 0.0);
        drop(cr);
        let path = std::env::temp_dir().join("pygoscribe-label-sheet.png");
        texture_from_surface(surface).unwrap().save_to_png(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }
}
