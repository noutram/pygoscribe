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

//! The About window: version, license (GPL-3.0-only) and the licenses of the
//! libraries compiled in or linked. This is where GPL-3 §5(d)'s "Appropriate
//! Legal Notices" are shown.

use adw::prelude::*;
use gtk::glib;

/// The project page, used for the website and issue-tracker links.
const REPO_URL: &str = "https://github.com/noutram/pygoscribe";

/// Builds the About dialog: app details, the GPL-3.0-only license text
/// (libadwaita shows it from `License::Gpl30Only`), and extra legal sections
/// for Poppler, the GNOME stack and the bundled Rust crates.
fn dialog() -> adw::AboutDialog {
    let about = adw::AboutDialog::builder()
        .application_name("Pygoscribe")
        .application_icon(crate::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Free software, provided as is, without any warranty.")
        .developer_name("Nicholas Outram")
        .developers(["Nicholas Outram"])
        .copyright("© 2026 Nicholas Outram")
        .license_type(gtk::License::Gpl30Only)
        .website(REPO_URL)
        .issue_url(format!("{REPO_URL}/issues"))
        .build();
    about.add_legal_section(
        "Poppler",
        Some("© The Poppler developers"),
        gtk::License::Gpl20,
        None,
    );
    about.add_legal_section(
        "GTK, libadwaita, GLib, Pango and Cairo",
        Some("© The GTK and GNOME developers, © The Cairo developers"),
        gtk::License::Lgpl21,
        None,
    );
    about.add_legal_section(
        "Rust libraries",
        None,
        gtk::License::Custom,
        Some(
            "Pygoscribe includes Rust libraries (crates) under the MIT, Apache-2.0, \
             Unlicense and Unicode-3.0 licenses. Their copyright notices and \
             license texts are in THIRD-PARTY-LICENSES.md, installed in \
             /usr/share/doc/pygoscribe/ by the .deb and in \
             /app/share/licenses/org.outram.Pygoscribe/ inside the Flatpak.\n\n\
             The complete source code of each release, including these \
             libraries, is published next to its packages on the release page.",
        ),
    );
    about
}

/// Shows the About window over `parent`, or as a window of its own.
pub fn show(parent: Option<&gtk::Widget>) {
    dialog().present(parent);
}

/// Shows the About window on its own (from the top bar), using the top bar's
/// activation token so it takes focus and the busy pointer stops.
pub fn show_with_token(app: &adw::Application, token: Option<String>) {
    let about = dialog();
    // With no parent, AdwDialog wraps itself in a window of its own.
    about.present(None::<&gtk::Widget>);
    let Some(token) = token else { return };
    match about.root().and_downcast::<gtk::Window>() {
        // On a mapped window this hands the token to the compositor
        // (xdg_activation), which focuses it and completes the launch.
        Some(win) => win.set_startup_id(&token),
        // No window to give the token to: just end the launch so the
        // busy pointer stops.
        None => {
            let app = app.clone();
            glib::spawn_future_local(async move {
                crate::window::release_activation_token(&app, Some(token)).await;
            });
        }
    }
}
