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

//! The editor window: header bar, markup toolbar and canvas.
//!
//! # Layout
//!
//! ```text
//! AdwApplicationWindow
//! └─ AdwToolbarView
//!    ├─ AdwHeaderBar   pages · paste · open · zoom · scale … undo · send · save · copy · menu
//!    ├─ toolbar        tool buttons · border/fill colours · width · font · size
//!    └─ AdwToastOverlay
//!       └─ AdwOverlaySplitView (Sidebar)
//!          ├─ page thumbnails
//!          └─ GtkStack: "empty" status page | "image" (the Canvas)
//! ```
//!
//! Most commands are `win.*` actions, so the header buttons, menus, context
//! menu and keyboard shortcuts all share one implementation and GTK keeps
//! their sensitivity in step.
//!
//! # Wayland activation tokens
//!
//! Under Wayland a new window may only take focus if it presents an XDG
//! activation token from the event that caused it. Clicks on the top-bar
//! icon carry such a token (see `tray.rs`), which is parked in
//! [`set_activation_token`] and handed to the next window that is presented.
//! When a click opens no window, [`release_activation_token`] passes the
//! token straight back to GNOME Shell so its busy pointer stops.

use crate::canvas::Canvas;
use crate::document::{Rgba, Style, Tool};
use crate::pages::{PageState, Pages};
use crate::settings::SaveFormat;
use crate::sidebar::Sidebar;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One editor window. Cheap to clone: every field is a GObject or an `Rc`.
#[derive(Clone)]
pub struct MarkupWindow {
    /// The toplevel window.
    window: adw::ApplicationWindow,
    /// Switches between the empty status page and the canvas.
    stack: gtk::Stack,
    /// The header's title, with the image size as its subtitle.
    title: adw::WindowTitle,
    /// The markup canvas, which holds the current page's state.
    canvas: Canvas,
    /// Shows short messages ("Copied", errors) over the content.
    toasts: adw::ToastOverlay,
    /// What is open: a picture, or every page of a PDF. None before anything
    /// is opened.
    pages: Rc<RefCell<Option<Pages>>>,
    /// The page thumbnails.
    sidebar: Sidebar,
}

/// The tool buttons, in tool-bar order: the tool, its icon (from the
/// app's bundled icons) and its tooltip.
const TOOLS: &[(Tool, &str, &str)] = &[
    (Tool::Select, "markup-select-symbolic", "Select (V) · Alt+drag to rotate"),
    (Tool::Pen, "markup-pen-symbolic", "Draw (P)"),
    (Tool::Highlighter, "markup-highlighter-symbolic", "Highlight (H)"),
    (Tool::Line, "markup-line-symbolic", "Line (L)"),
    (Tool::Arrow, "markup-arrow-symbolic", "Arrow (A)"),
    (Tool::Rect, "markup-rect-symbolic", "Rectangle (R)"),
    (Tool::Oval, "markup-oval-symbolic", "Oval (O)"),
    (Tool::Bubble, "markup-bubble-symbolic", "Speech Bubble (B)"),
    (Tool::Text, "markup-text-symbolic", "Text (T): click a text mark again, double-click it or press Enter to edit it. Double-click a shape, line or arrow to put text in it"),
    (Tool::Loupe, "markup-loupe-symbolic", "Loupe (M)"),
    (Tool::Crop, "markup-crop-symbolic", "Crop (C)"),
];

/// Converts a document colour to GDK's (f32 channels).
fn to_gdk(c: Rgba) -> gdk::RGBA {
    gdk::RGBA::new(c.r as f32, c.g as f32, c.b as f32, c.a as f32)
}

/// Converts a GDK colour to a document colour.
fn from_gdk(c: &gdk::RGBA) -> Rgba {
    Rgba::new(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64)
}

/// A colour button with an opacity control beside it: a menu button showing
/// the opacity ("50%") that opens a slider and presets. The colour dialog's
/// own alpha slider stays available and the two agree.
#[derive(Clone)]
struct ColourControl {
    /// The colour button and opacity button side by side.
    widget: gtk::Box,
    /// Opens GTK's colour chooser (with alpha).
    colour: gtk::ColorDialogButton,
    /// Opacity in percent, 0–100; mirrors the colour's alpha.
    opacity: gtk::Adjustment,
}

/// The quick opacity choices under the slider, in percent.
const OPACITY_PRESETS: [u32; 4] = [100, 75, 50, 25];

impl ColourControl {
    /// A control starting at colour `c`; `what` ("Border", "Fill") names it in
    /// tooltips.
    fn new(c: Rgba, what: &str) -> Self {
        let colour = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().with_alpha(true).build()));
        colour.set_tooltip_text(Some(&format!("{what} Colour")));
        let opacity = gtk::Adjustment::new(100.0, 0.0, 100.0, 5.0, 25.0, 0.0);

        let scale = gtk::Scale::new(gtk::Orientation::Horizontal, Some(&opacity));
        scale.set_digits(0);
        scale.set_round_digits(0);
        scale.set_draw_value(true);
        scale.set_value_pos(gtk::PositionType::Right);
        scale.set_format_value_func(|_, v| format!("{v:.0}%"));
        scale.set_size_request(180, -1);
        let presets = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        presets.add_css_class("linked");
        presets.set_homogeneous(true);
        for p in OPACITY_PRESETS {
            let b = gtk::Button::with_label(&format!("{p}%"));
            let adj = opacity.clone();
            b.connect_clicked(move |_| adj.set_value(p as f64));
            presets.append(&b);
        }
        let title = gtk::Label::new(Some(&format!("{what} Opacity")));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.append(&title);
        body.append(&scale);
        body.append(&presets);
        let popover = gtk::Popover::builder().child(&body).build();
        let menu = gtk::MenuButton::builder()
            .popover(&popover)
            .tooltip_text(format!("{what} Opacity"))
            .always_show_arrow(false)
            .build();
        menu.add_css_class("flat");
        menu.add_css_class("numeric");

        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        widget.append(&colour);
        widget.append(&menu);

        let this = Self { widget, colour, opacity };
        // Keep the slider, the button's label and the colour's alpha in step.
        // Setting an equal value emits nothing, so this doesn't loop.
        {
            let this2 = this.clone();
            let menu = menu.clone();
            this.opacity.connect_value_changed(move |adj| {
                menu.set_label(&format!("{:.0}%", adj.value()));
                let mut c = this2.colour.rgba();
                let a = (adj.value() / 100.0) as f32;
                if (c.alpha() - a).abs() > 1e-4 {
                    c.set_alpha(a);
                    this2.colour.set_rgba(&c);
                }
            });
        }
        {
            let adj = this.opacity.clone();
            this.colour.connect_rgba_notify(move |b| {
                let v = (b.rgba().alpha() as f64 * 100.0).round();
                if (adj.value() - v).abs() > 0.5 {
                    adj.set_value(v);
                }
            });
        }
        menu.set_label("100%");
        this.set(c);
        this
    }

    /// The current colour, alpha included.
    fn rgba(&self) -> Rgba {
        from_gdk(&self.colour.rgba())
    }

    /// Shows colour `c` (the opacity follows through the notify handler).
    fn set(&self, c: Rgba) {
        self.colour.set_rgba(&to_gdk(c));
    }

    /// Calls `f` once per change, whether from the dialog or the opacity
    /// control (which changes the colour's alpha).
    fn connect_changed(&self, f: impl Fn() + 'static) {
        self.colour.connect_rgba_notify(move |_| f());
    }
}

/// The style controls in the markup toolbar.
#[derive(Clone)]
struct StyleControls {
    /// Border on/off for rectangles, ovals and bubbles.
    border_on: gtk::ToggleButton,
    /// Border, line and text colour.
    stroke: ColourControl,
    /// Fill on/off.
    fill_on: gtk::ToggleButton,
    /// Fill colour.
    fill: ColourControl,
    /// Line width.
    width: gtk::SpinButton,
    /// Text size.
    font: gtk::SpinButton,
    /// Font family and style (no size).
    face: gtk::FontDialogButton,
    /// Set while [`Self::show`] loads a selected mark's style into the
    /// controls, so that isn't mistaken for the user restyling it.
    syncing: Rc<Cell<bool>>,
}

