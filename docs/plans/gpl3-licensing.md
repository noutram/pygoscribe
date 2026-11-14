# Plan: license Pygoscribe under GPL-3.0-only

Status: **implemented** in 0.1.14 (14 Nov 2026), except optional step 17
(offline Flatpak cargo sources), which was left for later. Written 14 Nov 2026.

## Decision

Nick chose **GPL-3.0-only** (SPDX `GPL-3.0-only`, `gtk::License::Gpl30Only`)
on 14 Nov 2026. Copyright holder: **Nicholas Outram**.

`LGPL-2.1-or-later.txt` stays as is: that identifier is for GTK's
gtk-shell.xml, not for Pygoscribe.

## Where things stand today

| Place | Now | Problem |
|---|---|---|
| `LICENSE` / `COPYING` | missing | no license grant at all |
| `Cargo.toml` `license` | missing | `cargo metadata` reports no license |
| `data/org.outram.Pygoscribe.metainfo.xml` `project_license` | `LicenseRef-proprietary` | says the opposite of the decision |
| `[package.metadata.deb]` | `copyright = "2026, Nick"`, no license file | .deb has no proper `/usr/share/doc/pygoscribe/copyright` |
| Source files | no headers | no per-file notice |
| About dialog | none (no main menu exists) | GPL-3 §5(d) wants legal notices shown in an interactive UI |
| README | no license section | |
| Third-party notices | none shipped | MIT/Apache crates are compiled into the binary and their licenses ask for the notice to travel with it |
| Source for the binaries | Gitea tag archives only | they lack the vendored crates and the poppler source the Flatpak bundles |
| `data/protocol/gtk-shell.xml` | copied from GTK, no header | it is LGPL-2.1-or-later (GTK), needs to be marked as such |

## Compatibility check (done for this plan)

I ran `cargo metadata --locked` on the current `Cargo.lock` (139 packages).
Every crate is GPL-3 compatible:

- **MIT**: the whole gtk-rs stack (glib, gio, gtk4, gdk4, gsk4, cairo-rs,
  pango, pangocairo, gdk-pixbuf, graphene), libadwaita-rs, poppler-rs,
  wayland-client/scanner/backend, zbus/zvariant, tokio, tracing, quick-xml,
  winnow, and others.
- **MIT OR Apache-2.0** (and variants with Unlicense, LGPL-2.1-or-later or
  the LLVM exception): serde, syn, futures, rustix, libc, bitflags and others.
  Apache-2.0 is compatible with GPL-3 (it is not with GPL-2-only, which is
  one reason to stay on 3).
- **Unlicense**: ksni.
- **(MIT OR Apache-2.0) AND Unicode-3.0**: unicode-ident (build-time only, but
  harmless).
- Nothing GPL-2.0-only, nothing proprietary, nothing unlicensed.

Native libraries:

- **poppler** (libpoppler, libpoppler-glib): GPL-2.0-or-later overall, with
  the xpdf-derived code under "GPL-2 or GPL-3". Both allow GPL-3, so a
  GPL-3.0-only Pygoscribe linking poppler is fine. The combined binary is
  effectively distributed under GPL-3.0. This is the dependency that makes
  GPL the right call anyway: a proprietary app couldn't link poppler.
- GTK 4, libadwaita, GLib, Pango, gdk-pixbuf, graphene: LGPL-2.1-or-later.
  Cairo: LGPL-2.1 or MPL-1.1. All compatible, and all dynamically linked.

## Changes, in order

### 1. License text and project metadata

1. Add `LICENSE` at the repo root with the unmodified GPL-3.0 text from
   <https://www.gnu.org/licenses/gpl-3.0.txt>.
2. Add `LICENSES/` (REUSE layout) holding `GPL-3.0-only.txt`,
   `LGPL-2.1-or-later.txt` (for gtk-shell.xml) and `CC0-1.0.txt` (for the
   metainfo metadata). `reuse download --all` fetches them.
3. `Cargo.toml`:
   ```toml
   license = "GPL-3.0-only"
   authors = ["Nicholas Outram <email>"]
   repository = "https://github.com/noutram/pygoscribe"
   publish = false   # stops an accidental crates.io publish
   ```
4. Metainfo: `<project_license>GPL-3.0-only</project_license>`; keep
   `<metadata_license>CC0-1.0</metadata_license>` (Flathub requires a
   permissive metadata license). Add `<developer id="org.outram">` with a
   `<name>`, and `<url type="vcs-browser">`. Add a line to the next
   `<release>` saying the app is now free software under GPL-3.0-only.

### 2. Per-file notices (REUSE style)

