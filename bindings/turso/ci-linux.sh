#!/usr/bin/env bash
# Build and test `fylo link` on Linux inside a container (ADR 0019): Turso's
# JavaScript addon and tursogo's SDK-kit library with FYLO inside, per target,
# then the conformance suite against the stock SDKs for the container's own
# architecture. x86_64 on an aarch64 host is cross-compiled with Ubuntu's
# x86_64-linux-gnu toolchain (installed here when missing); ci-linux-x64.sh
# runs its conformance in an amd64 container.
#
# Usage: ci-linux.sh <fylo.bundle> <branch> <work-dir> <rust-target>...
set -u
bundle=$1 branch=$2 work=$3
shift 3
if [ -f /opt/tachyon/ci-env.sh ]; then . /opt/tachyon/ci-env.sh; fi
export GOPATH=$work/go GOMODCACHE=$work/go/pkg/mod GOCACHE=$work/go/cache npm_config_cache=$work/npm
step() { echo "=== $1 $(date +%T)"; }
overall=0
mkdir -p "$work"

step clone
if [ -d "$work/fylo/.git" ]; then
    git -C "$work/fylo" fetch -q "$bundle" "+$branch:refs/remotes/bundle/$branch"
    git -C "$work/fylo" checkout -q -f "refs/remotes/bundle/$branch"
else
    git clone -q --branch "$branch" "$bundle" "$work/fylo"
fi
echo "fylo $(git -C "$work/fylo" rev-parse --short HEAD) on $(uname -m), $(rustc -V 2>/dev/null), node $(node -v), $(go version)"

for target in "$@"; do
    if [ "$target" = x86_64-unknown-linux-gnu ] && [ "$(uname -m)" != x86_64 ]; then
        if ! command -v x86_64-linux-gnu-gcc > /dev/null; then
            step "install x86_64 cross toolchain"
            apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
                gcc-x86-64-linux-gnu g++-x86-64-linux-gnu libc6-dev-amd64-cross > /dev/null
        fi
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
        export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc
        export CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++
        export AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar
    fi
    step "bindings $target"
    started=$(date +%s)
    CARGO_TARGET_DIR=$work/target "$work/fylo/bindings/turso/build.sh" "$work/bindings" "$work/out-$target" "$target"
    rc=$?; [ $rc -ne 0 ] && overall=1
    echo "=== bindings $target rc=$rc seconds=$(($(date +%s) - started))"
    step "fylo-cli $target"
    (cd "$work/fylo" && CARGO_TARGET_DIR=$work/fylo-target RUSTUP_TOOLCHAIN=1.97.1 cargo build -p fylo-cli --bin fylo-rust --locked --target "$target") || overall=1
done

# The x86_64 Go conformance app, compiled here so the emulated run only runs it.
if [ -d "$work/out-x86_64-unknown-linux-gnu" ]; then
    step "go app x86_64"
    (cd "$work/fylo/bindings/turso/conformance/go" && GOARCH=amd64 go build -o "$work/go-conformance-x86_64" .) || overall=1
fi
native=$(uname -m)-unknown-linux-gnu
for method in auto copy; do
    step "conformance $native $method"
    suite=$work/conformance-$native-$method
    rm -rf "$suite" && cp -R "$work/fylo/bindings/turso/conformance" "$suite"
    "$suite/run.sh" "$work/fylo-target/$native/debug/fylo-rust" "$work/out-$native" all --method "$method"
    rc=$?; [ $rc -ne 0 ] && overall=1
    echo "=== conformance $native $method rc=$rc"
done
step "done overall=$overall"
exit $overall