impl StyleControls {
    /// The controls, set to the remembered settings.
    fn new() -> Self {
        let saved = crate::settings::get();
        let border_on = gtk::ToggleButton::builder()
            .icon_name("markup-border-symbolic")
            .tooltip_text("Border on Rectangles, Ovals and Bubbles")
            .active(saved.border_on)
            .build();
        let stroke = ColourControl::new(saved.stroke, "Border");
        let fill_on = gtk::ToggleButton::builder()
            .icon_name("markup-fill-symbolic")
            .tooltip_text("Fill Shapes")
            .active(saved.fill_on)
            .build();
        let fill = ColourControl::new(saved.fill, "Fill");
        let (lo, hi) = crate::settings::WIDTH_RANGE;
        let width = gtk::SpinButton::with_range(lo, hi, 1.0);
        width.set_value(saved.width);
        width.set_tooltip_text(Some("Line Width"));
        let (lo, hi) = crate::settings::FONT_RANGE;
        let font = gtk::SpinButton::with_range(lo, hi, 2.0);
        font.set_value(saved.font_size);
        font.set_tooltip_text(Some("Text Size"));
        let dialog = gtk::FontDialog::builder().title("Text Font").modal(true).build();
        let face = gtk::FontDialogButton::builder()
            .dialog(&dialog)
            .level(gtk::FontLevel::Face)
            .use_font(true)
            .use_size(false)
            .tooltip_text("Text Font")
            .build();
        face.set_font_desc(&gtk::pango::FontDescription::from_string(&saved.font));
        let show_name = |b: &gtk::FontDialogButton| {
            let name = b.font_desc().map(|d| d.to_str().to_string()).unwrap_or_default();
            b.set_tooltip_text(Some(&format!("Text Font: {name}")));
        };
        show_name(&face);
        face.connect_font_desc_notify(show_name);
        // Long names like "DejaVu Sans Condensed Bold Oblique" would widen the
        // toolbar (and the window's minimum width), so shorten the labels.
        // FontDialogButton doesn't expose its labels, so walk its widget tree
        // depth-first (child, else next sibling, else an ancestor's next
        // sibling, stopping at the button) and ellipsize every label found.
        let mut child = face.first_child();
        while let Some(w) = child {
            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_width_chars(10);
                l.set_max_width_chars(16);
            }
            child = w.first_child().or_else(|| {
                let mut w = Some(w);
                while let Some(cur) = w {
                    if let Some(next) = cur.next_sibling() {
                        return Some(next);
                    }
                    w = cur.parent().filter(|p| p != face.upcast_ref::<gtk::Widget>());
                }
                None
            });
        }
        let this = Self { border_on, stroke, fill_on, fill, width, font, face, syncing: Rc::new(Cell::new(false)) };
        // A shape needs a border or a fill: turning one off turns the other on.
        // Connected before `connect_changed`, so the change is seen as one.
        let other = this.fill_on.clone();
        this.border_on.connect_toggled(move |b| {
            if !b.is_active() && !other.is_active() {
                other.set_active(true);
            }
        });
        let other = this.border_on.clone();
        this.fill_on.connect_toggled(move |b| {
            if !b.is_active() && !other.is_active() {
                other.set_active(true);
            }
        });
        this
    }

    /// The style the controls describe, for new marks or the selected one.
    fn style(&self) -> Style {
        Style {
            stroke: self.stroke.rgba(),
            border: self.border_on.is_active(),
            fill: self.fill_on.is_active().then(|| self.fill.rgba()),
            width: self.width.value(),
            font_size: self.font.value(),
            font: self.font_name(),
        }
    }

    /// The chosen font as a Pango description string, without a size.
    fn font_name(&self) -> String {
        match self.face.font_desc() {
            Some(mut d) => {
                // The size comes from the Text Size control instead.
                d.unset_fields(gtk::pango::FontMask::SIZE);
                let name = d.to_str().to_string();
                if name.trim().is_empty() { crate::document::DEFAULT_FONT.to_string() } else { name }
            }
            None => crate::document::DEFAULT_FONT.to_string(),
        }
    }

    /// Remembers the controls' current values for next time.
    fn remember(&self) {
        let (stroke, fill) = (self.stroke.rgba(), self.fill.rgba());
        let (border_on, fill_on) = (self.border_on.is_active(), self.fill_on.is_active());
        let (width, font_size) = (self.width.value(), self.font.value());
        let font = self.font_name();
        crate::settings::update(|s| {
            s.stroke = stroke;
            s.border_on = border_on;
            s.fill = fill;
            s.fill_on = fill_on;
            s.width = width;
            s.font_size = font_size;
            s.font = font;
        });
    }

    /// Loads `style` (the selected mark's) into the controls without
    /// treating it as a change.
    fn show(&self, style: &Style) {
        self.syncing.set(true);
        self.stroke.set(style.stroke);
        self.border_on.set_active(style.border);
        self.fill_on.set_active(style.fill.is_some());
        // With fill off, keep showing the last fill colour, so turning fill
        // on again brings it back.
        if let Some(f) = style.fill {
            self.fill.set(f);
        }
        self.width.set_value(style.width);
        self.font.set_value(style.font_size);
        // Only when it differs: setting it re-emits notify and rebuilds labels.
        if self.font_name() != style.font {
            self.face.set_font_desc(&gtk::pango::FontDescription::from_string(&style.font));
        }
        self.syncing.set(false);
    }

    /// Calls `f` with the new style whenever the user changes any control,
    /// and remembers the new values in the settings.
    fn connect_changed(&self, f: impl Fn(Style) + 'static) {
        let f = Rc::new(f);
        let emit = {
            let this = self.clone();
            move || {
                if !this.syncing.get() {
                    this.remember();
                    f(this.style());
                }
            }
        };
        let e = emit.clone();
        self.stroke.connect_changed(e);
        let e = emit.clone();
        self.border_on.connect_toggled(move |_| e());
        let e = emit.clone();
        self.fill.connect_changed(e);
        let e = emit.clone();
        self.fill_on.connect_toggled(move |_| e());
        let e = emit.clone();
        self.width.connect_value_changed(move |_| e());
        let e = emit.clone();
        self.font.connect_value_changed(move |_| e());
        self.face.connect_font_desc_notify(move |_| emit());
    }
}

/// A dimmed caption for a tool-bar control ("Width", "Text").
fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    l.set_margin_start(6);
    l
}

