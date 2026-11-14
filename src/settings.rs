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

//! Settings kept between uses: the toolbar style, the last open and save
//! folders, and the last save format.
//!
//! They live in a small key file, `<config dir>/pygoscribe/settings.ini`.
//! That is `~/.config/pygoscribe/` for the .deb and `cargo run`, and the app's
//! own `~/.var/app/<app id>/config/pygoscribe/` inside the Flatpak, so no extra
//! sandbox permissions or installed GSettings schema are needed. A missing or
//! unreadable file, or a bad value, falls back to the defaults.

use crate::document::{Rgba, Style};
use gtk::{gio, glib};
use gtk::prelude::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// Key-file group for the toolbar style (colours, width, font).
const STYLE: &str = "style";
/// Key-file group for the last open and save folders.
const FOLDERS: &str = "folders";
/// Key-file group for the last save format.
const SAVE: &str = "save";
/// Key-file group for view options (the page sidebar).
const VIEW: &str = "view";

/// A format Save As can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFormat {
    /// Lossless, with transparency. The default.
    Png,
    /// Lossy, smaller; transparency is flattened.
    Jpeg,
    /// One page for a picture, every page for an open PDF.
    Pdf,
}

impl SaveFormat {
    /// The extension used for new file names (`jpg`, not `jpeg`).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Pdf => "pdf",
        }
    }

    /// The format a file name's extension asks for, if it names one.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }

    /// The value stored in the settings file. Kept separate from
    /// [`Self::extension`] so the file format can't change by accident.
    fn key(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Pdf => "pdf",
        }
    }
}

/// `name` with its image extension swapped for `format`'s, or `format`'s
/// added if it has none.
pub fn with_extension(name: &str, format: SaveFormat) -> String {
    // Only strip an extension we recognise, so "notes.v2" keeps its ".v2";
    // a name that is all extension (".png") is kept whole.
    let stem = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && SaveFormat::from_path(Path::new(&format!("x.{ext}"))).is_some() => stem,
        _ => name,
    };
    format!("{stem}.{}", format.extension())
}

/// Everything Pygoscribe remembers between uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Border (stroke) colour, also used for text.
    pub stroke: Rgba,
    /// Whether new shapes get a border.
    pub border_on: bool,
    /// Kept even while fill is off, so turning it back on restores the colour.
    pub fill: Rgba,
    /// Whether new shapes are filled.
    pub fill_on: bool,
    /// Line width in image pixels.
    pub width: f64,
    /// Text size in points (image pixels at 100 %).
    pub font_size: f64,
    /// Pango font description without a size.
    pub font: String,
    /// Where the Open dialog starts.
    pub open_folder: Option<PathBuf>,
    /// Where the Save dialogs start.
    pub save_folder: Option<PathBuf>,
    /// What Save As writes by default.
    pub save_format: SaveFormat,
    /// Whether the page sidebar is shown.
    pub sidebar_visible: bool,
}

/// The fill colour before the user picks one: a pale yellow.
pub const DEFAULT_FILL: Rgba = Rgba::new(1.0, 0.96, 0.6, 1.0);
/// Allowed line widths (min, max); stored values are clamped to it.
pub const WIDTH_RANGE: (f64, f64) = (1.0, 40.0);
/// Allowed text sizes (min, max); stored values are clamped to it.
pub const FONT_RANGE: (f64, f64) = (6.0, 200.0);

/// The defaults follow [`Style::default`], so a fresh install and a new mark
/// agree.
impl Default for Settings {
    fn default() -> Self {
        let style = Style::default();
        Self {
            stroke: style.stroke,
            border_on: style.border,
            fill: DEFAULT_FILL,
            fill_on: style.fill.is_some(),
            width: style.width,
            font_size: style.font_size,
            font: style.font,
            open_folder: None,
            save_folder: None,
            save_format: SaveFormat::Png,
            sidebar_visible: true,
        }
    }
}

