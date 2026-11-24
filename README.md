# Pygoscribe

A GTK4 / libadwaita app for marking up images. It lives in the Ubuntu top bar and opens an image from the
clipboard, from a file, or a whole PDF.

The name comes from *Pygoscelis*, the genus of the Adélie penguin, plus
"scribe".

## Disclaimer

Pygoscribe is free software, provided **"as is", WITHOUT ANY WARRANTY**, and
to the extent the law allows, the author accepts no liability for anything
that happens from using it. Use it at your own risk. This is the disclaimer of the GNU General Public License,
version 3, under which Pygoscribe is licensed (see [License](#license)).
Sections 15 and 16 of the license say:

> **15. Disclaimer of Warranty.**
>
> THERE IS NO WARRANTY FOR THE PROGRAM, TO THE EXTENT PERMITTED BY
> APPLICABLE LAW.  EXCEPT WHEN OTHERWISE STATED IN WRITING THE COPYRIGHT
> HOLDERS AND/OR OTHER PARTIES PROVIDE THE PROGRAM "AS IS" WITHOUT WARRANTY
> OF ANY KIND, EITHER EXPRESSED OR IMPLIED, INCLUDING, BUT NOT LIMITED TO,
> THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
> PURPOSE.  THE ENTIRE RISK AS TO THE QUALITY AND PERFORMANCE OF THE PROGRAM
> IS WITH YOU.  SHOULD THE PROGRAM PROVE DEFECTIVE, YOU ASSUME THE COST OF
> ALL NECESSARY SERVICING, REPAIR OR CORRECTION.

> **16. Limitation of Liability.**
>
> IN NO EVENT UNLESS REQUIRED BY APPLICABLE LAW OR AGREED TO IN WRITING
> WILL ANY COPYRIGHT HOLDER, OR ANY OTHER PARTY WHO MODIFIES AND/OR CONVEYS
> THE PROGRAM AS PERMITTED ABOVE, BE LIABLE TO YOU FOR DAMAGES, INCLUDING ANY
> GENERAL, SPECIAL, INCIDENTAL OR CONSEQUENTIAL DAMAGES ARISING OUT OF THE
> USE OR INABILITY TO USE THE PROGRAM (INCLUDING BUT NOT LIMITED TO LOSS OF
> DATA OR DATA BEING RENDERED INACCURATE OR LOSSES SUSTAINED BY YOU OR THIRD
> PARTIES OR A FAILURE OF THE PROGRAM TO OPERATE WITH ANY OTHER PROGRAMS),
> EVEN IF SUCH HOLDER OR OTHER PARTY HAS BEEN ADVISED OF THE POSSIBILITY OF
> SUCH DAMAGES.

Section 17 of the license adds that where these can't take full legal effect,
courts apply the local law that comes closest to an absolute waiver of all
civil liability.

## Install

Ready-made packages (`.deb` and `.flatpak`) are attached to each release on
the project's [GitHub releases page](https://github.com/noutram/pygoscribe/releases), together with the complete source for them (see
[License](#license)). Install the .deb with `sudo apt install
./pygoscribe_<version>-1_amd64.deb`, or the Flatpak with `flatpak install
--user pygoscribe.flatpak`.

The .deb is the better fit for Ubuntu: the app uses the system GTK and
libadwaita, mail apps get the attachment directly, and nothing extra needs
installing.

## Building from a clean Ubuntu install

These steps start from a fresh Ubuntu 26.04 LTS and end with the app
running, a `.deb` and a `.flatpak`. They were checked in a clean
`ubuntu:26.04` container. Ubuntu 24.04 LTS also works, because it has
GTK 4.14 and libadwaita 1.5, the oldest versions Pygoscribe supports.

### 1. Build tools and libraries

```sh
sudo apt update
sudo apt install build-essential pkg-config curl git \
    libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev
```

- `build-essential` and `pkg-config` provide the C compiler and linker
  that Rust uses, and let the Rust crates find the system libraries.
- `libgtk-4-dev` brings in GTK 4, GLib (including `glib-compile-resources`,
  which bundles the app's icons), Cairo, Pango and gdk-pixbuf.
- `libadwaita-1-dev` is libadwaita, and `libpoppler-glib-dev` is Poppler,
  which opens and renders PDFs.
- `curl` fetches the Rust installer and `git` fetches the source.

Everything else (the Wayland client, D-Bus for the top-bar icon) is pure
Rust and comes from crates.io.

### 2. Rust

Pygoscribe uses the Rust 2024 edition and needs a recent stable compiler
(1.88 or later). Ubuntu's `rustc` package may be older, so install Rust with
[rustup](https://rustup.rs):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"     # or open a new terminal
```

Keep it current later with `rustup update`.

### 3. Get the source and build

```sh
git clone https://github.com/noutram/pygoscribe.git
cd pygoscribe
cargo build --release --locked
cargo test --release --locked
./target/release/pygoscribe --window
```

`cargo run --release` builds and runs in one step. If Pygoscribe is already
running in the top bar, quit it first (`pkill pygoscribe`): otherwise the
new build hands the request to the running copy.

Instead of `git clone`, you can unpack `pygoscribe-<version>-src.tar.xz`
from a release page. It includes every crate in `vendor/`, so it builds
without a network connection: `cargo build --release --offline --locked`.

### 4. The .deb

```sh
cargo install --locked cargo-deb        # once
cargo deb --locked
sudo apt install ./target/debian/pygoscribe_*_amd64.deb
```

This installs `pygoscribe` in `/usr/bin` with its app icon, menu entry and
AppStream data. Remove it with `sudo apt remove pygoscribe`.

### 5. The Flatpak

The Flatpak is built against the GNOME 50 runtime from Flathub, with Rust
from Flathub's rust-stable SDK extension. It also builds its own copy of
Poppler, which the GNOME runtime doesn't include.

```sh
sudo apt install flatpak flatpak-builder
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.gnome.Platform//50 org.gnome.Sdk//50 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build-dir \
    build-aux/flatpak/org.outram.Pygoscribe.json
flatpak run org.outram.Pygoscribe
```

To make a single `pygoscribe.flatpak` file you can install on another machine:

```sh
flatpak-builder --repo=repo --force-clean build-dir build-aux/flatpak/org.outram.Pygoscribe.json
flatpak build-bundle repo pygoscribe.flatpak org.outram.Pygoscribe \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user pygoscribe.flatpak
```

The manifest downloads crates while building, which is fine for a local
build. (Flathub would need the crates listed in advance with
`flatpak-cargo-generator`.) In the sandbox the app can only write to
`~/Pictures/Markup-Sent`, and it reaches everything else through portals:
Save uses the file chooser, Open With… uses the Open URI portal and Email…
uses the Email portal, which attaches the image in any mail app the desktop
supports. The top-bar icon talks to the AppIndicator extension directly.

### Everything with one script

Once steps 1 and 2 are done, `install.sh` runs the tests and builds both
packages into `dist/`:

```sh
sudo apt install pipx flatpak flatpak-builder   # pipx for reuse, below
pipx install 'reuse[charset-normalizer]'
./install.sh                    # tests, release build, dist/*.deb and dist/pygoscribe.flatpak
./install.sh --install flatpak  # ...and install the Flatpak for your user
./install.sh --install deb      # ...and install the .deb (asks for sudo)
```

`--skip-deb` and `--skip-flatpak` build only one package. The script installs
`cargo-deb`, `cargo-deny`, `cargo-about` and the Flatpak SDK the first time it
needs them. If `flatpak-builder` isn't installed, it skips the Flatpak with a
warning.

Before building, `install.sh` checks licensing: `cargo deny check licenses`
(every crate's license must be on the allow-list in `deny.toml`), `reuse
lint` (every file carries license information) and `cargo about`
(regenerates `THIRD-PARTY-LICENSES.md`). If you don't install `reuse` first, the script does it with `pipx` or
`uv`. The `charset-normalizer` extra lets it work on minimal installs that
lack the `file` package.

To make the packages for a new release, run `scripts/release.sh v<version>`
(see `docs/PROJECT_CONTEXT.md`, Releasing).

### Reading the code

Every source file in `src/` starts with a module comment explaining its part
of the app, and `src/main.rs` has a map of the modules. `cargo doc
--document-private-items --open` renders all of it as browsable docs.

## Usage

- `pygoscribe`: starts in the background with a top-bar icon. Its menu has
  **Mark Up Clipboard**, **Open Image…**, **Start at Login**, **About** and
  **Quit**.
- `pygoscribe --clipboard`: marks up the clipboard image. If the app is already
  running, the request goes to that instance, which makes this a good command
  to bind to a keyboard shortcut in Settings → Keyboard.
- `pygoscribe FILE…`: opens the listed images or PDFs.
- `pygoscribe --window`: starts the top-bar icon and opens a window. This is
  what the app launcher runs, so clicking Pygoscribe in the app grid shows a
  window (and the launch spinner stops) instead of only adding the icon.
- `pygoscribe --no-tray`: opens a window without the top-bar icon.
- `pygoscribe --background`: what runs at login. Like plain `pygoscribe`, but
  if the top bar isn't ready yet it waits for it instead of opening a window.
- `pygoscribe --autostart=on` or `--autostart=off`: turns start at login on or
  off.
- `pygoscribe --version`: prints the version, copyright, license and the
  no-warranty notice.
- **Drag and drop:** drop images or PDFs from the file manager (or an image
  from a web browser) anywhere on a window to open them. An empty window
  opens the first one itself; anything else gets a window of its own, so a
  picture you're marking up is never replaced. Other kinds of file are
  skipped with a message. **Hold Shift while dropping** on a page that is
  already open to add the pictures to it as image marks instead (see
  Image marks).

## Start at login

Tick **Start at Login** in the top-bar menu, or run `pygoscribe --autostart=on`.
Pygoscribe then starts quietly in the top bar each time you log in.

With the .deb or a source build, this writes
`~/.config/autostart/org.outram.Pygoscribe.desktop`, which GNOME
Settings → Apps and GNOME Tweaks → Startup Applications also show. In Flatpak, the app asks the Background portal, and GNOME may ask you to allow
it the first time. Turn it on again after switching between the .deb and the
Flatpak, so the entry runs the right one.

The top-bar icon uses the StatusNotifierItem protocol, which Ubuntu's built-in
"Ubuntu AppIndicators" extension displays. Plain GNOME needs the
AppIndicator extension installed.

## License

Copyright © 2026 Nicholas Outram.

Pygoscribe is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License, version 3 only
(`GPL-3.0-only`), as published by the Free Software Foundation. It comes with
**no warranty** (see [Disclaimer](#disclaimer)). The full text is in [`LICENSE`](LICENSE); the About window
(main menu → About Pygoscribe, or About in the top-bar menu) shows it too.

- Every file says how it is licensed, following [REUSE](https://reuse.software):
  an SPDX header, or an entry in `REUSE.toml`. License texts are in `LICENSES/`.
  The AppStream metadata is CC0-1.0 and `data/protocol/gtk-shell.xml` is
  LGPL-2.1-or-later (from GTK).
- [`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) lists every Rust crate
  compiled into the binary with its license text (MIT, Apache-2.0, Unlicense,
  Unicode-3.0), plus Poppler (GPL-2.0-or-later), which the Flatpak bundles.
  The .deb installs it and a Debian `copyright` file in
  `/usr/share/doc/pygoscribe/`; the Flatpak installs it and `LICENSE` in
  `/app/share/licenses/org.outram.Pygoscribe/`.
- **Source.** Each release page carries, next to the .deb and .flatpak,
  `pygoscribe-<version>-src.tar.xz` (the tagged source plus every crate in
  `vendor/`, so `cargo build --offline --locked` works from it alone) and the
  Poppler source tarball the Flatpak was built from. Anyone passing a package
  on should pass those source files along with it.

## Credits

`data/protocol/gtk-shell.xml` is GTK's gtk_shell Wayland protocol description
(LGPL-2.1-or-later, from the GTK project). Pygoscribe uses it to tell GNOME Shell
that a top-bar click which opens no window has finished, so the busy pointer
stops.

## Markup tools

Select (V), Draw (P), Highlight (H), Line (L), Arrow (A), Rectangle (R),
Oval (O), Speech Bubble (B), Text (T), Loupe (M) and Crop (C). The
single-letter shortcuts work while the canvas has focus.

- Every mark stays editable. With Select you can click a mark to move it, drag
  its handles to resize it, or press Delete to remove it. Changing the colour,
  fill, width, line style, line ends, text font or text size applies to the
  selected mark.
- Edit a text mark by double-clicking it, by clicking it a second time with
  the Text tool, or by selecting it and pressing Enter or F2. In the editor,
  Enter finishes, Shift+Enter starts a new line, and Esc puts back the old
  text. Clicking away keeps your changes, and clearing all the text deletes
  the mark. Dragging a text mark's handles scales the font.
- Text in shapes and on lines: double-click a rectangle, oval, speech
  bubble, line or arrow (or select it and press Enter or F2) to type text
  into it, with the same editor. In a shape the text is centred by default
  and can sit left/centre/right and top/middle/bottom; on a line or arrow it
  sits above (default) or below, left/centre/right along it, turns with the
  line and always reads the right way up. The editor has alignment buttons,
  and the right-click menu has a Text Alignment submenu (for a text mark it
  lines up its lines left, centre or right). Text uses the border colour,
  font and text size, and turns with a rotated shape.
- Text font and size: the "Aa 28" button at the end of the tool bar (drawn
  in the current font, with the current size) opens Font and Size. The
  font button there opens GTK's font chooser, where you can pick any
  installed font and style, such as Bold or Italic. It applies to new text
  and to the selected text mark.
- Border and fill: the Colours button (a small box drawn in the current
  border and fill) opens Border and Fill, each with an on/off toggle, a
  colour button and an opacity slider beside it, with 100/75/50/25 %
  presets under it, so fills and lines can be
  see-through. The Border toggle turns the outline of rectangles, ovals and
  speech bubbles off, for a filled box with no edge; lines, arrows, drawing
  and text always keep their colour. A shape keeps at least one of border
  and fill, so turning one off turns the other on. A speech bubble with fill
  off is see-through. The highlighter's opacity scales with the colour's.
- Line: the button after Colours shows a sample of the current line and
  opens Width, plus rows of buttons for Style, Start and End (hover one to
  see its name).
- Line style: Style picks Solid, Dashed, Dotted, Dash Dot, Dash Dot Dot or
  Long Dash for drawing, lines, arrows and the borders of rectangles, ovals
  and speech bubbles. Dashes and dots scale with the line width, and an
  arrow's head stays solid. Highlighter strokes, the loupe's rim and text
  are always solid. Dashes are kept in PNG, JPEG and PDF output (as real
  dashed vector lines in a PDF).
- Line ends: Start and End in the Line pop-up put an end on either end of a
  drawing, line or arrow: None, Arrow, Open Arrow, Swept Arrow, Dot, Circle,
  Square, Diamond or Bar. Ends grow with the line width and stay solid on a
  dashed line. Draw, Line and Arrow each remember their own ends, so the
  Arrow tool starts with a head at the end and the Line tool with none;
  choose an arrowhead at both ends for a double-headed arrow. On a drawing,
  the end follows the direction the stroke was going. Ends are kept in PNG,
  JPEG and PDF output (as vector shapes in a PDF).
- Hold Shift while drawing a line or arrow, or while dragging one of its
  ends, to snap its angle to 15° steps, so level and upright lines come out
  exact. The length still follows the pointer.
- Rotate a mark by holding Alt and dragging it (or dragging anywhere while it
  is selected). Hold Shift as well to snap to 15° steps (a line or arrow
  snaps to the same directions as when drawing it). Every mark except the
  loupe can be rotated. If GNOME is set to move windows with Alt+drag (it uses
  Super by default), use Super+drag instead.
- Crop: drag the area to keep, then press Enter or double-click. Esc cancels.
  Crops are non-destructive and can be undone.
- Undo: Ctrl+Z. Redo: Ctrl+Shift+Z or Ctrl+Y.
- Select several marks: with Select, Shift+click (or Ctrl+click) marks to
  add them to the selection or take them out, or drag a box on an empty
  part of the page to select every mark wholly inside it (hold Shift to add
  to what is already selected). **Ctrl+A** selects every mark on the page
  (and switches to Select). Dragging any selected mark moves them all.
  Delete, cut, copy, paste, duplicate and the stacking commands act
  on all of them, each as one undo step. Changing a colour, width, line
  style, line ends or font changes just that on every selected mark, so
  each keeps the rest of its own style. Handles, text editing and rotating
  work on one mark at a time; Esc clears the selection.
- Cut, copy, paste and duplicate marks: select one or more marks, then **Ctrl+X**
  (cut), **Ctrl+C** (copy), **Ctrl+V** (paste) or **Ctrl+D** (duplicate).
  This works for every kind of mark, with its text, style and rotation;
  several marks keep their layout. A paste lands a little down and right of
  the original, and each further
  paste one step further on, so copies never hide on top of each other; the
  first paste after a cut lands where the mark was. Duplicate does the same
  without touching the clipboard, and repeating it makes a row of copies.
  Paste from the right-click menu puts the mark where you clicked. Marks can
  be pasted onto another page or into another window; one that would land
  out of sight is put in the middle of the view. Each cut, paste and
  duplicate is one undo step, and the pasted marks are selected.
- **Marks and the image share the clipboard.** Ctrl+C copies the selected
  marks, or the whole image when nothing is selected (press Esc to clear the
  selection). **Copy Image** (the blue Copy button, **Ctrl+Shift+C**, or the
  image's right-click menu) always copies the whole image. A copied mark also
  reaches other apps, as a picture of just the marks on a transparent
  background. Ctrl+V pastes marks only while they are still the last thing
  copied: once anything else has been copied, here or in another app, it
  doesn't. Otherwise Ctrl+V adds a picture on the clipboard (or picture
  files copied in the file manager) to the page as an image mark, and in an
  empty window it marks up the clipboard's image instead. While you are
  typing text, the shortcuts cut, copy and paste text as usual.
- **Image marks:** put another picture on the page, for example a logo or
  a screenshot of a detail. Three ways in: the **Insert Image** button
  after the tools (or Insert Image… in the image's right-click menu, which
  puts it where you clicked), which can pick several files at once;
  **Ctrl+V** with a picture on the clipboard; or **Shift+drop** a picture
  file, or a picture from a web browser, onto the page. A plain drop still
  opens it in a window of its own (the toast says to hold Shift); Shift is
  used because GNOME passes it on to the window under a drag as the "move"
  action, while other keys held during a drag from another app don't reach
  it. A new image mark is centred in the view (or where it was dropped),
  at its own size in pixels or smaller to fit, and selected. It moves,
  resizes, rotates (Alt+drag), restacks, cuts, copies, pastes, duplicates
  and undoes like any other mark. Dragging a corner keeps its proportions;
  hold **Shift** to stretch it freely. A picture's own transparency is
  kept, and the **Picture** opacity slider in the Colours pop-up (active
  while a picture is selected) fades the whole picture; each new picture
  starts at 100 %. Saved PNGs and JPEGs include it, and
  Save as PDF embeds it as an image at its full resolution while the page
  and the other marks stay vector.
- **Right-click** (or long-press on a touch screen, Shift+F10 or the Menu
  key) opens a context menu. On a mark it selects the mark (or keeps the
  selection, if the mark is one of several selected) and offers Edit
  Text or Add Text and Text Alignment (text marks, shapes, lines and arrows),
  Cut, Copy, Paste and Duplicate, Bring to Front (Ctrl+Shift+Up), Bring
  Forward (Ctrl+Up, one place up), Send Backward (Ctrl+Down, one place
  down), Send to Back (Ctrl+Shift+Down) and Delete. Elsewhere on the image it offers
  Paste (copied marks or a picture, at the click), Insert Image…, Select All Marks, Copy Image, Mark
  Up Clipboard, Save (Replace Original)…, Save As…, Scale Image to Visible
  Size, Actual Size and Zoom to Fit.

## Zoom

The zoom controls are in the header bar: − and + step through 5 %–1600 %, and
the percentage between them opens a menu with Actual Size and Zoom to Fit.

- **Ctrl++ / Ctrl+=** zooms in, **Ctrl+−** zooms out, **Ctrl+0** fits the
  page to the window (the default, which follows the window size) and
  **Ctrl+1** shows actual size.
- **Ctrl+scroll** or a **touchpad pinch** zooms about the pointer.
- Zoomed in, **scroll** (Shift+scroll sideways with a mouse wheel), drag the
  scrollbars, **middle-drag**, or **hold Space and drag** with the left
  button (handy without a middle button) to move around. The **arrow keys**
  scroll too (Shift+arrow scrolls further).
- 100 % is one image pixel per screen pixel for pictures, and a PDF page at
  its printed size. Zoomed-in PDF pages are redrawn sharply from the PDF a
  moment after you stop moving.
- Zoom only changes the view: marks stay put and can be selected and edited at
  any zoom, and Copy, Save and Send To are always full resolution. Opening a
  new image goes back to Zoom to Fit; changing PDF page keeps the zoom and
  starts at the top of the page.
- The header shows the result's size under the file name: "1920 × 1080 px"
  for a picture, the page size in points for a PDF, with "(cropped)" after a
  crop.
- **Scale Image to Visible Size** (the button right of the zoom controls,
  also in the percentage menu) resizes a picture
  to the size it's shown at: zoom to 50 % and choose it, and the image
  becomes half as wide and high, with its marks, line widths and text scaled
  to match. It looks the same afterwards, now at 100 %. It's one undo step.
  It's not offered for PDFs.

## PDFs

Open Image…, `pygoscribe FILE.pdf`, Open With from the file manager and
dragging a file onto a window accept PDFs. The whole document opens, with page 1 on the canvas.

- **Page sidebar.** Thumbnails of every page scroll down the left of the
  window. Show or hide it with the button at the left of the header bar or
  **F9**; Pygoscribe remembers which. An image shows as a single thumbnail.
- **Going to a page.** Click its thumbnail, or press **Page Up / Page Down**
  (also Alt+Up / Alt+Down) and **Home / End** for the first and last page.
  The title shows "name.pdf (page 3 of 12)".
- **Marking up.** Each page keeps its own marks, crop and undo history, so
  Ctrl+Z only undoes changes on the page you're looking at. Pages you've
  marked up get a dot under their thumbnail, which updates to show the marks.
- **Reordering pages.** Drag a thumbnail up or down the sidebar and drop it
  where a line shows it will go; the list scrolls when you drag near its top
  or bottom. The page's marks and undo history move with it, and the pages
  are renumbered. Moving pages can't be undone with Ctrl+Z; drag the page
  back instead.
- **Deleting pages.** Right-click a thumbnail and choose **Delete Page**, or
  select it and press **Delete**. To delete several at once, Ctrl+click
  thumbnails to add or remove them, Shift+click to select a run of pages, or
  press **Ctrl+A** in the sidebar to select them all, then press **Delete** or
  right-click one of them and choose **Delete N Pages**. Click **Undo** on the
  message that appears to put every deleted page back in its place with its
  marks. If you delete the page you're on, the nearest page that stays opens.
  A document always keeps at least one page.
- **Saving.** **Save as PDF…** writes every page, with your marks, as a new
  PDF in their current order, leaving out deleted ones (suggested name "name-marked.pdf"). The pages
  are copied as vectors, so
  their text stays selectable and the file stays small; a cropped page takes
  the crop's size. Links, form fields and bookmarks of the original are not
  kept. PNG, JPEG, Copy and Send To use the current page
  ("name-page3.png").

Pages are rendered at 144 dpi, and redrawn sharper when you zoom in past
that. Password-protected PDFs can't be opened.

## Getting the result out

- **Copy** (Ctrl+Shift+C, or Ctrl+C with no mark selected), the blue
  button, puts the full-resolution result on the clipboard, ready to paste
  into chat, email or documents.
- **Save (Replace Original)…**, the plain save button left of Save As, writes the
  result over the file you opened, in that file's own format (PNG, JPEG,
  TIFF, BMP, WebP… or the whole PDF). It asks first, because the original
  can't be got back. It's greyed out for clipboard images and for formats
  that can't be written, such as GIF and SVG.
- **Save As** (Ctrl+S, the save-as icon with a dropdown) saves in the format you used last. Its dropdown has
  **Save as PNG…**, **Save as JPEG…** and **Save as PDF…**. An extension you
  type in the name (`.png`, `.jpg`, `.jpeg`, `.pdf`) wins over the chosen
  format. With a picture open, a PDF holds one page with the marked-up
  image; with a PDF open, it holds the whole document (see PDFs above). In the .deb, changing
  the file type in the save dialog also changes the name's extension. In the
  Flatpak the dialog lists only the format chosen from the Save menu, because
  the portal dialog can't rename the file. Save starts in the
  folder you last saved to (Pictures the first time).
- **Send To** has two options. **Open With…** asks which app should open the
  image, using the desktop's app chooser. **Email…** starts a new message in
  your mail app with the image attached, using the desktop Email portal
  (Thunderbird, Evolution and others).

The result always includes the crop and every mark. To email or open the
image in another app, Markup first saves a copy in `~/Pictures/Markup-Sent`
and removes copies older than a week. The folder isn't hidden, so snap apps
such as Ubuntu's Thunderbird can read it. Email… opens your default mail app.
Thunderbird gets the image as an attachment, and so do Evolution, KMail and
Geary through `xdg-email`. Mail apps that can't take attachments from other
apps, such as Proton Mail, get a blank email, and the image goes on the
clipboard for you to paste in.

## Remembered settings

Pygoscribe remembers, between uses: the border and fill colours and their
opacity, whether border and fill are on, the line width and line style, the line ends of drawings, lines and arrows (each
remembered separately), the text font and size, the last folder you opened an image from,
the last folder you saved to, and the last save format. Changing a control
with a mark selected restyles that mark and also becomes the new default;
just selecting a mark doesn't change the defaults.

They're kept in `~/.config/pygoscribe/settings.ini` (the .deb and `cargo run`)
or `~/.var/app/org.outram.Pygoscribe/config/pygoscribe/settings.ini` (the
Flatpak). Delete the file to go back to the defaults. In the Flatpak, folders
are remembered when the desktop's document portal reports the real folder
(xdg-desktop-portal 1.17 or later, as in Ubuntu 24.04); otherwise the dialogs
start in Pictures.

## Roadmap

1. ~~Skeleton app and top-bar icon~~
2. ~~Markup canvas with drawing tools and editable objects~~
3. ~~Output: copy, save and send to~~
4. ~~Packaging (.deb and Flatpak) and start at login~~
5. Package for Raspberry Pi
6. User Testing
7. First release
8. User feedback and requirements gathering
9. TBD :)
