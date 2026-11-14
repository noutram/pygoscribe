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

//! Start at login.
//!
//! Outside a sandbox this writes (or removes) an XDG autostart entry in
//! `~/.config/autostart`, which GNOME's session manager reads at login. A
//! snap would do the same in its own config folder
//! (`~/snap/pygoscribe/current/.config/autostart`), where snapd looks for the
//! entry named by `autostart:` in snapcraft.yaml. (Snap packaging has been
//! dropped, but the detection is harmless and kept.) Inside Flatpak the app
//! can't see the host's folder, so it asks the Background portal instead,
//! which writes the entry on the host, and remembers the answer in a marker
//! file in its own config folder, because the portal has no way to ask
//! whether autostart is currently on.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

/// The flag the autostart entry passes, so the app starts quietly in the top
/// bar and waits for the top bar to be ready instead of opening a window.
pub const BACKGROUND_FLAG: &str = "--background";

/// Whether we are running inside Flatpak. Flatpak always mounts
/// `/.flatpak-info` in the sandbox.
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Whether we are running inside a snap: snapd sets `$SNAP` for its apps.
pub fn in_snap() -> bool {
    std::env::var_os("SNAP").is_some()
}

/// Flatpak or snap: other apps can't be run directly, so use portals.
pub fn sandboxed() -> bool {
    in_flatpak() || in_snap()
}

/// `~/.config/autostart/org.outram.Pygoscribe.desktop` (or the sandbox's
/// own config folder when sandboxed).
fn desktop_file() -> PathBuf {
    glib::user_config_dir()
        .join("autostart")
        .join(format!("{}.desktop", crate::APP_ID))
}

/// Inside Flatpak: a marker file in the sandbox's own config folder.
pub fn sandbox_marker() -> PathBuf {
    glib::user_config_dir().join("pygoscribe").join("autostart-enabled")
}

/// Whether Start at Login is on, as far as this process can tell.
pub fn is_enabled() -> bool {
    if in_flatpak() {
        sandbox_marker().exists()
    } else {
        // Respect an entry the user switched off elsewhere (e.g. GNOME Tweaks).
        std::fs::read_to_string(desktop_file()).is_ok_and(|text| entry_is_enabled(&text))
    }
}

/// The command the autostart entry runs: the installed `pygoscribe` if that is
/// what is running, otherwise the full path (for example a `cargo build`).
fn exec_line() -> String {
    let exe = std::env::current_exe().ok();
    // Use the bare name when the binary on $PATH is this very binary
    // (comparing canonical paths so symlinks match).
    let on_path = exe.as_ref().is_some_and(|exe| {
        glib::find_program_in_path("pygoscribe")
            .and_then(|p| p.canonicalize().ok())
            .zip(exe.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
    });
    let program = match exe {
        // snapd swaps the program for the snap's own launcher anyway.
        _ if in_snap() => "pygoscribe".into(),
        Some(exe) if !on_path => {
            let s = exe.display().to_string();
            // Desktop Entry Exec quoting: wrap in double quotes and escape
            // backslashes and quotes when the path has reserved characters.
            if s.contains([' ', '"', '\\', '\'']) {
                format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                s
            }
        }
        _ => "pygoscribe".into(),
    };
    format!("{program} {BACKGROUND_FLAG}")
}

/// The contents of the autostart `.desktop` file. `NoDisplay` keeps it out of
/// the app grid; `X-GNOME-Autostart-enabled` is what GNOME Tweaks toggles.
fn entry_text() -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Pygoscribe\n\
         Comment=Mark up images from the clipboard or a file\n\
         Exec={}\n\
         Icon={}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_line(),
        crate::APP_ID
    )
}

/// Wraps an I/O (or any displayable) error as a `glib::Error`, so both the
/// file and the portal paths return one error type.
fn io_err(e: impl std::fmt::Display) -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string())
}

/// Turns start at login on or off. Returns the state that is now in effect.
pub async fn set_enabled(enable: bool) -> Result<bool, glib::Error> {
    if in_flatpak() {
        let granted = request_background(enable).await?;
        let marker = sandbox_marker();
        // Turning it off always clears the marker, even if the portal said
        // no; turning it on only sets it if the user allowed it.
        if granted && enable {
            std::fs::create_dir_all(marker.parent().expect("has parent")).map_err(io_err)?;
            std::fs::write(&marker, b"").map_err(io_err)?;
        } else if granted || !enable {
            let _ = std::fs::remove_file(&marker);
        }
        return Ok(is_enabled());
    }

    write_entry(enable)?;
    Ok(is_enabled())
}

/// Outside Flatpak: writes or removes the XDG autostart entry.
pub fn write_entry(enable: bool) -> Result<(), glib::Error> {
    let path = desktop_file();
    if enable {
        std::fs::create_dir_all(path.parent().expect("has parent")).map_err(io_err)?;
        std::fs::write(&path, entry_text()).map_err(io_err)?;
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io_err(e)),
            _ => {}
        }
    }
    Ok(())
}

/// Whether the text of an autostart entry leaves it switched on.
pub fn entry_is_enabled(text: &str) -> bool {
    !text.lines().any(|l| {
        let l = l.trim().replace(' ', "");
        l == "Hidden=true" || l == "X-GNOME-Autostart-enabled=false"
    })
}

/// Asks the Background portal to add or remove the autostart entry, and waits
/// for the answer. Returns whether the request was granted.
#[allow(deprecated)] // signal_subscribe: its replacement needs a newer GLib
async fn request_background(autostart: bool) -> Result<bool, glib::Error> {
    // Portal requests follow the org.freedesktop.portal.Request pattern: the
    // method returns a request object path at once, and the real answer
    // comes later as a Response signal on that object (0 = granted,
    // 1 = cancelled by the user, 2 = other failure).
    let conn = gio::bus_get_future(gio::BusType::Session).await?;

    // Subscribe to the Response signal before calling, using the predictable
    // request path, so the answer can't arrive before we listen.
    let token = format!("pygoscribe{}", glib::random_int());
    // The request path is .../request/<sender>/<token>, where <sender> is our
    // unique bus name without the leading ':' and with '.' as '_'.
    let sender = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

    let (tx, rx) = async_channel::bounded::<u32>(1);
    let sub = conn.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&request_path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            let code = params.child_value(0).get::<u32>().unwrap_or(2);
            let _ = tx.try_send(code);
        },
    );

    let options = glib::VariantDict::new(None);
    options.insert_value("handle_token", &token.to_variant());
    options.insert_value("reason", &"Start Pygoscribe in the top bar when you log in".to_variant());
    options.insert_value("autostart", &autostart.to_variant());
    options.insert_value("commandline", &vec!["pygoscribe", BACKGROUND_FLAG].to_variant());
    // The first argument is the parent window handle; there is none.
    let params = ("", options.end()).to_variant();

    let call = conn
        .call_future(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Background",
            "RequestBackground",
            Some(&params),
            Some(glib::VariantTy::new("(o)").expect("valid type")),
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await;
    let result = match call {
        // The call succeeded; now wait for the user's answer.
        Ok(_) => Ok(rx.recv().await.unwrap_or(2) == 0),
        Err(e) => Err(e),
    };
    conn.signal_unsubscribe(sub);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry must start the app quietly, not open a window at login.
    #[test]
    fn entry_runs_in_background() {
        let text = entry_text();
        assert!(text.starts_with("[Desktop Entry]\n"));
        let exec = text.lines().find(|l| l.starts_with("Exec=")).unwrap();
        assert!(exec.ends_with(" --background"), "{exec}");
    }
}