impl Settings {
    /// Reads the key file at `path`. Each value is checked on its own, so
    /// one bad entry only resets that entry to its default.
    pub fn load_from(path: &Path) -> Self {
        let mut s = Self::default();
        let kf = glib::KeyFile::new();
        if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
            return s;
        }
        if let Some(c) = read_colour(&kf, STYLE, "stroke") {
            s.stroke = c;
        }
        if let Some(c) = read_colour(&kf, STYLE, "fill") {
            s.fill = c;
        }
        if let Ok(b) = kf.boolean(STYLE, "fill-on") {
            s.fill_on = b;
        }
        if let Ok(b) = kf.boolean(STYLE, "border-on") {
            s.border_on = b;
        }
        // A shape needs a border or a fill.
        if !s.border_on && !s.fill_on {
            s.border_on = true;
        }
        if let Some(w) = read_number(&kf, STYLE, "width", WIDTH_RANGE) {
            s.width = w;
        }
        if let Some(f) = read_number(&kf, STYLE, "font-size", FONT_RANGE) {
            s.font_size = f;
        }
        // Reject empty or absurdly long font names rather than hand Pango junk.
        if let Ok(f) = kf.string(STYLE, "font") {
            let f = f.trim();
            if !f.is_empty() && f.len() < 200 {
                s.font = f.to_string();
            }
        }
        s.open_folder = read_folder(&kf, "open");
        s.save_folder = read_folder(&kf, "save");
        s.save_format = match kf.string(SAVE, "format").ok().as_deref() {
            Some("jpeg") => SaveFormat::Jpeg,
            Some("pdf") => SaveFormat::Pdf,
            _ => SaveFormat::Png,
        };
        if let Ok(b) = kf.boolean(VIEW, "sidebar-visible") {
            s.sidebar_visible = b;
        }
        s
    }

    /// Writes every setting to `path`, creating its folder if needed.
    pub fn save_to(&self, path: &Path) -> Result<(), glib::Error> {
        let kf = glib::KeyFile::new();
        // Colours are stored as "r;g;b;a" (0–1 each), which KeyFile reads
        // back as a double list.
        let colour = |key: &str, c: Rgba| kf.set_string(STYLE, key, &format!("{};{};{};{}", c.r, c.g, c.b, c.a));
        colour("stroke", self.stroke);
        colour("fill", self.fill);
        kf.set_boolean(STYLE, "fill-on", self.fill_on);
        kf.set_boolean(STYLE, "border-on", self.border_on);
        kf.set_double(STYLE, "width", self.width);
        kf.set_double(STYLE, "font-size", self.font_size);
        kf.set_string(STYLE, "font", &self.font);
        for (key, dir) in [("open", &self.open_folder), ("save", &self.save_folder)] {
            if let Some(dir) = dir {
                kf.set_string(FOLDERS, key, &dir.to_string_lossy());
            }
        }
        kf.set_string(SAVE, "format", self.save_format.key());
        kf.set_boolean(VIEW, "sidebar-visible", self.sidebar_visible);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string()))?;
        }
        kf.save_to_file(path)
    }
}

/// A colour stored as four numbers between 0 and 1, or `None` if it's
/// missing or malformed.
fn read_colour(kf: &glib::KeyFile, group: &str, key: &str) -> Option<Rgba> {
    let v = kf.double_list(group, key).ok()?;
    let ok = v.len() == 4 && v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x));
    ok.then(|| Rgba::new(v[0], v[1], v[2], v[3]))
}

/// A finite number clamped into `lo..=hi`, or `None` if missing or NaN/inf.
fn read_number(kf: &glib::KeyFile, group: &str, key: &str, (lo, hi): (f64, f64)) -> Option<f64> {
    let v = kf.double(group, key).ok()?;
    v.is_finite().then(|| v.clamp(lo, hi))
}