5. Put a two-line SPDX header at the top of every file Nick owns:
   ```rust
   // SPDX-FileCopyrightText: 2026 Nicholas Outram
   // SPDX-License-Identifier: GPL-3.0-only
   ```
   Files: `src/*.rs`, `build.rs`, `install.sh`, `scripts/release.sh`,
   `build-aux/flatpak/*.json` (JSON can't hold comments, so it goes in
   `REUSE.toml` instead).
6. Add `REUSE.toml` for files that can't carry a header or aren't Nick's:
   - `data/icons/**/*.svg`, `data/*.desktop`, `docs/**`, `*.md`,
     `Cargo.lock`, `build-aux/**`: GPL-3.0-only, Nick.
   - `data/org.outram.Pygoscribe.metainfo.xml`: CC0-1.0.
   - `data/protocol/gtk-shell.xml`: LGPL-2.1-or-later, "The GTK Project
     Authors". It came from GTK and stays under GTK's license.
7. Check with `reuse lint` (pipx install reuse). It should report the project
   compliant.

### 3. Third-party notices shipped with the binaries

8. Add `about.toml` + `about.hbs` and generate
   `THIRD-PARTY-LICENSES.md` with `cargo about generate`. It lists every
   compiled-in crate with its full license text, which MIT, Apache-2.0 and
   Unicode-3.0 all require to accompany binaries. Add a short hand-written
   section to it for poppler (GPL-2.0-or-later, source link) since it isn't
   a crate.
9. Ship it in both packages (steps 11 and 13) and link it from the About
   dialog.

### 4. In-app notices (About dialog)

