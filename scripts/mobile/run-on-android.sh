#!/bin/sh
# Cargo target runner for Android (ADR 0026): runs one test program on the
# running emulator or device that `adb` sees, and returns its exit code.
#
#   CARGO_TARGET_AARCH64_LINUX_ANDROID_RUNNER=scripts/mobile/run-on-android.sh
#
# The program is pushed once per build (its SHA-256 names the copy), so
# nextest's one process per test does not push it again for every test. Each
# push uses its own temporary name, because nextest lists a program twice at
# the same time.
# Tests write under /data/local/tmp/fylo-tests/tmp, the emulator's own
# filesystem.
set -eu

remote_root() {
    echo /data/local/tmp/fylo-tests
}

adb_command() {
    if [ -n "${ANDROID_HOME:-}" ]; then
        echo "$ANDROID_HOME/platform-tools/adb"
        return
    fi
    echo "$HOME/Library/Android/sdk/platform-tools/adb"
}

program_digest() {
    shasum -a 256 "$1" | cut -c1-16
}

push_once() {
    local_program=$1
    remote_program=$2
    adb=$(adb_command)
    if "$adb" shell "test -x '$remote_program'" 2>/dev/null; then
        return
    fi
    partial="$remote_program.partial-$$"
    "$adb" push "$local_program" "$partial" >/dev/null
    "$adb" shell "chmod 755 '$partial' && mv -f '$partial' '$remote_program'"
}

quoted_arguments() {
    for argument in "$@"; do
        printf " '%s'" "$(printf '%s' "$argument" | sed "s/'/'\\\\''/g")"
    done
}

run_remote() {
    local_program=$1
    shift
    root=$(remote_root)
    remote_program="$root/bin/$(basename "$local_program")-$(program_digest "$local_program")"
    adb=$(adb_command)
    "$adb" shell "mkdir -p '$root/bin' '$root/tmp'"
    push_once "$local_program" "$remote_program"
    exec "$adb" shell "cd '$root/tmp' && TMPDIR='$root/tmp' RUST_BACKTRACE=1 '$remote_program'$(quoted_arguments "$@")"
}

run_remote "$@"
