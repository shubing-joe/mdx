#!/usr/bin/env bash
#
# release.sh — build the workspace in release mode, strip the binaries and pack
# the result into `dist/`.
#
# The archive is `dist/mdx-<version>-<host-target>.tar.gz`, laid out as
#
#   mdx-<version>-<host-target>/
#     bin/mdx        bin/mdx-mcp        bin/mdx-gui
#     README.md      mdx.desktop        mdx.svg
#     LICENSE*       (when the tree carries them)
#
# The version comes from the `[workspace.package]` table in Cargo.toml and the
# target triple from `rustc -vV`, so nothing in the archive is inherited from the
# environment. The tar step is written for GNU tar, which is what the CI image and
# the Arch package use.
#
# Usage: scripts/release.sh
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

die() {
  printf 'release.sh: %s\n' "$*" >&2
  exit 1
}

for tool in cargo rustc strip tar gzip sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not on PATH"
done

# The first bare `version = "…"` line in Cargo.toml is the workspace one (the
# dependency table spells versions inside inline tables).
version=$(awk -F'"' '/^version[[:space:]]*=[[:space:]]*"/ { print $2; exit }' Cargo.toml)
[[ -n $version ]] || die "cannot read the workspace version from Cargo.toml"

target=$(rustc -vV | awk '/^host: / { print $2 }')
[[ -n $target ]] || die "cannot determine the host target triple"

bundle="mdx-$version-$target"
archive="dist/$bundle.tar.gz"

printf 'building %s for %s\n' "$version" "$target"
# `--locked` keeps the build on the committed Cargo.lock.
cargo build --release --workspace --locked

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p "$stage/$bundle/bin"

for binary in mdx mdx-mcp mdx-gui; do
  built="target/release/$binary"
  [[ -f $built ]] || die "$built was not produced by the release build"
  install -Dm755 "$built" "$stage/$bundle/bin/$binary"
done

# Strip the copies, never the build tree: `target/` keeps whatever the profile
# asked for, while the archive carries small binaries.
strip --strip-unneeded "$stage/$bundle/bin/"*

# Files that make the archive self-describing. Missing ones are skipped: the
# license texts are declared in Cargo.toml and may not be checked in.
for file in README.md packaging/mdx.desktop packaging/mdx.svg LICENSE LICENSE-MIT LICENSE-APACHE; do
  if [[ -f $file ]]; then
    install -Dm644 "$file" "$stage/$bundle/$(basename -- "$file")"
  fi
done

mkdir -p dist

# `--sort=name`, fixed ownership and a fixed mtime make the tarball
# reproducible; the mtime is taken from SOURCE_DATE_EPOCH, then the last commit,
# and is left to the files themselves when neither is available. `gzip -n` drops
# the name and timestamp gzip would otherwise embed. `"${mtime[@]}"` stays safe
# with an empty array on bash 4.4 and newer.
mtime=()
if [[ -n ${SOURCE_DATE_EPOCH:-} ]]; then
  mtime=(--mtime="@$SOURCE_DATE_EPOCH")
else
  epoch=$(git -C "$root" log -1 --format=%ct 2>/dev/null || true)
  if [[ -n $epoch ]]; then
    mtime=(--mtime="@$epoch")
  fi
fi

tar --sort=name --owner=0 --group=0 --numeric-owner "${mtime[@]}" \
  -cf - -C "$stage" "$bundle" | gzip -9n > "$archive"

bytes=$(wc -c < "$archive")
digest=$(sha256sum -- "$archive" | cut -d' ' -f1)

printf 'wrote %s (%s bytes)\n' "$archive" "$bytes"
printf 'sha256 %s\n' "$digest"