impl MarkupWindow {
    /// Builds a window with every widget, action and shortcut, showing the
    /// empty page. It isn't shown until [`Self::present`].
    pub fn new(app: &adw::Application) -> Self {
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("Pygoscribe", "");
        header.set_title_widget(Some(&title));

        let sidebar_btn = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Show Pages (F9)")
            .build();
        header.pack_start(&sidebar_btn);

        let paste_btn = gtk::Button::from_icon_name("edit-paste-symbolic");
        paste_btn.set_tooltip_text(Some("Mark Up Clipboard"));
        header.pack_start(&paste_btn);

        let open_btn = gtk::Button::from_icon_name("document-open-symbolic");
        open_btn.set_tooltip_text(Some("Open Image…"));
        header.pack_start(&open_btn);

        // Zoom: out, the zoom level (a menu), in.
        let zoom_menu = gio::Menu::new();
        zoom_menu.append(Some("Zoom In"), Some("win.zoom-in"));
        zoom_menu.append(Some("Zoom Out"), Some("win.zoom-out"));
        zoom_menu.append(Some("Actual Size"), Some("win.zoom-actual"));
        zoom_menu.append(Some("Zoom to Fit"), Some("win.zoom-fit"));
        let scale_section = gio::Menu::new();
        scale_section.append(Some("Scale Image to Visible Size"), Some("win.scale-to-visible"));
        zoom_menu.append_section(None, &scale_section);
        let zoom_label = gtk::Label::builder().label("100%").width_chars(5).build();
        zoom_label.add_css_class("numeric");
        let zoom_level = gtk::MenuButton::builder()
            .child(&zoom_label)
            .tooltip_text("Zoom")
            .menu_model(&zoom_menu)
            .build();
        let zoom_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        zoom_box.add_css_class("linked");
        zoom_box.append(
            &gtk::Button::builder()
                .icon_name("zoom-out-symbolic")
                .tooltip_text("Zoom Out (Ctrl+−)")
                .action_name("win.zoom-out")
                .build(),
        );
        zoom_box.append(&zoom_level);
        zoom_box.append(
            &gtk::Button::builder()
                .icon_name("zoom-in-symbolic")
                .tooltip_text("Zoom In (Ctrl++)")
                .action_name("win.zoom-in")
                .build(),
        );
        header.pack_start(&zoom_box);
        header.pack_start(
            &gtk::Button::builder()
                .icon_name("markup-scale-symbolic")
                .tooltip_text("Scale Image to Visible Size")
                .action_name("win.scale-to-visible")
                .build(),
        );

        let undo_btn = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text("Undo")
            .action_name("win.undo")
            .build();
        let redo_btn = gtk::Button::builder()
            .icon_name("edit-redo-symbolic")
            .tooltip_text("Redo")
            .action_name("win.redo")
            .build();
        let undo_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        undo_box.add_css_class("linked");
        undo_box.append(&undo_btn);
        undo_box.append(&redo_btn);
        // Output, rightmost first: Copy (the main action), Save, Send To.
        let copy_btn = gtk::Button::builder()
            .label("Copy")
            .tooltip_text("Copy to Clipboard (Ctrl+C)")
            .action_name("win.copy")
            .build();
        copy_btn.add_css_class("suggested-action");
        // Primary menu at the far end (GNOME HIG), then Copy.
        let main_menu = gio::Menu::new();
        main_menu.append(Some("_About Pygoscribe"), Some("app.about"));
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .menu_model(&main_menu)
            .primary(true)
            .build();
        header.pack_end(&menu_btn);
        header.pack_end(&copy_btn);
        // Save: the button saves in the last format used; the menu picks one.
        let save_menu = gio::Menu::new();
        save_menu.append(Some("Save as PNG…"), Some("win.save-png"));
        save_menu.append(Some("Save as JPEG…"), Some("win.save-jpeg"));
        save_menu.append(Some("Save as PDF…"), Some("win.save-pdf"));
        let save_btn = adw::SplitButton::builder()
            .icon_name("document-save-as-symbolic")
            .tooltip_text("Save As… (Ctrl+S)")
            .dropdown_tooltip("Choose Format")
            .action_name("win.save")
            .menu_model(&save_menu)
            .build();
        header.pack_end(&save_btn);
        // Save: write back over the opened file (asks first).
        header.pack_end(
            &gtk::Button::builder()
                .icon_name("document-save-symbolic")
                .tooltip_text("Save (Replace Original)…")
                .action_name("win.save-original")
                .build(),
        );
        let send_menu = gio::Menu::new();
        send_menu.append(Some("Open With…"), Some("win.open-with"));
        send_menu.append(Some("Email…"), Some("win.email"));
        let send_btn = gtk::MenuButton::builder()
            .icon_name("send-to-symbolic")
            .tooltip_text("Send To")
            .menu_model(&send_menu)
            .build();
        header.pack_end(&send_btn);
        header.pack_end(&undo_box);

        // Markup toolbar: tools, then style controls.
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        toolbar.add_css_class("toolbar");
        let mut group: Option<gtk::ToggleButton> = None;
        let mut tool_buttons = Vec::new();
        for (tool, icon, tip) in TOOLS {
            let b = gtk::ToggleButton::builder().icon_name(*icon).tooltip_text(*tip).build();
            b.add_css_class("flat");
            // All tool buttons share one group, so exactly one is active.
            if let Some(g) = &group {
                b.set_group(Some(g));
            } else {
                b.set_active(true);
                group = Some(b.clone());
            }
            // Separators group the tools: select | freehand | lines and
            // shapes | text | loupe and crop.
            if matches!(tool, Tool::Select | Tool::Highlighter | Tool::Bubble | Tool::Loupe) {
                toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            }
            toolbar.append(&b);
            tool_buttons.push((*tool, b));
        }
        let style = StyleControls::new();
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&style.border_on);
        toolbar.append(&style.stroke.widget);
        toolbar.append(&style.fill_on);
        toolbar.append(&style.fill.widget);
        toolbar.append(&label("Width"));
        toolbar.append(&style.width);
        toolbar.append(&label("Text"));
        toolbar.append(&style.face);
        toolbar.append(&style.font);

        let canvas = Canvas::new();
        canvas.set_style(style.style());
        canvas.connect_zoom_changed(move |percent| zoom_label.set_label(&format!("{percent:.0}%")));

        let empty = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .title("No Image")
            .description("Copy an image and choose Mark Up Clipboard, or open an image file")
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(canvas.root(), Some("image"));

