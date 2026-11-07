#!/bin/bash
# Runs every benchmark case in order, one job at a time, and appends each
# result to $LOGS/results.jsonl. Usage: run.sh <logs dir> [step ...]
set -u

LOGS=${1:?logs directory}
shift
STEPS=${*:-local10k crash damage atlas10k atlasfylo local-top local-mid variant atlas-top}
BIN=${BENCH_BIN:-$HOME/Developer/Builds/parquet-bench/release/fylo-parquet-bench}
LOCAL=${BENCH_LOCAL:-$HOME/Developer/Builds/parquet-bench-data}
ATLAS=${BENCH_ATLAS:-/Volumes/ATLAS/FYLO/parquet-bench}
RESULTS=$LOGS/results.jsonl
mkdir -p "$LOGS" "$LOCAL" "$ATLAS"

step() {
  local name=$1
  shift
  echo "=== $name $(date '+%F %T')"
  "$@"
  local code=$?
  echo "=== $name rc=$code $(date '+%F %T')"
  return 0
}

free_space() {
  df -h "$HOME" "$ATLAS" | sed 's/^/    /'
}

bench_run() {
  local engine=$1 shape=$2 n=$3 where=$4 dir=$5
  shift 5
  rm -rf "$dir"
  "$BIN" run --engine "$engine" --shape "$shape" --n "$n" --dir "$dir" \
    --results "$RESULTS" --location "$where" "$@"
}

local10k() {
  for shape in small medium; do
    for engine in fylo-files parquet; do
      step "local10k-$engine-$shape" bench_run "$engine" "$shape" 10000 local \
        "$LOCAL/10k-$shape-$engine"
    done
  done
}

crash() {
  for engine in fylo-files parquet; do
    rm -rf "$LOCAL/crash-$engine"
    step "crash-put-$engine" "$BIN" crash --engine "$engine" --shape small --n 5000 \
      --rounds 10 --mode put --dir "$LOCAL/crash-$engine" --results "$RESULTS"
  done
  rm -rf "$LOCAL/crash-parquet-compact"
  step "crash-compact-parquet" "$BIN" crash --engine parquet --shape small --n 20000 \
    --rounds 10 --mode compact --dir "$LOCAL/crash-parquet-compact" --results "$RESULTS"
}

damage() {
  for engine in fylo-files parquet; do
    for kind in tail middle index; do
      rm -rf "$LOCAL/damage-$engine-$kind"
      step "damage-$kind-$engine" "$BIN" damage --engine "$engine" --shape medium \
        --n 10000 --kind "$kind" --dir "$LOCAL/damage-$engine-$kind" --results "$RESULTS"
    done
  done
}

atlas10k() {
  for shape in ${ATLAS_SHAPES:-small medium}; do
    step "atlas10k-parquet-$shape" bench_run parquet "$shape" 10000 atlas \
      "$ATLAS/10k-$shape-parquet" --reads 300 --queries 10 --scans 3 --writes 100
  done
}

atlasfylo() {
  for shape in small medium; do
    local source=$LOCAL/fylo-copy-$shape
    local target=$ATLAS/fylo-copy-$shape
    rm -rf "$source" "$target"
    step "atlasfylo-write-probe-$shape" bench_run fylo-files "$shape" 1000 atlas \
      "$ATLAS/fylo-write-probe-$shape" --batch 100 --no-extras
    step "atlasfylo-load-$shape" "$BIN" load --engine fylo-files --shape "$shape" --n 10000 \
      --dir "$source" --results "$RESULTS" --location local
    mkdir -p "$target"
    local started=$(date +%s)
    step "atlasfylo-copy-$shape" cp -R "$source/root" "$target/root"
    echo "    copy of the 10k root took $(( $(date +%s) - started )) s"
    cp "$source/ids.txt" "$target/ids.txt"
    step "atlasfylo-read-$shape" "$BIN" run --engine fylo-files --shape "$shape" --n 10000 \
      --dir "$target" --prebuilt --read-only --ids "$target/ids.txt" --no-extras \
      --results "$RESULTS" --location atlas --reads 300 --queries 3 --scans 2
    rm -rf "$source"
  done
}

local-top() {
  free_space
  step "localtop-fylo-files-small" bench_run fylo-files small 1000000 local \
    "$LOCAL/top-small-fylo" --queries 5 --scans 3
  rm -rf "$LOCAL/top-small-fylo"
  step "localtop-parquet-small" bench_run parquet small 1000000 local \
    "$LOCAL/top-small-parquet" --queries 5 --scans 3
  rm -rf "$LOCAL/top-small-parquet"
  step "localtop-fylo-files-medium" bench_run fylo-files medium 100000 local \
    "$LOCAL/top-medium-fylo" --queries 5 --scans 3
  rm -rf "$LOCAL/top-medium-fylo"
  step "localtop-parquet-medium" bench_run parquet medium 100000 local \
    "$LOCAL/top-medium-parquet" --queries 5 --scans 3
  rm -rf "$LOCAL/top-medium-parquet"
  free_space
}

# FYLO's per-document layout stops accepting writes after about 101k small
# or 24k medium documents (keys.wal passes MAX_INDEX_WAL_BYTES), so its
# second scale is 50k small; Parquet runs at the same size for comparison.
local-mid() {
  for engine in fylo-files parquet; do
    step "localmid-$engine-small" bench_run "$engine" small 50000 local \
      "$LOCAL/mid-small-$engine" --queries 5 --scans 3
    rm -rf "$LOCAL/mid-small-$engine"
  done
}

# Parquet tuned for point reads: 64-row data pages instead of 1024.
variant() {
  export BENCH_PAGE_ROWS=64
  for shape in small medium; do
    step "variant-page64-$shape" bench_run parquet "$shape" 10000 local-page64 \
      "$LOCAL/page64-$shape"
    rm -rf "$LOCAL/page64-$shape"
  done
  unset BENCH_PAGE_ROWS
}

# Bulk loads on the share use 10,000-document files: one SMB file create,
# flush and rename per 1,000 documents stalled for up to 100 s at a time.
atlas-top() {
  step "atlastop-parquet-small" bench_run parquet small 1000000 atlas \
    "$ATLAS/top-small-parquet" --batch 10000 --reads 300 --queries 10 --scans 3 --writes 100
  step "atlastop-parquet-medium" bench_run parquet medium 100000 atlas \
    "$ATLAS/top-medium-parquet" --batch 10000 --reads 300 --queries 10 --scans 3 --writes 100
  du -sh "$ATLAS" | sed 's/^/    ATLAS bench folder: /'
}

echo "=== bench $(date '+%F %T') steps: $STEPS"
echo "    host $(hostname) $(sysctl -n machdep.cpu.brand_string) $(sysctl -n hw.ncpu) cores $(( $(sysctl -n hw.memsize) / 1073741824 )) GiB"
free_space
for name in $STEPS; do
  "$name"
done
echo "=== bench done $(date '+%F %T')"
