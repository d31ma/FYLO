#!/usr/bin/env bash
# fylo link conformance: JS (@tursodatabase/database 0.8.3) and Go (tursogo
# v0.8.3), each against a fresh FYLO root, on macOS, Linux or Windows (Git
# Bash). Run it from a copy of this folder outside the repository: it
# installs node_modules, builds the Go app and places links there.
# Usage: run.sh <fylo-binary> <artifacts-dir> [js|go|all] [--method <m>]
set -u
here=$(cd "$(dirname "$0")" && pwd)
fylo=$1
artifacts=$2
only=${3:-all}
method=
if [ "${4:-}" = "--method" ]; then method="--method $5"; fi
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=.exe ;; *) exe= ;; esac
native() { if command -v cygpath > /dev/null; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
work=$here/work
rm -rf "$work" && mkdir -p "$work/bin"
# postinstall runs `fylo link`, so a `fylo` must be on PATH.
cp "$fylo" "$work/bin/fylo$exe"
art=$(native "$artifacts")
overall=0
step() { echo "=== $1 $(date +%T)"; }
expect() { # expect <ok|fail> <label> <command...>
    local want=$1 label=$2; shift 2
    local output rc
    output=$("$@" 2>&1); rc=$?
    if { [ "$want" = ok ] && [ $rc -eq 0 ]; } || { [ "$want" = fail ] && [ $rc -ne 0 ]; }; then
        echo "PASS $label"
    else
        echo "FAIL $label (rc=$rc)"; overall=1
    fi
    printf '%s\n' "$output" | sed 's/^/    /' | head -14
}
set_scripts() { # set_scripts <json or "">
    node -e "const fs=require('fs');const p=JSON.parse(fs.readFileSync('package.json'));if(process.argv[1]){p.scripts=JSON.parse(process.argv[1])}else{delete p.scripts}fs.writeFileSync('package.json',JSON.stringify(p,null,2)+'\n')" "$1"
}

if [ "$only" != go ]; then
    step "js: setup"
    cd "$here/js"
    set_scripts ""
    npm ci --no-audit --no-fund > /dev/null 2>&1
    "$fylo" link --undo --artifacts "$art" > /dev/null 2>&1
    "$here/make-root.sh" "$fylo" "$work/js-root"
    root=$(native "$work/js-root")
    expect fail "js: the stock SDK cannot open a FYLO root" node conformance.mjs "$root" "$(native "$work/js-plain-0.db")"
    expect fail "js: --check before linking reports no link" "$fylo" link --check --artifacts "$art"
    mkdir -p "$work/mismatch" && cp "$artifacts"/* "$work/mismatch/"
    node -e "const fs=require('fs');const f=process.argv[1];const m=JSON.parse(fs.readFileSync(f));m.turso='0.8.2';fs.writeFileSync(f,JSON.stringify(m))" "$(native "$work/mismatch/fylo-link.json")"
    expect fail "js: a version mismatch is refused with the install command" "$fylo" link --artifacts "$(native "$work/mismatch")"
    expect ok "js: fylo link $method" "$fylo" link $method --artifacts "$art"
    expect ok "js: fylo link --check" "$fylo" link --check --artifacts "$art"
    expect ok "js: conformance" node conformance.mjs "$root" "$(native "$work/js-plain-1.db")"

    step "js: npm ci and postinstall"
    expect ok "js: npm ci" npm ci --no-audit --no-fund
    expect fail "js: npm ci removed the link" "$fylo" link --check --artifacts "$art"
    set_scripts "{\"postinstall\":\"fylo link $method --artifacts \\\"${art//\\/\\\\}\\\"\"}"
    expect ok "js: npm ci with postinstall" env PATH="$work/bin:$PATH" npm ci --no-audit --no-fund
    expect ok "js: postinstall restored the link" "$fylo" link --check --artifacts "$art"
    "$here/make-root.sh" "$fylo" "$work/js-root-2"
    expect ok "js: conformance after npm ci" node conformance.mjs "$(native "$work/js-root-2")" "$(native "$work/js-plain-2.db")"
    expect ok "js: fylo link --undo" "$fylo" link --undo --artifacts "$art"
    expect fail "js: --check after --undo" "$fylo" link --check --artifacts "$art"
    set_scripts ""
fi

if [ "$only" != js ]; then
    step "go: setup"
    cd "$here/go"
    "$fylo" link --undo --artifacts "$art" > /dev/null 2>&1
    # A prebuilt app (FYLO_GO_CONFORMANCE_BIN) lets an emulated platform skip
    # compiling it under emulation.
    if [ -n "${FYLO_GO_CONFORMANCE_BIN:-}" ]; then
        cp "$FYLO_GO_CONFORMANCE_BIN" "$work/go-conformance$exe"
    elif ! go build -o "$work/go-conformance$exe" .; then
        echo "FAIL go: build"; overall=1; step "done overall=$overall"; exit 1
    fi
    # tursogo's "system" search needs LD_LIBRARY_PATH on Linux (fylo link says so).
    golib=(env "LD_LIBRARY_PATH=$here/go${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}")
    "$here/make-root.sh" "$fylo" "$work/go-root"
    root=$(native "$work/go-root")
    expect fail "go: without the link, the system strategy finds no library" "${golib[@]}" "$work/go-conformance$exe" "$root" "$(native "$work/go-plain-0.db")"
    expect ok "go: fylo link $method" "$fylo" link $method --artifacts "$art"
    expect ok "go: fylo link --check" "$fylo" link --check --artifacts "$art"
    if [ "$(uname -s)" = Linux ]; then
        expect fail "go: on Linux, without LD_LIBRARY_PATH tursogo cannot load the link" env -u LD_LIBRARY_PATH "$work/go-conformance$exe" "$root" "$(native "$work/go-plain-unset.db")"
    fi
    expect ok "go: conformance" "${golib[@]}" "$work/go-conformance$exe" "$root" "$(native "$work/go-plain-1.db")"
    expect ok "go: fylo link --undo" "$fylo" link --undo --artifacts "$art"
    expect fail "go: --check after --undo" "$fylo" link --check --artifacts "$art"
fi

step "done overall=$overall"
exit $overall
