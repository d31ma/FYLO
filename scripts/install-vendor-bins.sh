#!/bin/sh
# Install the `ttid` and `chex` binaries FYLO drives at runtime.
#
# FYLO consumes CHEX and TTID as standalone binaries from their GitHub Releases
# (not npm). Each project's installer downloads the right binary for your
# OS/arch, verifies its checksum, and puts it on your PATH. The vendored shims
# in src/vendor/ spawn these binaries by name.
#
# Windows: run the PowerShell installers instead:
#   irm https://github.com/d31ma/TTID/releases/latest/download/install.ps1 | iex
#   irm https://github.com/d31ma/CHEX/releases/latest/download/install.ps1 | iex
set -eu

curl -fsSL https://github.com/d31ma/TTID/releases/latest/download/install.sh | sh
curl -fsSL https://github.com/d31ma/CHEX/releases/latest/download/install.sh | sh

echo "Verifying binaries are on PATH..."
ttid --help >/dev/null 2>&1 && echo "  ttid OK" || echo "  ttid NOT on PATH — add its install dir (e.g. ~/.local/bin) to PATH" >&2
chex --help >/dev/null 2>&1 && echo "  chex OK" || echo "  chex NOT on PATH — add its install dir (e.g. ~/.local/bin) to PATH" >&2
