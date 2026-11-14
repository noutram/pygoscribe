#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Nicholas Outram
# SPDX-License-Identifier: GPL-3.0-only
#
# Builds the release files for a tag and attaches them to its release on both
# GitHub and Gitea (pi5):
#   pygoscribe_<version>-1_amd64.deb
#   pygoscribe-<version>.flatpak
#   pygoscribe-<version>-src.tar.xz   source at the tag + vendored crates
#   poppler-<x.y.z>.tar.xz           the Poppler source the Flatpak bundles
#
# Usage:
#   scripts/release.sh v0.1.16            build from the tag and upload all four
#   scripts/release.sh v0.1.16 --no-upload  only build (files land in dist/release/)
#
# The tag must already exist (and be pushed). The build runs install.sh in a
# clean export of the tag, so local changes never leak into a release.
# Pygoscribe is GPL-3.0-only: a binary never ships without its source next to
# it, so the source tarballs go on the same release page as the packages.
# `cargo build --offline --locked` works from the unpacked -src tarball.
# A release that doesn't exist yet is created as a pre-release. Existing
# assets with the same name are replaced, so rerunning is safe.
# Needs $GITHUB_TOKEN or $PYGOSCRIBE (Contents: read and write on the repo) and $GITEA (a
# Gitea API token); both are read from ~/.profile if unset. A missing token
# skips that host and makes the script fail at the end.
set -euo pipefail

GITHUB_REPO=noutram/pygoscribe
GITEA_URL=http://pi5:3000
GITEA_REPO=noutram/gtkMarkUp

