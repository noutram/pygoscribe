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

//! Pygoscribe: mark up images from the clipboard, from files, or whole PDFs.
//!
//! This is the entry point. It builds the [`adw::Application`], declares the
//! command-line options, wires up the app-level actions and starts the
//! top-bar (StatusNotifierItem) icon.
//!
//! # Process model
//!
//! Pygoscribe is a single-instance `GApplication` with the ID
//! [`APP_ID`]. The first process to start becomes the *primary instance*: it
//! owns the D-Bus name, the tray icon and every window. Any later invocation
//! (for example `pygoscribe --clipboard` bound to a keyboard shortcut) is a
//! *remote* instance: GApplication forwards its command line to the primary
//! over D-Bus and the remote process exits straight away.
//!
//! # Module map
//!
//! - [`window`]: the markup window (header bar, tool bar, menus, file I/O).
//! - [`canvas`]: the drawing area, tools, selection, hit testing and editing.
//! - [`document`]: the mark model ([`document::Shape`]) and per-page undo.
//! - [`render`]: draws marks and the base image with cairo.
//! - [`pages`] / [`pdf`] / [`sidebar`]: multi-page PDF documents, rendering
//!   with Poppler, vector PDF export and the page-thumbnail sidebar.
//! - [`view`]: zoom and scroll state.
//! - [`output`]: copy, save, Open With… and Email….
//! - [`tray`]: the top-bar icon (via `ksni`).
//! - [`launch`]: Wayland launch feedback (the gtk_shell protocol).
//! - [`autostart`]: Start at Login, for the .deb and for Flatpak.
//! - [`settings`]: remembered colours, fonts, folders and so on.
//! - [`migrate`]: carries settings over from the app's earlier IDs.
//! - [`about`]: the About window.

mod about;
mod autostart;
mod launch;
mod migrate;
mod canvas;
mod document;
mod output;
mod pages;
mod pdf;
mod render;
mod settings;
mod sidebar;
mod tray;
mod view;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};

/// The application ID: the D-Bus name, the .desktop file name, the icon name
/// and the Flatpak ID all use it.
pub const APP_ID: &str = "org.outram.Pygoscribe";

/// Requests that can reach the app from the tray icon or the command line.
///
/// The tray runs on its own thread (see [`tray`]), so it can't touch GTK
/// directly. It sends one of these over an `async_channel` instead, and
/// [`start_tray`] handles it on the GTK main loop.
///
/// Each variant carries the XDG activation token that GNOME Shell attached to
/// the click, if it sent one. Passing the token on to the window that opens
/// lets that window take focus under Wayland, and releasing it when no
/// window opens stops the busy pointer (see [`window::set_activation_token`]
/// and [`window::release_activation_token`]).
#[derive(Debug, Clone)]
pub enum Request {
    /// Open a window on the clipboard image.
    MarkupClipboard(Option<String>),
    /// Show the file chooser and open what the user picks.
    OpenImage(Option<String>),
    /// Flip Start at Login and update the tray menu's tick.
    ToggleAutostart(Option<String>),
    /// Show the About window.
    About(Option<String>),
    /// Remove the tray icon and quit.
    Quit(Option<String>),
}

