# Pygoscribe: project context

This file captures the decisions and history from the Claude project
conversations (10–11 Nov 2026), so work can resume on any machine without them.
The README covers usage and install steps; this file covers *why* things are
the way they are and what is still open.

## Goal

A small Rust + GTK4/libadwaita app for Ubuntu that marks up an image, taken
from the **clipboard** or a **selected image file**, with a familiar set of
**markup tools** (draw, highlight, shapes, arrows, bubbles, text, loupe,
crop). It lives in the **Ubuntu top bar**
(tray icon, top right). The result goes to the **clipboard**, a **file**, or
**"Send To"** (Open With… / Email…).

Requirements as Nick stated them (don't re-ask):

1. Started from nothing.
2. Rust and libadwaita.
3. Tools: a full markup toolbar (see Design decisions).
4. Output: clipboard, file, or send to.
5. Launch: Ubuntu top bar.

## Roadmap status

All four planned steps are done.

| Step | What | Status |
|---|---|---|
| 1 | Skeleton app, ksni tray icon, open clipboard/file in an Adwaita window | Done 10 Nov |
| 2 | Markup canvas: editable objects, undo/redo, all tools | Done 10 Nov |
| 3 | Output: Copy, Save PNG/JPEG, Send To (Open With…, Email…) | Done 10 Nov |
| 4 | Packaging (.deb + Flatpak), start at login, `install.sh` | Done 11 Nov |
| 0.1.1 | Remember settings; Save as PNG/JPEG fix; busy-pointer fixes | Released 12 Nov (`v0.1.1`) |
| 0.1.3 | Editable text marks (multi-line, Esc cancels, Enter/F2) | Released 12 Nov (`v0.1.3`) |
| 0.1.4 | Alt+drag rotates marks (Shift snaps to 15°) | Released 12 Nov (`v0.1.4`) |
| 0.1.5 | Transparent colours: opacity controls, Border on/off | Released 12 Nov (`v0.1.5`) |
| 0.1.6 | Text font choice (font and style), remembered | Released 12 Nov (`v0.1.6`) |
| 0.1.7 | Open PDF pages, Save as PDF | Released 12 Nov (`v0.1.7`) |
| 0.1.8 | Whole PDFs: page sidebar, per-page marks/undo, vector Save as PDF | Released 12 Nov (`v0.1.8`) |
| 0.1.9 | Fix: Start at Login and Quit no longer leave a busy pointer spinning | Released 12 Nov (`v0.1.9`) |
| 0.1.10 | Zoom: header controls, Ctrl+=/−/0/1, Ctrl+scroll, pinch, sharp PDF zoom | Released 13 Nov (`v0.1.10`) |
| 0.1.11 | Scale Image to Visible Size; Save (Replace Original); image size in header | Tagged `v0.1.11` 13 Nov (shipped in the 0.1.12 release) |
| 0.1.12 | Right-click context menus; Bring to Front / Send to Back; Space+drag and arrow-key panning | Released 13 Nov (`v0.1.12`, Gitea pre-release with .deb) |
| 0.1.13 | Text in rectangles, ovals, bubbles, lines and arrows, with alignment | Released 13 Nov (`v0.1.13`, Gitea pre-release with .deb and Flatpak) |
| 0.1.14 | Licensed GPL-3.0-only; About window (main menu, tray); third-party notices; source tarballs on each release | Released 14 Nov (`v0.1.14`, .deb, Flatpak, src and Poppler tarballs) |
| 0.1.15 | No-warranty disclaimer: README (GPL §15–16), About comments, metainfo, debian/copyright, source notices, `--version` | Released 14 Nov (`v0.1.15`) |

## Architecture

| File | Role |
|---|---|
| `src/main.rs` | `adw::Application`, CLI flags (`--clipboard`, `--no-tray`, `--background`, `--autostart=on\|off`, files). Single instance: a second launch forwards to the running one. |
| `src/tray.rs` | Top-bar icon via `ksni` 0.3 (blocking) StatusNotifierItem. Menu: Mark Up Clipboard, Open Image…, Start at Login (checkbox), About, Quit. |
| `src/about.rs` | `AdwAboutDialog`: version, GPL-3.0-only, legal sections for Poppler, GTK/GNOME libraries and the Rust crates. |
| `src/window.rs` | Main window: header bar, markup toolbar, style controls, Copy / Save / Send To. |
| `src/canvas.rs` | Canvas interaction: tools, selection, handles, text editing, crop. |
| `src/view.rs` | Zoom and scroll arithmetic for the canvas (steps, fit, clamping, scrollbars), GTK-free and unit-tested. |
| `src/document.rs` | Shape model and undo/redo history. |
| `src/render.rs` | Cairo drawing, shared by the on-screen canvas and full-resolution export. |
| `src/pdf.rs` | PDFs via poppler: detect, page sizes, render a page, draw it as vectors, save every page with marks (`save_marked`). |
| `src/pages.rs` | What is open: one picture or every PDF page; each page's marks and undo history; thumbnails. |
| `src/sidebar.rs` | Page sidebar: `AdwOverlaySplitView` with a `GtkListBox` of thumbnails, drawn in idle time. |
| `src/output.rs` | Export: clipboard, save PNG/JPEG/PDF, Open With (FileLauncher / OpenURI portal), Email. |
| `src/settings.rs` | Remembered settings: style, open/save folders, save format, in a GLib key file. |
| `src/autostart.rs` | Start at login: `~/.config/autostart/<app id>.desktop` natively, Background portal in Flatpak. |
| `src/migrate.rs` | One-time carry-over of settings and Start at Login from releases before 0.1.16 (old IDs are in its module comment). |
| `build.rs` + `data/icons/resources.gresource.xml` | Bundles the `markup-*-symbolic` tool icons as a GResource. |
| `data/` | `.desktop`, AppStream `metainfo.xml`, app icon. |
| `build-aux/flatpak/org.outram.Pygoscribe.json` | Flatpak manifest. |
| `install.sh` | Tests, release build, `.deb` and `.flatpak` into `dist/`, optional `--install deb\|flatpak`. |

Versions: gtk4 0.11 (feature `v4_14`), libadwaita 0.9 (`v1_5`), ksni 0.3,
poppler-rs 0.26, cairo-rs 0.22 (feature `pdf`), edition 2024. Flatpak uses GNOME runtime 50 and rust-stable extension 25.08.

## Design decisions (and why)

- **Tools.** Select (V), Draw (P), Highlight (H),
  Line (L), Arrow (A), Rectangle (R), Oval (O), Speech Bubble (B), Text (T),
  Loupe (M), Crop (C). Every mark stays an editable object (move, resize,
  restyle, delete). Crop is non-destructive. Undo/redo covers everything.
- **Tray via ksni (StatusNotifierItem).** Ubuntu's built-in "Ubuntu
  AppIndicators" extension shows it. Plain GNOME needs the AppIndicator
  extension. At login, `--background` waits for the tray watcher to appear
  instead of opening a window.
- **`pygoscribe --clipboard`** reaches the already-running instance, so it can
  be bound to a keyboard shortcut.
- **Name: Pygoscribe** (Nick's choice, 14 Nov 2026, from 0.1.16). From
  *Pygoscelis*, the Adélie penguin genus, plus "scribe". Command and package
  `pygoscribe`, config folder `~/.config/pygoscribe/`, log domain
  `pygoscribe`. Documentation never mentions the earlier name (Nick's rule).
- **App ID is `org.outram.Pygoscribe`.** Use `org.outram.*` for any new IDs.
- **Migration from releases before 0.1.16** (`src/migrate.rs`; the old IDs
  are in its module comment) runs once on first start, then writes a stamp
  in the config folder. It copies the old `settings.ini` and, outside
  Flatpak, replaces the old autostart entry with the new one. The Flatpak
  gets read-only access to the old app's config folder for this, and asks
  the Background portal for Start at Login if the old app had it on. The .deb
  `Conflicts`/`Replaces`/`Breaks` the old package, so apt removes it. The
  desktop file has `X-Flatpak-RenamedFrom` and the metainfo `<replaces>`.
  Some user-facing strings keep the word "Markup" on purpose: the
  `Markup-Sent` folder, default file names ("Markup <date>.png") and "Mark Up
  Clipboard".
- **Shared files go in `~/Pictures/Markup-Sent`** (copies older than a week are
  deleted). It is not hidden, because snap apps (e.g. Thunderbird snap) can't
  read hidden folders like `~/.cache`. The Flatpak gets
  `--filesystem=xdg-pictures/Markup-Sent:create` for the same reason.
- **Email… and Proton Mail.** Nick's default mail app is **Proton Mail
  (snap)**, which can't accept attachments from other apps. Email… detects
  this, opens a blank compose window, puts the image on the clipboard and shows
  a toast telling you to paste. Thunderbird/Evolution/KMail/Geary get a real
  attachment via `xdg-email`, with the Email portal as fallback (and the only
  route inside Flatpak).
- **Packaging: .deb is recommended for Ubuntu, Flatpak is the other supported
  format.** The .deb uses system GTK and gives mail apps the attachment
  directly. The Flatpak manifest downloads crates during the build
  (`--share=network`), fine locally; publishing on Flathub would need
  `flatpak-cargo-generator`.
- **Flatpak verified on Nick's laptop (11 Nov 2026):** tray icon, Mark Up
  Clipboard, Copy / Save / Open With… / Email… all work. Start at Login was
  reported working but its autostart file wasn't found afterwards (see Open
  items). The Flatpak with the original app ID and its autostart file are
  gone.
- **Versions and tags.** `v0.1.0` (tag on d301061) is the first baseline
  pre-release. 0.1.1 adds remembered settings. Bump `Cargo.toml` and add a
  `<release>` to the metainfo for each point release.
- **Settings are a key file, not GSettings** (0.1.1). `settings.ini` under
  `glib::user_config_dir()/pygoscribe/`, read and written with `glib::KeyFile`.
  GSettings would need a compiled schema installed, which breaks plain
  `cargo run` and adds packaging steps; the key file needs nothing, and in the
  Flatpak it lands in the app's private config folder without extra
  permissions. Bad or missing values fall back to defaults. Style is saved
  when the user changes a control (not when selecting a mark shows its style).
- **Remembered folders under Flatpak.** Portal file choosers return
  `/run/user/<uid>/doc/<id>/…` paths. `settings::folder_of` reads the real
  path from the `user.document-portal.host-path` xattr (xdg-desktop-portal
  1.17+); without it the folder isn't remembered.
- **Save format.** `GtkFileDialog` doesn't report which file type filter was
  picked, so 0.1.0 saved PNGs even with JPEG chosen. Save is now a split
  button: the main part uses the last format, the menu offers PNG or JPEG, and
  a typed extension still wins. Nick also wants to pick the type inside the
  dialog and see the extension change, which `GtkFileDialog` can't do (it
  neither renames nor reports the type). Since 0.1.2 the .deb uses the
  deprecated `GtkFileChooserDialog` and renames on `notify::filter`
  (`settings::with_extension`). The Flatpak keeps the portal `GtkFileDialog`
  (needed for sandboxed file access) listing only the chosen format.
- **App launcher opens a window** (0.1.1). The `.desktop` file has
  `StartupNotify=true`, and plain `pygoscribe` only adds the tray icon, so
  launching from the app grid left the busy pointer spinning until GNOME timed
  it out (on Wayland the activation token is only consumed by a presented
  window). The launcher now runs `pygoscribe --window`; plain `pygoscribe` from a
  terminal is unchanged.
- **Windows opened from the top bar use the click's activation token**
  (0.1.1). GNOME's AppIndicator extension sends an XDG activation token with
  each click (`ProvideXdgActivationToken`), which starts a launch with a busy
  pointer. The tray reads it with `ksni::current_xdg_activation_token()` and
  `window::set_activation_token` hands it to the next window presented
  (`gtk_window_set_startup_id`), which ends the busy pointer and lets the
  window take focus. Open Image… from the top bar therefore opens an editor
  window first, with the file chooser on top (cancelling closes it), since a
  `GtkFileDialog` can't take a token itself. Quit and Start at Login get a
  token too but open nothing, so `window::release_activation_token` ends the
  launch without a window (0.1.9): `src/launch.rs` opens a short-lived Wayland
  connection and passes the token to `gtk_shell1.set_startup_id`, which makes
  mutter complete the startup sequence. (GTK itself only uses that request
  when xdg_activation is missing.) The earlier approach, a 1×1 transparent
  window shown with the token, never got activated on GNOME 50, so the pointer
  spun until the shell timed it out; it stays only as the fallback for X11 or
  compositors without gtk_shell1. The protocol XML is vendored in
  `data/protocol/gtk-shell.xml` (LGPL, from GTK); `wayland-client` and
  `wayland-scanner` are pure Rust, so the Flatpak needs no new system libs.
  Debug with
  `G_MESSAGES_DEBUG=pygoscribe pygoscribe`.
- **Editing text marks** (0.1.3, Nick's choices). The editor stays a popover
  under the mark rather than typing onto the image. Enter finishes,
  Shift+Enter adds a line, Esc restores the old text, clicking away keeps it;
  the whole edit is one undo step. With the Text tool, the first click on
  existing text selects it and the second edits it. The editor is a
  multi-line `GtkTextView` inside a **fixed-size** `GtkScrolledWindow`: GTK
  pops a popover down the moment its minimum size grows past the popup's
  current size (`gtk_popover_native_layout` → `is_acceptable_size`), and a bare
  TextView's size changes after it opens, so the box vanished ~15 ms after
  opening. The popover is not autohide (it stays open while restyling) and
  opens on button release, so the app finishes the edit itself: clicking the
  canvas, Esc/Enter, undo, redo, tool changes, a new image and Copy/Save all
  finish it first, so the editor never writes into the wrong mark. Debug with
  `G_MESSAGES_DEBUG=pygoscribe`. Broadway (`gtk4-broadwayd`) can show the
  window headlessly, but canvas clicks and typing don't get through from the
  browser pane, so it only helps for code-triggered checks.
- **Rotating marks** (0.1.4). Alt+drag rotates the mark under the pointer,
  or the selected mark if the drag starts elsewhere, with any tool;
  Shift+Alt snaps to 15°. Rectangles, ovals, bubbles and text keep a
  `Shape::angle` (radians, about the centre of `bounds()`); hit testing,
  handles and resizing work in the mark's own frame, so the opposite corner
  stays put on screen. Lines, arrows and freehand marks are rotated by moving
  their points (angle stays 0). The loupe doesn't rotate. Rotated text keeps
  its top-left corner fixed when its size changes. GNOME's window-move
  modifier (`org.gnome.desktop.wm.preferences mouse-button-modifier`) is
  `<Super>` by default, so Alt+drag reaches the app; if someone sets it to
  `<Alt>`, GNOME swallows Alt+drag, so Super (and Meta) also rotate.
- **Transparent colours** (0.1.5, Nick chose "both": opacity *and* a
  none option). Border and fill each get a `ColourControl` in `window.rs`:
  the colour button plus a "NN%" menu button with a slider and presets; it
  sets the colour's alpha, and the colour dialog's own alpha slider stays in
  step. A Border toggle (`Style::border`, `border-on` in settings) turns off
  the outline of rectangles, ovals and bubbles only (`Kind::has_optional_border`);
  it's greyed out for other tools/marks. Border and fill can't both be off.
  Rendering: arrows paint shaft + head in a Cairo group with the alpha applied
  once (no dark overlap); the bubble is one outline path with the tail cut
  in (no seam cover), and fill off now means see-through instead of white;
  the highlighter uses 0.4 × the colour's alpha. Restyles of the same mark
  within 1 s merge into one undo step, so dragging the slider is one undo.
  Plan: `docs/plans/transparent-colours.md`.
- **Text font** (0.1.6). `Style::font` holds a Pango font description
  without a size ("Sans", "DejaVu Serif Bold Italic"); `font=` in
  settings.ini. The toolbar has a `GtkFontDialogButton` (level Face, so
  family plus style, no size; the spin button keeps the size) whose label is
  ellipsized so long names don't widen the window; the tooltip shows the full
  name. Text is now drawn and measured with **pangocairo** (new dependency)
  instead of cairo's toy font API, which only knew Normal/Bold and
  Normal/Italic. The font uses an absolute size in image pixels and hinting is
  off, so text keeps its proportions at any zoom and in the export. Line
  spacing (1.25 × size) and padding are unchanged, so old marks keep their
  box size apart from small width differences between the two text engines.
  Speech bubbles hold no text, so the font only affects Text marks. The
  font chooser is a dialog, not a popover, so it doesn't hit the
  popover-closing problem.
- **PDF support** (0.1.7). PDFs open
  wherever images do (Open Image… filter, command line, `.desktop` MimeType
  `application/pdf`; there is no drag and drop). `poppler-rs` 0.26 (it uses
  the same glib/cairo 0.22 stack) renders the page with Cairo at 2 px per
  point (144 dpi), capped at 8192 px on the longest side, on white. A PDF with
  several pages asks for the page in an `AdwAlertDialog` with a spin button;
  the title shows "name.pdf (page N)". Detection is by `.pdf` extension or a
  `%PDF-` header. Encrypted PDFs get a toast ("password protected").
  **Save as PDF** writes one page with `cairo::PdfSurface`, painting the
  exported image (raster, not vector). `MarkupWindow::points_per_pixel` is
  0.5 for a page rendered from a PDF and 1 otherwise, so a marked-up PDF page
  keeps its original page size; pictures are 1 px = 1 pt. Vector output
  (re-rendering the poppler page plus marks into the PDF) wasn't done:
  crop and the canvas background would need the poppler page kept around.
  Build needs `libpoppler-glib-dev`; the .deb depends on
  `libpoppler-glib8t64` (via `$auto`). The GNOME Flatpak runtime has no
  poppler, so the manifest builds poppler 26.11.0 (glib binding only, no
  OpenJPEG/NSS/GPGME/curl/Qt) as a module first.
- **Whole PDFs and the page sidebar** (0.1.8, plan in
  `docs/plans/multipage-pdf.md`; Nick took all the defaults). Opening a PDF
  opens every page (the 0.1.7 page dialog is gone). `Pages` holds the
  source and one `PageState` (document + undo `History`) per page; the
  current page's state lives in the canvas and `Canvas::swap_page` trades it
  when you change page, so **undo is per page**. Only the current and last
  rendered page images are cached (a 144-dpi Letter page is ~7.7 MB).
  The sidebar is an `AdwOverlaySplitView` (start side) with thumbnails
  120 px wide; F9 or the header button toggles it, remembered as
  `[view] sidebar-visible`. Page Up/Down, Home/End (canvas focus) and
  Alt+Up/Down change page. Thumbnails are drawn a few per idle tick and
  redrawn 300 ms after the page's history changes; marked pages get a dot.
  No collapse breakpoint: the toolbar already makes the window ~1300 px
  minimum, and an `AdwBreakpoint` made libadwaita clip the window instead.
  **Save as PDF with a PDF open writes every page as vectors**: poppler
  draws each page straight into a `cairo::PdfSurface` and the marks go on
  top (`render::draw_page` takes a background painter, which the loupe
  reuses), so text stays selectable and files stay small. A cropped page
  gets the crop's size. Links, form fields and bookmarks are lost
  (poppler/cairo can't copy PDF structure; keeping them would need
  something like `lopdf`). PNG/JPEG/Copy/Send use the current page; names
  default to `name-marked.pdf` / `name-page3.png`.
- **Zoom** (0.1.10). The canvas keeps a view transform
  (`screen = image * scale + offset`) used for drawing and all hit-testing,
  so marks stay in image pixels and export never sees the zoom. The
  `DrawingArea` stays window-sized (no huge `ScrolledWindow` child), with
  `GtkScrollbar`s laid over it in a `GtkOverlay`. Zoom to Fit is a mode that
  follows the window and never enlarges past 1:1 image pixels. 100 % means
  1 image pixel per screen pixel for pictures and printed size (1 pt per
  screen px) for PDFs, via `Canvas::set_page_view(unit, painter)`. When the
  144-dpi raster would be enlarged on screen, the part of the PDF page in
  view (+10 %) is re-rendered by poppler at the current zoom, 120 ms after
  the view settles, and drawn over the raster (the loupe uses it too).
  Pictures are just enlarged. A display-backed test,
  `cargo test zoomed_view -- --ignored` (run under `gtk4-broadwayd` with
  `GDK_BACKEND=broadway`), writes snapshots to the temp folder.
- **Scale Image to Visible Size** (0.1.11, a header button after the zoom
  controls with a custom `markup-scale-symbolic` icon, and in the zoom menu;
  pictures only). Resamples the image to its size on screen at the current zoom
  (logical pixels, so 100 % = no change; cairo `Filter::Best`, max side
  16384 px) and scales marks, line widths, text size and crop with it
  (`Document::scale`). The zoom becomes 100 %, so nothing moves on screen.
  The new image lives in `Document::image` (a `Picture`, compared by
  identity) rather than replacing the page's source, so undo/redo restore
  the old image with the old marks in one step; undo/redo also rescale the
  view so the page keeps its on-screen size. PDFs are excluded: their pages
  are re-rendered from vectors, so resampling means nothing there.
- **Image size in the header** (0.1.11, Nick's request). The header's
  title is an `AdwWindowTitle` whose subtitle is the result's size: pixels
  for pictures, points for PDF pages, using the crop if any, "(cropped)"
  appended. Refreshed on every history change and page change.
- **Save (Replace Original)** (0.1.11, a `document-save` header button;
  the Save As split button now uses `document-save-as`). Writes
  over the opened file in its own format after an `AdwAlertDialog` (Cancel
  default, Replace destructive). `output::Original` records the file and its
  format, detected from the contents with `gdk_pixbuf::Pixbuf::file_info`:
  PNG/JPEG use Save As's writers, other writable gdk-pixbuf formats (TIFF,
  BMP, ICO, WebP if its loader is installed) use `save_pixbuf`, PDFs use the
  vector `save_marked`. The result is written to a temp file, then swapped in
  with `g_file_replace_contents` (temp file + rename next to the original),
  so a failure leaves the original intact and poppler keeps reading the old
  PDF. Disabled for clipboard images and unwritable formats (GIF, SVG).
  EXIF and other metadata are not kept. Ctrl+S stays Save As.
- **Context menus** (0.1.12, Nick asked after checking they're GNOME-HIG
  conformant). Right-click, touch long-press, Shift+F10 or Menu on the
  canvas. A mark under the pointer is selected and gets Edit Text / Bring to
  Front / Send to Back / Delete; elsewhere the image menu (Copy, Mark Up
  Clipboard, Save, Save As, Scale, Actual Size, Zoom to Fit). Per the HIG
  every item also exists elsewhere: Bring to Front / Send to Back got
  Ctrl+Shift+Up / Down (`Document::restack`, one undo step). The
  `GtkPopoverMenu` is built per use, parented to the drawing area, and
  unparented in idle after closing, like the text editor. Display-backed
  tests (`context_menu_targets`, `scale_to_visible_size`) must be run one at
  a time: each calls `gtk::init` on its own test thread.
- **Panning without a middle button** (0.1.12, Nick's request). Holding
  Space over the focused canvas shows a grab cursor and turns a primary drag
  into a pan (`Drag::Pan`), whatever the tool; letting go of Space (or the
  canvas losing focus) restores the tool's cursor. Arrow keys scroll 40 px
  (Shift: 160 px) when zoomed in, and pass through when the page fits.
  Test: `space_drag_pans`.
- **Text in shapes and on lines** (0.1.13, Nick's request). Rectangles,
  ovals, bubbles, lines and arrows hold text in `Shape::label`; a Text mark
  still keeps its text in `Kind::Text`, and `Shape::text`/`set_text` hide
  the difference so the same popover editor serves both (double-click,
  Enter/F2, or Edit/Add Text in the context menu; the Text tool still
  creates a separate text mark when clicking a shape). `Shape::halign` /
  `valign` place it: in a box, inside the border (an oval's inscribed box,
  a bubble's body), default centre/centre; on a line, `Top` = above
  (default) and `Bottom` = below, left/centre/right as read. Line text is
  drawn in a frame along the line, turned by 180° when the line runs right
  to left (and reading bottom to top when upright) so it is never upside
  down, and kept off an arrow's head. For a Text mark only `halign` counts
  (lines of a multi-line mark line up left/centre/right). Text uses the
  stroke colour, font and size, is not clipped to the shape, and rotates
  with the shape because it is drawn in the shape's rotated frame. Clicking
  a line's text selects the line (`render::hits_line_label`); the selection
  shows its outline faintly. Alignment: buttons in the editor and a Text
  Alignment submenu (stateful `win.mark-halign` / `win.mark-valign`), one
  undo step outside the editor, part of the edit's step inside it. Nick
  asked for "tight justified"; read as right-aligned, not full
  justification. Visual check: `cargo test label_sheet -- --ignored`
  writes `pygoscribe-label-sheet.png` to the temp folder. Double-clicks
  are spotted in `drag_begin` (time and distance from the last press, by
  GTK's double-click settings), not by the `GestureClick`: the first
  version did it in the click handler and double-clicking a shape didn't
  work for Nick, since the two gestures' order isn't reliable and the
  first press of the pair has already started a new mark with drawing or
  Text tools. Test: `double_click_edits_shape_text` (display-backed, run
  alone). Fixed on the way:
  drawn text left a current point, so a following oval's path started with
  a stray line.
- **License: GPL-3.0-only** (Nick's decision, 14 Nov 2026; plan in
  `docs/plans/gpl3-licensing.md`). Copyright "2026 Nicholas Outram". GPL
  because the app links Poppler (GPL-2.0-or-later, xpdf parts GPL-2/3), which
  a non-GPL app couldn't; "only" rather than "or later" was Nick's choice.
  Every crate is MIT, Apache-2.0, Unlicense or Unicode-3.0, all compatible.
  Releases 0.1.0–0.1.13 said `LicenseRef-proprietary` in their metainfo; as
  sole copyright holder Nick simply relicensed, nothing was re-released.
  The pieces:
  - `LICENSE` (verbatim GPL-3), `LICENSES/` (REUSE layout), SPDX headers on
    `src/*.rs`, `build.rs` and the scripts, `REUSE.toml` for the rest
    (`gtk-shell.xml` LGPL-2.1-or-later, The GTK Project Authors; metainfo
    CC0-1.0, as Flathub wants). `Cargo.toml` has `license`, `authors`,
    `repository` and `publish = false`.
  - `install.sh` runs `cargo deny check licenses` (allow-list in
    `deny.toml`), `reuse lint` and `cargo about generate` (→
    `THIRD-PARTY-LICENSES.md`, template `about.hbs` with a hand-written
    Poppler section) before building, installing cargo-deny/cargo-about with
    `cargo install --locked` and reuse with uv/pipx if missing. In an
    unpacked source tarball (vendor/, no .git) it skips `reuse lint`.
  - The .deb ships a DEP-5 `debian/copyright` (as
    `/usr/share/doc/pygoscribe/copyright`, replacing cargo-deb's generated
    one) and `THIRD-PARTY-LICENSES.md`. The Flatpak installs `LICENSE` and
    `THIRD-PARTY-LICENSES.md` to `/app/share/licenses/org.outram.Pygoscribe/`.
  - About window: main menu button (`open-menu-symbolic`, rightmost in the
    header) → About Markup, and About in the tray menu, which presents the
    dialog on its own and hands the top bar's activation token to its window.
  - **A binary never ships without its source next to it.** GPL-3 §6(d):
    `scripts/release.sh` also uploads `pygoscribe-<v>-src.tar.xz` (`git
    archive` of the tag + `cargo vendor` in `vendor/` + `.cargo/config.toml`,
    so `cargo build --offline --locked` works from it) and the Poppler
    tarball named in the Flatpak manifest (sha256-checked), and appends a
    "Source: … License GPL-3.0-only." line to the release notes. Since 0.1.16
    releases go to both GitHub and Gitea on `pi5` (LAN-only, the only host up to 0.1.15);
    anything handed out another way must take the source tarballs along.
  - **No warranty** (0.1.15, Nick: "This software is free and provided as
    is"). Uses the GPL's own wording rather than a custom waiver, which
    could conflict with the license: README "Disclaimer" quotes GPL-3 §15
    and §16 verbatim; the About window's comments line, the metainfo
    description, a DEP-5 `Disclaimer:` field in `debian/copyright`, the GPL
    short notice under every source file's SPDX header, and `pygoscribe
    --version` (`-v`, prints version, copyright, license, no-warranty line;
    handled locally, so it never reaches a running instance) all say it.
  - Not done (optional step 17): offline Flatpak cargo sources via
    `flatpak-cargo-generator`; the manifest still builds with
    `--share=network`.
- **Snap was tried and abandoned** (Nick, 11 Nov: "stick with flatpak").
  `snapcraft --destructive-mode` refuses because Nick's laptop has Ubuntu Pro
  attached, and the LXD build kept failing on the Rust plugin's rustup check.
  The `SNAP` / `SNAP_REAL_HOME` handling left in the code is harmless. Don't
  offer a snap again.

## Build and install

README.md, "Building from a clean Ubuntu install", has the full steps
(apt packages, rustup, cargo-deb, Flatpak SDK), checked in clean `ubuntu:26.04`
and `ubuntu:24.04` containers in Nov 2026. In short:

```sh
sudo apt install build-essential pkg-config curl git libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev flatpak flatpak-builder
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust 1.88+ (let chains)
cargo run --release                  # run from source

./install.sh                         # tests + dist/*.deb + dist/pygoscribe.flatpak
./install.sh --install deb           # ...and install the .deb
./install.sh --install flatpak       # ...and install the Flatpak (user)
```

If the Flatpak SDK prompt asks system vs user, choose **user**
(`flatpak install --user …`), because the build uses `--user`.

Then turn on **Start at Login** in the tray menu (or `pygoscribe --autostart=on`).
Re-tick it after switching between the .deb and the Flatpak.

## Releasing

**Every release ships both a .deb and a Flatpak bundle on its GitHub and Gitea releases**
(Nick's standing rule, 13 Nov 2026), **and since 0.1.14 their source**
(`pygoscribe-<ver>-src.tar.xz` and the Poppler tarball; GPL-3.0). After bumping `Cargo.toml`, adding the
metainfo `<release>`, committing, tagging and pushing the tag:

```sh
scripts/release.sh v0.1.14
```

It exports the tag to a temporary folder, runs `install.sh` there (tests,
.deb, Flatpak), copies `pygoscribe_<ver>-1_amd64.deb` and
`pygoscribe-<ver>.flatpak` to `dist/release/`, and uploads both to the tag's
release on both https://github.com/noutram/pygoscribe/releases and Gitea on
pi5 (Nick, 14 Nov 2026), creating a pre-release if there is none and replacing
assets of the same name, together with the source tarball and the Poppler
tarball. Tokens come from `$PYGOSCRIBE` or `$GITHUB_TOKEN` (a fine-grained token with Contents
read and write on the repo) and `$GITEA`, both in `~/.profile`; a missing one
skips that host and the script exits non-zero, so rerun it once it is set.
Push `master` and the tag to both remotes first (`origin` = GitHub `main`,
`pi5` = Gitea `master`).
`--no-upload` only builds. Smoke-test the Flatpak with
`flatpak install --user --reinstall dist/release/pygoscribe-<ver>.flatpak`.

## Code comments

Since 0.1.18 every source file has a `//!` module comment, every item a `///`
doc comment, and the non-obvious logic inline comments (the borrow pattern
in `canvas.rs`, the undo model in `document.rs`, Wayland activation tokens
in `window.rs` and `launch.rs`, raster vs vector PDF rendering in `pdf.rs`).
`cargo doc --document-private-items` builds without warnings; keep it that
way, and comment new code to the same standard.

## Testing tips

- Poppler 24.02 (Ubuntu 24.04) sets up its global state on the first open
  without locking, so PDFs opened on several threads at once corrupted the
  heap ("double free or corruption") and `install.sh` failed there now and
  then. Since 0.1.19 `Pdf::open` takes a process-wide lock. The ignored
  stress test `cargo test --release concurrent_opens -- --ignored` checks it
  (it crashed about 1 run in 6 on 24.04 without the lock).

- Headless testing works with `dbus-run-session` + `xvfb-run` and a fake
  `org.kde.StatusNotifierWatcher` written in Python with `gi`.
- If the app seems unchanged after a rebuild, an old instance is still running
  in the tray and is receiving the request: `pkill pygoscribe` first.

## Open items

1. **Repo:** committed on `master` and pushed to `origin`
   (https://github.com/noutram/pygoscribe, branch `main`, since 0.1.16) and
   to `pi5` (a self-hosted Gitea/Forgejo on Nick's pi5, branch `master`;
   URL in `git remote -v`). Clone from GitHub on
   another machine.
2. **Start at Login under Flatpak not confirmed on disk.** Nick reported the
   Flatpak checks working (11 Nov), but on checking afterwards there was no
   `~/.config/autostart/org.outram.Pygoscribe.desktop` (the Background portal
   should write it). Run `flatpak run org.outram.Pygoscribe --autostart=on`,
   then confirm the file exists and survives a log-out/in.
3. **`install.sh` Flatpak path untested end to end.** The .deb path was tested;
   the Flatpak path couldn't reach Flathub from the build container. It should
   work on the laptop, where the SDK is already installed.
4. **0.1.1 released (12 Nov 2026)**, tag `v0.1.1` on `master`, verified on the
   laptop with the .deb (remembered settings, Save as JPEG, no busy pointer
   from the app grid or top bar). Remembered folders in the Flatpak are still
   unverified.
5. Optional: delete the stray `~/Pictures/Markup-Sent/test.png` from testing.

## Ideas not started

- Flathub submission (needs `flatpak-cargo-generator` sources and screenshots;
  the license side is ready). With generated cargo sources,
  `flatpak-builder --bundle-sources` could replace the two source tarballs.
- A GitHub Actions workflow running `install.sh --skip-flatpak`.