tag="${1:-}"
upload=1
[[ "${2:-}" == "--no-upload" ]] && upload=0
[[ -n $tag ]] || { sed -n '5,24p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

cd "$(dirname "$(readlink -f "$0")")/.."
root=$PWD
git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "No tag $tag" >&2; exit 1; }
version=${tag#v}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git archive "$tag" | tar -x -C "$work"
(cd "$work" && ./install.sh)

out=$root/dist/release
mkdir -p "$out"
deb=$(ls "$work"/dist/pygoscribe_"$version"-*_amd64.deb)
cp "$deb" "$out/"
cp "$work/dist/pygoscribe.flatpak" "$out/pygoscribe-$version.flatpak"

# Corresponding Source for the binaries (GPL-3 section 6): Pygoscribe at the
# tag plus the exact source of every crate compiled in, set up so cargo uses
# vendor/ without a network.
src_name=pygoscribe-$version
src_dir=$work/src/$src_name
mkdir -p "$src_dir"
git archive "$tag" | tar -x -C "$src_dir"
(
    cd "$src_dir"
    mkdir -p .cargo
    cargo vendor --locked vendor 2>/dev/null > .cargo/config.toml
    grep -q "vendored-sources" .cargo/config.toml || { echo "cargo vendor gave no config" >&2; exit 1; }
)
tar -C "$work/src" --sort=name --owner=0 --group=0 --numeric-owner \
    -cJf "$out/$src_name-src.tar.xz" "$src_name"

# The Poppler source the Flatpak manifest builds, checked against the
# manifest's sha256. flatpak-builder has usually downloaded it already.
read -r poppler_url poppler_sha < <(python3 -c '
import json, sys
m = json.load(open(sys.argv[1]))
for mod in m["modules"]:
    if isinstance(mod, dict) and mod["name"] == "poppler":
        s = mod["sources"][0]
        print(s["url"], s["sha256"])
' build-aux/flatpak/org.outram.Pygoscribe.json)
poppler_tar=$out/$(basename "$poppler_url")
if ! [[ -f $poppler_tar ]] || ! echo "$poppler_sha  $poppler_tar" | sha256sum -c --status 2>/dev/null; then
    cached=""
    for d in "$work" "$root"; do
        f=$d/.flatpak-builder/downloads/$poppler_sha/$(basename "$poppler_url")
        [[ -f $f ]] && { cached=$f; break; }
    done
    if [[ -n $cached ]]; then
        cp "$cached" "$poppler_tar"
    else
        curl -fL --retry 3 -o "$poppler_tar" "$poppler_url"
    fi
    echo "$poppler_sha  $poppler_tar" | sha256sum -c --status \
        || { rm -f "$poppler_tar"; echo "Poppler tarball doesn't match the manifest's sha256" >&2; exit 1; }
fi

files=("$out/$(basename "$deb")" "$out/pygoscribe-$version.flatpak"
       "$out/$src_name-src.tar.xz" "$poppler_tar")
ls -lh "${files[@]}"
[[ $upload == 1 ]] || exit 0

if [[ -z ${GITHUB_TOKEN:-}${PYGOSCRIBE:-} || -z ${GITEA:-} ]]; then
    # shellcheck disable=SC1090
    source ~/.profile >/dev/null 2>&1 || true
fi
# Nick keeps the GitHub token in ~/.profile as $PYGOSCRIBE.
GITHUB_TOKEN=${GITHUB_TOKEN:-${PYGOSCRIBE:-}}

note="Source: $src_name-src.tar.xz and $(basename "$poppler_tar") on this page. License GPL-3.0-only."

# Appends the source note to a release's body, once. Reads the release JSON.
body_with_note() {
    python3 -c 'import json,sys
r=json.load(sys.stdin); b=(r.get("body") or "").rstrip(); note=sys.argv[1]
if note not in b:
    b = (b + "\n\n" + note).strip()
print(json.dumps({"body": b}))' "$note"
}

# Ids of a release's assets with this name. Reads the release JSON.
asset_ids() {
    python3 -c 'import json,sys
r=json.load(sys.stdin)
print(" ".join(str(a["id"]) for a in r["assets"] if a["name"]==sys.argv[1]))' "$1"
}

release_id() { python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'; }

create_body="{\"tag_name\":\"$tag\",\"name\":\"Pygoscribe $version\",\"prerelease\":true}"

upload_github() {
    local api="https://api.github.com/repos/$GITHUB_REPO"
    local uploads="https://uploads.github.com/repos/$GITHUB_REPO"
    local auth=(-H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/vnd.github+json"
                -H "X-GitHub-Api-Version: 2022-11-28")
    local release id f name a
    release=$(curl -sf "${auth[@]}" "$api/releases/tags/$tag" || true)
    if [[ -z $release ]]; then
        echo "GitHub: creating pre-release $tag"
        release=$(curl -sf "${auth[@]}" -X POST "$api/releases" -d "$create_body")
    fi
    id=$(release_id <<<"$release")
    release=$(curl -sf "${auth[@]}" -X PATCH "$api/releases/$id" -d "$(body_with_note <<<"$release")")
    for f in "${files[@]}"; do
        name=$(basename "$f")
        for a in $(asset_ids "$name" <<<"$release"); do
            echo "GitHub: replacing existing $name"
            curl -sf "${auth[@]}" -X DELETE "$api/releases/assets/$a" >/dev/null
        done
        echo "GitHub: uploading $name"
        curl -sf "${auth[@]}" -H "Content-Type: application/octet-stream" \
            --data-binary "@$f" "$uploads/releases/$id/assets?name=$name" >/dev/null
    done
    echo "GitHub done: https://github.com/$GITHUB_REPO/releases/tag/$tag"
}

upload_gitea() {
    local api="$GITEA_URL/api/v1/repos/$GITEA_REPO"
    local auth=(-H "Authorization: token $GITEA")
    local release id f name a
    release=$(curl -sf "${auth[@]}" "$api/releases/tags/$tag" || true)
    if [[ -z $release ]]; then
        echo "Gitea: creating pre-release $tag"
        release=$(curl -sf "${auth[@]}" -H 'Content-Type: application/json' -X POST "$api/releases" -d "$create_body")
    fi
    id=$(release_id <<<"$release")
    release=$(curl -sf "${auth[@]}" -H 'Content-Type: application/json' -X PATCH "$api/releases/$id" \
        -d "$(body_with_note <<<"$release")")
    for f in "${files[@]}"; do
        name=$(basename "$f")
        for a in $(asset_ids "$name" <<<"$release"); do
            echo "Gitea: replacing existing $name"
            curl -sf "${auth[@]}" -X DELETE "$api/releases/$id/assets/$a" >/dev/null
        done
        echo "Gitea: uploading $name"
        curl -sf "${auth[@]}" -F "attachment=@$f" "$api/releases/$id/assets?name=$name" >/dev/null
    done
    echo "Gitea done: $GITEA_URL/$GITEA_REPO/releases/tag/$tag"
}

failed=0
if [[ -n ${GITHUB_TOKEN:-} ]]; then upload_github || { echo "GitHub upload failed" >&2; failed=1; }
else echo "GITHUB_TOKEN not set: skipped GitHub" >&2; failed=1; fi
if [[ -n ${GITEA:-} ]]; then upload_gitea || { echo "Gitea upload failed" >&2; failed=1; }
else echo "GITEA not set: skipped Gitea" >&2; failed=1; fi
exit $failed
