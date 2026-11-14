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

pub const APP_ID: &str = "org.outram.Pygoscribe";

/// Requests that can reach the app from the tray icon or the command line.
#[derive(Debug, Clone)]
pub enum Request {
    /// With the top bar's XDG activation token, if it sent one.
    MarkupClipboard(Option<String>),
    OpenImage(Option<String>),
    ToggleAutostart(Option<String>),
    About(Option<String>),
    Quit(Option<String>),
}

fn main() -> glib::ExitCode {
    gio::resources_register_include!("pygoscribe.gresource").expect("bundled resources");
    // Settings and Start at Login from releases before 0.1.16.
    let migrated = migrate::run();
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

    app.connect_startup(move |app| {
        gtk::Window::set_default_icon_name(APP_ID);
        setup_actions(app);
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
        if !cmdline.is_remote() && !opts.contains("no-tray") {
            start_tray(app, opts.contains("background"));
        }
        let files: Vec<gio::File> = cmdline
            .arguments()
            .iter()
            .skip(1)
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
        glib::ExitCode::SUCCESS
    });

    app.connect_activate(|app| {
        window::MarkupWindow::new(app).present();
    });

    app.connect_open(|app, files, _| {
        for file in files {
            let win = window::MarkupWindow::new(app);
            win.present();
            win.load_file(file);
        }
    });

    app.run()
}

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
    match glib::MainContext::default().block_on(autostart::set_enabled(enable)) {
        Ok(now) => {
            println!("Start at login is {}.", if now { "on" } else { "off" });
            if now == enable { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE }
        }
        Err(err) => {
            eprintln!("pygoscribe: could not change start at login: {err}");
            glib::ExitCode::FAILURE
        }
    }
}

fn start_tray(app: &adw::Application, background: bool) {
    let (tx, rx) = async_channel::unbounded::<Request>();
    match tray::spawn(tx, autostart::is_enabled(), background) {
        Ok(handle) => {
            // Keep the app (and the tray) running with no windows open.
            let guard = app.hold();
            let app = app.clone();
            glib::spawn_future_local(async move {
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
            if !background {
                app.activate();
            }
        }
    }
}
