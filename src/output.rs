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

//! Getting the result out of the app: save to a file, hand off to another app,
//! or attach to a new email. Copying to the clipboard lives in the window.
//!
//! Everything here takes the finished, full-resolution result as a
//! `gdk::Texture` (made by `render::export`), so crops and marks are already
//! baked in. Multi-page PDF saving is in `pdf::Pdf::save_marked` instead,
//! because it draws the pages as vectors rather than from a texture.

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};
use crate::settings::SaveFormat;
use std::path::{Path, PathBuf};

/// A default file name such as "Markup 2026-11-10 at 21.04.33.png".
pub fn default_name(ext: &str) -> String {
    // Dots, not colons, in the time: colons are awkward in file names
    // (and not allowed on some file systems the file may be copied to).
    let stamp = glib::DateTime::now_local()
        .and_then(|d| d.format("%Y-%m-%d at %H.%M.%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "image".into());
    format!("Markup {stamp}.{ext}")
}

/// Wraps an I/O (or any displayable) error as a `glib::Error`.
fn io_err(e: impl std::fmt::Display) -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string())
}

/// Writes the texture as a PNG, JPEG or single-page PDF. For a PDF,
/// `points_per_pixel` sets the page size: 1 makes each pixel a point, and an
/// image rendered from a PDF passes its render scale so the page keeps its
/// original size (at full resolution).
pub fn save(texture: &gdk::Texture, path: &Path, format: SaveFormat, points_per_pixel: f64) -> Result<(), glib::Error> {
    match format {
        SaveFormat::Pdf => save_pdf(texture, path, points_per_pixel).map_err(io_err),
        SaveFormat::Jpeg => {
            // JPEG has no alpha: download as packed RGB (transparent areas
            // come out as their premultiplied colour, usually black) and use
            // gdk-pixbuf's JPEG saver, since GDK can only write PNG and TIFF.
            let mut dl = gdk::TextureDownloader::new(texture);
            dl.set_format(gdk::MemoryFormat::R8g8b8);
            let (bytes, stride) = dl.download_bytes();
            let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(
                &bytes,
                gdk_pixbuf::Colorspace::Rgb,
                false,
                8,
                texture.width(),
                texture.height(),
                stride as i32,
            );
            pixbuf.savev(path, "jpeg", &[("quality", "92")])
        }
        SaveFormat::Png => texture.save_to_png(path).map_err(io_err),
    }
}

/// A one-page PDF holding the image, the page sized from `points_per_pixel`
/// (anything non-positive or non-finite counts as 1). The image is embedded
/// at full resolution and scaled down to the page.
fn save_pdf(texture: &gdk::Texture, path: &Path, points_per_pixel: f64) -> Result<(), gtk::cairo::Error> {
    use gtk::cairo;
    let scale = if points_per_pixel.is_finite() && points_per_pixel > 0.0 { points_per_pixel } else { 1.0 };
    let image = crate::render::surface_from_texture(texture)?;
    let (w, h) = (f64::from(texture.width()) * scale, f64::from(texture.height()) * scale);
    let pdf = cairo::PdfSurface::new(w, h, path)?;
    {
        let cr = cairo::Context::new(&pdf)?;
        cr.scale(scale, scale);
        cr.set_source_surface(&image, 0.0, 0.0)?;
        cr.paint()?;
        cr.show_page()?;
    }
    pdf.finish();
    pdf.status()
}

/// The file an image or PDF was opened from, and the format Save writes
/// back to it.
#[derive(Clone, Debug)]
pub struct Original {
    /// The file to write back to.
    pub file: gio::File,
    /// Its real format, from its contents rather than its name.
    pub format: OriginalFormat,
}

/// How Save (Replace Original) writes the file back.
#[derive(Clone, Debug, PartialEq)]
pub enum OriginalFormat {
    /// PNG, JPEG or PDF, written as Save As writes them.
    Known(SaveFormat),
    /// Another format gdk-pixbuf can write (TIFF, BMP, ICO, WebP…), by its
    /// gdk-pixbuf name.
    Pixbuf(String),
}

