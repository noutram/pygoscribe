#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Nicholas Outram
# SPDX-License-Identifier: GPL-3.0-only
#
# Builds Pygoscribe, runs the tests and makes every package in dist/:
#   dist/pygoscribe_<version>_amd64.deb   (cargo-deb)
#   dist/pygoscribe.flatpak               (flatpak-builder bundle)
# Before building it checks licensing (cargo deny, reuse lint) and
# regenerates THIRD-PARTY-LICENSES.md (cargo about).
#
# Usage:
#   ./install.sh                  build everything
#   ./install.sh --install deb    ...then install the .deb (asks for sudo)
#   ./install.sh --install flatpak ...then install the Flatpak for your user
#   ./install.sh --skip-flatpak   only build the .deb
#   ./install.sh --skip-deb       only build the Flatpak
set -euo pipefail

APP_ID=org.outram.Pygoscribe
MANIFEST=build-aux/flatpak/$APP_ID.json
FLATHUB=https://flathub.org/repo/flathub.flatpakrepo
GNOME_VERSION=50
RUST_EXT_VERSION=25.08

cd "$(dirname "$(readlink -f "$0")")"

install_target=""
build_deb=1
build_flatpak=1
while [[ $# -gt 0 ]]; do
    case "$1" in
        --install) install_target="${2:-}"; shift 2 ;;
        --skip-deb) build_deb=0; shift ;;
        --skip-flatpak) build_flatpak=0; shift ;;
        -h|--help) sed -n '5,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $1 (try --help)" >&2; exit 1 ;;
    esac
done
case "$install_target" in
    ""|deb|flatpak) ;;
    *) echo "--install takes deb or flatpak" >&2; exit 1 ;;
esac

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33mWarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

mkdir -p dist

# --- Native build and tests ---------------------------------------------------
step "Checking build dependencies"
command -v cargo >/dev/null || die "cargo not found. Install Rust from https://rustup.rs"
missing=()
for pc in gtk4 libadwaita-1 poppler-glib; do
    pkg-config --exists "$pc" 2>/dev/null || missing+=("$pc")
done
if [[ ${#missing[@]} -gt 0 ]]; then
    die "missing ${missing[*]}. Run: sudo apt install build-essential libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev"
fi

# --- Licensing ----------------------------------------------------------------------
# Pygoscribe is GPL-3.0-only: every crate compiled in must have a compatible
# license (deny.toml), every file must say how it is licensed (REUSE.toml and
# SPDX headers), and the third-party notices shipped in both packages must
# match Cargo.lock.
step "Checking licenses"
cargo_tool() { # <command> <crate> [install args...]
    local cmd=$1 crate=$2; shift 2
    if ! cargo "$cmd" --version >/dev/null 2>&1; then
        echo "$crate not found, installing it (one time)..."
        cargo install --locked "$crate" "$@"
    fi
}
cargo_tool deny cargo-deny
cargo_tool about cargo-about --features cli
if ! command -v reuse >/dev/null; then
    if command -v uv >/dev/null; then
        uv tool install reuse
    elif command -v pipx >/dev/null; then
        pipx install reuse
    fi
    command -v reuse >/dev/null || die "reuse not found. Install it with: pipx install reuse (sudo apt install pipx)"
fi
cargo deny --log-level error check licenses
if [[ -d vendor && ! -e .git ]]; then
    # An unpacked release source tarball: the vendored crates carry their own
    # license files, which REUSE.toml doesn't describe.
    echo "Source tarball with vendor/: skipping reuse lint"
else
    reuse lint --quiet || { reuse lint | tail -n 30; die "reuse lint failed: a file is missing license information (add an SPDX header or a REUSE.toml entry)"; }
fi
cargo about generate --fail about.hbs -o THIRD-PARTY-LICENSES.md
echo "Licenses OK; THIRD-PARTY-LICENSES.md regenerated"

step "Running tests"
cargo test --release --locked

step "Building release binary"
cargo build --release --locked

# --- .deb -----------------------------------------------------------------------
deb_file=""
if [[ $build_deb == 1 ]]; then
    step "Building .deb"
    if ! command -v cargo-deb >/dev/null; then
        echo "cargo-deb not found, installing it (one time)..."
        cargo install cargo-deb --locked
    fi
    deb_file=$(cargo deb --no-build --locked | tail -n 1)
    cp "$deb_file" dist/
    deb_file="dist/$(basename "$deb_file")"
    echo "Made $deb_file"
fi

# --- Flatpak --------------------------------------------------------------------
flatpak_file=""
if [[ $build_flatpak == 1 ]]; then
    step "Building Flatpak"
    if ! command -v flatpak-builder >/dev/null; then
        warn "flatpak-builder not found, skipping the Flatpak. Install it with: sudo apt install flatpak-builder"
    else
        flatpak remote-add --user --if-not-exists flathub "$FLATHUB"
        flatpak install --user --noninteractive flathub \
            "org.gnome.Sdk//$GNOME_VERSION" "org.gnome.Platform//$GNOME_VERSION" \
            "org.freedesktop.Sdk.Extension.rust-stable//$RUST_EXT_VERSION"
        flatpak-builder --user --force-clean --repo=repo build-dir "$MANIFEST"
        flatpak build-bundle repo dist/pygoscribe.flatpak "$APP_ID" --runtime-repo="$FLATHUB"
        flatpak_file=dist/pygoscribe.flatpak
        echo "Made $flatpak_file"
    fi
fi

# --- Optional install -------------------------------------------------------------
case "$install_target" in
    deb)
        [[ -n $deb_file ]] || die "no .deb was built, so it can't be installed"
        step "Installing $deb_file"
        sudo apt install -y "./$deb_file"
        ;;
    flatpak)
        [[ -n $flatpak_file ]] || die "no Flatpak was built, so it can't be installed"
        step "Installing $flatpak_file"
        flatpak install --user --noninteractive --reinstall "$flatpak_file"
        ;;
esac

step "Done"
ls -lh dist/
if [[ -z $install_target ]]; then
    echo
    [[ -n $deb_file ]] && echo "Install the .deb with:      sudo apt install ./$deb_file"
    [[ -n $flatpak_file ]] && echo "Install the Flatpak with:  flatpak install --user $flatpak_file"
    true
fi
