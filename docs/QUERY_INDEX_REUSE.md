# Equality planning and content-verified index reuse

## Query correctness

Native document and file queries must produce the same result as evaluating their
predicates over all accessible records. An index candidate set may include extra
records, but it must never omit a matching record. Document predicates, access
checks, decryption, TTID ordering, and positive limits still run after planning.

FYLO uses loose equality: for example, numeric `1` can match the string `"01"`
and boolean `true`. Exact text keys alone are insufficient. Equality planning
unions the existing exact-value key with matching numeric-coercion keys. Both
positive and negative zero keys are included for zero comparisons. The stored
index format does not change, and indexes do not need rebuilding for this change.

Null equality requires a scan because it also matches a missing field, which has
no index entry. Container comparisons and schema-encrypted fields are not planned.
A safe equality conjunct can still narrow candidates when another conjunct is
unsupported. If any OR branch has no safe narrowing predicate, the whole query
falls back to a document scan. Dot/slash path normalization matches predicate
evaluation, including ignored empty path segments.

## Why the generation-only PoC is not promoted

Missing transaction state means generation zero. Collection replacement, branch
copying, and repository restore/rollback can also leave or reuse generation state;
counters saturate at their maximum value. An unchanged generation number alone
does not prove an unchanged index. The existing stable-read check remains useful
around a complete query, but it is not sufficient as a persistent cache key.

## Content-verified reuse

Native snapshot reuse retains one immutable parsed snapshot, its original snapshot
bytes, and its WAL bytes. Every call still uses the existing bounded, path-safe
file reads. Only byte-for-byte identical snapshot and WAL inputs can reuse the
parsed view. Changed, missing, corrupt, inaccessible, or unsafe input is not hidden
by a cached result. Complete WAL records retain their existing semantics; torn
tails remain ignored during merging but participate in the freshness comparison.

The process-wide cache permits reuse across short-lived native engine instances,
including machine-protocol requests. It retains at most one entry with a 64 MiB
admission budget for input buffer capacities plus parsed snapshot bytes; larger inputs
are parsed without retention. Concurrent readers can retain their own shared
views, and parsing/read buffers are additional transient memory. This is not a
64 MiB cap on total process memory.

The existing owned `index_snapshot()` API remains available. Native query planning
and explicit index scanning use a shared immutable snapshot to avoid cloning its
bytes. No document bodies, authorization decisions, or query results are cached.
An authorization change is therefore checked on the next document read, even
when the index bytes remain unchanged.

This optimization removes repeated parsing, sorted-set merging, and validation
when inputs match. It does **not** eliminate file I/O, fix pre-existing repository
swap concurrency limitations, or promise the generation-only PoC's latency.
Changes still pass through the engine's existing stable-read checks.

## Validation and remaining work

The subsequent [release qualification](QUERY_RELEASE_QUALIFICATION.md) records
broader tests, defects found and fixed, browser behavior, end-to-end measurements,
and the remaining platform/load gates. The measurements below describe the
earlier warm component benchmark, not a whole-query speedup.

Regression tests compare native and SQL results with full scans for coercion,
missing/null values, signed zero, mixed conjuncts, OR fallback, paths, and limits.
Cache tests exercise changed input, unsafe/corrupt input, admission limits, and
reuse without relying on generation changes. Document permissions remain checked
after selecting candidates.

Validation on macOS arm64 with Rust 1.97.1 passed all 134 tests across
`fylo-engine`, `fylo-query`, `fylo-storage-native`, and `fylo-machine`. Strict
Clippy (`--all-targets -- -D warnings`) and formatting checks passed for the
three changed crates. The browser engine also passed a
`wasm32-unknown-unknown` compile check using the existing build script's
`RUSTFLAGS='--cfg getrandom_backend="custom"'` entropy configuration. This is
not a claim of browser runtime, cross-platform, or full SDK release qualification.

On the same 10,000-document synthetic fixture used for the PoC, the content-verified
shared snapshot path measured 4.369 ms p50 / 5.214 ms p95, versus 72.307 ms p50 /
89.311 ms p95 when the parsed cache was evicted before each timed load. That is
about 16.6x faster for this warm-filesystem snapshot-load/merge/scan component.
Both paths include generation reads. Eviction occurs outside the timer by reading
a different small fixture; OS file caches are not purged. The 19,704,300-byte
snapshot fits the cache admission budget. These are not end-to-end database or
cold-storage timings, and they do not claim the unsafe generation-only PoC's
0.29 ms result.

Raw measurements and harness changes remain ignored under
`benchmarks/tin-poc/results-content-reuse-10000.json`. The standalone benchmark
crate has its own lockfile; its dependency resolution is separate from the main
workspace's tested dependency graph.

BM25, SIMD/compressed query execution, LIKE promotion, and broad range planning
remain experimental. A future generation-only fast path needs an incarnation and
publication protocol covering every writer, rebuild, branch/root replacement,
restore, and recovery operation, plus a policy for out-of-band filesystem edits.
