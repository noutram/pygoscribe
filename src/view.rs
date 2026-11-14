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

//! View zoom and scroll arithmetic for the canvas, kept free of GTK so it can
//! be tested. The canvas maps image pixels to screen pixels as
//! `screen = image * scale + offset`, one axis at a time.

/// Space kept around the page on screen, in screen pixels.
pub const MARGIN: f64 = 24.0;

/// Zoom levels Zoom In and Zoom Out step through, in percent of actual size.
pub const STEPS: [f64; 17] =
    [5.0, 10.0, 25.0, 33.0, 50.0, 67.0, 75.0, 100.0, 125.0, 150.0, 200.0, 300.0, 400.0, 600.0, 800.0, 1200.0, 1600.0];

/// The smallest zoom, in percent.
pub const MIN_PERCENT: f64 = 5.0;
/// The largest zoom, in percent.
pub const MAX_PERCENT: f64 = 1600.0;

/// The next zoom step above `percent` (or the largest).
pub fn step_in(percent: f64) -> f64 {
    // The 1 % slack stops a level that is a hair off a step (from rounding
    // in fit or pinch zoom) from "stepping" to itself.
    STEPS.iter().copied().find(|&s| s > percent * 1.01).unwrap_or(MAX_PERCENT)
}

/// The next zoom step below `percent` (or the smallest).
pub fn step_out(percent: f64) -> f64 {
    STEPS.iter().rev().copied().find(|&s| s < percent * 0.99).unwrap_or(MIN_PERCENT)
}

/// The scale that fits `len` image pixels (each axis) in `view` screen pixels
/// with a margin, never enlarging past 1 image pixel per screen pixel.
pub fn fit_scale(len: (f64, f64), view: (f64, f64)) -> f64 {
    // The smaller of the two axis ratios fits both; 0.02 stops a tiny window
    // from producing a zero or negative scale.
    ((view.0 - 2.0 * MARGIN) / len.0).min((view.1 - 2.0 * MARGIN) / len.1).clamp(0.02, 1.0)
}

/// Whether content `len` image pixels long overflows `view` at `scale`.
pub fn overflows(len: f64, scale: f64, view: f64) -> bool {
    // Half a pixel of tolerance avoids scrollbars flickering on rounding.
    len * scale + 2.0 * MARGIN > view + 0.5
}

/// One axis: the offset that keeps the visible part of the page (`start`,
/// `len` in image pixels) in view. Content that fits is centred; content
/// larger than the view may scroll until its edge is a margin from the
/// view's edge.
pub fn clamp_offset(start: f64, len: f64, scale: f64, view: f64, offset: f64) -> f64 {
    let size = len * scale;
    if !overflows(len, scale, view) {
        return (view - size) / 2.0 - start * scale;
    }
    // `edge` is where the content's left/top edge lands on screen. It may go
    // no further right than MARGIN, and no further left than leaves the far
    // edge MARGIN from the view's far side.
    let edge = (start * scale + offset).clamp(view - size - MARGIN, MARGIN);
    edge - start * scale
}

/// The offset that keeps image point `p` under screen point `anchor` (one
/// axis) at `scale`.
pub fn offset_about(anchor: f64, p: f64) -> impl Fn(f64) -> f64 {
    move |scale| anchor - p * scale
}

/// One axis as a scrollbar: (upper, page size, value) for content `start`,
/// `len` in image pixels. The lower bound is 0.
pub fn scrollbar(start: f64, len: f64, scale: f64, view: f64, offset: f64) -> (f64, f64, f64) {
    let upper = len * scale + 2.0 * MARGIN;
    (upper, view, MARGIN - (start * scale + offset))
}

/// The offset for a scrollbar `value` (the inverse of [`scrollbar`]).
pub fn offset_for_scroll(start: f64, scale: f64, value: f64) -> f64 {
    MARGIN - value - start * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zoom In/Out move to the neighbouring step, stopping at the ends.
    #[test]
    fn steps_move_between_levels() {
        assert_eq!(step_in(100.0), 125.0);
        assert_eq!(step_out(100.0), 75.0);
        assert_eq!(step_in(110.0), 125.0);
        assert_eq!(step_out(110.0), 100.0);
        assert_eq!(step_in(1600.0), 1600.0);
        assert_eq!(step_out(5.0), 5.0);
        // A fitted 63% steps to the neighbouring levels.
        assert_eq!(step_in(63.0), 67.0);
        assert_eq!(step_out(63.0), 50.0);
    }

    /// Fit shrinks large content but never blows small content up.
    #[test]
    fn fitting_never_enlarges() {
        assert_eq!(fit_scale((100.0, 100.0), (1000.0, 800.0)), 1.0);
        let s = fit_scale((2000.0, 1000.0), (1048.0, 800.0));
        assert!((s - 0.5).abs() < 1e-9);
    }

    /// Content smaller than the view is always centred.
    #[test]
    fn small_content_is_centred() {
        // 100 px at 2x = 200 px in a 1000 px view: centred whatever the offset.
        assert_eq!(clamp_offset(0.0, 100.0, 2.0, 1000.0, -5000.0), 400.0);
        // A crop starting at 50 still centres the cropped part.
        assert_eq!(clamp_offset(50.0, 100.0, 2.0, 1000.0, 0.0), 300.0);
    }

    /// Large content scrolls, but only until its edges are a margin in.
    #[test]
    fn large_content_scrolls_to_its_edges_only() {
        // 1000 px at 2x = 2000 px in a 500 px view.
        let o = |off| clamp_offset(0.0, 1000.0, 2.0, 500.0, off);
        assert_eq!(o(0.0), 0.0);
        assert_eq!(o(100.0), MARGIN, "left edge stops a margin in");
        assert_eq!(o(-5000.0), 500.0 - 2000.0 - MARGIN, "right edge stops a margin in");
    }

    /// Zooming about a point keeps that point under the pointer.
    #[test]
    fn zooming_about_a_point_keeps_it_still() {
        // Image point 30 is under screen 100 at scale 1 (offset 70).
        let (scale, offset) = (1.0, 70.0);
        let p = (100.0 - offset) / scale;
        let new = offset_about(100.0, p)(4.0);
        assert_eq!(p * 4.0 + new, 100.0);
    }

    /// Offset → scrollbar value → offset gives back the same offset.
    #[test]
    fn scrollbar_round_trips() {
        let (start, scale, off) = (10.0, 3.0, -200.0);
        let (upper, page, value) = scrollbar(start, 1000.0, scale, 600.0, off);
        assert_eq!(upper, 3000.0 + 2.0 * MARGIN);
        assert_eq!(page, 600.0);
        assert_eq!(offset_for_scroll(start, scale, value), off);
        // At the top the page edge sits a margin from the view's edge.
        assert_eq!(start * scale + offset_for_scroll(start, scale, 0.0), MARGIN);
    }
}
