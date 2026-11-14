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

//! Top-bar icon using the StatusNotifierItem protocol, shown on Ubuntu by the
//! built-in AppIndicator extension.
//!
//! GTK4 has no tray API, so the icon is served over D-Bus by the `ksni`
//! crate. `ksni`'s blocking API runs the D-Bus service on a thread of its
//! own; menu clicks arrive on that thread, so [`MarkupTray`] never touches
//! GTK. It sends a [`Request`] to the main thread instead (see
//! `start_tray` in `main.rs`).

use crate::Request;
use async_channel::Sender;
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};

/// The tray icon's state, owned by the `ksni` thread.
pub struct MarkupTray {
    /// Where clicks are sent, to be handled on the GTK main thread.
    tx: Sender<Request>,
    /// Whether "Start at Login" shows a tick. The app updates this through
    /// the handle after it has changed the setting.
    pub autostart: bool,
}

impl MarkupTray {
    /// Queues `req` for the main thread. The channel is unbounded, so this
    /// only fails once the app has stopped listening, when there is nothing
    /// useful to do about it.
    fn send(&self, req: Request) {
        let _ = self.tx.try_send(req);
    }

    /// Sends a request that opens a window, with the top bar's XDG activation
    /// token. GNOME's AppIndicator extension hands one over with each click;
    /// the token both lets the window take focus and ends the busy pointer
    /// the shell shows for it. It can arrive just after the click, so wait
    /// briefly for it.
    fn send_with_token(&self, req: fn(Option<String>) -> Request) {
        let mut token = ksni::current_xdg_activation_token();
        for _ in 0..20 {
            if token.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            token = ksni::current_xdg_activation_token();
        }
        gtk::glib::g_debug!("pygoscribe", "top-bar click, activation token: {token:?}");
        self.send(req(token));
    }

    /// A plain menu item that sends `req` (with the activation token) when
    /// clicked. `_` in the label marks the mnemonic.
    fn item(label: &str, icon: &str, req: fn(Option<String>) -> Request) -> MenuItem<Self> {
        StandardItem {
            label: label.into(),
            icon_name: icon.into(),
            activate: Box::new(move |t: &mut Self| t.send_with_token(req)),
            ..Default::default()
        }
        .into()
    }
}

/// What the StatusNotifierItem host (the AppIndicator extension) asks for.
impl ksni::Tray for MarkupTray {
    /// The item's ID, which the host uses to tell icons apart.
    fn id(&self) -> String {
        crate::APP_ID.into()
    }

    /// The tooltip and accessible name.
    fn title(&self) -> String {
        "Pygoscribe".into()
    }

    /// A symbolic theme icon, so it takes the top bar's colour.
    fn icon_name(&self) -> String {
        "document-edit-symbolic".into()
    }

    /// A primary click on the icon itself. The AppIndicator extension
    /// normally opens the menu instead, but other hosts may call this.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send_with_token(Request::MarkupClipboard);
    }

    /// The icon's menu. ksni calls this again after each `Handle::update`,
    /// which is how the Start at Login tick follows the setting.
    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            Self::item("Mark Up _Clipboard", "edit-paste-symbolic", Request::MarkupClipboard),
            Self::item("_Open Image…", "document-open-symbolic", Request::OpenImage),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Start at _Login".into(),
                checked: self.autostart,
                activate: Box::new(|t: &mut Self| t.send_with_token(Request::ToggleAutostart)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            Self::item("_About", "help-about-symbolic", Request::About),
            StandardItem {
                label: "_Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|t: &mut Self| t.send_with_token(Request::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Shows the top-bar icon. With `wait_for_top_bar`, a missing top bar isn't
/// an error: the icon appears once the AppIndicator extension is ready, which
/// at login can be a few seconds after the app starts.
pub fn spawn(
    tx: Sender<Request>,
    autostart: bool,
    wait_for_top_bar: bool,
) -> Result<ksni::blocking::Handle<MarkupTray>, ksni::Error> {
    MarkupTray { tx, autostart }
        // Flatpak doesn't let apps own the StatusNotifierItem-* bus names.
        .disable_dbus_name(crate::autostart::in_flatpak())
        .assume_sni_available(wait_for_top_bar)
        .spawn()
}
