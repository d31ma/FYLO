#!/bin/sh
# Build Turso's own language-binding libraries with FYLO inside (ADR 0019
# section 2, `fylo link`). This is a separate Cargo workspace: Turso's
# repository at the pinned tag, never a member of FYLO's workspace. FYLO's
# crates stay `unsafe`-free; Turso's binding crates keep their upstream
# `unsafe` code here only (ADR 0004 amendment, proposed).
#
# What it changes in Turso's checkout (turso-<version>-bindings.patch):
# - the workspace's turso_core is FYLO's patched vendor/turso_core, shared
#   with the fylo crate; turso_ext, turso_macros and turso_parser resolve to
#   the checkout's own sources, so the build has one copy of each;
# - the JavaScript binding and the SDK kit install FYLO's open hook before
#   they open a database.
#
# Usage: bindings/turso/build.sh <work-dir> <out-dir>
#   <work-dir> holds the Turso checkout and its cargo target (large).
#   <out-dir> receives turso.<triple>.node and libturso_sync_sdk_kit.<ext>.
set -eu

VERSION=0.8.3
TAG=v$VERSION
COMMIT=607bff9fd0e547a25a97b3bf06e83bcc5d99d53c

fylo=$(cd "$(dirname "$0")/../.." && pwd)
here="$fylo/bindings/turso"
work=${1:?work directory}
out=${2:?output directory}
mkdir -p "$work" "$out"
checkout="$work/turso"

step() { echo "=== $1 $(date +%T)"; }

step checkout
if [ ! -d "$checkout/.git" ]; then
    git clone --depth 1 --branch "$TAG" https://github.com/tursodatabase/turso "$checkout"
fi
actual=$(git -C "$checkout" rev-parse HEAD)
if [ "$actual" != "$COMMIT" ]; then
    echo "turso checkout is at $actual, not $TAG ($COMMIT)" >&2
    exit 1
fi
git -C "$checkout" checkout -q -- .

step patch
sed "s|@FYLO@|$fylo|g" "$here/turso-$VERSION-bindings.patch" | git -C "$checkout" apply --whitespace=nowarn
cp "$here/Cargo.lock" "$checkout/Cargo.lock" 2>/dev/null || true

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) triple=darwin-arm64; dylib=dylib ;;
    Darwin-x86_64) triple=darwin-x64; dylib=dylib ;;
    Linux-x86_64) triple=linux-x64-gnu; dylib=so ;;
    Linux-aarch64) triple=linux-arm64-gnu; dylib=so ;;
    *) echo "unsupported host $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$work/target}"
# FYLO's crates need its toolchain; Turso's rust-toolchain.toml names 1.88.
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-$(sed -n 's/^channel = "\(.*\)"/\1/p' "$fylo/rust-toolchain.toml")}"
echo "toolchain $RUSTUP_TOOLCHAIN"
for package in turso_node turso_sync_sdk_kit; do
    step "build $package"
    started=$(date +%s)
    (cd "$checkout" && cargo build --release --package "$package")
    echo "=== build $package seconds=$(($(date +%s) - started))"
done
cp "$checkout/Cargo.lock" "$out/Cargo.lock"

step artifacts
cp "$CARGO_TARGET_DIR/release/libturso_node.$dylib" "$out/turso.$triple.node"
cp "$CARGO_TARGET_DIR/release/libturso_sync_sdk_kit.$dylib" "$out/libturso_sync_sdk_kit.$dylib"
for artifact in "$out/turso.$triple.node" "$out/libturso_sync_sdk_kit.$dylib"; do
    echo "$(shasum -a 256 "$artifact" | cut -d' ' -f1)  $(du -h "$artifact" | cut -f1)  $(basename "$artifact")"
done
printf '{"turso":"%s","commit":"%s","fylo":"%s","triple":"%s"}\n' \
    "$VERSION" "$COMMIT" "$(git -C "$fylo" rev-parse HEAD)" "$triple" > "$out/fylo-link.json"
step done
