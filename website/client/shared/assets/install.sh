#!/bin/sh
# Fylo installer for macOS and Linux.
#   curl -fsSL https://fylo.del.ma/install.sh | sh
# Downloads the right binary from the latest GitHub release, verifies its
# checksum, and installs it to a directory on your PATH.
set -eu

REPO="d31ma/Fylo"
BASE="https://github.com/${REPO}/releases/latest/download"

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
if [ -w /usr/local/bin ]; then
    dest="/usr/local/bin"
else
    dest="${HOME}/.local/bin"
    mkdir -p "$dest"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading ${asset}..."
curl -fSL "$url" -o "$tmp/fylo"

# Verify checksum against the release's SHA256SUMS (best-effort: skip if tools absent).
if command -v sha256sum >/dev/null 2>&1; then hash_cmd="sha256sum"; \
    elif command -v shasum >/dev/null 2>&1; then hash_cmd="shasum -a 256"; else hash_cmd=""; fi
if [ -n "$hash_cmd" ]; then
    curl -fsSL "${BASE}/SHA256SUMS" -o "$tmp/SHA256SUMS" || true
    if [ -f "$tmp/SHA256SUMS" ]; then
        expected=$(grep " ${asset}\$" "$tmp/SHA256SUMS" | awk '{print $1}')
        actual=$($hash_cmd "$tmp/fylo" | awk '{print $1}')
        if [ -n "$expected" ] && [ "$expected" != "$actual" ]; then
            echo "Checksum mismatch for ${asset}. Aborting." >&2
            exit 1
        fi
    fi
fi

chmod +x "$tmp/fylo"
mv "$tmp/fylo" "$dest/fylo"

echo "Installed fylo to ${dest}/fylo"
case ":$PATH:" in
    *":$dest:"*) : ;;
    *) echo "Note: ${dest} is not on your PATH. Add it, e.g.:"; echo "  export PATH=\"${dest}:\$PATH\"" ;;
esac
echo "Run 'fylo --help' to get started."
