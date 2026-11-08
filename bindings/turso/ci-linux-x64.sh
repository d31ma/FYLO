#!/usr/bin/env bash
# Run the `fylo link` conformance suite for linux-x64 artifacts in an amd64
# container (ADR 0019), with Node fetched for x86_64. On an aarch64 host the
# container runs under emulation, where the Go toolchain itself crashes
# ("found bad pointer in Go heap"), so ci-linux.sh cross-compiles the Go app
# and this script only runs it.
# Usage: ci-linux-x64.sh <work-dir>   (the work dir ci-linux.sh filled)
set -u
work=$1
node_version=24.22.2
step() { echo "=== $1 $(date +%T)"; }
step tools
if ! command -v curl > /dev/null; then
    apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates xz-utils git > /dev/null
fi
tools=$work/x64-tools
if [ ! -x "$tools/node/bin/node" ]; then
    mkdir -p "$tools/node" && curl -fsSL "https://nodejs.org/dist/v$node_version/node-v$node_version-linux-x64.tar.xz" | tar -xJ -C "$tools/node" --strip-components=1
fi
export PATH=$tools/node/bin:$PATH
export npm_config_cache=$work/npm-x64
echo "on $(uname -m): node $(node -v) $(node -p process.arch), go app $(file -b "$work/go-conformance-x86_64" 2>/dev/null | cut -d, -f1-2)"
target=x86_64-unknown-linux-gnu
overall=0
for method in auto copy; do
    step "conformance $target $method"
    suite=$work/conformance-$target-$method
    rm -rf "$suite" && cp -R "$work/fylo/bindings/turso/conformance" "$suite"
    FYLO_GO_CONFORMANCE_BIN=$work/go-conformance-x86_64 \
        "$suite/run.sh" "$work/fylo-target/$target/debug/fylo-rust" "$work/out-$target" js,go --method "$method"
    rc=$?; [ $rc -ne 0 ] && overall=1
    echo "=== conformance $target $method rc=$rc"
done
step "done overall=$overall"
exit $overall
