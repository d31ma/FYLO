#!/bin/zsh
# fylo link conformance on macOS arm64: JS (@tursodatabase/database 0.8.3)
# and Go (tursogo v0.8.3), each against a fresh FYLO root. Run it from a copy
# of this folder outside the repository: it installs node_modules, builds the
# Go app and places links there.
# Usage: run.sh <fylo-binary> <artifacts-dir>
set -u
fylo=$1
artifacts=$2
here=${0:A:h}
work=$here/work
rm -rf $work && mkdir -p $work
overall=0
step() { echo "=== $1 $(date +%T)"; }
expect() { # expect <ok|fail> <label> <command...>
    local want=$1 label=$2; shift 2
    local output; output=$("$@" 2>&1); local rc=$?
    if [[ ($want == ok && $rc -eq 0) || ($want == fail && $rc -ne 0) ]]; then
        echo "PASS $label"
    else
        echo "FAIL $label (rc=$rc)"; overall=1
    fi
    print -r -- "$output" | sed 's/^/    /' | head -12
}

step "js: setup"
cd $here/js
npm ci --no-audit --no-fund > /dev/null
rm -f node_modules/@tursodatabase/database/turso.darwin-arm64.node
node -e "const p=require('./package.json'); delete p.scripts; require('fs').writeFileSync('package.json', JSON.stringify(p,null,2)+'\n')"
$here/make-root.sh $fylo $work/js-root
expect fail "js: the stock SDK cannot open a FYLO root" node conformance.mjs $work/js-root $work/js-plain-0.db
expect fail "js: --check before linking reports no link" $fylo link --check --artifacts $artifacts
mkdir -p $work/mismatch && cp $artifacts/* $work/mismatch/ && sed -i '' 's/"turso":"0.8.3"/"turso":"0.8.2"/' $work/mismatch/fylo-link.json
expect fail "js: a version mismatch is refused with the install command" $fylo link --artifacts $work/mismatch
expect ok "js: fylo link" $fylo link --artifacts $artifacts
ls -l node_modules/@tursodatabase/database/turso.darwin-arm64.node | sed 's/^/    /'
expect ok "js: fylo link --check" $fylo link --check --artifacts $artifacts
expect ok "js: conformance" node conformance.mjs $work/js-root $work/js-plain-1.db

step "js: npm ci and postinstall"
expect ok "js: npm ci" npm ci --no-audit --no-fund
expect fail "js: npm ci removed the link" $fylo link --check --artifacts $artifacts
node -e "const p=require('./package.json'); p.scripts={postinstall:'fylo link --artifacts $artifacts'}; require('fs').writeFileSync('package.json', JSON.stringify(p,null,2)+'\n')"
expect ok "js: npm ci with postinstall" env PATH=${fylo:h}/link-bin:$PATH npm ci --no-audit --no-fund
expect ok "js: postinstall restored the link" $fylo link --check --artifacts $artifacts
$here/make-root.sh $fylo $work/js-root-2
expect ok "js: conformance after npm ci" node conformance.mjs $work/js-root-2 $work/js-plain-2.db
expect ok "js: fylo link --undo" $fylo link --undo --artifacts $artifacts
expect fail "js: --check after --undo" $fylo link --check --artifacts $artifacts
node -e "const p=require('./package.json'); delete p.scripts; require('fs').writeFileSync('package.json', JSON.stringify(p,null,2)+'\n')"

step "go: setup"
cd $here/go
rm -f libturso_sync_sdk_kit.dylib
go mod tidy > /dev/null 2>&1
if ! go build -o $work/go-conformance .; then echo "FAIL go: build"; overall=1; step "done overall=$overall"; exit 1; fi
$here/make-root.sh $fylo $work/go-root
expect fail "go: without the link, the system strategy finds no library" $work/go-conformance $work/go-root $work/go-plain-0.db
expect ok "go: fylo link" $fylo link --artifacts $artifacts
expect ok "go: fylo link --check" $fylo link --check --artifacts $artifacts
expect ok "go: conformance" $work/go-conformance $work/go-root $work/go-plain-1.db
expect ok "go: fylo link --undo" $fylo link --undo --artifacts $artifacts
expect fail "go: --check after --undo" $fylo link --check --artifacts $artifacts

step "done overall=$overall"
exit $overall
