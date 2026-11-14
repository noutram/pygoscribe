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
//! The top bar starts a launch for each click and gives us its token.
//! Mutter completes the launch when a client passes the token to
//! gtk_shell1.set_startup_id, so do that on a short-lived Wayland connection.
//!
//! `data/protocol/gtk-shell.xml` is GTK's protocol description (LGPL), copied
//! from GTK's `gdk/wayland/protocol`.

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

struct State;

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
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let shell: GtkShell1 = globals
        .bind(&queue.handle(), 1..=1, ())
        .map_err(|e| e.to_string())?;
    shell.set_startup_id(Some(token.to_owned()));
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    Ok(())
}
