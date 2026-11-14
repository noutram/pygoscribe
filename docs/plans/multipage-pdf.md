# Plan: multi-page PDF editing with a page sidebar (0.1.8)

Nick's ask (12 Nov 2026): open a complete PDF; show page previews in a
slide-out panel on the left that scrolls vertically and can be shown and
hidden; select a page and mark it up; save the complete document with the
markup. When an image is opened, the panel shows a single scaled preview.

Branch: `feature/multipage-pdf` (off `master` at `v0.1.7`).

## Where things stand today (v0.1.7)

- `pdf.rs` opens a PDF with poppler and renders **one** page at 144 dpi.
  `load_pdf` asks for the page in an `AdwAlertDialog`, then hands the page
  image to the canvas like any picture. The poppler document is dropped.
- The canvas holds exactly one image, one `Document` (marks + crop) and one
  undo `History`. `set_image` wipes all three.
- Save as PDF writes one raster page (`output::save_pdf`), sized with
  `points_per_pixel` so it keeps the original page size.

## What the user will see

1. **Opening a PDF opens the whole file.** No page-picker dialog. Page 1 is
   shown on the canvas; the title reads "name.pdf (page 1 of 12)".
2. **Page sidebar on the left.** An `AdwOverlaySplitView` (libadwaita 1.4+,
   we target 1.5) with the sidebar on the start side. It holds a vertically
   scrolling list of page thumbnails (about 140 px wide) with the page number
   under each. The current page is highlighted. Pages you have marked up get
   a small dot, and their thumbnail refreshes to show the marks.
3. **Show/hide.** A sidebar toggle button (`sidebar-show-symbolic`) at the
   left of the header bar, plus **F9** (the GNOME convention). The panel
   slides in and out. On a narrow window it overlays the canvas instead of
   squeezing it. Whether it is shown is remembered in `settings.ini`
   (`sidebar-visible`, default on).
4. **Selecting a page.** Click a thumbnail, or use **Page Up / Page Down**
   (and Ctrl+Home / Ctrl+End for first/last) while the canvas has focus.
   The canvas switches to that page; any text being edited is finished first.
5. **Marking up.** Every tool works exactly as today, on the current page.
   Each page keeps its own marks, crop and **its own undo/redo history**:
   Ctrl+Z on page 3 never undoes something on page 1.
6. **Images and the clipboard.** The sidebar shows one scaled preview of the
   image (and updates as you mark it up). Everything else works as now.
7. **Saving.**
   - **Save as PDF** with a PDF open writes **all pages**, with the markup,
     in order. Default name: the original name with "-marked" added
     ("report-marked.pdf"), in the remembered save folder.
   - **Save as PNG / JPEG, Copy, Open With…, Email…** act on the **current
     page** (an image format can only hold one page). The toast says
     "page 3" so this is clear.
   - With an image open, everything behaves as in 0.1.7.

## Design

### Model: a `Pages` list owned by the window

New module `src/pages.rs`:

```rust
pub enum Source {
    Image(ImageSurface),                 // picture or clipboard
    Pdf { pdf: Rc<Pdf>, index: i32 },    // a PDF page, rendered on demand
}

pub struct Page {
    pub source: Source,
    pub doc: Document,                   // marks + crop
    pub history: History,                // per-page undo/redo
    pub points_per_pixel: f64,           // 0.5 for 144-dpi PDF pages, 1 for pictures
    pub size_px: (i32, i32),             // full-resolution size, known without rendering
}
```

- The canvas keeps working on one page at a time. New API:
  `Canvas::take_page() -> (Document, History)` and
  `Canvas::show_page(image, doc, history)`. Switching page = take the state
  out of the canvas into the old `Page`, render the new page, put its state
  in. `set_image` stays for the single-image case.
- `points_per_pixel` moves from the window into `Page`.

### Memory

A 144-dpi Letter page is about 7.7 MB as an image, so a 200-page PDF can't
keep every page rendered. Only the **current** page's full-resolution image
is kept (plus the previous one, so flicking back is instant). Other pages are
re-rendered from poppler when you go to them (tens of milliseconds for a
normal page). Marks are tiny and are kept for every page.

### Thumbnails

- `GtkListView` over a `gio::ListStore` of small page objects, so a long
  document only creates widgets for visible rows. Each row: a
  `GtkPicture` thumbnail, page number, marked dot.