impl Original {
    /// The file and its format, or None if it isn't a local file or its
    /// format can't be written (GIF, SVG and the like).
    pub fn detect(file: &gio::File, is_pdf: bool) -> Option<Self> {
        let path = file.path()?;
        let format = if is_pdf { OriginalFormat::Known(SaveFormat::Pdf) } else { pixbuf_format(&path)? };
        Some(Self { file: file.clone(), format })
    }

    /// The file's name, for the confirmation dialog.
    pub fn name(&self) -> String {
        self.file.basename().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }

    /// The extension to give a temporary copy, so savers see the right type.
    fn extension(&self) -> &str {
        match &self.format {
            OriginalFormat::Known(f) => f.extension(),
            OriginalFormat::Pixbuf(name) => name,
        }
    }

    /// Replaces the file with what `write` writes to a path, in one step:
    /// it is written to a temporary file first and then swapped in, so a
    /// failure leaves the original as it was.
    pub fn replace(&self, write: impl FnOnce(&Path) -> Result<(), glib::Error>) -> Result<(), glib::Error> {
        let tmp = glib::tmp_dir().join(format!("pygoscribe-save-{}.{}", std::process::id(), self.extension()));
        // replace_contents writes to a temporary file beside the original and
        // renames it over it, so the swap itself is atomic too.
        let result = write(&tmp).and_then(|()| std::fs::read(&tmp).map_err(io_err)).and_then(|bytes| {
            self.file
                .replace_contents(&bytes, None, false, gio::FileCreateFlags::NONE, gio::Cancellable::NONE)
                .map(|_| ())
        });
        let _ = std::fs::remove_file(&tmp);
        result
    }
}

/// The format of the picture at `path`, if gdk-pixbuf can write it.
fn pixbuf_format(path: &Path) -> Option<OriginalFormat> {
    // file_info sniffs the contents, so a mislabelled file is still right.
    let (format, _, _) = gdk_pixbuf::Pixbuf::file_info(path)?;
    let name = format.name()?.to_string();
    match name.as_str() {
        "png" => Some(OriginalFormat::Known(SaveFormat::Png)),
        "jpeg" => Some(OriginalFormat::Known(SaveFormat::Jpeg)),
        _ if format.is_writable() => Some(OriginalFormat::Pixbuf(name)),
        _ => None,
    }
}

/// Writes the texture with gdk-pixbuf's saver for `format` ("tiff", "bmp"…).
pub fn save_pixbuf(texture: &gdk::Texture, path: &Path, format: &str) -> Result<(), glib::Error> {
    // Formats whose gdk-pixbuf saver accepts an alpha channel; the others
    // fail if given one, so they get RGB.
    let alpha = matches!(format, "png" | "tiff" | "ico" | "webp" | "avif");
    let mut dl = gdk::TextureDownloader::new(texture);
    dl.set_format(if alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 });
    let (bytes, stride) = dl.download_bytes();
    let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(
        &bytes,
        gdk_pixbuf::Colorspace::Rgb,
        alpha,
        8,
        texture.width(),
        texture.height(),
        stride as i32,
    );
    pixbuf.savev(path, format, &[])
}

/// The user's Pictures folder. In a snap, HOME points inside
/// ~/snap/pygoscribe, so fall back to the real home's Pictures if GLib didn't
/// find the real one.
pub fn pictures_dir() -> PathBuf {
    let dir = glib::user_special_dir(glib::UserDirectory::Pictures);
    if let Some(real_home) = std::env::var_os("SNAP_REAL_HOME").map(PathBuf::from) {
        match dir {
            Some(d) if d.starts_with(&real_home) => d,
            _ => real_home.join("Pictures"),
        }
    } else {
        dir.unwrap_or_else(glib::home_dir)
    }
}

/// Writes the texture to a PNG for handing to other apps.
///
/// This deliberately isn't a hidden folder like ~/.cache: snap apps such as
/// Ubuntu's Thunderbird and Firefox can't read hidden folders in your home.
pub fn save_temp(texture: &gdk::Texture) -> Result<PathBuf, glib::Error> {
    let dir = pictures_dir().join("Markup-Sent");
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    // Keep the folder from growing forever: drop files older than a week.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > 7 * 86_400);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // No spaces: some mail clients mangle attachment paths that contain them.
    let path = dir.join(default_name("png").replace(' ', "-"));
    texture.save_to_png(&path).map_err(io_err)?;
    Ok(path)
}

