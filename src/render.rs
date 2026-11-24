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
//! the on-screen canvas, export (Copy, Save, Send To), page thumbnails and
//! Save as PDF, so every output looks exactly like the screen.
//!
//! # Conventions
//!
//! - Every function draws in *image pixels*. Callers set up the transform
//!   (zoom, scroll, crop offset, PDF points) on the cairo context first.
//! - A mark with an angle is drawn by rotating the context about the mark's
//!   centre ([`draw_shape_on`]); the geometry itself is always unrotated.
//! - Text is laid out with Pango with hinting turned off, so a text mark has
//!   the same proportions at every zoom and in the full-resolution export.
//! - Surfaces are cairo `ARgb32`, which is premultiplied BGRA in memory on
//!   little-endian machines; that matches GDK's `B8g8r8a8Premultiplied`, so
//!   textures and surfaces convert without touching the pixels.
//! - The highlighter blends with `Operator::Multiply`, so callers that want
//!   it to blend only with the page (not with what is under the output) draw
//!   the page into a group first (`push_group` / `pop_group_to_source`).

use crate::document::{rotate_point, Document, HAlign, Kind, LineEnd, Point, Rect, Rgba, Shape, Style, VAlign};
use gtk::cairo::{self, Context, ImageSurface};
use gtk::gdk;
use gtk::pango;
use gtk::prelude::*;
use std::f64::consts::PI;

/// Distance between text baselines, as a multiple of the font size.
const LINE_SPACING: f64 = 1.25;

/// Converts a GDK texture into a cairo surface we can paint and sample.
pub fn surface_from_texture(texture: &gdk::Texture) -> Result<ImageSurface, cairo::Error> {
    // Download straight into cairo's own pixel layout (see module docs).
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

/// Makes `c` the source colour.
fn set_color(cr: &Context, c: Rgba) {
    cr.set_source_rgba(c.r, c.g, c.b, c.a);
}

/// A Pango layout for one line of text in a mark's font, drawing on `cr`.
/// Hinting is off so text has the same proportions at every zoom and in the
/// full-resolution export. Also returns the font's ascent in image pixels,
/// used to line up baselines.
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
    // Pango needs a context to measure with; a 1×1 surface is enough.
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
        // Never narrower than half an em, so an empty mark can still be hit.
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
            // The largest axis-aligned box inside an ellipse spans 1/√2 of
            // each axis, so inset each side by (1 − 1/√2)/2 of the width/height.
            let k = (1.0 - std::f64::consts::FRAC_1_SQRT_2) / 2.0;
            let inner = Rect { x: r.x + r.w * k, y: r.y + r.h * k, w: r.w * (1.0 - 2.0 * k), h: r.h * (1.0 - 2.0 * k) };
            (inner, pad)
        }
        // The top 78 % is the body; the rest is the tail (see bubble_path).
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
    /// The end of the line the text frame starts from, in image coordinates.
    origin: Point,
    /// The frame's rotation, in radians, already flipped to read upright.
    angle: f64,
    /// The text block's top-left corner in the frame.
    pos: Point,
    /// The text block's width and height.
    size: (f64, f64),
}

/// Lays out a line's or arrow's text. None for other marks, for no text, or
/// for a line too short to have a direction.
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
    // `flipped` means the line was drawn right to left (or top to bottom),
    // so the frame turns 180° and starts from `b`.
    let flipped = angle.cos() < -1e-9 || (angle.cos().abs() <= 1e-9 && angle.sin() > 0.0);
    let origin = if flipped {
        angle += PI;
        b
    } else {
        a
    };
    let (tw, th) = text_size(&shape.label, &st.font, st.font_size);
    let pad = st.font_size * 0.2;
    // Keep text off the line's ends (an arrowhead, a dot…).
    let size = end_size(st, len);
    let (at_a, at_b) = (end_reach(st.start, size, st.width), end_reach(st.end, size, st.width));
    let (head_start, head_end) = if flipped { (at_b, at_a) } else { (at_a, at_b) };
    let x = match shape.halign {
        HAlign::Left => head_start + pad,
        HAlign::Center => (len - tw) / 2.0,
        HAlign::Right => len - head_end - pad - tw,
    };
    // Clear the line and, near the tip, the head's spread. y is negative
    // (above the line) unless Bottom was chosen.
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
    // Turn the point into the text's frame, where the block is axis-aligned.
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
        // The same padding draw_shape_on leaves around the text.
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

/// Draws a picture and its marks: [`draw_page`] with the image as background.
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