        let sidebar = Sidebar::new(&stack);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(sidebar.widget()));

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.add_top_bar(&toolbar);
        view.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Pygoscribe")
            .default_width(1000)
            .default_height(720)
            .content(&view)
            .build();

        // The header toggle and the split view's own state stay in step
        // both ways (F9 and swipe gestures change the split view directly).
        sidebar
            .widget()
            .bind_property("show-sidebar", &sidebar_btn, "active")
            .bidirectional()
            .sync_create()
            .build();

        let this = Self { window, stack, title, canvas, toasts, pages: Rc::new(RefCell::new(None)), sidebar };

        {
            // Remember the sidebar's visibility; only write when it changed.
            this.sidebar.widget().connect_show_sidebar_notify(move |s| {
                let shown = s.shows_sidebar();
                if crate::settings::get().sidebar_visible != shown {
                    crate::settings::update(|st| st.sidebar_visible = shown);
                }
            });
        }
        {
            let w = this.clone();
            this.sidebar.connect_page_chosen(move |i| {
                w.go_to_page(i);
                w.canvas.widget().grab_focus();
            });
        }
        {
            let w = this.clone();
            this.sidebar.connect_page_moved(move |from, to| w.move_page(from, to));
        }
        {
            let w = this.clone();
            this.sidebar.connect_delete_requested(move |i| w.delete_pages(i));
        }

        for (tool, button) in &tool_buttons {
            let canvas = this.canvas.clone();
            let tool = *tool;
            let border_on = style.border_on.clone();
            button.connect_toggled(move |b| {
                if b.is_active() {
                    canvas.set_tool(tool);
                    border_on.set_sensitive(canvas.border_is_optional());
                }
            });
        }
        // Single-key tool shortcuts while the canvas has focus.
        let keys = gtk::EventControllerKey::new();
        let buttons = tool_buttons.clone();
        let w = this.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            // Page Up/Down and Home/End change PDF page (with or without Ctrl).
            if mods.is_empty() || mods == gdk::ModifierType::CONTROL_MASK {
                let step = match key {
                    gdk::Key::Page_Up | gdk::Key::KP_Page_Up => Some(PageStep::Previous),
                    gdk::Key::Page_Down | gdk::Key::KP_Page_Down => Some(PageStep::Next),
                    gdk::Key::Home | gdk::Key::KP_Home => Some(PageStep::First),
                    gdk::Key::End | gdk::Key::KP_End => Some(PageStep::Last),
                    _ => None,
                };
                if let Some(step) = step {
                    w.step_page(step);
                    return glib::Propagation::Stop;
                }
            }
            // Letters with Ctrl or Alt are shortcuts for other things (Ctrl+C
            // is Copy), so only bare or Shifted letters pick a tool.
            if !mods.is_empty() && mods != gdk::ModifierType::SHIFT_MASK {
                return glib::Propagation::Proceed;
            }
            let tool = match key.to_lower() {
                gdk::Key::v => Tool::Select,
                gdk::Key::p => Tool::Pen,
                gdk::Key::h => Tool::Highlighter,
                gdk::Key::l => Tool::Line,
                gdk::Key::a => Tool::Arrow,
                gdk::Key::r => Tool::Rect,
                gdk::Key::o => Tool::Oval,
                gdk::Key::b => Tool::Bubble,
                gdk::Key::t => Tool::Text,
                gdk::Key::m => Tool::Loupe,
                gdk::Key::c => Tool::Crop,
                _ => return glib::Propagation::Proceed,
            };
            // Activating the button runs its toggled handler, so the canvas
            // and the tool bar can't disagree.
            if let Some((_, b)) = buttons.iter().find(|(t, _)| *t == tool) {
                b.set_active(true);
            }
            glib::Propagation::Stop
        });
        this.canvas.widget().add_controller(keys);

        {
            let canvas = this.canvas.clone();
            style.connect_changed(move |s| canvas.set_style(s));
        }
        {
            let style = style.clone();
            let canvas = this.canvas.clone();
            let w = this.clone();
            // Show a newly selected mark's style in the tool bar.
            this.canvas.connect_selection_changed(move |s| {
                if let Some(s) = s {
                    style.show(&s);
                }
                style.border_on.set_sensitive(canvas.border_is_optional());
                w.update_mark_actions();
            });
        }
        {
            let w = this.clone();
            this.canvas.connect_context_menu(move |at, mark| w.show_context_menu(at, mark));
        }
        {
            let toasts = this.toasts.clone();
            this.canvas.connect_hint(move |msg| {
                let t = adw::Toast::new(msg);
                t.set_timeout(3);
                toasts.add_toast(t);
            });
        }

        let undo = gio::SimpleAction::new("undo", None);
        let redo = gio::SimpleAction::new("redo", None);
        undo.set_enabled(false);
        redo.set_enabled(false);
        {
            let c = this.canvas.clone();
            undo.connect_activate(move |_, _| c.undo());
            let c = this.canvas.clone();
            redo.connect_activate(move |_, _| c.redo());
            let c = this.canvas.clone();
            let (u, r) = (undo.clone(), redo.clone());
            let w = this.clone();
            this.canvas.connect_history_changed(move || {
                w.update_size();
                u.set_enabled(c.can_undo());
                r.set_enabled(c.can_redo());
                // Redraw this page's thumbnail once the changes settle.
                let w2 = w.clone();
                w.sidebar.refresh_later(move || {
                    // try_borrow: this may fire while a page switch holds the
                    // pages; the switch redraws thumbnails itself then.
                    let cur = w2.pages.try_borrow().ok().and_then(|p| p.as_ref().map(|p| p.current()));
                    if let Some(cur) = cur {
                        w2.sidebar.refresh(cur);
                        w2.draw_thumbnails();
                    }
                });
            });
        }
        this.window.add_action(&undo);
        this.window.add_action(&redo);
        app.set_accels_for_action("win.undo", &["<Control>z"]);
        app.set_accels_for_action("win.redo", &["<Control><Shift>z", "<Control>y"]);

        // Adds a stateless `win.<name>` action that calls `f` on this window.
        let add = |name: &str, f: fn(&MarkupWindow)| {
            let action = gio::SimpleAction::new(name, None);
            let target = this.clone();
            action.connect_activate(move |_, _| f(&target));
            this.window.add_action(&action);
        };
        add("copy", MarkupWindow::copy);
        add("save", |w| w.save_as(crate::settings::get().save_format));
        add("save-png", |w| w.save_as(SaveFormat::Png));
        add("save-jpeg", |w| w.save_as(SaveFormat::Jpeg));
        add("save-pdf", |w| w.save_as(SaveFormat::Pdf));
        add("save-original", MarkupWindow::save_original);
        add("paste", MarkupWindow::load_clipboard);
        add("mark-edit", |w| {
            w.canvas.edit_selected_text();
        });
        // Where the selected mark's text sits, as radio items in the context
        // menu (also in the text editor).
        for (name, horizontal) in [("mark-halign", true), ("mark-valign", false)] {
            let action = gio::SimpleAction::new_stateful(name, Some(glib::VariantTy::STRING), &"".to_variant());
            let target = this.clone();
            action.connect_activate(move |a, v| {
                let Some(id) = v.and_then(|v| v.get::<String>()) else { return };
                a.set_state(&id.to_variant());
                if horizontal {
                    target.canvas.align_selected(crate::document::HAlign::from_id(&id), None);
                } else {
                    target.canvas.align_selected(None, crate::document::VAlign::from_id(&id));
                }
            });
            this.window.add_action(&action);
        }
        add("mark-front", |w| w.canvas.restack_selected(true));
        add("mark-back", |w| w.canvas.restack_selected(false));
        add("mark-delete", |w| w.canvas.delete_selected());
        app.set_accels_for_action("win.mark-front", &["<Control><Shift>Up"]);
        app.set_accels_for_action("win.mark-back", &["<Control><Shift>Down"]);
        add("scale-to-visible", MarkupWindow::scale_to_visible);
        add("open-with", MarkupWindow::open_with);
        add("email", MarkupWindow::email);
        add("toggle-sidebar", |w| {
            let split = w.sidebar.widget();
            split.set_show_sidebar(!split.shows_sidebar());
        });
        add("zoom-in", |w| w.canvas.zoom_in());
        add("zoom-out", |w| w.canvas.zoom_out());
        add("zoom-actual", |w| w.canvas.zoom_actual());
        add("zoom-fit", |w| w.canvas.zoom_fit());
        app.set_accels_for_action(
            "win.zoom-in",
            &["<Control>equal", "<Control>plus", "<Control><Shift>plus", "<Control>KP_Add"],
        );
        app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
        app.set_accels_for_action("win.zoom-actual", &["<Control>1", "<Control>KP_1"]);
        app.set_accels_for_action("win.zoom-fit", &["<Control>0", "<Control>KP_0"]);
        add("next-page", |w| w.step_page(PageStep::Next));
        add("previous-page", |w| w.step_page(PageStep::Previous));
        app.set_accels_for_action("win.toggle-sidebar", &["F9"]);
        app.set_accels_for_action("win.next-page", &["<Alt>Down"]);
        app.set_accels_for_action("win.previous-page", &["<Alt>Up"]);
        app.set_accels_for_action("win.copy", &["<Control>c"]);
        app.set_accels_for_action("win.save", &["<Control>s", "<Control><Shift>s"]);
        this.update_actions();
        this.update_mark_actions();

        paste_btn.connect_clicked(glib::clone!(
            #[strong]
            this,
            move |_| this.load_clipboard()
        ));
        open_btn.connect_clicked(glib::clone!(
            #[strong]
            this,
            move |_| choose_and_open(&this.app(), Some(&this))
        ));
        this.add_file_drop_target(&view);

        this
    }

    /// Opens files (from the file manager) or image data (from a browser)
    /// dropped anywhere on the window. The sidebar's own page drags carry a
    /// page number, not these types, so they never reach this target.
    fn add_file_drop_target(&self, widget: &impl IsA<gtk::Widget>) {
        let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        target.set_types(&[gdk::FileList::static_type(), gdk::Texture::static_type()]);
        let this = self.clone();
        target.connect_drop(move |_, value, _, _| {
            if let Ok(list) = value.get::<gdk::FileList>() {
                this.open_dropped_files(&list.files());
                true
            } else if let Ok(texture) = value.get::<gdk::Texture>() {
                this.target_for_drop().set_texture(&texture, None);
                true
            } else {
                false
            }
        });
        widget.add_controller(target);
    }

    /// Opens dropped files: images and PDFs only, the first one in this
    /// window if it's empty, and each other one in a window of its own (as
    /// Open With does), so nothing already marked up is replaced.
    fn open_dropped_files(&self, files: &[gio::File]) {
        let (wanted, skipped): (Vec<_>, Vec<_>) = files.iter().partition(|f| is_openable(f));
        for file in wanted {
            self.target_for_drop().load_file(file);
        }
        match skipped.as_slice() {
            [] => {}
            [one] => self.toast(&format!(
                "{} is not an image or a PDF",
                file_name(one).unwrap_or_else(|| "The file".into())
            )),
            many => self.toast(&format!("{} files are not images or PDFs", many.len())),
        }
    }

    /// This window while it's empty, otherwise a new one.
    fn target_for_drop(&self) -> MarkupWindow {
        if self.pages.borrow().is_none() {
            return self.clone();
        }
        let w = MarkupWindow::new(&self.app());
        w.present();
        w
    }

    /// The application this window belongs to.
    fn app(&self) -> adw::Application {
        self.window.application().and_downcast().expect("window has an app")
    }

    /// Shows the window and gives it focus. A pending top-bar activation
    /// token is used up here, so the window may take focus under Wayland and
    /// GNOME Shell's launch (busy pointer) completes.
    pub fn present(&self) {
        if let Some(token) = ACTIVATION_TOKEN.take() {
            self.window.set_startup_id(&token);
        }
        self.window.present();
    }

    /// The finished image, or a toast explaining why there isn't one.
    fn result(&self) -> Option<gdk::Texture> {
        match self.canvas.export() {
            Some(Ok(t)) => Some(t),
            Some(Err(e)) => {
                self.toast(&format!("Could not render the image: {e}"));
                None
            }
            None => {
                self.toast("There is no image to export yet");
                None
            }
        }
    }

    /// "page 3 " when several pages are open, so messages about the current
    /// page say which one.
    fn page_word(&self) -> String {
        match &*self.pages.borrow() {
            Some(p) if p.len() > 1 => format!("page {} ", p.current() + 1),
            _ => String::new(),
        }
    }

    /// Copy: puts the full-resolution result on the clipboard.
    fn copy(&self) {
        if let Some(tex) = self.result() {
            self.window.clipboard().set_texture(&tex);
            let what = self.page_word();
            self.toast(&format!("Copied {what}to clipboard"));
        }
    }

    /// The suggested file name: for a PDF, "name-marked.pdf" (the whole
    /// document) or "name-page3.png"; otherwise a time-stamped name.
    fn default_save_name(&self, format: SaveFormat) -> String {
        if let Some(p) = &*self.pages.borrow()
            && p.is_pdf()
            && let Some(name) = &p.name
        {
            let stem = std::path::Path::new(name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "document".into());
            return match format {
                SaveFormat::Pdf => format!("{stem}-marked.pdf"),
                _ if p.len() > 1 => format!("{stem}-page{}.{}", p.current() + 1, format.extension()),
                _ => format!("{stem}-marked.{}", format.extension()),
            };
        }
        crate::output::default_name(format.extension())
    }

    /// Save As in `format`: asks for a file name, then saves. Uses GTK's own
    /// dialog outside Flatpak and the file-chooser portal inside it.
    fn save_as(&self, format: SaveFormat) {
        if self.pages.borrow().is_none() {
            self.toast("There is no image to export yet");
            return;
        }
        self.canvas.commit();
        let folder = crate::settings::get().save_folder.unwrap_or_else(crate::output::pictures_dir);
        let name = self.default_save_name(format);
        if crate::autostart::in_flatpak() {
            self.save_with_portal(format, &folder, &name);
        } else {
            self.save_with_gtk_chooser(format, &folder, &name);
        }
    }

    /// One file filter per save format, for the dialogs' type lists.
    fn format_filters() -> [(SaveFormat, gtk::FileFilter); 3] {
        let png = gtk::FileFilter::new();
        png.set_name(Some("PNG Image"));
        png.add_suffix("png");
        let jpeg = gtk::FileFilter::new();
        jpeg.set_name(Some("JPEG Image"));
        jpeg.add_suffix("jpg");
        jpeg.add_suffix("jpeg");
        let pdf = gtk::FileFilter::new();
        pdf.set_name(Some("PDF Document"));
        pdf.add_suffix("pdf");
        [(SaveFormat::Png, png), (SaveFormat::Jpeg, jpeg), (SaveFormat::Pdf, pdf)]
    }

    /// GTK's own save dialog, so the file type list can rename the file as
    /// you change it. `GtkFileDialog` (the portal) can't: it neither renames
    /// the file nor reports the type picked. Deprecated in GTK 4.10, but still
    /// supported in GTK 4.
    #[allow(deprecated)]
    fn save_with_gtk_chooser(&self, format: SaveFormat, folder: &std::path::Path, name: &str) {
        let dialog = gtk::FileChooserDialog::new(
            Some("Save Image"),
            Some(&self.window),
            gtk::FileChooserAction::Save,
            &[("_Cancel", gtk::ResponseType::Cancel), ("_Save", gtk::ResponseType::Accept)],
        );
        dialog.set_modal(true);
        dialog.set_default_response(gtk::ResponseType::Accept);
        let filters = Self::format_filters();
        for (_, f) in &filters {
            dialog.add_filter(f);
        }
        if let Some((_, f)) = filters.iter().find(|(fmt, _)| *fmt == format) {
            dialog.set_filter(f);
        }
        let _ = dialog.set_current_folder(Some(&gio::File::for_path(folder)));
        dialog.set_current_name(name);
        let format_of = move |d: &gtk::FileChooserDialog| -> Option<SaveFormat> {
            let current = d.filter()?;
            filters.iter().find(|(_, f)| *f == current).map(|(fmt, _)| *fmt)
        };
        let format_of = Rc::new(format_of);
        {
            let format_of = format_of.clone();
            // Picking a type in the list rewrites the name's extension.
            dialog.connect_filter_notify(move |d| {
                let (Some(fmt), Some(name)) = (format_of(d), d.current_name()) else { return };
                let renamed = crate::settings::with_extension(&name, fmt);
                if renamed != name.as_str() {
                    d.set_current_name(&renamed);
                }
            });
        }
        let this = self.clone();
        dialog.connect_response(move |d, response| {
            let chosen = format_of(d).unwrap_or(format);
            let file = (response == gtk::ResponseType::Accept).then(|| d.file()).flatten();
            d.destroy();
            if let Some(file) = file {
                this.finish_save(&file, chosen);
            }
        });
        dialog.present();
    }

    /// Inside the Flatpak the save dialog must go through the portal, which
    /// can't rename the file to match a type picked in it, so it lists only
    /// the format chosen from the Save button's menu.
    fn save_with_portal(&self, format: SaveFormat, folder: &std::path::Path, name: &str) {
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        let filter = Self::format_filters()
            .into_iter()
            .find(|(fmt, _)| *fmt == format)
            .map(|(_, f)| f)
            .expect("every format has a filter");
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Save Image")
            .modal(true)
            .initial_name(name)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&this.window)).await {
                this.finish_save(&file, format);
            }
        });
    }

    /// Writes the result to `file` once a name has been chosen, then
    /// remembers the format and folder and shows a toast.
    fn finish_save(&self, file: &gio::File, chosen: SaveFormat) {
        let Some(path) = file.path() else { return };
        // An extension typed in the name wins; with none, use the chosen format.
        let used = SaveFormat::from_path(&path).unwrap_or(chosen);
        let result = if used == SaveFormat::Pdf && self.pages.borrow().as_ref().is_some_and(Pages::is_pdf) {
            // The whole document, every page with its marks.
            self.canvas.commit();
            let current = self.canvas.document();
            let pages = self.pages.borrow();
            let pages = pages.as_ref().expect("checked above");
            pages.save_pdf(&current, &path).map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e))
        } else {
            let Some(tex) = self.result() else { return };
            let ppp = self.pages.borrow().as_ref().map(|p| p.points_per_pixel(p.current())).unwrap_or(1.0);
            crate::output::save(&tex, &path, used, ppp)
        };
        match result {
            Ok(()) => {
                // A Flatpak document-portal path may not reveal its real
                // folder; then keep the previously remembered one.
                let folder = crate::settings::folder_of(file);
                crate::settings::update(|s| {
                    s.save_format = used;
                    if folder.is_some() {
                        s.save_folder = folder;
                    }
                });
                self.toast(&format!(
                    "Saved {}",
                    path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                ))
            }
            Err(e) => self.toast(&format!("Could not save: {}", e.message())),
        }
    }

    /// Enables the actions that depend on what is open: Save needs a file
    /// to write back to, and scaling only applies to pictures.
    fn update_actions(&self) {
        let (original, picture) = match &*self.pages.borrow() {
            Some(p) => (p.original.is_some(), !p.is_pdf()),
            None => (false, false),
        };
        let enable = |name: &str, on: bool| {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        };
        enable("save-original", original);
        enable("scale-to-visible", picture);
    }

    /// Enables the mark actions for the selected mark (none: all off).
    fn update_mark_actions(&self) {
        let mark = self.canvas.selected_mark();
        let enable = |name: &str, on: bool| {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        };
        enable("mark-edit", mark.is_some_and(|m| m.holds_text));
        enable("mark-halign", mark.is_some_and(|m| m.holds_text));
        enable("mark-valign", mark.is_some_and(|m| m.holds_text && !m.is_text));
        if let Some(m) = mark {
            for (name, id) in [("mark-halign", m.halign.id()), ("mark-valign", m.valign.id())] {
                if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                    a.set_state(&id.to_variant());
                }
            }
        }
        enable("mark-front", mark.is_some_and(|m| m.can_raise));
        enable("mark-back", mark.is_some_and(|m| m.can_lower));
        enable("mark-delete", mark.is_some());
    }

    /// The right-click menu: for a mark, what can be done to it; for the
    /// image, the output and size actions. Every item is also a button or
    /// shortcut elsewhere, as the GNOME guidelines ask.
    fn show_context_menu(&self, at: (f64, f64), mark: Option<crate::canvas::MarkInfo>) {
        let menu = gio::Menu::new();
        if let Some(m) = mark {
            if m.holds_text {
                let edit = gio::Menu::new();
                let label = if m.is_text || m.has_text { "Edit Text" } else { "Add Text" };
                edit.append(Some(label), Some("win.mark-edit"));
                // A Text mark only lines up its lines; shapes and lines also
                // place their text up and down (above or below a line).
                let align = gio::Menu::new();
                let h = gio::Menu::new();
                for (label, id) in [("Left", "left"), ("Centre", "center"), ("Right", "right")] {
                    h.append(Some(label), Some(&format!("win.mark-halign::{id}")));
                }
                align.append_section(None, &h);
                if !m.is_text {
                    let v = gio::Menu::new();
                    let vertical: &[(&str, &str)] = if m.is_line {
                        &[("Above Line", "top"), ("Below Line", "bottom")]
                    } else {
                        &[("Top", "top"), ("Middle", "middle"), ("Bottom", "bottom")]
                    };
                    for (label, id) in vertical {
                        v.append(Some(label), Some(&format!("win.mark-valign::{id}")));
                    }
                    align.append_section(None, &v);
                }
                edit.append_submenu(Some("Text Alignment"), &align);
                menu.append_section(None, &edit);
            }
            let order = gio::Menu::new();
            order.append(Some("Bring to Front"), Some("win.mark-front"));
            order.append(Some("Send to Back"), Some("win.mark-back"));
            menu.append_section(None, &order);
            let delete = gio::Menu::new();
            delete.append(Some("Delete"), Some("win.mark-delete"));
            menu.append_section(None, &delete);
        } else {
            let clip = gio::Menu::new();
            clip.append(Some("Copy"), Some("win.copy"));
            clip.append(Some("Mark Up Clipboard"), Some("win.paste"));
            menu.append_section(None, &clip);
            let save = gio::Menu::new();
            save.append(Some("Save (Replace Original)…"), Some("win.save-original"));
            save.append(Some("Save As…"), Some("win.save"));
            menu.append_section(None, &save);
            let view = gio::Menu::new();
            view.append(Some("Scale Image to Visible Size"), Some("win.scale-to-visible"));
            view.append(Some("Actual Size"), Some("win.zoom-actual"));
            view.append(Some("Zoom to Fit"), Some("win.zoom-fit"));
            menu.append_section(None, &view);
        }
        // A fresh popover each time, pointing at the click, parented to the
        // canvas and unparented again (from an idle, not inside its own
        // `closed` handler) once it closes.
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_parent(self.canvas.widget());
        popover.set_pointing_to(Some(&gdk::Rectangle::new(at.0 as i32, at.1 as i32, 1, 1)));
        let area = self.canvas.widget().clone();
        popover.connect_closed(move |p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
            area.grab_focus();
        });
        popover.popup();
    }

    /// Resamples the picture to the size it's shown at (see
    /// `Canvas::scale_to_visible`).
    fn scale_to_visible(&self) {
        match self.canvas.scale_to_visible() {
            Ok(Some((w, h))) => self.toast(&format!("Scaled the image to {w} × {h} pixels")),
            Ok(None) => self.toast("The image is already shown at its actual size"),
            Err(e) => self.toast(&e),
        }
    }

    /// Save: writes the result over the file it was opened from, in that
    /// file's format, after asking, since the original can't be got back.
    fn save_original(&self) {
        let Some(original) = self.pages.borrow().as_ref().and_then(|p| p.original.clone()) else {
            self.toast("This image wasn't opened from a file it can be saved back to. Use Save As");
            return;
        };
        self.canvas.commit();
        let name = original.name();
        let body = if original.format == crate::output::OriginalFormat::Known(SaveFormat::Pdf) {
            "The original PDF will be overwritten with every page and its marks. Links, form fields and bookmarks in it will be lost. This can't be undone."
        } else {
            "The original file will be overwritten with the marked-up image. This can't be undone."
        };
        let dialog = adw::AlertDialog::new(Some(&format!("Replace “{name}”?")), Some(body));
        dialog.add_responses(&[("cancel", "_Cancel"), ("replace", "_Replace")]);
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await == "replace" {
                this.write_original(&original);
            }
        });
    }

    /// Writes the result over the original file once the user has agreed:
    /// every page for a PDF, the current picture otherwise.
    fn write_original(&self, original: &crate::output::Original) {
        use crate::output::OriginalFormat;
        let result = match &original.format {
            OriginalFormat::Known(SaveFormat::Pdf) => {
                self.canvas.commit();
                let current = self.canvas.document();
                let pages = self.pages.borrow();
                match pages.as_ref() {
                    Some(p) if p.is_pdf() => original.replace(|path| {
                        p.save_pdf(&current, path).map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e))
                    }),
                    _ => return,
                }
            }
            format => {
                let Some(tex) = self.result() else { return };
                original.replace(|path| match format {
                    OriginalFormat::Known(f) => crate::output::save(&tex, path, *f, 1.0),
                    OriginalFormat::Pixbuf(name) => crate::output::save_pixbuf(&tex, path, name),
                })
            }
        };
        match result {
            Ok(()) => self.toast(&format!("Saved {}", original.name())),
            Err(e) => self.toast(&format!("Could not save: {}", e.message())),
        }
    }

    /// Send To → Open With…: hands a copy of the result to an app the user
    /// picks. Closing the chooser isn't an error.
    fn open_with(&self) {
        let Some(tex) = self.result() else { return };
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = crate::output::open_with(&tex, &this.window).await
                && !e.matches(gtk::DialogError::Dismissed) && !e.matches(gtk::DialogError::Cancelled) {
                    this.toast(&format!("Could not open: {}", e.message()));
                }
        });
    }

    /// Send To → Email…: starts an email with the result attached, or, for a
    /// mail app that can't take attachments, a blank email with the image on
    /// the clipboard to paste in.
    fn email(&self) {
        let Some(tex) = self.result() else { return };
        let this = self.clone();
        glib::spawn_future_local(async move {
            match crate::output::email(&tex).await {
                Ok(crate::output::EmailOutcome::Attached) => {}
                Ok(crate::output::EmailOutcome::PasteNeeded) => {
                    this.window.clipboard().set_texture(&tex);
                    let t = adw::Toast::new("Your mail app can't take attachments, so the image is on the clipboard. Paste it into the email");
                    t.set_timeout(8);
                    this.toasts.add_toast(t);
                }
                Err(e) => this.toast(&format!("Could not start an email: {}", e.message())),
            }
        });
    }

    /// Shows `msg` briefly at the bottom of the window.
    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Opens a picture (from the clipboard, or a file called `name`).
    pub fn set_texture(&self, texture: &gdk::Texture, name: Option<String>) {
        self.open_picture(texture, name, None);
    }

    /// Opens a single picture; `original` is the file Save can write back
    /// to, if any.
    fn open_picture(&self, texture: &gdk::Texture, name: Option<String>, original: Option<crate::output::Original>) {
        match crate::render::surface_from_texture(texture) {
            Ok(surface) => {
                let mut pages = Pages::picture(surface, name);
                pages.original = original;
                self.show_pages(pages)
            }
            Err(err) => self.toast(&format!("Could not load image: {err}")),
        }
    }

    /// Shows what was opened: its first page on the canvas, every page in
    /// the sidebar.
    fn show_pages(&self, mut pages: Pages) {
        let image = match pages.image(0) {
            Ok(i) => i,
            Err(e) => {
                self.toast(&format!("Could not show the page: {e}"));
                return;
            }
        };
        // Drop the old document's state; the new first page starts empty.
        self.canvas.swap_page(image, PageState::default());
        self.canvas.set_page_view(pages.points_per_pixel(0), pages.painter(0));
        self.canvas.zoom_fit();
        self.sidebar.rebuild(&pages);
        *self.pages.borrow_mut() = Some(pages);
        self.update_actions();
        self.update_title();
        self.stack.set_visible_child_name("image");
        self.canvas.widget().grab_focus();
        self.draw_thumbnails();
    }

    /// Puts page `to` (from 0) on the canvas, keeping the current page's marks
    /// and undo history for when you come back.
    fn go_to_page(&self, to: usize) {
        let image = {
            let mut pages = self.pages.borrow_mut();
            let Some(p) = pages.as_mut() else { return };
            if to >= p.len() || to == p.current() {
                return;
            }
            match p.image(to) {
                Ok(i) => i,
                Err(e) => {
                    drop(pages);
                    self.toast(&format!("Could not show page {}: {e}", to + 1));
                    self.sidebar.select(self.pages.borrow().as_ref().map_or(0, |p| p.current()));
                    return;
                }
            }
        };
        // Take the leaving page off the canvas (finishing any text edit), then
        // put the arriving page's own state on. Two swaps because the canvas
        // must give up the leaving state before `Pages::switch` can store it
        // and hand back the arriving one; the placeholder in between is
        // never drawn.
        let leaving = self.canvas.swap_page(image.clone(), PageState::default());
        let (left, arriving) = {
            let mut pages = self.pages.borrow_mut();
            let p = pages.as_mut().expect("checked above");
            let left = p.current();
            (left, p.switch(to, leaving).unwrap_or_default())
        };
        self.canvas.swap_page(image, arriving);
        if let Some(p) = self.pages.borrow().as_ref() {
            self.canvas.set_page_view(p.points_per_pixel(to), p.painter(to));
        }
        self.sidebar.refresh(left);
        self.sidebar.select(to);
        self.update_title();
        self.draw_thumbnails();
    }

    /// Moves the page at position `from` to position `to` (dragged in the
    /// sidebar). Its marks and undo history go with it; the page on the
    /// canvas stays there, and Save as PDF writes the new order.
    fn move_page(&self, from: usize, to: usize) {
        let current = {
            let mut pages = self.pages.borrow_mut();
            let Some(p) = pages.as_mut() else { return };
            if !p.move_page(from, to) {
                return;
            }
            p.current()
        };
        self.sidebar.move_row(from, to);
        self.sidebar.select(current);
        self.update_title();
        self.toast(&format!("Moved page {} to {}", from + 1, to + 1));
    }

    /// Deletes the pages at positions `indices` (from the sidebar: one, or
    /// all the selected ones), with an Undo button on the toast that puts
    /// them all back. Deleting the page on the canvas shows the nearest page
    /// that stays first, the next one if there is one. Save as PDF leaves
    /// deleted pages out.
    fn delete_pages(&self, mut indices: Vec<usize>) {
        let (len, current) = match self.pages.borrow().as_ref() {
            Some(p) if p.is_pdf() => (p.len(), p.current()),
            _ => return,
        };
        indices.sort_unstable();
        indices.dedup();
        indices.retain(|&i| i < len);
        if indices.is_empty() {
            return;
        }
        if indices.len() >= len {
            self.toast("A document needs at least one page");
            return;
        }
        if indices.contains(&current) {
            let keep = |i: &usize| !indices.contains(i);
            let Some(to) = (current + 1..len).find(keep).or_else(|| (0..current).rev().find(keep)) else { return };
            self.go_to_page(to);
            if self.pages.borrow().as_ref().map(Pages::current) != Some(to) {
                return; // The page couldn't be shown; go_to_page said why.
            }
        }
        let (id, deleted, current) = {
            let mut pages = self.pages.borrow_mut();
            let Some(p) = pages.as_mut() else { return };
            let Some(deleted) = p.delete_pages(&indices) else { return };
            (p.id(), deleted, p.current())
        };
        for &i in indices.iter().rev() {
            self.sidebar.remove_row(i);
        }
        self.sidebar.select(current);
        self.update_title();

        let msg = match indices.as_slice() {
            [one] => format!("Deleted page {}", one + 1),
            many => format!("Deleted {} pages", many.len()),
        };
        let toast = adw::Toast::new(&msg);
        toast.set_button_label(Some("_Undo"));
        // The toast's handler is Fn; the pages can only be put back once.
        let deleted = RefCell::new(Some(deleted));
        let w = self.clone();
        toast.connect_button_clicked(move |_| {
            if let Some(deleted) = deleted.borrow_mut().take() {
                w.restore_pages(id, deleted);
            }
        });
        self.toasts.add_toast(toast);
    }

    /// Undo for [`Self::delete_pages`]: puts the pages back where they were,
    /// with their marks and history, unless another document has been
    /// opened since.
    fn restore_pages(&self, id: u64, deleted: Vec<(usize, usize, PageState)>) {
        {
            let mut pages = self.pages.borrow_mut();
            let Some(p) = pages.as_mut().filter(|p| p.id() == id) else { return };
            for index in p.restore_pages(deleted) {
                self.sidebar.insert_row(p, index);
            }
            self.sidebar.select(p.current());
        }
        self.update_title();
        self.draw_thumbnails();
    }

    /// Moves to the previous, next, first or last page, stopping at the ends.
    fn step_page(&self, step: PageStep) {
        let target = {
            let pages = self.pages.borrow();
            let Some(p) = pages.as_ref() else { return };
            let (cur, last) = (p.current(), p.len() - 1);
            match step {
                PageStep::Previous => cur.saturating_sub(1),
                PageStep::Next => (cur + 1).min(last),
                PageStep::First => 0,
                PageStep::Last => last,
            }
        };
        self.go_to_page(target);
    }

    /// Draws waiting thumbnails a few at a time while the app is idle, so
    /// opening a long PDF doesn't freeze the window.
    fn draw_thumbnails(&self) {
        if self.sidebar.idle().borrow().is_some() || !self.sidebar.has_pending() {
            return;
        }
        let this = self.clone();
        let id = glib::idle_add_local(move || {
            // Work in slices of about 30 ms, then return to the main loop so
            // input and drawing stay responsive; GLib calls us again when idle.
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(30) {
                let Some(i) = this.sidebar.next_pending() else {
                    this.sidebar.idle().borrow_mut().take();
                    return glib::ControlFlow::Break;
                };
                // Busy (a page switch in progress): try again next idle.
                let Ok(pages) = this.pages.try_borrow() else { break };
                let Some(p) = pages.as_ref() else { continue };
                // The current page's marks live on the canvas, not in Pages.
                let doc = match p.doc(i) {
                    Some(d) => d.clone(),
                    None if i == p.current() => this.canvas.document(),
                    None => continue,
                };
                this.sidebar.draw(p, i, &doc);
            }
            glib::ControlFlow::Continue
        });
        *self.sidebar.idle().borrow_mut() = Some(id);
    }

    /// Sets the window and header title from the file name and page.
    fn update_title(&self) {
        let title = match &*self.pages.borrow() {
            Some(p) => match (&p.name, p.len()) {
                (Some(name), n) if n > 1 => format!("{name} (page {} of {n})", p.current() + 1),
                (Some(name), _) => name.clone(),
                (None, _) => "Pygoscribe".into(),
            },
            None => "Pygoscribe".into(),
        };
        self.window.set_title(Some(&title));
        self.title.set_title(&title);
        self.update_size();
    }

    /// Shows the result's size under the title: pixels for a picture, the
    /// page size in points for a PDF. Crops count.
    fn update_size(&self) {
        let ppp = match &*self.pages.borrow() {
            Some(p) if p.is_pdf() => Some(p.points_per_pixel(p.current())),
            Some(_) => None,
            None => {
                self.title.set_subtitle("");
                return;
            }
        };
        let Some((w, h)) = self.canvas.result_size() else { return };
        let mut size = match ppp {
            Some(k) => format!("{:.0} × {:.0} pt", w * k, h * k),
            None => format!("{w:.0} × {h:.0} px"),
        };
        if self.canvas.is_cropped() {
            size.push_str(" (cropped)");
        }
        self.title.set_subtitle(&size);
    }

    /// Reads an image from the clipboard. On Wayland a window may only read the
    /// clipboard once it has focus, so wait until the window is active.
    pub fn load_clipboard(&self) {
        if self.window.is_active() {
            self.read_clipboard();
            return;
        }
        // A one-shot handler: it disconnects itself the first time the window
        // becomes active. It needs its own ID, hence the shared cell.
        let handler = std::rc::Rc::new(std::cell::Cell::new(None));
        let id = self.window.connect_is_active_notify(glib::clone!(
            #[strong(rename_to = this)]
            self,
            #[strong]
            handler,
            move |w| {
                if w.is_active() {
                    if let Some(id) = handler.take() {
                        w.disconnect(id);
                    }
                    this.read_clipboard();
                }
            }
        ));
        handler.set(Some(id));
    }

    /// Reads the clipboard's image asynchronously and opens it.
    fn read_clipboard(&self) {
        let clipboard = self.window.clipboard();
        let this = self.clone();
        glib::spawn_future_local(async move {
            match clipboard.read_texture_future().await {
                Ok(Some(texture)) => this.set_texture(&texture, None),
                Ok(None) => this.toast("The clipboard does not contain an image"),
                Err(err) => this.toast(&format!("Could not read the clipboard: {err}")),
            }
        });
    }

    /// Opens `file`: a PDF (every page) or any image gdk-pixbuf/GDK can read.
    pub fn load_file(&self, file: &gio::File) {
        if crate::pdf::is_pdf(file) {
            self.load_pdf(file);
            return;
        }
        match gdk::Texture::from_file(file) {
            Ok(texture) => self.open_picture(&texture, file_name(file), crate::output::Original::detect(file, false)),
            Err(err) => self.toast(&format!("Could not open image: {err}")),
        }
    }

    /// Opens every page of a PDF.
    fn load_pdf(&self, file: &gio::File) {
        let pdf = match crate::pdf::Pdf::open(file) {
            Ok(pdf) => pdf,
            Err(e) => {
                let msg = if e.matches(poppler::Error::Encrypted) {
                    "This PDF is password protected".to_string()
                } else {
                    format!("Could not open the PDF: {}", e.message())
                };
                self.toast(&msg);
                return;
            }
        };
        if pdf.n_pages() < 1 {
            self.toast("The PDF has no pages");
            return;
        }
        let mut pages = Pages::pdf(pdf, file_name(file));
        pages.original = crate::output::Original::detect(file, true);
        self.show_pages(pages);
    }
}

