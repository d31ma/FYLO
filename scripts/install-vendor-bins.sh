#!/bin/sh
# Install the repository-pinned TACHYON binary without executing remote
# installer code. The version and SHA-256 digests are anchored in this script
# so a mutable release, installer, or checksum file cannot silently change CI.
# TTID and CHEX are built into FYLO, so there is nothing to install for them.
set -eu

TACHYON_VERSION='v26.46.02'
DESTINATION=${FYLO_VENDOR_BIN_DIR:-"$HOME/.local/bin"}
SCRIPTS=$(dirname "$0")

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
        TACHYON_ASSET='ty-linux-x64'
        TACHYON_SHA256='125423c88dac3e888cc442c807f1beffc10ff8e5db2ac4cd70b331ecda5ea1fd'
        ;;
    Linux-aarch64|Linux-arm64)
        TACHYON_ASSET='ty-linux-arm64'
        TACHYON_SHA256='3006c7bcd1bd7e879055d60492b307c2903dde61ec72bb36950ace7ff3031b2e'
        ;;
    Darwin-arm64)
        TACHYON_ASSET='ty-macos-arm64'
        TACHYON_SHA256='a062ed5e7523739515b25117dc3426bd1b9f7e82a6eac110611df06adff462a6'
        ;;
    Darwin-x86_64)
        TACHYON_ASSET='ty-macos-x64'
        TACHYON_SHA256='28268aa54999298eb9663e1bd7ed8b52467804e9157e0771fe00f18d879d3ed8'
        ;;
    MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64)
        TACHYON_ASSET='ty-windows-x64.exe'
        TACHYON_SHA256='5950bc91fd7eef88e7cd03b9ef4d26ecbdb248eca9a068cb2f45576e91fb6bab'
        EXECUTABLE_SUFFIX='.exe'
        ;;
    *)
        echo "Unsupported vendor-binary platform: $(uname -s)/$(uname -m)" >&2
        exit 1
        ;;
esac

# Windows needs the extension to be executable; every other platform must not
# have one, so callers can always spawn a bare `ty` command.
EXECUTABLE_SUFFIX=${EXECUTABLE_SUFFIX:-}

verify_sha256() {
    file=$1
    expected=$2
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    else
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    fi
    if [ "$actual" != "$expected" ]; then
        echo "SHA-256 mismatch for $(basename "$file")" >&2
        exit 1
    fi
}

install_binary() {
    repository=$1
    version=$2
    asset=$3
    expected=$4
    executable=$5
    temporary=$(mktemp "${TMPDIR:-/tmp}/fylo-vendor.XXXXXX")
    trap 'rm -f "$temporary"' EXIT HUP INT TERM
    sh "$SCRIPTS/download-release-asset.sh" "$repository" "$version" "$asset" "$temporary"
    verify_sha256 "$temporary" "$expected"
    install -m 0755 "$temporary" "$DESTINATION/$executable"
    rm -f "$temporary"
    trap - EXIT HUP INT TERM
    echo "Installed verified $repository@$version/$asset."
}

mkdir -p "$DESTINATION"
install_binary 'd31ma/Tachyon' "$TACHYON_VERSION" "$TACHYON_ASSET" "$TACHYON_SHA256" "ty$EXECUTABLE_SUFFIX"

test "$("$DESTINATION/ty$EXECUTABLE_SUFFIX" --version)" = "26.46.02"
echo "Verified TACHYON in $DESTINATION."