/// Paints `image` at the origin, with good (bilinear-or-better) filtering
/// so it looks smooth when scaled.
pub fn paint_image(cr: &Context, image: &ImageSurface) {
    let _ = cr.set_source_surface(image, 0.0, 0.0);
    cr.source().set_filter(cairo::Filter::Good);
    let _ = cr.paint();
}

/// Adds a path through `pts`. A single point gets a tiny segment so a round
/// cap draws it as a dot (a click with the pen).
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

/// Adds a rectangle with corners rounded by `radius` (limited to half the
/// shorter side), as a closed sub-path, clockwise from the top right.
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
        // Going right to left along the bottom: the tail's right foot, its
        // tip (down at the bottom of the bounds, leaning left), its left foot.
        cr.line_to(tx + tw, base);
        cr.line_to(tx - tw * 0.6, r.y + r.h);
        cr.line_to(tx, base);
    }
    cr.arc(left + rad, base - rad, rad, PI / 2.0, PI);
    cr.arc(left + rad, top + rad, rad, PI, 1.5 * PI);
    cr.close_path();
}

/// Sets the line width and the dash pattern of `style` for the next stroke.
/// `draw_shape_on` saves and restores the context, so the dash never leaks
/// into anything drawn after the mark.
fn set_line(cr: &Context, style: &Style) {
    cr.set_line_width(style.width);
    cr.set_dash(&style.line.dashes(style.width), 0.0);
}

