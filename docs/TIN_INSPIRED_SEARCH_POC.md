# TIN-inspired query and search proof of concept

## Scope and decision

This is a benchmark-first experiment, not a new FYLO storage format or a shipped
search API. The implementation and raw measurements live in the ignored
`benchmarks/tin-poc/` directory, consistent with the repository's benchmark policy.
No production engine, client, website, dependency manifest, or release workflow is
changed by this experiment. This report is the durable record; a fresh clone will
not contain the ignored experiment sources or outputs.

The motivation is PlanetScale's [TIN introduction](https://planetscale.com/blog/introducing-tin)
and [search-engine internals](https://planetscale.com/blog/anatomy-of-a-postgres-search-engine).
We borrow the ideas of reusable index views, candidate selection, and inverted
term postings, not PostgreSQL's physical tuple identifiers or TIN's performance
claims. FYLO documents remain authoritative JSON and identifiers remain independent
of physical disk positions. No service, S3 primary, or PostgreSQL dependency is added.

## Acceptance criteria

- Exercise the real native snapshot loader against generated snapshot/WAL files.
- Compare repeated loading with generation-checked reuse of the same validated view.
- Reject writing generations, generation changes during reads, corrupt refreshed
  snapshots, and corrupt generation state. Never reuse another collection's view.
- Candidate planning must not omit matching documents. Verify every candidate with
  FYLO's production `StructuredQuery::matches` predicate and compare with a full scan.
- Account for missing string expansions, coercion, OR branches, and unsupported
  conjuncts. Preparation and warm query costs must be reported separately.
- Compare BM25 ranked identifiers and scores with a simple scan implementation,
  including empty input, Unicode, repeated terms, ties, and update/delete rebuilds.
- Report warm component latency, preparation time, and actual or explicitly labeled
  estimated sizes. Do not imply cold-disk or full-engine throughput was measured.

## Components

### Reusable snapshot

The baseline uses `NativeCollection::index_snapshot`, which loads and validates
the snapshot, materializes a sorted key set, applies complete WAL records, and
serializes and validates the merged result. Both measured paths read generation
state before and after the scan. The experimental cache retains one immutable
view and reloads it on a generation change; a changed read is rejected, not retried.
The first load is measured separately.

This assumes every mutation follows the existing monotonic generation protocol.
Out-of-band filesystem edits, restoring an old generation, root replacement at
the same path, bounded retries, multi-root eviction, concurrent writers, and
power-loss recovery are production integration gates, not solved by this cache.
The test fixture writes generation files directly to exercise rejection paths;
it does not simulate native transactional writes or certify crash safety.

### Candidate planning

An explicitly prepared plaintext fixture view is used to prove index coverage.
Warm planning timing does not include that preparation. Actual documents are
still evaluated by the production predicate implementation. This is not a
replacement for access checks, schema encryption handling, or committed-generation
validation in the engine.

Preparation regenerates all prefix keys and requires exact equality with the
snapshot. The borrowed snapshot and document set then remain immutable. This
expensive check is a PoC correctness guard, not the proposed production preparation
algorithm. Candidate rules cover homogeneous-type equality, numeric ranges, LIKE
prefix/suffix/trigram filters, and supported conjuncts alongside unsupported ones.
An unbounded OR branch falls back to a full scan. Documents with long strings
missing their f/r/g3 expansions are explicitly unioned into candidates.

The review exposed a pre-existing production-planner concern: predicates implement
loose equality, but exact-key candidates are not necessarily a superset. For
example, `{"age":"01"}` matches `$eq: 1` through coercion but is indexed under
`age/eq/01/`, not `age/eq/1/`. Missing fields also match `$eq: null` without a
corresponding null key. The experimental planner declines unsafe rules. Production
was unchanged during the PoC; the subsequent native correction is recorded in
[Query index reuse](QUERY_INDEX_REUSE.md).

### Optional BM25

Only a caller-selected synthetic plaintext text field is indexed. The index keeps
terms, term frequencies, document lengths, and an identifier table, not document
bodies. BM25 uses `k1 = 1.2` and `b = 0.75`. Tokens split at non-Unicode-alphanumeric
characters and are lowercased; there is no normalization, stemming, phrase position,
fuzzy matching, or language-specific analyzer. Queries deduplicate terms and use
OR matching. Results sort by score descending, then identifier ascending.

The scan oracle computes document frequencies and counts from source text. The
prototype rebuilds after mutations; it does not implement durable incremental
postings. Common-term top-k still scores and sorts all matching documents.
Reported logical index size is a fixed-width serialization estimate, not heap RSS
or an emitted file format. This introduces term/count data beyond the existing
prefix-only contract and therefore requires an explicit format/API decision
before promotion.

A separate size experiment emits per-term delta-varint buffers for document
ordinals and frequencies, compares their byte lengths with raw 64-bit pairs, and
tests decoding round trips and malformed input rejection. Dictionary, identifier
table, lengths, and framing are excluded from both posting-size figures. Search
still uses uncompressed postings; no SIMD or compressed-query speedup is claimed.

## Reproduction

From the repository root, using the pinned toolchain through the existing wrapper:

```sh
bun scripts/run-rust.mjs cargo test --manifest-path benchmarks/tin-poc/Cargo.toml
bun scripts/run-rust.mjs cargo run --release --manifest-path benchmarks/tin-poc/Cargo.toml -- 1000 30 benchmarks/tin-poc/results-1000.json
bun scripts/run-rust.mjs cargo run --release --manifest-path benchmarks/tin-poc/Cargo.toml -- 10000 30 benchmarks/tin-poc/results-10000.json
```

The harness creates a unique temporary directory, removes only that owned fixture
on exit, and never opens an existing user database. Limits are 1-100,000 documents
and 1-1,000 samples. There are three warmup iterations per timed operation; p50/p95
use nearest-rank percentiles. The deterministic corpus contains names, ages,
groups, and text. Every hundredth document has `name = samuel` and rare search
terms. Structured-query and BM25 tests run on resident documents; only snapshot
loading reads filesystem data. OS caches are not purged.

## Production promotion gates

1. Integrate snapshot reuse behind the engine's existing stable-read protocol;
   bound memory, retry counts, and WAL overlay work; cover process restart, writer
   races, index rebuild, root replacement, and recovery with fault injection.
2. Promote candidate rules individually, with differential/property tests covering
   actual coercion, missing/null values, arrays, dates, encrypted fields, access
   checks, limits, ordering, Unicode, and long strings. Persist coverage metadata
   or retain safe fallbacks rather than scanning the entire corpus per query.
3. Measure native end-to-end document I/O, writes, incremental maintenance,
   multiple concurrent processes, cache pressure, and cold-cache behavior on
   representative user data before promising performance gains.
4. Define opt-in searchable fields, authorization-before-ranking/top-k semantics,
   score/statistics disclosure policy, tokenizer versioning, atomic publication,
   integrity checks, rebuild, and disk-format compatibility before exposing BM25.
5. Benchmark compressed postings decode/intersection/query latency before adopting
   a new representation. Encoded-size savings alone do not establish faster search.

## Results

Measured 2026-09-26 UTC on Apple M4, 16 GiB RAM, macOS 26.6.2, Rust 1.97.1,
release `opt-level = 3`. These are single-host warm component measurements, not
end-to-end FYLO latency, statistically independent trials, or PlanetScale-scale
evidence. Other desktop activity was not controlled.

The 10,000-document run used 30 timed samples per operation. All times below are
milliseconds; parenthesized values are p95. The baseline for structured queries
is an in-memory full document scan; BM25's baseline includes tokenization and
statistics construction on each query. The snapshot baseline is the actual native
snapshot loader plus scan, with generation reads in both paths.

| Component/workload | Baseline p50 (p95) | Prototype p50 (p95) | Interpretation |
| --- | ---: | ---: | --- |
| Snapshot load/merge + selective scan | 71.448 (76.422) | 0.296 (0.308) | About 241x faster repeated component execution; first load still 67.659 ms |
| Equality plus unsupported `$ne` conjunct | 0.199 (0.284) | 0.028 (0.042) | 100 candidates instead of 10,000 documents |
| Name prefix `sam%` | 1.331 (1.504) | 0.036 (0.041) | About 37x faster after preparation |
| Name suffix `%uel` | 1.394 (1.579) | 0.038 (0.042) | About 37x faster after preparation |
| Name contains `%amu%` | 1.562 (1.775) | 0.036 (0.037) | About 43x faster after preparation |
| Numeric range 40 <= age < 45 | 0.194 (0.211) | 1.968 (2.149) | About 10x slower; broad scans/intersection need cost selection |
| Bounded OR | 1.747 (1.902) | 0.069 (0.074) | Both branches safely narrowed |
| Unbounded OR | 0.560 (0.636) | 0.596 (0.642) | Safe scan fallback with planning overhead |
| BM25 rare terms `rust search` | 2.394 (2.629) | 0.009 (0.010) | About 262x versus the intentionally simple scan oracle |
| BM25 common term `storage` | 2.436 (2.632) | 1.247 (1.395) | About 2x; broad scoring/sorting remains expensive |

At 1,000 documents, the snapshot comparison was 7.031 ms vs 0.261 ms p50
(about 27x), with a 1,946,778-byte prefix snapshot. At 10,000 documents:

- Prefix snapshot: 19,704,300 bytes, also retained by the cache, excluding allocator
  overhead. Prefix-key generation/assembly: 393.417 ms, excluding file write.
- Exact-coverage planner preparation: 790.354 ms. This cost must be amortized;
  it is not included in the warm planner table and is not a production algorithm.
- BM25 construction: 5.295 ms over 477,700 bytes of selected text.
- Estimated fixed-width BM25 logical size: 1,229,298 bytes, including identifiers,
  lengths, and term metadata. This is larger than the text input in this corpus;
  avoiding document duplication does not mean zero index overhead.
- Actual posting buffers: 956,800 raw pair bytes vs 119,600 delta-varint bytes,
  an 87.5% reduction for postings only. There is no compressed query benchmark.
- No allocation counts, peak RSS, device read bytes, incremental write latency,
  cold-cache measurements, or crash-recovery benchmark were collected.

The harness compares every benchmark result with its reference before timing.
The test suite covers 21 behaviors across snapshot invalidation, planner parity,
BM25 ranking/rebuilds, and compressed-buffer round trips/corruption. A parallel
test run exposed timestamp-only temporary-directory collisions; fixture names now
include a per-process atomic sequence.

Validation: all 21 tests passed through the repository toolchain wrapper, followed
by five repeated runs of the same compiled test executable. Formatting checks,
Clippy with warnings denied, and `git diff --check` passed. Independent review
found no blocking issue within the documented synthetic-fixture scope. Planner
timing includes query parsing while its full-scan baseline uses a parsed query,
which slightly disadvantages the prototype.

## Recommendation after measurement

Proceed first with **planner correctness and generation-safe snapshot reuse**.
Then integrate selective equality/LIKE candidates with explicit coverage rules.
Do not automatically promote the numeric-range strategy: the measured CPU
regression needs a selectivity/cost decision and disk-I/O evidence. Keep BM25
opt-in and experimental until persistence, authorization, and maintenance costs
are defined. Defer SIMD and any index-format replacement until compressed-query
benchmarks justify them. The PoC is complete; production integration is not.

## Subsequent native integration

The follow-up implemented coercion-safe equality candidates and bounded,
content-verified snapshot reuse. Generation-only reuse was deliberately not
promoted: replacement and rollback paths do not guarantee a unique generation.
See [Query index reuse](QUERY_INDEX_REUSE.md) for the safety contract, scope,
and separate production-path component measurements. BM25, LIKE promotion,
and broad range planning remain experimental.