10. The app has no main menu yet. Add a primary menu button
    (`open-menu-symbolic`) at the end of the header bar with **About
    Markup**, and an **About** item in the tray menu. The action shows an
    `adw::AboutDialog` (available with the crate's existing `v1_5` feature):
    - `application_name` "Markup", `application_icon` `org.outram.Pygoscribe`,
      `version` from `env!("CARGO_PKG_VERSION")`, `developer_name`.
    - `copyright` "© 2026 Nicholas Outram".
    - `license_type(gtk::License::Gpl30Only)`, which shows the GPL-3.0-only
      summary, the no-warranty statement and a link to the full text. That
      covers GPL-3 §5(d) "Appropriate Legal Notices".
    - `website` and `issue_url` pointing at the Gitea repo.
    - `add_legal_section("Poppler", Some("© The Poppler developers"),
      gtk::License::Gpl20, None)` and one section saying the Rust crates'
      licenses are in THIRD-PARTY-LICENSES.md.
    - Optionally `add_credit_section` for libraries.

### 5. README

11. Add a "License" section: GPL-3.0-only, copyright line, a one-line
    "no warranty" note, pointers to `LICENSE`, `THIRD-PARTY-LICENSES.md` and
    where to get the source of each release (step 15).

### 6. .deb packaging

12. Add a DEP-5 machine-readable `debian/copyright` and install it as
    `usr/share/doc/pygoscribe/copyright` through `assets`. Stanzas:
    `Files: *` GPL-3.0-only (Nick); `data/protocol/gtk-shell.xml`
    LGPL-2.1-or-later; `data/org.outram.Pygoscribe.metainfo.xml` CC0-1.0; one
    stanza noting the statically linked Rust crates with a pointer to
    THIRD-PARTY-LICENSES.md. Per Debian policy the GPL-3 body is referenced
    as `/usr/share/common-licenses/GPL-3` rather than copied.
13. Add `THIRD-PARTY-LICENSES.md` to `assets` under
    `usr/share/doc/pygoscribe/`. Don't set cargo-deb's `license-file`; the
    explicit copyright asset replaces the file cargo-deb would generate.
14. The .deb links Ubuntu's own libpoppler, GTK etc. Those are system
    libraries Ubuntu ships with source, so Nick doesn't have to supply their
    source with the .deb. He does have to supply the source of the Rust
    crates compiled into the binary (step 15).

### 7. Flatpak packaging

15. Install the license files into the app:
    `install -Dm644 LICENSE THIRD-PARTY-LICENSES.md -t /app/share/licenses/org.outram.Pygoscribe/`
    in the pygoscribe module's build-commands. flatpak-builder also
    collects COPYING/LICENSE from modules such as poppler into
    `/app/share/licenses/`; check that poppler's ends up there after a
    build and add an explicit install line if it doesn't.
16. The .flatpak bundle contains a poppler Nick built, so he distributes
    poppler's object code and owes its Corresponding Source (GPL-2/3), as
    well as Pygoscribe's. The GNOME 50 runtime isn't inside the bundle
    (users get it from Flathub, which provides its source), so that isn't
    Nick's obligation.
17. Recommended, not strictly required: replace the network-enabled
    `cargo build` (`--share=network`) with offline sources generated by
    `flatpak-cargo-generator.py Cargo.lock -o build-aux/flatpak/cargo-sources.json`.
    Then the manifest names every input exactly, `flatpak-builder
    --bundle-sources` can produce a complete source bundle in one step,
    and the manifest is Flathub-ready if that ever happens.

### 8. Source for every release (the important part)

GPL-3 §6 says whoever conveys object code must also make the
**Corresponding Source** available. The simplest option is §6(d): offer it
"in the same way through the same place" as the binaries, at no extra charge.
For Pygoscribe that place is the release page, so attach the source to
the same release as the .deb and .flatpak.

Corresponding Source here means: the Pygoscribe source at the tag, the build
scripts and Flatpak manifest (already in the repo), the exact source of every
crate compiled in, and for the .flatpak the poppler source tarball.

18. Extend `scripts/release.sh` to build and upload, alongside the .deb and
    .flatpak:
    - `pygoscribe-<v>-src.tar.xz`: `git archive <tag>` plus `cargo vendor`
      output in `vendor/` and a `.cargo/config.toml` that points cargo at it,
      so `cargo build --offline --locked` works from the tarball alone.
    - `poppler-26.11.0.tar.xz`: the exact archive named in the manifest,
      downloaded and checked against its sha256, so the source survives
      even if poppler.freedesktop.org moves it.
    - (With step 17 done, a single `pygoscribe-<v>-sources.flatpak` from
      `flatpak-builder --bundle-sources` can replace these two for the
      Flatpak.)
19. Release notes on each release say: "Source: pygoscribe-<v>-src.tar.xz
    and poppler-26.11.0.tar.xz on this page. License GPL-3.0-only."
20. **The private-LAN caveat.** The Gitea server at `http://pi5:3000` only
    works on Nick's LAN. That is fine while the binaries are only downloaded
    from that same page, because the source sits in the same place under the
    same access. It stops being enough as soon as a .deb or .flatpak travels
    any other way (emailed, put on a USB stick, uploaded elsewhere, or the
    Gitea is exposed publicly while the source isn't). A written offer
    pointing at `pi5` wouldn't help, since nobody outside can reach it, and a
    §6(b) written offer also binds Nick for three years. So the rule is:
    **a binary never leaves without its source tarball(s) next to it.**
    If the project goes public, mirror the repo and release assets somewhere
    reachable (Codeberg, GitHub, Flathub) and the same rule then holds there.
21. Note for completeness: GPL obligations only apply when a copy is given to
    someone else. Nick installing his own builds triggers none of this, but
    doing it on every release keeps the pre-releases safe to share.

### 9. Guard rails

22. Add `deny.toml` for cargo-deny with an allow-list: `GPL-3.0-only`
    (own crate), `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`,
    `Unlicense`, `Unicode-3.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`,
    `Zlib`, `MPL-2.0`, `LGPL-2.1-or-later`. Anything else (notably
    `GPL-2.0-only`, `AGPL`, `SSPL`, unknown) fails.
23. Run `cargo deny check licenses` and `reuse lint` in `install.sh` before
    the build, so a new dependency with a bad license stops a release.
    Regenerate THIRD-PARTY-LICENSES.md in the same step so it can't go stale.

### 10. First GPL release

24. Do all of the above on a `feature/gpl3-license` branch, merge to master,
    and release it as **v0.1.14** through `scripts/release.sh`, which now
    uploads .deb, .flatpak, source tarball and poppler source.
25. Releases 0.1.0 to 0.1.13 are marked proprietary in their metainfo. As
    the sole copyright holder Nick can simply relicense; nothing needs
    re-releasing. Optionally add a line to their Gitea pages saying v0.1.14
    is the first GPL release and that the same source is available from the
    tags.
26. Update `CLAUDE.md` / `docs/PROJECT_CONTEXT.md` with the license decision
    and the "binaries never ship without source" rule.

## Decisions made

1. Copyright holder: Nicholas Outram (email optional in the SPDX line).
2. GPL-3.0-only, not "or later".
3. Step 17 (offline Flatpak cargo sources): later, not part of 0.1.14.

## Checklist when done

- [x] `reuse lint` passes
- [x] `cargo deny check licenses` passes
- [x] About dialog shows GPL-3.0-only and the poppler section
- [x] `dpkg -c` on the .deb shows `/usr/share/doc/pygoscribe/copyright` and THIRD-PARTY-LICENSES.md
- [x] `/app/share/licenses/org.outram.Pygoscribe/` holds LICENSE, THIRD-PARTY-LICENSES.md and poppler/COPYING, COPYING3 (flatpak-builder collects poppler's itself)
- [x] The v0.1.14 release has four assets: .deb, .flatpak, `-src.tar.xz`, poppler tarball
- [x] `cargo build --offline --locked` works from the unpacked `-src.tar.xz`