/// Registers resources, migrates old settings, declares the options and runs
/// the application's main loop.
fn main() -> glib::ExitCode {
    // The app icon is compiled into the binary by build.rs; registering the
    // resource bundle makes it findable by icon name.
    gio::resources_register_include!("pygoscribe.gresource").expect("bundled resources");
    // Settings and Start at Login from releases before 0.1.16.
    let migrated = migrate::run();
    // HANDLES_OPEN lets file arguments reach `connect_open`; HANDLES_COMMAND_LINE
    // forwards a remote instance's whole command line to the primary, so
    // options like --clipboard work against an already-running tray instance.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.add_main_option(
        "clipboard",
        b'c'.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Mark up the image on the clipboard",
        None,
    );
    app.add_main_option(
        "no-tray",
        0.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Do not show the top-bar icon",
        None,
    );

    app.add_main_option(
        "window",
        0.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Open a window as well as the top-bar icon (used by the app launcher)",
        None,
    );
    app.add_main_option(
        "background",
        0.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start quietly in the top bar, waiting for it if needed (used at login)",
        None,
    );
    app.add_main_option(
        "autostart",
        0.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Turn start at login on or off",
        Some("on|off"),
    );

    app.add_main_option(
        "version",
        b'v'.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Show the version, copyright and license, and exit",
        None,
    );

    // Runs in the process that was started, before it registers with D-Bus.
    // --version and --autostart are handled here and exit at once, so they
    // never reach (or start) the primary instance.
    app.connect_handle_local_options(|_, opts| {
        if opts.contains("version") {
            print!("{}", version_text());
            return std::ops::ControlFlow::Break(glib::ExitCode::SUCCESS);
        }
        match opts.lookup::<String>("autostart").ok().flatten() {
            Some(value) => std::ops::ControlFlow::Break(set_autostart_from_cli(&value)),
            None => std::ops::ControlFlow::Continue(()),
        }
    });

    // Runs once, in the primary instance only.
    app.connect_startup(move |app| {
        gtk::Window::set_default_icon_name(APP_ID);
        setup_actions(app);
        // A Flatpak autostart entry under the old app ID can't be renamed in
        // place: it has to be requested again through the Background portal,
        // which needs the main loop, so it happens here rather than in migrate::run.
        if migrated.flatpak_autostart {
            glib::spawn_future_local(async {
                if let Err(err) = autostart::set_enabled(true).await {
                    eprintln!("pygoscribe: could not carry over start at login: {err}");
                }
                migrate::finish();
            });
        }
    });

    // The first instance owns the tray icon; later invocations (e.g. from a keyboard
    // shortcut running `pygoscribe --clipboard`) are forwarded here by GApplication.
    app.connect_command_line(|app, cmdline| {
        let opts = cmdline.options_dict();
        // Only the primary instance's own command line starts the tray; a
        // forwarded one finds the tray already there.
        if !cmdline.is_remote() && !opts.contains("no-tray") {
            start_tray(app, opts.contains("background"));
        }
        // Resolve file arguments against the *caller's* working directory,
        // which may differ from the primary instance's.
        let files: Vec<gio::File> = cmdline
            .arguments()
            .iter()
            .skip(1) // argv[0]
            .map(|a| cmdline.create_file_for_arg(a))
            .collect();
        if !files.is_empty() {
            app.open(&files, "");
        } else if opts.contains("clipboard") {
            app.activate_action("markup-clipboard", None);
        } else if cmdline.is_remote() || opts.contains("no-tray") || opts.contains("window") {
            // From the app launcher (--window), a window must appear: it ends
            // the launch feedback, which otherwise spins until it times out.
            app.activate();
        }
        // Plain `pygoscribe` (or --background) in the primary instance opens
        // nothing: the app sits in the top bar.
        glib::ExitCode::SUCCESS
    });

    // An empty markup window, ready for Open or Paste.
    app.connect_activate(|app| {
        window::MarkupWindow::new(app).present();
    });

    // One window per file, whether from the command line, Open With in the
    // file manager, or a forwarded remote instance.
    app.connect_open(|app, files, _| {
        for file in files {
            let win = window::MarkupWindow::new(app);
            win.present();
            win.load_file(file);
        }
    });

    app.run()
}

/// Adds the `app.*` actions shared by the tray, the command line and every
/// window's menus: `markup-clipboard`, `open-image`, `about` and `quit`
/// (Ctrl+Q).
fn setup_actions(app: &adw::Application) {
    let clip = gio::SimpleAction::new("markup-clipboard", None);
    clip.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            let win = window::MarkupWindow::new(&app);
            win.present();
            win.load_clipboard();
        }
    ));
    app.add_action(&clip);

    let open = gio::SimpleAction::new("open-image", None);
    open.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| window::choose_and_open(&app, None)
    ));
    app.add_action(&open);

    // From a window's main menu; the tray uses Request::About.
    let about_action = gio::SimpleAction::new("about", None);
    about_action.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            let parent = app.active_window().map(|w| w.upcast::<gtk::Widget>());
            about::show(parent.as_ref());
        }
    ));
    app.add_action(&about_action);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| app.quit()
    ));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

