#!/bin/sh
# Rebuild vendor/turso_core from Turso's crates.io release and FYLO's patch
# (ADR 0019). With --check, rebuild into a temporary folder and fail when it
# differs from the committed vendor/turso_core.
#
# The release tarball is checked against crates.io's SHA-256 before it is
# unpacked. Moving the pin means changing VERSION and SHA256 here, rebasing
# vendor/turso_core-<version>-fylo.patch, and the `=` pin in crates/fylo.
set -eu

VERSION=0.8.3
SHA256=810ee7b139fd6aa9304cb2fa278c1be0bb964c92457e2a24c1d384196ce5e217

root=$(cd "$(dirname "$0")/.." && pwd)
patch_file="$root/vendor/turso_core-$VERSION-fylo.patch"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "=== download turso_core $VERSION $(date +%T)"
curl -fsSL -H 'User-Agent: fylo-vendor-script' \
    "https://crates.io/api/v1/crates/turso_core/$VERSION/download" -o "$work/turso_core.crate"
actual=$(shasum -a 256 "$work/turso_core.crate" | cut -d' ' -f1)
if [ "$actual" != "$SHA256" ]; then
    echo "turso_core $VERSION checksum mismatch: $actual" >&2
    exit 1
fi

echo "=== unpack and patch $(date +%T)"
mkdir "$work/turso_core"
tar xzf "$work/turso_core.crate" -C "$work/turso_core" --strip-components=1
(cd "$work" && patch -p1 --quiet < "$patch_file")

if [ "${1:-}" = "--check" ]; then
    diff -r "$work/turso_core" "$root/vendor/turso_core"
    echo "=== vendor/turso_core matches turso_core $VERSION plus the fylo patch"
    exit 0
fi

rm -rf "$root/vendor/turso_core"
mv "$work/turso_core" "$root/vendor/turso_core"
echo "=== vendor/turso_core rebuilt from turso_core $VERSION plus the fylo patch"
