#!/bin/sh
# Cargo target runner for the iOS simulator (ADR 0026): runs one test program
# inside the booted simulator named by FYLO_IOS_SIMULATOR (a UDID), and
# returns its exit code.
#
#   CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUNNER=scripts/mobile/run-on-ios-simulator.sh
#
# A simulator process reads the Mac's files, so nothing is copied. Its
# temporary folder is FYLO_IOS_TMPDIR, passed through simctl's SIMCTL_CHILD_
# prefix.
set -eu

simulator() {
    if [ -z "${FYLO_IOS_SIMULATOR:-}" ]; then
        echo "run-on-ios-simulator.sh: set FYLO_IOS_SIMULATOR to a booted simulator's UDID" >&2
        exit 2
    fi
    echo "$FYLO_IOS_SIMULATOR"
}

temporary_folder() {
    folder=${FYLO_IOS_TMPDIR:-${TMPDIR:-/tmp}/fylo-ios-tests}
    mkdir -p "$folder"
    echo "$folder"
}

run_in_simulator() {
    device=$(simulator)
    folder=$(temporary_folder)
    SIMCTL_CHILD_TMPDIR="$folder" SIMCTL_CHILD_RUST_BACKTRACE=1 \
        exec xcrun simctl spawn "$device" "$@"
}

run_in_simulator "$@"
