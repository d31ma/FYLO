#!/bin/sh
# Install the repository-pinned TACHYON binary without executing remote
# installer code. The version and SHA-256 digests are anchored in this script
# so a mutable release, installer, or checksum file cannot silently change CI.
# TTID and CHEX are built into FYLO, so there is nothing to install for them.
set -eu

TACHYON_VERSION='v26.36.01'
DESTINATION=${FYLO_VENDOR_BIN_DIR:-"$HOME/.local/bin"}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
        TACHYON_ASSET='ty-linux-x64'
        TACHYON_SHA256='7eceb7d95340abeea49d8a3c709ca0b2ab3767a46869fdd349b1c0901e19a3e9'
        ;;
    Linux-aarch64|Linux-arm64)
        TACHYON_ASSET='ty-linux-arm64'
        TACHYON_SHA256='8bc5badcfd55bb194ab2d4c07fa321283207618612f61c9767b5bebef26db319'
        ;;
    Darwin-arm64)
        TACHYON_ASSET='ty-macos-arm64'
        TACHYON_SHA256='1bf8d005bc2552dec0b8b1fbfeefde1b01ef6c4342f98b8d958d76d75c8d9f80'
        ;;
    Darwin-x86_64)
        TACHYON_ASSET='ty-macos-x64'
        TACHYON_SHA256='4b3948797942de117177ce8f466a1f1356b5223427abff1d6485d141f5d29bf5'
        ;;
    MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64)
        TACHYON_ASSET='ty-windows-x64.exe'
        TACHYON_SHA256='8b8cc73a8407e5397e66d7fab6f3946162847c4b5639d089d0f16af57f3631f8'
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
    curl --fail --silent --show-error --location --retry 3 \
        "https://github.com/$repository/releases/download/$version/$asset" \
        --output "$temporary"
    verify_sha256 "$temporary" "$expected"
    install -m 0755 "$temporary" "$DESTINATION/$executable"
    rm -f "$temporary"
    trap - EXIT HUP INT TERM
    echo "Installed verified $repository@$version/$asset."
}

mkdir -p "$DESTINATION"
install_binary 'd31ma/Tachyon' "$TACHYON_VERSION" "$TACHYON_ASSET" "$TACHYON_SHA256" "ty$EXECUTABLE_SUFFIX"

test "$("$DESTINATION/ty$EXECUTABLE_SUFFIX" --version)" = "26.36.01"
echo "Verified TACHYON in $DESTINATION."