/// A remembered folder, if it still exists.
fn read_folder(kf: &glib::KeyFile, key: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(kf.string(FOLDERS, key).ok()?.as_str());
    (dir.is_absolute() && dir.is_dir()).then_some(dir)
}

/// Where the settings file lives (see the module docs).
pub fn path() -> PathBuf {
    glib::user_config_dir().join("pygoscribe").join("settings.ini")
}

// The in-memory copy, shared by every window. GTK is single-threaded, so a
// thread-local RefCell on the main thread is all the sharing needed.
thread_local! {
    /// `None` until the first [`get`].
    static CURRENT: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

/// The current settings, loaded from disk on first use.
pub fn get() -> Settings {
    CURRENT.with(|c| c.borrow_mut().get_or_insert_with(|| Settings::load_from(&path())).clone())
}

/// Changes the settings and writes them to disk. A write failure is only
/// logged: losing a remembered colour isn't worth interrupting the user.
pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut s = get();
    f(&mut s);
    if let Err(e) = s.save_to(&path()) {
        glib::g_warning!("pygoscribe", "Could not save settings: {}", e.message());
    }
    CURRENT.with(|c| *c.borrow_mut() = Some(s));
}

/// The folder holding `file`, as a real path the next file dialog can start
/// in. Inside the Flatpak, a file picked through the portal arrives as a
/// document-portal path (`/run/user/<uid>/doc/<id>/name`); the portal exposes
/// the real path as an extended attribute (xdg-desktop-portal 1.17+). If that
/// isn't available, the folder isn't remembered.
pub fn folder_of(file: &gio::File) -> Option<PathBuf> {
    let path = file.path()?;
    let real = if is_document_portal_path(&path) {
        let info = file
            .query_info("xattr::document-portal.host-path", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            .ok()?;
        let host = info.attribute_as_string("xattr::document-portal.host-path")?;
        PathBuf::from(unescape_xattr(&host))
    } else {
        path
    };
    let dir = real.parent()?.to_path_buf();
    dir.is_absolute().then_some(dir)
}

/// Whether `path` is under `/run/user/<uid>/doc/`, where the document portal
/// exposes files picked from inside a sandbox.
fn is_document_portal_path(path: &Path) -> bool {
    let mut parts = path.components().skip(1).map(|c| c.as_os_str().to_string_lossy());
    parts.next().as_deref() == Some("run")
        && parts.next().as_deref() == Some("user")
        && parts.next().is_some()
        && parts.next().as_deref() == Some("doc")
}

/// GIO escapes non-printable bytes in xattr strings as `\xNN`.
fn unescape_xattr(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'x')
            && let Some(hex) = s.get(i + 2..i + 4)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // The host path is NUL-terminated in some portal versions.
    while out.last() == Some(&0) {
        out.pop();
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder for one test, unique to this process.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pygoscribe-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No file at all is not an error.
    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        assert_eq!(Settings::load_from(&dir.join("nope.ini")), Settings::default());
    }

    /// A file that isn't a key file is ignored as a whole.
    #[test]
    fn corrupt_file_gives_defaults() {
        let dir = temp_dir("corrupt");
        let path = dir.join("settings.ini");
        std::fs::write(&path, "this is \0 not [a key file").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
    }

    /// Every field survives a save and load.
    #[test]
    fn round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("sub").join("settings.ini");
        let s = Settings {
            stroke: Rgba::new(0.1, 0.2, 0.3, 0.25),
            border_on: false,
            fill: Rgba::new(0.5, 0.25, 0.0, 0.5),
            fill_on: true,
            width: 7.0,
            font_size: 42.0,
            font: "DejaVu Serif Bold Italic".into(),
            open_folder: Some(dir.clone()),
            save_folder: Some(dir.clone()),
            save_format: SaveFormat::Jpeg,
            sidebar_visible: false,
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
    }

    /// Bad individual values fall back to defaults or are clamped.
    #[test]
    fn bad_values_fall_back_or_clamp() {
        let dir = temp_dir("bad");
        let path = dir.join("settings.ini");
        std::fs::write(
            &path,
            "[style]\nstroke=1;2;3\nfill=0.1;0.2;0.3;9\nwidth=1000\nfont-size=nan\nfont=  \n\
             [folders]\nopen=/does/not/exist\nsave=relative/path\n[save]\nformat=gif\n",
        )
        .unwrap();
        let s = Settings::load_from(&path);
        let d = Settings::default();
        assert_eq!(s.stroke, d.stroke);
        assert_eq!(s.fill, d.fill);
        assert_eq!(s.width, WIDTH_RANGE.1);
        assert_eq!(s.font_size, d.font_size);
        assert_eq!(s.font, d.font);
        assert_eq!(s.open_folder, None);
        assert_eq!(s.save_folder, None);
        assert_eq!(s.save_format, SaveFormat::Png);
    }

    /// A file with both border and fill off gets its border back.
    #[test]
    fn border_and_fill_not_both_off() {
        let dir = temp_dir("nofill");
        let path = dir.join("settings.ini");
        std::fs::write(&path, "[style]\nborder-on=false\nfill-on=false\n").unwrap();
        let s = Settings::load_from(&path);
        assert!(s.border_on);
        assert!(!s.fill_on);
    }

    /// Extensions are matched case-insensitively; unknown ones give `None`.
    #[test]
    fn format_from_extension() {
        assert_eq!(SaveFormat::from_path(Path::new("a.PNG")), Some(SaveFormat::Png));
        assert_eq!(SaveFormat::from_path(Path::new("a.jpeg")), Some(SaveFormat::Jpeg));
        assert_eq!(SaveFormat::from_path(Path::new("a.JPG")), Some(SaveFormat::Jpeg));
        assert_eq!(SaveFormat::from_path(Path::new("a.Pdf")), Some(SaveFormat::Pdf));
        assert_eq!(SaveFormat::from_path(Path::new("a")), None);
        assert_eq!(SaveFormat::from_path(Path::new("a.gif")), None);
    }

    /// Only known image/PDF extensions are replaced.
    #[test]
    fn swaps_extension() {
        assert_eq!(with_extension("Markup 1.png", SaveFormat::Jpeg), "Markup 1.jpg");
        assert_eq!(with_extension("a.JPEG", SaveFormat::Png), "a.png");
        assert_eq!(with_extension("a.jpg", SaveFormat::Jpeg), "a.jpg");
        assert_eq!(with_extension("notes.v2", SaveFormat::Png), "notes.v2.png");
        assert_eq!(with_extension("plain", SaveFormat::Jpeg), "plain.jpg");
        assert_eq!(with_extension("Markup 1.png", SaveFormat::Pdf), "Markup 1.pdf");
        assert_eq!(with_extension("scan.pdf", SaveFormat::Png), "scan.png");
        assert_eq!(with_extension(".png", SaveFormat::Jpeg), ".png.jpg");
    }

    /// An ordinary path's folder is its parent.
    #[test]
    fn folder_of_plain_file() {
        let f = gio::File::for_path("/home/someone/Pictures/shot.png");
        assert_eq!(folder_of(&f), Some(PathBuf::from("/home/someone/Pictures")));
    }

    /// Only `/run/user/<uid>/doc/...` counts as a document-portal path.
    #[test]
    fn document_portal_paths() {
        assert!(is_document_portal_path(Path::new("/run/user/1000/doc/abc123/x.png")));
        assert!(!is_document_portal_path(Path::new("/run/user/1000/x.png")));
        assert!(!is_document_portal_path(Path::new("/home/u/doc/x.png")));
    }

    /// `\xNN` escapes are decoded and trailing NULs dropped.
    #[test]
    fn unescapes_xattr() {
        assert_eq!(unescape_xattr("/home/u/My\\x20Pics/a.png\\x00"), "/home/u/My Pics/a.png");
        assert_eq!(unescape_xattr("/plain"), "/plain");
    }
}
