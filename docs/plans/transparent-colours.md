# Plan: transparent line and fill colours (0.1.5)

Nick's ask (12 Nov 2026): "Allow line and fill colours to be transparent."

Branch: `feature/transparent-colours` (off `master` at `v0.1.4`).

## Where things stand today

- `Rgba` already carries alpha, `render.rs` already passes it to Cairo, and
  `settings.ini` already stores four components (`r;g;b;a`). So the model,
  drawing and persistence can hold a transparent colour now.
- Fill can already be turned **off** (`fill_on` toggle → `Style::fill = None`).
  The border/line colour can't.
- The two `GtkColorDialogButton`s use a default `GtkColorDialog`, whose
  `with-alpha` is on, so the dialog's **Custom** editor probably already has
  an alpha slider. It's hidden behind "+ → Custom", nothing in the toolbar
  shows the opacity, and the swatch preview only hints at it with a
  checkerboard. Effectively, nobody will find it.
- Three spots draw badly once a colour isn't opaque:
  - **Arrow:** shaft and head are drawn separately and overlap, so the overlap
    shows darker.
  - **Speech bubble:** the "seam cover" stroke repaints the fill over the
    tail join, giving a darker band; and "no fill" silently means white.
  - **Highlighter:** forces alpha 0.4 and ignores the colour's own alpha.
- Hit testing for rectangles, ovals, bubbles and text uses the bounding box,
  not the paint, so a mark with no border and no fill can still be selected.

## The choice for Nick

"Transparent" could mean either of these. They can also be combined.

### Option A: "None" (fully transparent)

Add a **Border on/off** toggle next to the border colour, mirroring the
existing Fill toggle. Off means no outline at all.

- Applies to rectangle, oval and speech bubble. Line, arrow, pen,
  highlighter and loupe always need a line, so the toggle is greyed out while
  one of those tools, or a mark of that kind, is selected.
- Text is greyed out too: its "border" colour is the text colour, so turning
  it off would make the text invisible (its fill is the background box).
- Speech bubble with fill off becomes see-through instead of white. Existing behaviour change, worth flagging.
- Stops a shape from ending up with neither border nor fill: turning the last
  one off turns the other back on.
- Small: one toggle, a `stroke_on` setting, `Style::stroke` stays `Rgba` plus
  a `border: bool`, a few render branches.

### Option B: adjustable opacity

Each colour gets an **opacity** control (0–100 %), so a fill can be 30 %
yellow over the screenshot, a line can be half see-through, etc.

- UI: rather than relying on the hidden alpha slider in the colour dialog,
  each colour button gets a small popover (or the colour button becomes a
  split button) with an **Opacity** slider and a few presets
  (100 / 75 / 50 / 25 %). The colour dialog keeps alpha on so both routes
  agree. Picking a new colour keeps the current opacity.
- The toolbar swatch shows the checkerboard so a translucent colour is
  visibly different.
- Rendering fixes so translucent marks look clean: draw each mark's stroke
  parts into a Cairo group at full opacity and paint the group once with the
  stroke alpha (fixes arrow overlap and the bubble seam); fill likewise.
  Self-overlapping pen strokes stay even for free because a single path is
  stroked once.
- Highlighter: multiply its fixed 0.4 by the colour's alpha, so lowering
  opacity makes highlights fainter instead of being ignored.
- Opacity at 0 % is "none" in practice, but you lose the visible toggle state,
  and a 0 % line is an invisible mark you can still select.
- Medium: a reusable "colour + opacity" control, render group changes, tests.

### Recommendation: B, plus A's Border toggle for shapes

Opacity is the more useful feature for screenshots (translucent fills over
UI, softened lines), and it fixes the rendering quirks that a translucent
colour from the dialog would already hit today. The Border toggle is cheap
and makes "filled box with no outline" a single click, matching the Fill
toggle Nick already has. If he'd rather keep the toolbar lean, either half
stands alone.

## Implementation steps (for the recommended combination)

1. **Model** (`document.rs`): add `Style::border: bool` (default `true`).
   Add `Style::has_border(kind)` returning false only for box kinds with
   `border == false`. Unit tests for the defaults.
2. **Settings** (`settings.rs`): add `stroke_on` (key `border-on`, default
   true). Alpha is already stored; add a test that `a` round-trips and that
   out-of-range alpha is clamped. Old settings files load unchanged.
3. **Rendering** (`render.rs`):
   - Skip the stroke for box kinds when `border` is off.
   - Bubble: fill off = no fill (drop the white default); seam cover only when
     both fill and border are on, drawn inside the group.
   - Arrow: draw shaft + head in a group, paint with stroke alpha.
   - Rect/oval/bubble: fill and stroke each in their own group so translucent
     strokes don't double up at self-intersections (bubble tail).
   - Highlighter: alpha = 0.4 × colour alpha.
   - Export uses the same code, so PNG keeps transparency over the image;
     JPEG has no alpha but it's composited over the image first, so fine.
4. **Toolbar** (`window.rs`):
   - New `ColourControl` (colour button + opacity popover with slider and
     presets) used for both Border and Fill.
   - New Border toggle (`markup-border-symbolic` icon, added to the
     GResource) beside the border colour.
   - `StyleControls::show/style/remember` carry `border` and both alphas.
   - Grey out Border for line-type tools and marks; enforce "not both off".
5. **Canvas** (`canvas.rs`): no hit-test change needed (box kinds use bounds).
   Make the selection outline visible on borderless, fill-less marks (it
   already draws handles; check it's clear enough).
6. **Docs**: README style-controls section, `PROJECT_CONTEXT.md` design
   decision + roadmap row, metainfo `<release>`, `Cargo.toml` → 0.1.5.
7. **Test**: `cargo test`; headless smoke run; then Nick tests on the laptop
   (canvas interaction can't be driven headlessly). Checklist for Nick:
   - 50 % red arrow: no darker spot where head meets shaft.
   - Rectangle with border off + 30 % yellow fill over a screenshot.
   - Speech bubble with fill off is see-through; with fill on, no seam band.
   - Highlighter at 50 % is fainter than at 100 %.
   - Select a translucent mark: toolbar shows its opacity; change it; undo.
   - Restart the app: border toggle and opacities are remembered.
   - Copy / Save PNG / Save JPEG look the same as on screen.

## Release (after Nick approves and tests)

Commit on `feature/transparent-colours`, merge to `master`, tag `v0.1.5`,
push to the pi5 Gitea, publish a Gitea pre-release with the `.deb`.
