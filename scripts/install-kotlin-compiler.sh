#!/bin/sh
# Install the pinned Kotlin compiler after verifying a repository-anchored
# digest. The upstream checksum is not fetched at runtime.
set -eu

KOTLIN_VERSION='2.1.10'
KOTLIN_SHA256='c6e9e2636889828e19c8811d5ab890862538c89dc2a3101956dfee3c2a8ba6b1'
TEMPORARY_ROOT=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
DESTINATION=${KOTLIN_DESTINATION:-"$TEMPORARY_ROOT/kotlin"}
ARCHIVE=$(mktemp "$TEMPORARY_ROOT/kotlin-compiler.XXXXXX")
trap 'rm -f "$ARCHIVE"' EXIT HUP INT TERM

curl --fail --silent --show-error --location --retry 3 \
    "https://github.com/JetBrains/kotlin/releases/download/v$KOTLIN_VERSION/kotlin-compiler-$KOTLIN_VERSION.zip" \
    --output "$ARCHIVE"

if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "$ARCHIVE" | awk '{print $1}')
else
    ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
fi
if [ "$ACTUAL" != "$KOTLIN_SHA256" ]; then
    echo 'SHA-256 mismatch for the Kotlin compiler archive.' >&2
    exit 1
fi

rm -rf "$DESTINATION"
mkdir -p "$DESTINATION"
unzip -q "$ARCHIVE" -d "$DESTINATION"
if [ -n "${GITHUB_PATH:-}" ]; then
    echo "$DESTINATION/kotlinc/bin" >> "$GITHUB_PATH"
fi
"$DESTINATION/kotlinc/bin/kotlinc" -version