/// Asks the desktop which app to open the image with (the "Open With" portal).
pub async fn open_with(texture: &gdk::Texture, parent: &impl IsA<gtk::Window>) -> Result<(), glib::Error> {
    let path = save_temp(texture)?;
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    // Show the app chooser even when there is a default image viewer.
    launcher.set_always_ask(true);
    launcher.launch_future(Some(parent)).await
}

/// What `email` managed to do.
pub enum EmailOutcome {
    /// A compose window opened with the image attached.
    Attached,
    /// The mail app can't take attachments from other apps (for example
    /// Proton Mail or webmail), so a blank compose window was opened and the
    /// caller should put the image on the clipboard to paste in.
    PasteNeeded,
}

/// Mail apps known to accept attachments via `xdg-email --attach`.
const XDG_EMAIL_CLIENTS: &[&str] = &["evolution", "kmail", "claws", "geary", "mutt", "sylpheed"];

/// Starts a new email in the user's default mail app with the image attached.
pub async fn email(texture: &gdk::Texture) -> Result<EmailOutcome, glib::Error> {
    let path = save_temp(texture)?;
    // Flatpak and snap apps can't start the mail app themselves.
    if crate::autostart::sandboxed() {
        email_portal(&path).await?;
        return Ok(EmailOutcome::Attached);
    }

    // Identify the default mail app by its desktop ID and executable, in
    // lower case, so e.g. "thunderbird_thunderbird.desktop" (snap) matches.
    let handler = gio::AppInfo::default_for_uri_scheme("mailto");
    let id = handler
        .as_ref()
        .map(|a| format!("{} {}", a.id().unwrap_or_default(), a.executable().display()).to_lowercase())
        .unwrap_or_default();

    if id.contains("thunderbird") || id.contains("betterbird") {
        // Thunderbird (deb or snap) takes attachments on its command line.
        let exe = if id.contains("betterbird") { "betterbird" } else { "thunderbird" };
        let uri = gio::File::for_path(&path).uri();
        let arg = format!("subject='Image',attachment='{uri}'");
        gio::Subprocess::newv(
            &[exe.as_ref(), "-compose".as_ref(), std::ffi::OsStr::new(&arg)],
            gio::SubprocessFlags::NONE,
        )?;
        return Ok(EmailOutcome::Attached);
    }

    // xdg-email knows how to pass attachments to these. If it fails, fall
    // through to the blank-email path rather than report an error.
    if XDG_EMAIL_CLIENTS.iter().any(|c| id.contains(c)) {
        let args = [std::ffi::OsStr::new("xdg-email"), "--attach".as_ref(), path.as_os_str()];
        if let Ok(proc) = gio::Subprocess::newv(&args, gio::SubprocessFlags::STDERR_SILENCE)
            && proc.wait_check_future().await.is_ok()
        {
            return Ok(EmailOutcome::Attached);
        }
    }

    // Anything else (Proton Mail, webmail, no mail app): open a blank email.
    gio::AppInfo::launch_default_for_uri_future("mailto:?subject=Image", None::<&gio::AppLaunchContext>).await?;
    Ok(EmailOutcome::PasteNeeded)
}

