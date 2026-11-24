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

//! Copied marks, and how they share the one system clipboard with the
//! app's other use of it, copying the whole marked-up image.
//!
//! # The scheme
//!
//! There is one clipboard, as everywhere else on the desktop. Whatever was
//! copied last is what Paste gets:
//!
//! - **Copy (Ctrl+C) with a mark selected copies the mark**; with nothing
//!   selected it copies the whole image, as it always has. **Copy Image**
//!   (the header's Copy button, Ctrl+Shift+C, the image's context menu)
//!   always copies the whole image.
//! - Copying a mark puts **a picture of just that mark** (transparent
//!   background, see `render::render_marks`) on the clipboard, so pasting in
//!   another app gives something sensible, plus a private type
//!   ([`MIME`]) that marks the content as ours. The marks themselves stay
//!   in this process (every window lives in the one instance), in
//!   [`COPIED`], so they paste back fully editable.
//! - **Paste (Ctrl+V) pastes marks only while the clipboard still holds
//!   that copy.** Copying anything else, here (Copy Image) or in another
//!   app, replaces it, and [`take`] then finds nothing: the clipboard's
//!   content is compared with the provider set here, so there is no guessing
//!   about which copy is newer.
//!
//! Marks are in image pixels, so a mark pasted on a page of another size
//! keeps its size in pixels; `document::place` brings it into view.

use crate::document::Shape;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;

/// The private clipboard type offered with copied marks. Its bytes say
/// nothing: they only show the content came from here.
pub const MIME: &str = "application/x-org.outram.pygoscribe-marks";

/// The last marks copied, and the clipboard content that stands for them.
struct Copied {
    /// The marks, as they were when copied.
    shapes: Vec<Shape>,
    /// What was put on the clipboard; while the clipboard still holds it,
    /// these marks are what Paste gets.
    provider: gdk::ContentProvider,
    /// How many steps the next paste moves the marks: 1 after Copy, so the
    /// copy is seen beside the original; 0 after Cut, so the first paste
    /// lands where the marks were. Each paste adds one.
    next: u32,
}

thread_local! {
    /// The last marks copied, shared by every window (GTK runs on one thread).
    static COPIED: RefCell<Option<Copied>> = const { RefCell::new(None) };
}

/// Puts `shapes` on `clipboard`, with `picture` (of the marks alone) for
/// other apps. `cut` makes the first paste land in place.
pub fn put(clipboard: &gdk::Clipboard, shapes: Vec<Shape>, picture: Option<gdk::Texture>, cut: bool) -> Result<(), glib::BoolError> {
    let marker = gdk::ContentProvider::for_bytes(MIME, &glib::Bytes::from_static(b"marks"));
    let provider = match picture {
        Some(t) => gdk::ContentProvider::new_union(&[gdk::ContentProvider::for_value(&t.to_value()), marker]),
        None => marker,
    };
    clipboard.set_content(Some(&provider))?;
    COPIED.with_borrow_mut(|c| *c = Some(Copied { shapes, provider, next: u32::from(!cut) }));
    Ok(())
}

/// Whether `clipboard` still holds the marks copied last.
pub fn has_marks(clipboard: &gdk::Clipboard) -> bool {
    let current = clipboard.content();
    COPIED.with_borrow(|c| c.as_ref().is_some_and(|c| current.as_ref() == Some(&c.provider)))
}

/// The marks to paste and how many steps to move them (see
/// [`Copied::next`]), or None if `clipboard` holds something else now.
pub fn take(clipboard: &gdk::Clipboard) -> Option<(Vec<Shape>, u32)> {
    if !has_marks(clipboard) {
        return None;
    }
    COPIED.with_borrow_mut(|c| {
        let c = c.as_mut()?;
        let n = c.next;
        c.next += 1;
        Some((c.shapes.clone(), n))
    })
}
