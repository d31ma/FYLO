#!/bin/sh
# Fylo installer for macOS and Linux.
#   curl -fsSL https://fylo.del.ma/install.sh | sh
# Downloads the right binary from the latest GitHub release, verifies its
# checksum, and installs it to a directory on your PATH.
set -eu

REPO="d31ma/Fylo"
BASE="https://github.com/${REPO}/releases/latest/download"
BASE="${FYLO_RELEASE_BASE:-$BASE}"

os=$(uname -s)
arch=$(uname -m)

case "$os" in
    Darwin) os_tag="macos" ;;
    Linux) os_tag="linux" ;;
    *) echo "Unsupported OS: $os (use install.ps1 on Windows)" >&2; exit 1 ;;
esac

case "$arch" in
    x86_64 | amd64) arch_tag="x64" ;;
    arm64 | aarch64) arch_tag="arm64" ;;
    *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
esac

asset="fylo-${os_tag}-${arch_tag}"
url="${BASE}/${asset}"

# Pick an install dir on PATH we can write to; fall back to ~/.local/bin.
if [ -n "${FYLO_INSTALL_DIR:-}" ]; then
    dest="$FYLO_INSTALL_DIR"
elif [ -w /usr/local/bin ]; then
    dest="/usr/local/bin"
else
    dest="${HOME}/.local/bin"
    mkdir -p "$dest"
fi

mkdir -p "$dest"
tmp=$(mktemp -d "$dest/.fylo-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

if command -v sha256sum >/dev/null 2>&1; then
    hash_tool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    hash_tool="shasum"
else
    echo "A SHA-256 tool (sha256sum or shasum) is required. Aborting." >&2
    exit 1
fi

echo "Downloading ${asset}..."
curl -fSL "$url" -o "$tmp/fylo"
curl -fsSL "${BASE}/SHA256SUMS" -o "$tmp/SHA256SUMS"

row_counts=$(awk -v asset="$asset" '
    $2 == asset || $2 == "*" asset {
        matching += 1
        if (NF == 2 && length($1) == 64 && $1 !~ /[^[:xdigit:]]/) valid += 1
    }
    END { print matching + 0, valid + 0 }
' "$tmp/SHA256SUMS")
if [ "$row_counts" != "1 1" ]; then
    echo "SHA256SUMS must contain exactly one valid checksum for ${asset}. Aborting." >&2
    exit 1
fi
expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print tolower($1) }' "$tmp/SHA256SUMS")
if [ "$hash_tool" = "sha256sum" ]; then
    actual=$(sha256sum "$tmp/fylo" | awk '{ print tolower($1) }')
else
    actual=$(shasum -a 256 "$tmp/fylo" | awk '{ print tolower($1) }')
fi
if [ "$expected" != "$actual" ]; then
    echo "Checksum mismatch for ${asset}. Aborting." >&2
    exit 1
fi

chmod +x "$tmp/fylo"
mv -f "$tmp/fylo" "$dest/fylo"

echo "Installed fylo to ${dest}/fylo"
case ":$PATH:" in
    *":$dest:"*) : ;;
    *) echo "Note: ${dest} is not on your PATH. Add it, e.g.:"; echo "  export PATH=\"${dest}:\$PATH\"" ;;
esac
echo "Run 'fylo --help' to get started."