/// What `pygoscribe --version` prints.
fn version_text() -> String {
    format!(
        "pygoscribe {}\n\
         Copyright © 2026 Nicholas Outram\n\
         License GPL-3.0-only: GNU GPL version 3 <https://www.gnu.org/licenses/gpl-3.0.html>\n\
         This is free software, provided as is, WITHOUT ANY WARRANTY, to the extent permitted by law.\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// `pygoscribe --autostart=on|off`: changes the setting in this process and
/// exits, without touching a running instance.
fn set_autostart_from_cli(value: &str) -> glib::ExitCode {
    let enable = match value.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => true,
        "off" | "no" | "false" | "0" => false,
        _ => {
            eprintln!("pygoscribe: --autostart takes on or off");
            return glib::ExitCode::FAILURE;
        }
    };
    // No main loop is running yet (this is called from handle-local-options),
    // so drive the async portal call to completion here.
    match glib::MainContext::default().block_on(autostart::set_enabled(enable)) {
        Ok(now) => {
            // `now` is the state actually in effect: the portal may refuse.
            println!("Start at login is {}.", if now { "on" } else { "off" });
            if now == enable { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE }
        }
        Err(err) => {
            eprintln!("pygoscribe: could not change start at login: {err}");
            glib::ExitCode::FAILURE
        }
    }
}

/// Starts the top-bar icon and the loop that handles its [`Request`]s.
///
/// The tray thread sends requests down an unbounded channel; this function
/// spawns a future on the GTK main context that receives them and acts on
/// them, so all GTK work stays on the main thread. When the tray says Quit
/// (or its channel closes) the loop ends and the app quits.
///
/// `background` is true at login (`--background`): if the tray can't be
/// created then, no window is opened as a fallback, because the top bar may
/// simply not be ready yet (the tray module waits for it in that case).
fn start_tray(app: &adw::Application, background: bool) {
    let (tx, rx) = async_channel::unbounded::<Request>();
    match tray::spawn(tx, autostart::is_enabled(), background) {
        Ok(handle) => {
            // Keep the app (and the tray) running with no windows open.
            let guard = app.hold();
            let app = app.clone();
            glib::spawn_future_local(async move {
                // Moved into the future so the hold lasts as long as the loop.
                let _guard = guard;

                while let Ok(req) = rx.recv().await {
                    match req {
                        Request::MarkupClipboard(token) => {
                            window::set_activation_token(token);
                            app.activate_action("markup-clipboard", None)
                        }
                        Request::OpenImage(token) => {
                            window::set_activation_token(token);
                            app.activate_action("open-image", None)
                        }
                        Request::ToggleAutostart(token) => {
                            let enable = !autostart::is_enabled();
                            let now = match autostart::set_enabled(enable).await {
                                Ok(now) => now,
                                Err(err) => {
                                    eprintln!("pygoscribe: could not change start at login: {err}");
                                    autostart::is_enabled()
                                }
                            };
                            handle.update(|t| t.autostart = now);
                            // After the tick changes, so ending the busy
                            // pointer (which may wait on a window) can't delay it.
                            window::release_activation_token(&app, token).await;
                        }
                        Request::About(token) => about::show_with_token(&app, token),
                        Request::Quit(token) => {
                            window::release_activation_token(&app, token).await;
                            break;
                        }
                    }
                }
                app.quit();
            });
        }
        Err(err) => {
            eprintln!("pygoscribe: could not create top-bar icon: {err}");
            // Without an icon the user would have no way in, so show a window.
            if !background {
                app.activate();
            }
        }
    }
}