/// Fills and outlines the current path, leaving out whichever is off.
fn fill_and_stroke(cr: &Context, shape: &Shape) {
    if let Some(fill) = shape.style.fill {
        set_color(cr, fill);
        let _ = cr.fill_preserve();
    }
    if shape.draws_border() {
        set_color(cr, shape.style.stroke);
        set_line(cr, &shape.style);
        let _ = cr.stroke();
    } else {
        // fill_preserve kept the path; clear it so it can't leak into the
        // next thing drawn.
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

/// Draws one mark over `image` (tests only).
#[cfg(test)]
pub fn draw_shape(cr: &Context, image: &ImageSurface, shape: &Shape) {
    draw_shape_on(cr, &|cr: &Context| paint_image(cr, image), shape);
}

/// Draws one mark. `background` is only used by the loupe.
pub fn draw_shape_on(cr: &Context, background: Background, shape: &Shape) {
    let _ = cr.save();
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    // Rotate about the mark's centre: move the centre to the origin, turn,
    // and move back.
    if shape.angle != 0.0 {
        let (cx, cy) = shape.center();
        cr.translate(cx, cy);
        cr.rotate(shape.angle);
        cr.translate(-cx, -cy);
    }
    let st = &shape.style;
    match &shape.kind {
        Kind::Pen(pts) => draw_stroke(cr, st, pts),
        Kind::Highlighter(pts) => {
            polyline(cr, pts);
            // A see-through, multiplied stroke: like a marker pen, it
            // darkens what is under it without hiding text.
            set_color(cr, Rgba { a: 0.4 * st.stroke.a, ..st.stroke });
            cr.set_line_width(shape.highlighter_width());
            cr.set_line_cap(cairo::LineCap::Square);
            cr.set_operator(cairo::Operator::Multiply);
            let _ = cr.stroke();
        }
        // An arrow is a line whose ends came from the Arrow tool.
        Kind::Line | Kind::Arrow => {
            draw_stroke(cr, st, &[shape.a, shape.b]);
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
                // An ellipse is a unit circle scaled. Restore before stroking
                // so the line width isn't scaled too.
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
            // A magnifying glass: clip to a circle, zoom about its centre and
            // paint the page again (white first, for transparent pictures),
            // then draw the rim. Only the page is magnified, not other marks.
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
        Kind::Image(picture) => draw_picture(cr, &picture.0, &shape.bounds(), st.opacity),
    }
    let _ = cr.restore();
}

/// Stretches `image` over `r`. In a PDF it goes in as one embedded image
/// at its own resolution, with the marks around it still vector.
fn draw_picture(cr: &Context, image: &ImageSurface, r: &Rect, opacity: f64) {
    let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
    if r.w <= 0.0 || r.h <= 0.0 || iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let _ = cr.save();
    cr.rectangle(r.x, r.y, r.w, r.h);
    cr.clip();
    cr.translate(r.x, r.y);
    cr.scale(r.w / iw, r.h / ih);
    let _ = cr.set_source_surface(image, 0.0, 0.0);
    // Pad, so the edges stay sharp rather than fading into transparency
    // when the picture is enlarged. Not in a PDF, where the image is
    // embedded as is and padding could make cairo rasterise around it.
    if cr.target().type_() != cairo::SurfaceType::Pdf {
        cr.source().set_extend(cairo::Extend::Pad);
    }
    cr.source().set_filter(cairo::Filter::Good);
    let _ = cr.paint_with_alpha(opacity.clamp(0.0, 1.0));
    let _ = cr.restore();
}

/// Half the opening angle of a filled arrowhead, in radians (about 26°).
const ARROW_SPREAD: f64 = 0.45;
/// Half the opening angle of open and swept arrowheads (about 29°).
const WIDE_SPREAD: f64 = 0.5;

/// The size of a line's ends: the length of a filled arrowhead, which the
/// other ends are scaled from. It grows with the width but never takes more
/// than 60 % of the line (40 % each when both ends are drawn), so short
/// lines keep some line showing.
fn end_size(st: &Style, len: f64) -> f64 {
    let both = st.start != LineEnd::None && st.end != LineEnd::None;
    (st.width * 3.0 + 8.0).min(len * if both { 0.4 } else { 0.6 })
}

/// How far an end reaches back along the line from its point (to keep text
/// clear of it).
fn end_reach(end: LineEnd, size: f64, width: f64) -> f64 {
    match end {
        LineEnd::None => 0.0,
        LineEnd::Arrow | LineEnd::Stealth => size,
        LineEnd::OpenArrow => size * WIDE_SPREAD.cos() + width / 2.0,
        LineEnd::Dot => size * 0.35,
        LineEnd::Circle => size * 0.35 + width / 2.0,
        LineEnd::Square => size * 0.3,
        LineEnd::Diamond => size * 0.45,
        LineEnd::Bar => width / 2.0,
    }
}

/// How much of the line to leave out under an end: the line stops inside
/// a filled head, so its round cap can't poke out of the tip, and at the
/// rim of a hollow circle. Other ends sit on top of the line's end.
fn end_trim(end: LineEnd, size: f64) -> f64 {
    match end {
        LineEnd::Arrow => size * 0.7,
        LineEnd::Stealth => size * 0.6,
        LineEnd::Circle => size * 0.35,
        _ => 0.0,
    }
}

/// The distance between two points.
fn dist(p: Point, q: Point) -> f64 {
    ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt()
}

/// The length of the path through `pts`.
fn path_length(pts: &[Point]) -> f64 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// `pts` with the first `d` of its length removed.
fn cut_start(pts: &[Point], d: f64) -> Vec<Point> {
    if d <= 0.0 || pts.len() < 2 {
        return pts.to_vec();
    }
    let mut left = d;
    for i in 1..pts.len() {
        let (p, q) = (pts[i - 1], pts[i]);
        let seg = dist(p, q);
        if seg > left {
            let t = left / seg;
            let mut out = vec![(p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t)];
            out.extend_from_slice(&pts[i..]);
            return out;
        }
        left -= seg;
    }
    pts[pts.len() - 1..].to_vec()
}

/// `pts` with `from_start` of its length removed at the start and
/// `from_end` at the end.
fn trim_path(pts: &[Point], from_start: f64, from_end: f64) -> Vec<Point> {
    let mut v = cut_start(pts, from_start);
    v.reverse();
    let mut v = cut_start(&v, from_end);
    v.reverse();
    v
}

/// The unit direction a path leaves through its end point `pts[0]` (pass
/// the points reversed for the far end), taken over the last `reach` of
/// the path so a wobbly freehand stroke still gets a steady head.
fn outward(pts: impl Iterator<Item = Point>, reach: f64) -> Option<Point> {
    let mut pts = pts;
    let tip = pts.next()?;
    let mut back = None;
    for q in pts {
        back = Some(q);
        if dist(q, tip) >= reach {
            break;
        }
    }
    let q = back?;
    let d = dist(q, tip);
    (d > 1e-9).then(|| ((tip.0 - q.0) / d, (tip.1 - q.1) / d))
}

/// Strokes a freehand stroke, line or arrow through `pts` with its line
/// style, then draws its ends. Everything is painted with the colour's
/// alpha applied once, so a translucent head doesn't darken the line under it.
fn draw_stroke(cr: &Context, st: &Style, pts: &[Point]) {
    let len = path_length(pts);
    let ends = (st.start != LineEnd::None || st.end != LineEnd::None) && len >= 0.5;
    with_alpha(cr, st.stroke.a, || {
        set_color(cr, Rgba { a: 1.0, ..st.stroke });
        set_line(cr, st);
        if !ends {
            polyline(cr, pts);
            let _ = cr.stroke();
            return;
        }
        let size = end_size(st, len);
        polyline(cr, &trim_path(pts, end_trim(st.start, size), end_trim(st.end, size)));
        let _ = cr.stroke();
        // Ends are solid whatever the line style.
        cr.set_dash(&[], 0.0);
        let reach = size.min(len);
        if let Some(dir) = outward(pts.iter().copied(), reach) {
            draw_end(cr, st.start, pts[0], dir, size, st.width);
        }
        if let Some(dir) = outward(pts.iter().rev().copied(), reach) {
            draw_end(cr, st.end, pts[pts.len() - 1], dir, size, st.width);
        }
    });
}

/// Draws one end at `tip`, where the line leaves in direction `(ux, uy)`
/// (a unit vector), in the current colour. `size` is an arrowhead's length;
/// outlined ends use the line's `width`.
fn draw_end(cr: &Context, end: LineEnd, tip: Point, (ux, uy): Point, size: f64, width: f64) {
    // A point `along` back down the line from the tip and `across` to its side.
    let at = |along: f64, across: f64| (tip.0 - ux * along - uy * across, tip.1 - uy * along + ux * across);
    let polygon = |pts: &[Point]| {
        polyline(cr, pts);
        cr.close_path();
        let _ = cr.fill();
    };
    let (sin, cos) = ARROW_SPREAD.sin_cos();
    let (wsin, wcos) = WIDE_SPREAD.sin_cos();
    match end {
        LineEnd::None => {}
        LineEnd::Arrow => polygon(&[tip, at(size * cos, size * sin), at(size * cos, -size * sin)]),
        LineEnd::Stealth => {
            polygon(&[tip, at(size * wcos, size * wsin), at(size * 0.6, 0.0), at(size * wcos, -size * wsin)])
        }
        LineEnd::OpenArrow => {
            let (l, r) = (at(size * wcos, size * wsin), at(size * wcos, -size * wsin));
            cr.move_to(l.0, l.1);
            cr.line_to(tip.0, tip.1);
            cr.line_to(r.0, r.1);
            cr.set_line_width(width);
            let _ = cr.stroke();
        }
        LineEnd::Dot => {
            cr.new_sub_path();
            cr.arc(tip.0, tip.1, size * 0.35, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
        LineEnd::Circle => {
            cr.new_sub_path();
            cr.arc(tip.0, tip.1, size * 0.35, 0.0, 2.0 * PI);
            cr.set_line_width(width);
            let _ = cr.stroke();
        }
        LineEnd::Square => {
            let h = size * 0.3;
            polygon(&[at(-h, -h), at(-h, h), at(h, h), at(h, -h)]);
        }
        LineEnd::Diamond => {
            let (l, w) = (size * 0.45, size * 0.3);
            polygon(&[at(-l, 0.0), at(0.0, w), at(l, 0.0), at(0.0, -w)]);
        }
        LineEnd::Bar => {
            let (l, r) = (at(0.0, size * 0.45), at(0.0, -size * 0.45));
            cr.move_to(l.0, l.1);
            cr.line_to(r.0, r.1);
            cr.set_line_width(width);
            let _ = cr.stroke();
        }
    }
}

/// Renders the marked-up image at full resolution, cropped.
pub fn export(image: &ImageSurface, doc: &Document) -> Result<gdk::Texture, cairo::Error> {
    let r = doc.crop.unwrap_or(Rect { x: 0.0, y: 0.0, w: image.width() as f64, h: image.height() as f64 });
    let (w, h) = (r.w.round() as i32, r.h.round() as i32);
    let out = ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    {
        let cr = Context::new(&out)?;
        // Shift so the crop's corner lands at (0, 0); anything outside the
        // crop falls off the surface. The group keeps the highlighter's
        // multiply blending with the image only.
        cr.translate(-r.x, -r.y);
        cr.push_group();
        draw_document(&cr, image, doc);
        cr.pop_group_to_source()?;
        cr.paint()?;
    }
    texture_from_surface(out)
}

/// The largest side, in pixels, of a picture of copied marks.
const MARKS_MAX: f64 = 8192.0;

/// A picture of `shapes` alone on a transparent background, trimmed to
/// what they cover, for other apps to paste when marks are copied. A loupe
/// shows `image` magnified, as it does on the page. None if the marks draw
/// nothing or would make a picture over [`MARKS_MAX`] pixels across.
pub fn render_marks(image: &ImageSurface, shapes: &[Shape]) -> Result<Option<gdk::Texture>, cairo::Error> {
    let Some(r) = crate::document::outline_bounds(shapes) else { return Ok(None) };
    // Strokes, line ends and text reach past the marks' points: leave
    // room for them, then trim what stays empty.
    let mut room: f64 = 8.0;
    let mut r = r;
    for s in shapes {
        let w = s.style.width;
        room = room.max(s.highlighter_width() + 3.0 * w + 8.0 + w).max(s.style.font_size);
        if let Some(q) = line_label_outline(s) {
            for (x, y) in q {
                let (x0, y0) = (r.x.min(x), r.y.min(y));
                let (x1, y1) = ((r.x + r.w).max(x), (r.y + r.h).max(y));
                r = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
            }
        }
    }
    let (x0, y0) = ((r.x - room).floor(), (r.y - room).floor());
    let (w, h) = ((r.x + r.w + room).ceil() - x0, (r.y + r.h + room).ceil() - y0);
    if w > MARKS_MAX || h > MARKS_MAX || w < 1.0 || h < 1.0 {
        return Ok(None);
    }
    let surface = ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32)?;
    {
        let cr = Context::new(&surface)?;
        cr.translate(-x0, -y0);
        let background = |cr: &Context| paint_image(cr, image);
        for shape in shapes {
            draw_shape_on(&cr, &background, shape);
        }
    }
    surface.flush();
    let Some((tx, ty, tw, th)) = opaque_bounds(&surface) else { return Ok(None) };
    let out = ImageSurface::create(cairo::Format::ARgb32, tw, th)?;
    {
        let cr = Context::new(&out)?;
        cr.set_source_surface(&surface, -f64::from(tx), -f64::from(ty))?;
        cr.paint()?;
    }
    texture_from_surface(out).map(Some)
}

/// The smallest box (x, y, width, height) holding every pixel of an ARGB32
/// surface that isn't fully transparent, or None if all are.
fn opaque_bounds(surface: &ImageSurface) -> Option<(i32, i32, i32, i32)> {
    let (w, h, stride) = (surface.width(), surface.height(), surface.stride() as usize);
    let mut found: Option<(i32, i32, i32, i32)> = None;
    surface
        .with_data(|d| {
            for y in 0..h {
                let row = &d[y as usize * stride..][..w as usize * 4];
                // Native-endian ARGB32: alpha is the top byte of each u32.
                let alpha = |x: usize| u32::from_ne_bytes([row[4 * x], row[4 * x + 1], row[4 * x + 2], row[4 * x + 3]]) >> 24;
                let Some(first) = (0..w as usize).find(|&x| alpha(x) != 0) else { continue };
                let last = (0..w as usize).rfind(|&x| alpha(x) != 0).unwrap_or(first);
                let (first, last) = (first as i32, last as i32);
                found = Some(match found {
                    None => (first, y, last, y),
                    Some((x0, y0, x1, _)) => (x0.min(first), y0, x1.max(last), y),
                });
            }
        })
        .ok()?;
    found.map(|(x0, y0, x1, y1)| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
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
    // Make sure cairo has finished writing before reading the pixels.
    out.flush();
    let (w, h) = (out.width(), out.height());
    let stride = out.stride() as usize;
    let mut bytes = None;
    // Copies the pixels into GLib bytes the texture can own.
    out.with_data(|d| bytes = Some(gtk::glib::Bytes::from(d)))
        .map_err(|_| cairo::Error::SurfaceFinished)?;
    let bytes = bytes.ok_or(cairo::Error::SurfaceFinished)?;
    Ok(gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride).upcast())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws `shape` on white and returns a pixel reader (r, g, b as 0–255).
    fn draw_on_white(shape: &Shape) -> impl Fn(i32, i32) -> (u8, u8, u8) + use<> {
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

    /// A half-transparent arrow is evenly grey, even where shaft and head meet.
    #[test]
    fn translucent_arrow_has_no_dark_overlap() {
        let style = Style { stroke: Rgba::new(0.0, 0.0, 0.0, 0.5), width: 4.0, end: LineEnd::Arrow, ..Style::default() };
        let mut arrow = Shape::new(Kind::Arrow, (10.0, 50.0), style);
        arrow.b = (190.0, 50.0);
        let px = draw_on_white(&arrow);
        let shaft = px(100, 50);
        // Where the shaft runs into the head.
        let overlap = px(173, 50);
        assert!((shaft.0 as i32 - 128).abs() <= 2, "shaft {shaft:?}");
        assert_eq!(shaft, overlap);
    }

    /// A dashed line leaves gaps where a solid one doesn't, and the gaps
    /// grow with the width.
    #[test]
    fn dashed_line_has_gaps_that_scale_with_width() {
        use crate::document::LineStyle;
        let line = |width: f64, line: LineStyle| {
            let style = Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), width, line, ..Style::default() };
            let mut shape = Shape::new(Kind::Line, (10.0, 50.0), style);
            shape.b = (190.0, 50.0);
            draw_on_white(&shape)
        };
        let ink = |p: (u8, u8, u8)| p.0 < 128;
        // 4 px wide: dashes show 12 px long (8 to 20), then an 8 px gap.
        let dashed = line(4.0, LineStyle::Dashed);
        assert!(ink(dashed(14, 50)));
        assert!(!ink(dashed(24, 50)), "gap");
        assert!(ink(dashed(34, 50)), "next dash");
        let solid = line(4.0, LineStyle::Solid);
        assert!(ink(solid(24, 50)));
        // 8 px wide: the first dash shows from 6 to 30, so 24 is now inside
        // it, then a 16 px gap.
        let wide = line(8.0, LineStyle::Dashed);
        assert!(ink(wide(24, 50)));
        assert!(!ink(wide(40, 50)), "wider gap");
        assert!(ink(wide(52, 50)), "next dash");
        // Dots: 4 px circles every 8 px, centred on 10, 18, 26…
        let dotted = line(4.0, LineStyle::Dotted);
        assert!(ink(dotted(18, 50)));
        assert!(!ink(dotted(22, 50)), "between dots");
        // Dash-dot: a dash to 20, a 4 px dot centred on 30, then a dash.
        let dash_dot = line(4.0, LineStyle::DashDot);
        assert!(!ink(dash_dot(24, 50)), "gap before the dot");
        assert!(ink(dash_dot(30, 50)), "the dot is drawn");
        assert!(!ink(dash_dot(36, 50)), "gap after the dot");
        assert!(ink(dash_dot(46, 50)), "next dash");
    }

    /// A dashed arrow still has a solid, filled head.
    #[test]
    fn dashed_arrow_keeps_a_solid_head() {
        use crate::document::LineStyle;
        let style = Style {
            stroke: Rgba::new(0.0, 0.0, 0.0, 1.0),
            width: 4.0,
            line: LineStyle::Dotted,
            end: LineEnd::Arrow,
            ..Style::default()
        };
        let mut arrow = Shape::new(Kind::Arrow, (10.0, 50.0), style);
        arrow.b = (190.0, 50.0);
        let px = draw_on_white(&arrow);
        for x in [178, 182, 186] {
            assert!(px(x, 50).0 < 128, "head at {x}");
        }
    }

    /// Counts dark pixels inside `r`, outside a horizontal line's own band
    /// (|y − 50| ≤ half the width): the ink an end adds.
    fn ink_off_line(px: &impl Fn(i32, i32) -> (u8, u8, u8), r: std::ops::Range<i32>, half: i32) -> usize {
        r.flat_map(|x| (20..80).map(move |y| (x, y)))
            .filter(|&(x, y)| (y - 50i32).abs() > half + 1 && px(x, y).0 < 128)
            .count()
    }

    /// Every end but None adds ink at its own end only, and grows with the
    /// line width.
    #[test]
    fn line_ends_draw_at_their_end_and_scale_with_width() {
        let line = |width: f64, start: LineEnd, end: LineEnd| {
            let style = Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), width, start, end, ..Style::default() };
            let mut shape = Shape::new(Kind::Line, (40.0, 50.0), style);
            shape.b = (160.0, 50.0);
            draw_on_white(&shape)
        };
        for e in LineEnd::ALL {
            let at_end = line(4.0, LineEnd::None, e);
            let (near_a, near_b) = (ink_off_line(&at_end, 20..80, 2), ink_off_line(&at_end, 120..180, 2));
            assert_eq!(near_a, 0, "{e:?} drew at the start");
            let at_start = line(4.0, e, LineEnd::None);
            assert_eq!(ink_off_line(&at_start, 120..180, 2), 0, "{e:?} drew at the end");
            if e == LineEnd::None {
                assert_eq!(near_b, 0);
                continue;
            }
            assert!(near_b > 0, "{e:?} drew nothing");
            // The same shape at the start (give or take antialiasing).
            let mirrored = ink_off_line(&at_start, 20..80, 2);
            assert!(mirrored.abs_diff(near_b) * 10 <= near_b, "{e:?} start mirrors end: {mirrored} vs {near_b}");
            let wide = line(8.0, LineEnd::None, e);
            assert!(ink_off_line(&wide, 120..180, 4) > near_b, "{e:?} grows with width");
        }
    }

    /// The line stops inside a filled head and at a hollow circle's rim,
    /// so neither its cap nor its colour shows through.
    #[test]
    fn line_stops_under_heads_and_circles() {
        let style = |end| Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), width: 4.0, end, ..Style::default() };
        let mut ring = Shape::new(Kind::Line, (20.0, 50.0), style(LineEnd::Circle));
        ring.b = (160.0, 50.0);
        let px = draw_on_white(&ring);
        // Size 20, radius 7: the ring is centred on 160, empty inside.
        assert!(px(160, 50).0 > 200, "inside the circle is clear");
        assert!(px(153, 50).0 < 128, "the rim");
        assert!(px(167, 50).0 < 128, "the far rim");
        assert!(px(172, 50).0 > 200, "nothing past the rim");
        // Nothing past an arrow's tip but the antialiased edge.
        let mut arrow = Shape::new(Kind::Line, (20.0, 50.0), style(LineEnd::Arrow));
        arrow.b = (160.0, 50.0);
        assert!(draw_on_white(&arrow)(162, 50).0 > 200);
    }

    /// A freehand stroke's head points the way the stroke was going at its
    /// end: right, then down, ends with a head pointing down.
    #[test]
    fn pen_end_follows_the_stroke() {
        let style = Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), width: 2.0, end: LineEnd::Arrow, ..Style::default() };
        let pts: Vec<Point> = (0..=20).map(|i| (40.0 + i as f64 * 3.0, 20.0)).chain((1..=20).map(|i| (100.0, 20.0 + i as f64 * 3.0))).collect();
        let shape = Shape::new(Kind::Pen(pts), (0.0, 0.0), style);
        let px = draw_on_white(&shape);
        // Head 14 long ending at (100, 80): wide across x just above the tip.
        assert!(px(97, 70).0 < 128 && px(103, 70).0 < 128, "head spreads sideways");
        assert!(px(100, 83).0 > 200, "nothing past the tip");
    }

    /// Trimming takes length off either end of a path, across corners.
    #[test]
    fn trim_path_cuts_by_length() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(trim_path(&pts, 0.0, 0.0), pts.to_vec());
        assert_eq!(trim_path(&pts, 4.0, 0.0), vec![(4.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        assert_eq!(trim_path(&pts, 12.0, 3.0), vec![(10.0, 2.0), (10.0, 7.0)]);
        assert_eq!(trim_path(&pts, 30.0, 0.0), vec![(10.0, 10.0)]);
        assert_eq!(path_length(&pts), 20.0);
    }

    /// Border off: only the fill is painted.
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

    /// Fill off: the bubble's inside shows what's underneath.
    #[test]
    fn bubble_without_fill_is_see_through() {
        let style = Style { fill: None, width: 2.0, ..Style::default() };
        let mut bubble = Shape::new(Kind::Bubble, (20.0, 10.0), style);
        bubble.b = (180.0, 90.0);
        let px = draw_on_white(&bubble);
        assert_eq!(px(100, 40), (255, 255, 255));
    }

    /// The font is honoured, and text scales linearly with its size.
    #[test]
    fn text_uses_its_font() {
        let sans = text_size("iiii", "Sans", 20.0).0;
        let mono = text_size("iiii", "Monospace", 20.0).0;
        assert!(mono > sans * 1.2, "sans {sans}, mono {mono}");
        // Twice the size is twice as wide: no hinting differences with scale.
        let big = text_size("iiii", "Monospace", 40.0).0;
        assert!((big - 2.0 * mono).abs() < 0.5, "mono {mono}, big {big}");
    }

    /// A fitted text mark's ink lies inside its box.
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

    /// Black text on a transparent fill with no border, so only text is ink.
    fn black() -> Style {
        Style { stroke: Rgba::new(0.0, 0.0, 0.0, 1.0), border: false, fill: Some(Rgba::new(1.0, 1.0, 1.0, 0.0)), font_size: 16.0, ..Style::default() }
    }

    /// Shape text moves with its horizontal and vertical alignment.
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

    /// A picture of copied marks is trimmed to what they draw: a rectangle
    /// and its border, nothing of the page around it.
    #[test]
    fn copied_marks_picture_is_trimmed() {
        let image = ImageSurface::create(cairo::Format::ARgb32, 200, 200).unwrap();
        let mut rect = Shape::new(Kind::Rect, (10.0, 10.0), black());
        rect.style.width = 4.0;
        rect.style.border = true;
        rect.style.fill = None;
        rect.b = (50.0, 30.0);
        let t = render_marks(&image, &[rect]).unwrap().unwrap();
        // 40 × 20 plus half the border each side, give or take antialiasing.
        assert!((44..=46).contains(&t.width()) && (24..=26).contains(&t.height()), "{} × {}", t.width(), t.height());
        assert!(render_marks(&image, &[]).unwrap().is_none());
    }

    /// A solid red picture of `w` × `h` pixels.
    fn red_picture(w: i32, h: i32) -> crate::document::Picture {
        let img = ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        {
            let cr = Context::new(&img).unwrap();
            cr.set_source_rgb(1.0, 0.0, 0.0);
            cr.paint().unwrap();
        }
        crate::document::Picture(img)
    }

    /// A picture mark fills its box, stretched, and turns with its angle.
    #[test]
    fn image_mark_fills_its_box_and_rotates() {
        let mut shape = Shape::image(red_picture(10, 5), (100.0, 50.0), (1000.0, 1000.0), black());
        shape.a = (40.0, 30.0);
        shape.b = (160.0, 70.0);
        let px = draw_on_white(&shape);
        assert_eq!(px(45, 35), (255, 0, 0));
        assert_eq!(px(155, 65), (255, 0, 0));
        assert_eq!(px(35, 50), (255, 255, 255), "nothing outside");
        assert_eq!(px(100, 75), (255, 255, 255));
        // Upright: now 40 wide and 120 tall about the same centre.
        shape.angle = std::f64::consts::FRAC_PI_2;
        let px = draw_on_white(&shape);
        assert_eq!(px(45, 50), (255, 255, 255));
        assert_eq!(px(100, 5), (255, 0, 0));
    }

    /// A picture mark's opacity lets the page show through.
    #[test]
    fn image_mark_opacity() {
        let mut shape = Shape::image(red_picture(10, 10), (100.0, 50.0), (1000.0, 1000.0), black());
        assert_eq!(shape.style.opacity, 1.0);
        shape.style.opacity = 0.5;
        let (r, g, b) = draw_on_white(&shape)(100, 50);
        assert_eq!(r, 255);
        assert!((120..=135).contains(&g) && g == b, "half red on white: {g}");
    }

    /// Copying a picture mark gives a picture of its size.
    #[test]
    fn copied_image_mark_picture() {
        let image = ImageSurface::create(cairo::Format::ARgb32, 200, 200).unwrap();
        let mut shape = Shape::image(red_picture(10, 10), (0.0, 0.0), (100.0, 100.0), black());
        shape.a = (20.0, 20.0);
        shape.b = (80.0, 50.0);
        let t = render_marks(&image, &[shape]).unwrap().unwrap();
        assert_eq!((t.width(), t.height()), (60, 30));
    }

    /// Line text sits above or below, and reads upright either way round.
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
            if matches!(s.kind, Kind::Arrow) {
                s.style.end = LineEnd::Arrow;
            }
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

    /// Writes every line end at three widths, on lines, a dashed line and a
    /// curved freehand stroke, to a PNG for checking by eye
    /// (`cargo test line_end_sheet -- --ignored`).
    #[test]
    #[ignore]
    fn line_end_sheet() {
        let n = LineEnd::ALL.len() as f64;
        let surface = ImageSurface::create(cairo::Format::ARgb32, 1000, (n * 60.0 + 20.0) as i32).unwrap();
        let cr = Context::new(&surface).unwrap();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().unwrap();
        for (row, e) in LineEnd::ALL.into_iter().enumerate() {
            let y = 40.0 + row as f64 * 60.0;
            for (col, width) in [2.0, 4.0, 10.0].into_iter().enumerate() {
                let x = 20.0 + col as f64 * 200.0;
                let style = Style { width, start: e, end: e, ..Style::default() };
                let mut s = Shape::new(Kind::Line, (x + 20.0, y), style);
                s.b = (x + 170.0, y);
                draw_shape_on(&cr, &|_: &Context| {}, &s);
            }
            let style = Style {
                width: 4.0,
                line: crate::document::LineStyle::Dashed,
                stroke: Rgba::new(0.1, 0.3, 0.9, 0.6),
                end: e,
                ..Style::default()
            };
            let mut s = Shape::new(Kind::Line, (640.0, y + 15.0), style.clone());
            s.b = (780.0, y - 15.0);
            draw_shape_on(&cr, &|_: &Context| {}, &s);
            let pts: Vec<Point> = (0..=30).map(|i| {
                let t = i as f64 / 30.0 * PI;
                (820.0 + t * 45.0, y + 18.0 * t.sin())
            }).collect();
            draw_shape_on(&cr, &|_: &Context| {}, &Shape::new(Kind::Pen(pts), (0.0, 0.0), Style { end: e, ..style }));
        }
        drop(cr);
        let path = std::env::temp_dir().join("pygoscribe-line-end-sheet.png");
        texture_from_surface(surface).unwrap().save_to_png(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }
}