- Thumbnails are rendered by poppler at thumbnail scale (cheap) on the main
  loop in an idle handler, a few pages per tick, visible pages first, so
  opening a big PDF doesn't freeze the window. poppler documents aren't
  thread-safe, so no worker thread.
- After a change on a page (history changes), its thumbnail is re-drawn
  with the marks, debounced to ~300 ms so dragging doesn't re-render every
  frame. It reuses `render::draw_document` at thumbnail scale.

### Saving the whole PDF: vector, not raster

`output::save_pdf_document(pages, path)` writes one `cairo::PdfSurface`
page per page:

- The page content is drawn with **poppler `page.render()` straight into
  the PDF surface**, so text stays text (selectable, searchable, sharp) and
  the file stays small. A raster copy would make a 30-page text PDF tens of
  MB and blurry when zoomed.
- Marks are drawn on top with the existing `render::draw_shape`, scaled by
  `points_per_pixel`, so they are vector too.
- **Loupe** marks magnify the page *image*, so for those the loupe's area is
  painted from a rendered image of the page (only pages with a loupe need
  one).
- **Crop** on a page sets that page's size to the crop rectangle and clips
  to it. Pages without a crop keep their original size.
- Unmarked pages are copied as they are (still re-drawn via poppler, so
  vector). Links, form fields, bookmarks and annotations of the original are
  **not** kept: poppler + cairo can render a page but can't copy PDF
  structure. Pages look identical; interactive extras are lost. (Keeping
  them would need a PDF-writing library such as `lopdf` to stamp marks onto
  the original file; much bigger job, not proposed now.)

`render::draw_document` currently paints the image first; it gets split so
the marks can be drawn without the background (`draw_marks`).

### Window changes (`window.rs`)

- Wrap the `ToastOverlay` content in an `AdwOverlaySplitView`; the sidebar
  is a `GtkScrolledWindow` (vertical only) with the `ListView`.
- Header: sidebar toggle bound to `show-sidebar`; `win.toggle-sidebar`
  action with F9.
- Remove `choose_page` (the page dialog). `load_pdf` builds the `Pages`
  list and shows page 1.
- Actions `win.next-page` / `win.prev-page` / first / last.
- `save_as(Pdf)` with a multi-page source calls the document save; other
  formats export the current page. Default file name changes as described.
- Title: "name.pdf (page N of M)"; images unchanged.

### Things deliberately unchanged / out of scope

- No adding, deleting, reordering or rotating pages.
- No drag and drop (as today).
- No "unsaved changes" warning on close (none exists today either). Worth
  adding later now that a whole document's work could be lost; say if you
  want it in this release.
- Opening a second file replaces the current document (as today).

## Tests

- `pages.rs`: switching pages keeps each page's marks and undo history
  separate; undo on one page doesn't touch another.
- `output`: a 3-page PDF with marks on page 2 and a crop on page 3 saves as a
  3-page PDF; page sizes match (page 3 = crop size); page 1 is unchanged; the
  mark is visible on page 2; the text of page 1 is still extractable with
  poppler (proves it stayed vector).
- Existing PDF tests updated (no page dialog; `render(index)` stays).
- Manual check on the laptop: a real multi-page PDF, sidebar show/hide (F9,
  button, narrow window), thumbnails update, save, reopen in Document Viewer.

## Release steps

1. Branch `feature/multipage-pdf` from `master`.
2. Implement, `cargo test`, run on the laptop.
3. Bump `Cargo.toml` to 0.1.8, add a `<release>` to the metainfo, update
   README and `docs/PROJECT_CONTEXT.md` (roadmap row, PDF design decision).
4. After Nick checks it: merge to `master`, tag `v0.1.8`, `./install.sh`
   for the .deb, publish a Gitea pre-release with the .deb attached.
   Git steps run on the laptop through Remote Control, with Nick's OK.

## Choices for Nick (defaults in bold)

1. PDF save: **vector (keeps text selectable, small files; loses links and
   form fields)**, or raster like 0.1.7 (simpler, big blurry files).
2. Undo: **per page**, or one history across the whole document.
3. PNG/JPEG/Copy/Send with a PDF open: **current page only**, or also offer
   "all pages as numbered PNGs".