/// A relative page move, for the keyboard shortcuts.
#[derive(Clone, Copy)]
enum PageStep {
    /// Page Up, Alt+Up.
    Previous,
    /// Page Down, Alt+Down.
    Next,
    /// Home.
    First,
    /// End.
    Last,
}

/// Whether a dropped file looks like something Pygoscribe can open: a PDF or
/// any image type (guessed from the name, then from the first bytes).
fn is_openable(file: &gio::File) -> bool {
    if crate::pdf::is_pdf(file) {
        return true;
    }
    let head = file
        .read(gio::Cancellable::NONE)
        .and_then(|s| s.read_bytes(4096, gio::Cancellable::NONE))
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let (kind, _) = gio::content_type_guess(file.basename(), head.as_slice());
    kind.starts_with("image/")
}

/// The file's base name, for the title and suggested save names.
fn file_name(file: &gio::File) -> Option<String> {
    file.basename().map(|n| n.to_string_lossy().to_string())
}

// The pending top-bar activation token. GTK is single-threaded, so a
// thread-local on the main thread is enough.
thread_local! {
    /// Taken (and so used at most once) by [`MarkupWindow::present`].
    static ACTIVATION_TOKEN: Cell<Option<String>> = const { Cell::new(None) };
}

/// Makes the next window presented use `token`, the XDG activation token the
/// top bar sent with a click. On Wayland that lets the window take focus,
/// and it tells GNOME Shell the launch it started for the click is done, so
/// the busy pointer stops instead of spinning until it times out.
pub fn set_activation_token(token: Option<String>) {
    ACTIVATION_TOKEN.set(token);
}