/// Asks the Email portal for a compose window with `path` attached. The file
/// is passed as an open file descriptor (the portal's `attachment_fds`), so
/// the mail app can read it even though it can't see inside our sandbox.
async fn email_portal(path: &Path) -> Result<(), glib::Error> {
    let file = std::fs::File::open(path).map_err(io_err)?;
    // D-Bus sends file descriptors out of band: the message carries an index
    // (a Handle) into this list.
    let fds = gio::UnixFDList::new();
    let idx = fds.append(&file)?;

    let options = glib::VariantDict::new(None);
    options.insert_value("attachment_fds", &vec![glib::variant::Handle(idx)].to_variant());
    options.insert_value("subject", &"Image".to_variant());
    let params = ("", options.end()).to_variant();

    let conn = gio::bus_get_future(gio::BusType::Session).await?;
    conn.call_with_unix_fd_list_future(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Email",
        "ComposeEmail",
        Some(&params),
        Some(glib::VariantTy::new("(o)").expect("valid type")),
        gio::DBusCallFlags::NONE,
        -1,
        Some(&fds),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20 × 10 solid red texture.
    fn texture() -> gdk::Texture {
        let data: Vec<u8> = (0..20 * 10).flat_map(|_| [255u8, 0, 0, 255]).collect();
        gdk::MemoryTexture::new(20, 10, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(data), 80).upcast()
    }

    /// Each format writes a file with the right magic bytes, whatever the
    /// name's extension, and PNG/JPEG read back at the right size.
    #[test]
    fn saves_png_jpeg_and_pdf() {
        let dir = std::env::temp_dir().join(format!("pygoscribe-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png_magic: &[u8] = b"\x89PNG";
        let jpeg_magic: &[u8] = b"\xff\xd8\xff";
        let pdf_magic: &[u8] = b"%PDF-";
        for (name, format, magic) in [
            ("out.png", SaveFormat::Png, png_magic),
            ("out.jpg", SaveFormat::Jpeg, jpeg_magic),
            ("no-extension", SaveFormat::Jpeg, jpeg_magic),
            ("out.pdf", SaveFormat::Pdf, pdf_magic),
        ] {
            let path = dir.join(name);
            save(&texture(), &path, format, 1.0).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.starts_with(magic), "{name} has the wrong format");
            if format == SaveFormat::Pdf {
                continue;
            }
            let back = gdk::Texture::from_filename(&path).unwrap();
            assert_eq!((back.width(), back.height()), (20, 10), "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Save writes back in the file's own format, replacing it.
    #[test]
    fn replaces_original_in_its_format() {
        let dir = std::env::temp_dir().join(format!("pygoscribe-original-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small: gdk::Texture = {
            let data: Vec<u8> = (0..4 * 3).flat_map(|_| [0u8, 0, 255, 255]).collect();
            gdk::MemoryTexture::new(4, 3, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(data), 16).upcast()
        };
        // A .jpg that is really a PNG is saved as PNG: the contents decide.
        for (name, format) in [("a.png", "png"), ("b.jpg", "jpeg"), ("c.bmp", "bmp"), ("d.jpg", "png")] {
            let path = dir.join(name);
            save_pixbuf(&texture(), &path, format).unwrap();
            let original = Original::detect(&gio::File::for_path(&path), false).expect(name);
            let want = match format {
                "png" => OriginalFormat::Known(SaveFormat::Png),
                "jpeg" => OriginalFormat::Known(SaveFormat::Jpeg),
                f => OriginalFormat::Pixbuf(f.into()),
            };
            assert_eq!(original.format, want, "{name}");
            original
                .replace(|p| match &original.format {
                    OriginalFormat::Known(f) => save(&small, p, *f, 1.0),
                    OriginalFormat::Pixbuf(f) => save_pixbuf(&small, p, f),
                })
                .unwrap();
            let (info, w, h) = gdk_pixbuf::Pixbuf::file_info(&path).unwrap();
            assert_eq!((info.name().unwrap().as_str(), w, h), (format, 4, 3), "{name}");
        }
        // Not a picture at all: nothing to save back to.
        let path = dir.join("notes.txt");
        std::fs::write(&path, "hello").unwrap();
        assert!(Original::detect(&gio::File::for_path(&path), false).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A PDF page comes back at its original size after a round trip.
    #[test]
    fn pdf_page_size_follows_scale() {
        let dir = std::env::temp_dir().join(format!("pygoscribe-pdfsave-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("half.pdf");
        save(&texture(), &path, SaveFormat::Pdf, 0.5).unwrap();
        let pdf = crate::pdf::Pdf::open(&gio::File::for_path(&path)).unwrap();
        assert_eq!(pdf.n_pages(), 1);
        let (tex, _) = pdf.render(0).unwrap();
        // 20×10 px at 0.5 pt/px is a 10×5 pt page, rendered at 2 px/pt.
        assert_eq!((tex.width(), tex.height()), (20, 10));
        let _ = std::fs::remove_dir_all(dir);
    }
}
