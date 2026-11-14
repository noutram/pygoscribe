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

//! Ends a GNOME Shell launch, and so its busy pointer, without a window.
//!
//! # Why this exists
//!
//! When the user clicks a top-bar menu item, GNOME Shell starts a "launch"
//! (startup notification) and shows a busy pointer until the launch
//! completes or times out. The AppIndicator extension passes the launch's
//! XDG activation token to us with the click. Normally the token is handed
//! to a new window (`gtk::Window::set_startup_id`), which both focuses the
//! window and completes the launch. Some clicks open no window, though
//! (Start at Login, Quit, a cancelled file chooser), and then nothing would
//! complete the launch.
//!
//! Mutter also completes a launch when any client passes its token to the
//! private `gtk_shell1.set_startup_id` request. GTK doesn't expose that
//! request publicly, so this module opens its own short-lived Wayland
//! connection, binds `gtk_shell1`, sends the token and does one roundtrip to
//! make sure the compositor has received it before the connection closes.
//! The connection is separate from GTK's, so it doesn't disturb GTK's state.
//!
//! On X11, or on a compositor without `gtk_shell1`, [`complete`] simply
//! returns an error and the caller ignores it.
//!
//! `data/protocol/gtk-shell.xml` is GTK's protocol description (LGPL), copied
//! from GTK's `gdk/wayland/protocol`.

/// Client bindings generated at compile time from the protocol XML by
/// `wayland-scanner`. The generated code trips many lints, hence the allows.
#[allow(
    dead_code,
    non_camel_case_types,
    unused_unsafe,
    unused_variables,
    non_upper_case_globals,
    non_snake_case,
    unused_imports,
    clippy::all
)]
mod protocol {
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("data/protocol/gtk-shell.xml");
    }
    use self::__interfaces::*;
    use wayland_client;
    use wayland_client::protocol::*;
    wayland_scanner::generate_client_code!("data/protocol/gtk-shell.xml");
}

use protocol::gtk_shell1::{self, GtkShell1};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

/// Dispatch state for the event queue. We only send a request and never
/// need any events, so it holds nothing.
struct State;

/// Registry events (globals appearing or going) are ignored: the global list
/// was already read by `registry_queue_init`.
impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// `gtk_shell1` sends only a capabilities event, which we don't need.
impl Dispatch<GtkShell1, ()> for State {
    fn event(
        _: &mut Self,
        _: &GtkShell1,
        _: gtk_shell1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Completes the launch `token` belongs to. Fails outside Wayland or when the
/// compositor has no gtk_shell1.
pub fn complete(token: &str) -> Result<(), String> {
    // Uses WAYLAND_DISPLAY / WAYLAND_SOCKET; fails cleanly under X11.
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    // Version 1 is all set_startup_id needs.
    let shell: GtkShell1 = globals
        .bind(&queue.handle(), 1..=1, ())
        .map_err(|e| e.to_string())?;
    shell.set_startup_id(Some(token.to_owned()));
    // Flushes the request and waits for the compositor to process it, so
    // dropping the connection afterwards can't lose it.
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    Ok(())
}