/// Uses up a top-bar activation token for a menu item that opens no window
/// (Quit, Start at Login). GNOME Shell shows a busy pointer from the click
/// until the launch the token belongs to completes.
///
/// GTK only completes a launch when a window uses the token, so first pass it
/// to mutter directly through gtk_shell1 (see `launch`), which needs no
/// window. If that fails (X11, or a compositor without gtk_shell1), fall back
/// to briefly showing a tiny transparent window with the token.
pub async fn release_activation_token(app: &adw::Application, token: Option<String>) {
    let Some(token) = token else {
        glib::g_debug!("pygoscribe", "no activation token to release");
        return;
    };
    // launch::complete does blocking Wayland I/O, so run it on a worker
    // thread rather than stall the main loop.
    let t = token.clone();
    match gio::spawn_blocking(move || crate::launch::complete(&t)).await {
        Ok(Ok(())) => {
            glib::g_debug!("pygoscribe", "launch completed through gtk_shell");
            return;
        }
        Ok(Err(err)) => glib::g_debug!("pygoscribe", "gtk_shell launch completion failed: {err}"),
        Err(_) => glib::g_debug!("pygoscribe", "gtk_shell launch completion panicked"),
    }
    glib::g_debug!("pygoscribe", "releasing activation token {token}");
    // Fallback: a 1×1, undecorated, transparent window that presents the
    // token, waits to be activated (which completes the launch), and closes.
    let win = gtk::Window::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .default_width(1)
        .default_height(1)
        .css_classes(["pygoscribe-token"])
        .build();
    let css = gtk::CssProvider::new();
    css.load_from_string("window.pygoscribe-token { background: transparent; box-shadow: none; }");
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::WidgetExt::display(&win),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    win.set_startup_id(&token);
    let (tx, rx) = async_channel::bounded::<()>(1);
    win.connect_is_active_notify(move |w| {
        if w.is_active() {
            let _ = tx.try_send(());
        }
    });
    win.present();
    // Don't hang if the compositor never activates it.
    match glib::future_with_timeout(std::time::Duration::from_millis(1500), rx.recv()).await {
        Ok(_) => glib::g_debug!("pygoscribe", "token window activated"),
        Err(_) => glib::g_debug!("pygoscribe", "token window not activated within 1.5 s"),
    }
    // A short grace period so the compositor has processed the activation
    // before the window disappears.
    glib::timeout_future(std::time::Duration::from_millis(200)).await;
    win.destroy();
}

