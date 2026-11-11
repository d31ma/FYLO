#!/bin/sh
# Install the pinned binaryen release after verifying a repository-anchored
# digest. The upstream checksum is not fetched at runtime.
set -eu

BINARYEN_VERSION='133'
PINNED_VERSION=$(tr -d '[:space:]' < "$(dirname "$0")/../.binaryen-version")
TEMPORARY_ROOT=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
DESTINATION=${BINARYEN_DESTINATION:-"$TEMPORARY_ROOT/binaryen"}

platform() {
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64) echo 'x86_64-linux' ;;
        Linux-aarch64) echo 'aarch64-linux' ;;
        Darwin-arm64) echo 'arm64-macos' ;;
        Darwin-x86_64) echo 'x86_64-macos' ;;
        *)
            echo "No pinned binaryen archive for $(uname -s) $(uname -m)." >&2
            exit 1
            ;;
    esac
}

expected_digest() {
    case "$1" in
        x86_64-linux) echo '2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489' ;;
        aarch64-linux) echo '89c07ea56faf38d0fbecf36ca8ec0721756716185f265b568e133d427f299bf8' ;;
        arm64-macos) echo 'ad66da82ac13f163e424b1643f16c6dfcccc98b5966296b43e52d3cab04f84a8' ;;
        x86_64-macos) echo '13a9b90be775c6389ce3d1f879cb8627bea56708ba8c122983941d53a8199b95' ;;
    esac
}

actual_digest() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

main() {
    if [ "$PINNED_VERSION" != "$BINARYEN_VERSION" ]; then
        echo ".binaryen-version names $PINNED_VERSION; update this script's digests for it." >&2
        exit 1
    fi
    target=$(platform)
    archive=$(mktemp "$TEMPORARY_ROOT/binaryen.XXXXXX")
    trap 'rm -f "$archive"' EXIT HUP INT TERM
    sh "$(dirname "$0")/download-release-asset.sh" 'WebAssembly/binaryen' \
        "version_$BINARYEN_VERSION" "binaryen-version_$BINARYEN_VERSION-$target.tar.gz" "$archive"
    if [ "$(actual_digest "$archive")" != "$(expected_digest "$target")" ]; then
        echo 'SHA-256 mismatch for the binaryen archive.' >&2
        exit 1
    fi
    rm -rf "$DESTINATION"
    mkdir -p "$DESTINATION"
    tar -xzf "$archive" -C "$DESTINATION" --strip-components 1
    if [ -n "${GITHUB_PATH:-}" ]; then
        echo "$DESTINATION/bin" >> "$GITHUB_PATH"
    fi
    "$DESTINATION/bin/wasm-opt" --version
}

main
