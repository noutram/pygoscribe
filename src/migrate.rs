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

//! Carries settings and Start at Login over from the app's earlier ID, used
//! up to 0.1.15 (app ID [`OLD_APP_ID`], config folder [`OLD_DIR`]).
//!
//! Runs once: a stamp file in the new config folder stops it repeating, so a
//! later change made in Pygoscribe is never undone by the old files.
//!
//! - .deb and `cargo run`: copies `~/.config/gtkmarkup/settings.ini`, and
//!   swaps `~/.config/autostart/org.outram.GtkMarkup.desktop` (which runs the
//!   old `gtkmarkup` binary, gone once the .deb is upgraded) for a new entry.
//! - Flatpak: reads the old app's config folder through a read-only
//!   `--filesystem` permission. The old host autostart entry is outside the
//!   sandbox, so the caller asks the Background portal for a new one; the old
//!   entry goes away with `flatpak uninstall org.outram.GtkMarkup` or by hand.

use crate::autostart;
use gtk::glib;
use std::path::{Path, PathBuf};

/// The application ID used up to 0.1.15.
const OLD_APP_ID: &str = "org.outram.GtkMarkup";
/// The config folder name used up to 0.1.15.
const OLD_DIR: &str = "gtkmarkup";
/// The current config folder name.
const NEW_DIR: &str = "pygoscribe";
/// A file in the new config folder that marks the migration as done.
const STAMP: &str = "migrated-from-gtkmarkup";

/// What is left to do once the app is running.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Inside Flatpak: Start at Login was on under the old ID, so ask the portal.
    pub flatpak_autostart: bool,
}

/// Moves anything the old version left behind. Errors are only logged: a
/// failed migration must never stop the app from starting.
pub fn run() -> Pending {
    let new_dir = glib::user_config_dir().join(NEW_DIR);
    if new_dir.join(STAMP).exists() {
        return Pending::default();
    }
    let pending = if autostart::in_flatpak() {
        migrate_flatpak(&new_dir)
    } else {
        migrate_host(&glib::user_config_dir(), &new_dir)
    };
    // A portal request still to make is only recorded once it has been made
    // (`finish`), so `pygoscribe --version` can't use up the migration.
    if !pending.flatpak_autostart {
        finish();
    }
    pending
}

/// Records that the migration is done, so it never runs again.
pub fn finish() {
    let new_dir = glib::user_config_dir().join(NEW_DIR);
    if let Err(e) = std::fs::create_dir_all(&new_dir).and_then(|()| std::fs::write(new_dir.join(STAMP), b"")) {
        glib::g_warning!("pygoscribe", "Could not record the settings migration: {e}");
    }
}

/// Copies `old/settings.ini` to `new/settings.ini` unless the new one exists.
fn copy_settings(old_dir: &Path, new_dir: &Path) {
    let (old, new) = (old_dir.join("settings.ini"), new_dir.join("settings.ini"));
    if !old.is_file() || new.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(new_dir).and_then(|()| std::fs::copy(&old, &new)) {
        glib::g_warning!("pygoscribe", "Could not copy settings from the old version: {e}");
    }
}

/// The .deb and `cargo run` case: copy the settings, and replace an enabled
/// old autostart entry with a new one (unless one exists already).
fn migrate_host(config: &Path, new_dir: &Path) -> Pending {
    copy_settings(&config.join(OLD_DIR), new_dir);
    let old_entry = config.join("autostart").join(format!("{OLD_APP_ID}.desktop"));
    if let Ok(text) = std::fs::read_to_string(&old_entry) {
        if autostart::entry_is_enabled(&text) && !autostart::is_enabled() {
            if let Err(e) = autostart::write_entry(true) {
                glib::g_warning!("pygoscribe", "Could not carry over Start at Login: {}", e.message());
                return Pending::default();
            }
        }
        // Remove it even when it was switched off: it points at a binary that
        // no longer exists.
        let _ = std::fs::remove_file(&old_entry);
    }
    Pending::default()
}

/// The old Flatpak's config folder, `~/.var/app/org.outram.GtkMarkup/config/gtkmarkup`.
fn old_flatpak_dir() -> PathBuf {
    glib::home_dir().join(".var/app").join(OLD_APP_ID).join("config").join(OLD_DIR)
}

/// The Flatpak case: copy the settings through the read-only permission and
/// report whether the old app had Start at Login on.
fn migrate_flatpak(new_dir: &Path) -> Pending {
    let old_dir = old_flatpak_dir();
    copy_settings(&old_dir, new_dir);
    Pending {
        flatpak_autostart: old_dir.join("autostart-enabled").exists()
            && !autostart::sandbox_marker().exists(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder for one test, unique to this process.
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pygoscribe-migrate-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Settings are copied the first time and never overwrite newer ones.
    #[test]
    fn copies_settings_once() {
        let dir = temp("settings");
        let (old, new) = (dir.join("old"), dir.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.ini"), "[style]\nwidth=3\n").unwrap();
        copy_settings(&old, &new);
        assert_eq!(std::fs::read_to_string(new.join("settings.ini")).unwrap(), "[style]\nwidth=3\n");

        // An existing new file is never overwritten.
        std::fs::write(old.join("settings.ini"), "[style]\nwidth=9\n").unwrap();
        copy_settings(&old, &new);
        assert_eq!(std::fs::read_to_string(new.join("settings.ini")).unwrap(), "[style]\nwidth=3\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With nothing to migrate, nothing is created.
    #[test]
    fn missing_old_settings_is_fine() {
        let dir = temp("none");
        copy_settings(&dir.join("old"), &dir.join("new"));
        assert!(!dir.join("new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