/// Shows a file chooser and loads the result into `target`. With no target,
/// it opens a new window first and the chooser sits on top of it: a window
/// is needed to use the top bar's activation token, and it's where the image
/// goes anyway. Cancelling closes that new window again.
pub fn choose_and_open(app: &adw::Application, target: Option<&MarkupWindow>) {
    // Every format gdk-pixbuf has a loader for, plus PDF (by type and by
    // name, since some file systems don't report MIME types).
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images and PDFs"));
    filter.add_pixbuf_formats();
    filter.add_mime_type("application/pdf");
    filter.add_suffix("pdf");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);

    let dialog = gtk::FileDialog::builder()
        .title("Open Image")
        .modal(true)
        .filters(&filters)
        .default_filter(&filter)
        .build();
    if let Some(folder) = crate::settings::get().open_folder {
        dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
    }

    let (win, is_new) = match target {
        Some(t) => (t.clone(), false),
        None => {
            let w = MarkupWindow::new(app);
            w.present();
            (w, true)
        }
    };
    glib::spawn_future_local(async move {
        let Ok(file) = dialog.open_future(Some(&win.window)).await else {
            if is_new {
                win.window.close();
            }
            return; // cancelled
        };
        if let Some(folder) = crate::settings::folder_of(&file) {
            crate::settings::update(|s| s.open_folder = Some(folder));
        }
        win.load_file(&file);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `bytes` to a fresh temporary file called `name`.
    fn temp_file(name: &str, bytes: &[u8]) -> gio::File {
        let dir = std::env::temp_dir().join(format!("pygoscribe-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        gio::File::for_path(path)
    }

    #[test]
    fn dropped_files_are_opened_only_if_images_or_pdfs() {
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        assert!(is_openable(&temp_file("shot.png", PNG)));
        // Content counts when the name says nothing.
        assert!(is_openable(&temp_file("shot", PNG)));
        assert!(is_openable(&temp_file("doc", b"%PDF-1.7\n")));
        assert!(is_openable(&temp_file("doc.PDF", b"")));
        assert!(!is_openable(&temp_file("notes.txt", b"hello")));
        assert!(!is_openable(&temp_file("archive.zip", b"PK\x03\x04")));
    }
}
