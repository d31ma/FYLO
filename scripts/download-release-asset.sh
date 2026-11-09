#!/bin/sh
set -eu

download_with_curl() {
    curl --fail --location --show-error --no-progress-meter \
        --retry 6 --retry-delay 10 --retry-max-time 300 --retry-all-errors \
        "https://github.com/$1/releases/download/$2/$3" \
        --output "$4"
}

download_with_gh() {
    directory=$(mktemp -d "${TMPDIR:-/tmp}/fylo-release-asset.XXXXXX")
    gh release download "$2" --repo "$1" --pattern "$3" --dir "$directory"
    mv "$directory/$3" "$4"
    rm -rf "$directory"
}

require_token() {
    if [ -z "${GH_TOKEN:-}" ]; then
        echo "Download of $1@$2/$3 failed and GH_TOKEN is not set for the gh fallback." >&2
        exit 1
    fi
}

main() {
    if download_with_curl "$@"; then
        return 0
    fi
    require_token "$@"
    echo "Download of $1@$2/$3 failed; retrying through the GitHub API with gh." >&2
    download_with_gh "$@"
}

main "$@"
