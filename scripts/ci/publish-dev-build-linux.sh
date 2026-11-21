#!/bin/bash
# Builds the linux-arm64 fylo for one develop commit inside the publisher's Ubuntu 24.04 container (glibc 2.39).
# Usage: bash publish-dev-build-linux.sh <40-character commit>. Run by scripts/ci/publish-dev-build.mjs as user ci.
# Writes ~/fylo-publish/out/fylo and ~/fylo-publish/out/lane.json; the clone, cargo target and registry stay in
# ~/fylo-publish on the container's volume between runs.
set -euo pipefail
SHA=$1
HOME=/home/ci
LANE=$HOME/fylo-publish
REPO=$LANE/repo
OUT=$LANE/out
[[ $SHA =~ ^[0-9a-f]{40}$ ]] || { echo "pass the full 40-character commit"; exit 2; }
[ -f /opt/tachyon/ci-env.sh ] && source /opt/tachyon/ci-env.sh
export CARGO_HOME=$LANE/cargo-home CARGO_TARGET_DIR=$LANE/target CARGO_INCREMENTAL=0
export PATH=/opt/tachyon/cargo/bin:$PATH TMPDIR=$LANE/tmp
unset RUSTC_WRAPPER
mkdir -p "$LANE" "$TMPDIR" "$CARGO_HOME"
rm -rf "$OUT"
mkdir -p "$OUT"

step() {
    echo "=== $1 $(date '+%H:%M:%S')"
}

step source
if [ ! -d "$REPO/.git" ]; then git clone https://github.com/d31ma/FYLO.git "$REPO"; fi
git -C "$REPO" fetch --force --tags origin develop
git -C "$REPO" checkout --force --detach "$SHA"
git -C "$REPO" clean -fdx
[ "$(git -C "$REPO" rev-parse HEAD)" = "$SHA" ] || { echo "the clone is not at $SHA"; exit 1; }
cd "$REPO"
echo "bun $(bun --version) | $(rustc --version) | $(ldd --version | head -1)"

step build
FYLO_BUILD_COMMIT=$SHA FYLO_BUILD_KIND=development-compiled bun ./scripts/build-executable.mjs --outfile "$OUT/fylo"

step report
identity=$("$OUT/fylo" version --output json)
glibc=$(objdump -T "$OUT/fylo" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/^GLIBC_//' | sort -uV | tail -1)
rustc=$(rustc --version)
target=$(rustc -vV | sed -n 's/^host: //p')
digest=$(sha256sum "$OUT/fylo" | cut -c1-64)
printf '{"sha256":"%s","rustc":"%s","target":"%s","glibc":"%s","identity":%s}\n' \
    "$digest" "$rustc" "$target" "$glibc" "$identity" > "$OUT/lane.json"
echo "glibc needed: $glibc | sha256 $digest"
echo "=== done"
