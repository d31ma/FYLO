# Query integration qualification

## Scope and verdict

This records the local qualification of conservative query candidates and
content-verified index reuse on 2026-09-26 UTC. It is not a release certificate.
The worktree contains other uncommitted work, and no commit, push, release,
deployment, or remote CI run was performed by this qualification task.

Local native, interoperability, crash-recovery, and all three browser checks
passed. Linux/Windows execution on this worktree, cold storage measurements,
and sustained release-load qualification remain open. The user explicitly chose
to keep the follow-up local: no qualification branch was committed or pushed.
The next feature phase (LIKE acceleration and cost-aware range planning) has not
been promoted.

## Defects found and fixed

- The JavaScript browser planner used exact index keys for loose equality and
  array containment. These could omit numeric aliases, missing/null values, and
  nested-array coercion matches. These operators no longer narrow candidates in
  the JavaScript browser path; another safe conjunct can still narrow the query,
  otherwise it scans. All candidates still undergo full predicate evaluation.
  This trades acceleration
  for correctness; the native Rust candidate planner retains its safe equality
  optimization. Neither path changes the persisted index format.
- Native stable reads propagated a transient document parse error before checking
  whether a writer changed the generation. They now recheck the generation before
  returning either success or error, using the existing bounded retry loop.
  Unchanged-generation corruption and unsafe paths still fail closed.
- The retained scheduled-fuzz artifact from GitHub run `36122996084` exposed drift
  of a large finite JSON number after encoding and reparsing. The shared
  `serde_json/float_roundtrip` feature fixes this. Regression tests cover the exact
  90-byte artifact and 4,096 deterministic floating-point bit patterns. This is
  preservation of decoded finite floating-point values, not arbitrary-precision
  integers or exact preservation of all decimal digits in the input.
- Unknown CLI commands opened configuration/storage before showing usage. They
  now reject unknown commands first, including when application.json is invalid.
- Qualification tooling now supports custom Cargo output paths for browser
  artifacts and explicit prebuilt binaries for the crash matrix. Default CI still
  builds normally. Service-worker tests now use real Request/Response behavior and
  verified emitted asset bytes; integrity/privacy guards were not removed.
- Language probes suppress .NET first-run banner output so it cannot contaminate
  the returned document identifier.

## Local evidence

Host: Apple M4, 16 GiB RAM, macOS 26.6.2, Rust 1.97.1. Final interoperability
checks use pinned Bun 1.3.11. Missing Dart 3.13.4 and .NET SDK 8.0.425/runtime
8.0.31 were checksum-verified and installed into a temporary test-only directory;
machine-wide defaults were not replaced.

| Gate | Result |
| --- | --- |
| Rust workspace, all targets/features, locked dependencies | 179 tests passed |
| Strict whole-workspace Clippy, all targets/features | Passed |
| Rust formatting, JavaScript lint, TypeScript checking | Passed |
| Interop suite, including website and Explorer bundles | 156 passed; 5 platform/opt-in skips |
| Published language-client configuration/CRUD probes | All 9 passed: Node, Python, Ruby, PHP, Go, Java, Rust, C#, Dart |
| Native root lease, disk-pressure, validator cleanup | 3 passed separately against fresh development binary |
| Crash matrix against final native binaries | 270 interrupted mutations, 15 failpoints, 36 queue interruptions passed |
| Repeated concurrency/recovery integration suite | 10 repetitions of 5 tests passed |
| Browser JavaScript, adapters, and required WASM scanner | 69 tests, 288 assertions passed |
| Fresh browser WASM host protocol | 130 frames, 122 documents passed |
| Playwright 1.62.0, final local follow-up | 9 passed; zero skips |
| Bounded native restart soak | 19,518 operations, 24,794 assertions, 105 restarts passed |
| Bounded four-process contention | 965 patches, 47 rebuilds, 14,745 coherent reads; no lost writes or duplicate owners |
| Above-budget snapshot admission | Five oversized loads not retained; small cache and WAL freshness checks passed |
| Larger materialized database | 10,000 real documents; full-body/identifier oracles and post-mutation checks passed |
| Hardened soak runner | 15 lifecycle/process tests plus 6 CI-contract tests passed; 1,000 real-engine iterations and 40 verified restarts passed |

Chromium 151.0.7922.34, Firefox 153.0, and WebKit 26.5 each passed all three browser
scenarios. WebKit's earlier failures were isolated to ephemeral Playwright
contexts: raw OPFS access failed before FYLO ran on both localhost and 127.0.0.1,
but succeeded in fresh persistent contexts on both hosts. The E2E fixture now
creates and cleans up a unique persistent WebKit profile per test. It does not
change browser security settings or touch user profiles. Chromium/Firefox retain
their existing fixtures. The final strict matrix passed all nine tests in 32.4
seconds. Unavailable OPFS now fails these qualification tests rather than silently
skipping them; production fallback behavior is unchanged. Ephemeral/private-context
support is not established.
The final WASM engine SHA256 was
`b3e76ac66c78cffe97fc32afca81d01d75c83959c7bc6ac1feb97b52bbff7c28`.
Browser regressions assert candidate completeness against each engine's existing
predicate. They do not establish universal JS/Rust predicate equivalence: numeric
strings such as `"1"` and `"01"` retain existing cross-engine differences.

The repeated concurrency corpus includes 7,680 concurrent query attempts, 960
settled-state checks, 240 patches, and 80 rebuilds. It also checks drop/recreate,
delete/restore, same-generation root replacement, permanent corruption, and
symlink rejection. Successful concurrent reads must return coherent rows;
settled reads must observe the final state.

The five interop skips include the three explicitly enabled native tests above
and two native Windows tests. Passing the former against a development binary
does not qualify a signed/published release artifact.

Generation metadata replacement can still yield the existing fail-closed
`ENATIVE_UNSAFE_PATH` path-identity error during concurrent activity instead of
`EENGINE_CONCURRENT_WRITE`. The stress tests narrowly permit this outcome, not
arbitrary errors or stale settled results. Repository replacement protocols are
not certified as universally atomic by these tests.

## Bounded local restart/load follow-up

The existing smoke-soak harness ran for 122.459 seconds against the final debug
native binary, using one isolated three-document root and an exclusive owner.
It completed 2,638 iterations, 19,518 machine operations, 24,794 model assertions,
and 105 orderly process restarts. The model checks SQL and direct mutations,
equality queries, metadata, delete/restore, and continued operations after restart.
Unlike the hardened rerun below, it did not verify prior state before the first
post-restart mutation.

Sampled peak native RSS was 12,615,680 bytes (about 12.03 MiB); final root growth
was 11,272 bytes. Operation latency averaged 6.207 ms, with a 95.516 ms maximum.
RSS sampling is not a continuous high-water measurement. These shared-host debug
timings are not comparable to the optimized benchmark below.

The artifact is ignored at `benchmarks/tin-poc/results-soak-local-20260926.json`.
Reproduction uses `scripts/run-rust-soak.mjs --profile smoke --duration-hours 0.034
--restart-every 25 --checkpoint-every 25 --output <report.json>`, with
`FYLO_RUST_BINARY` pointing to the verified binary and `FYLO_SKIP_RUST_BUILD=1`.
This bounded smoke pass is not the 72-hour release profile or a large dataset
workload, and orderly restarts are separate from the crash matrix above.

An optimized, separate harness ran two competing leased writer processes and two
read-only processes for 30 seconds. It completed 965 patches, 47 rebuilds, and
14,745 coherent successful queries. It observed 10,591 expected lease rejections,
2,787 exhausted concurrent-write retries, and 534 exact changed-path identity
rejections. Successful rows satisfied `sequence % 2 == score == mirror`; all 100
settled checks passed, with zero duplicate owners or lost writes. This qualifies
the leased writer path, not direct unleased concurrent writes or a guarantee that
every overlapping query succeeds.

The same harness generated a valid 35,651,662-byte prefix snapshot using the real
document index-key generator for 379,273 synthetic documents. The minimum retained
size was 71,303,324 bytes, above the cache's 67,108,864-byte (64 MiB) admission
budget. All five loads released their oversized views rather than retaining them,
preserved the existing small cache, and observed WAL additions and truncation.
Loads took 81.2-100.8 ms. No document payloads were materialized: this tests snapshot
parsing and cache admission, not full queries over a 379,273-document database.

The full harness took 32.07 seconds; `/usr/bin/time -l` reported maximum RSS of
385,662,976 bytes (about 367.8 MiB) and zero swaps. It did not induce system-wide
memory pressure or purge filesystem caches. Worker cleanup was independently
reviewed and exercised with six real children across unwind, spawn failure,
parent timeout, and worker failure; all children were reaped before fixture
removal. Strict release Clippy passed for the standalone harness.

Final version-2 evidence is ignored at
`benchmarks/tin-poc/results-bounded-load-20260926.json`, with matching source/result
hashes and reproduction commands in
`benchmarks/tin-poc/results-bounded-load-resources-20260926.json`. Its source is
`benchmarks/tin-poc/src/bin/bounded-load.rs`. These local benchmark files are not
available in a fresh clone and use a separate lockfile, not the main workspace
dependency qualification.

## End-to-end native measurements

The ignored harness creates two real collections through NativeWriteRoot, with
500 documents each and a 912-byte body plus fields per document. Queries execute
ReadOnlyEngine.find and read complete documents from disk. Before timing, an
independent oracle compares expected identifiers and complete document bodies;
post-put/patch/delete checks also pass. Each measurement has 3 warmups and 30
timed samples. Results below use the final float-roundtrip and stable-read fixes.

| Operation | p50 ms | p95 ms |
| --- | ---: | ---: |
| Selective equality, 5 documents | 2.135 | 2.934 |
| Same query with parsed cache evicted before timing | 3.310 | 3.986 |
| Broad equality, 250 documents | 76.293 | 100.210 |
| Range fallback scan, 500 documents | 153.400 | 182.836 |
| Engine open plus selective query | 2.048 | 2.176 |
| Alternating collections, **two queries** | 7.555 | 16.971 |
| Durable put | 6.490 | 8.270 |
| Durable patch | 6.897 | 7.749 |
| Durable delete | 6.373 | 6.797 |

Selective real-document queries measured about 1.55x faster with parsed reuse in
this fixture, not the 16.6x warm component result in the earlier benchmark.
Broad queries spend much more time reading documents. Alternating collections
shows the effect of the single-entry cache; it is not a memory-exhaustion test.

The first query after fixture construction and parsed-cache eviction took
3.626 ms. This is **not cold storage**: filesystem caches were warmed by setup and
verification. Peak process-lifetime RSS was 19,136,512 bytes (18.25 MiB), including
fixture construction, verification, queries, mutations, and cleanup. Snapshot
size was 402,426 bytes, and the final mutation WAL was 49,632 bytes. Setup took
8.000 seconds; the full process took 17.77 seconds.

Raw results: `benchmarks/tin-poc/results-end-to-end-500-final.json`; resource
measurements and source hashes: `results-end-to-end-resources.json`. The harness
is `benchmarks/tin-poc/src/bin/end-to-end.rs`. These files remain ignored per the
project benchmark policy and are not available in a fresh clone. Its separate
lockfile does not replace the main workspace's locked-dependency test evidence.

This is a shared-host synthetic measurement, not a production latency guarantee.
It excludes machine-protocol transport/serialization and has no previous-release
write baseline. No causal write-overhead claim is made.

### Larger materialized database follow-up

The same harness also passed with 5,000 documents in each of two collections
(10,000 actual document files), using three warmups and ten timed samples per
operation. Full identifier/body oracles verified 50 selective, 2,500 broad, and
5,000 fallback results. It performed 39 mutation operations (13 puts, patches,
and deletes each), followed by the existing post-mutation checks.

| Operation | p50 ms | p95 ms |
| --- | ---: | ---: |
| Selective equality, 50 documents | 32.220 | 34.628 |
| Same query with parsed cache evicted before timing | 43.776 | 47.639 |
| Broad equality, 2,500 documents | 1,411.830 | 1,507.523 |
| Range fallback scan, 5,000 documents | 3,728.337 | 3,957.706 |
| Durable put | 10.608 | 16.388 |
| Durable patch | 13.188 | 15.953 |
| Durable delete | 13.698 | 17.269 |

The selective comparison measured about 1.36x faster with parsed reuse. With only
ten samples, nearest-rank p95 is the maximum observed sample, not a robust tail
estimate. Setup took 116.628 seconds; the full process took 196.32 seconds, with
113,655,808 bytes (108.4 MiB) maximum RSS and zero swaps. Maximum observed fixture
allocation was 51,304 KiB, sampled rather than continuously measured. The owned
fixture was cleaned normally. The final snapshot was 4,052,938 bytes and the WAL
was 19,472 bytes, so this did not test above-budget admission with real documents.

Results and command/source/binary hashes remain ignored in
`benchmarks/tin-poc/results-end-to-end-5000-20260926.json`,
`results-end-to-end-5000-resources-20260926.txt`, and
`results-end-to-end-5000-manifest-20260926.json`. One disk-usage traversal occurred
after timing began and may have introduced monitoring interference. This was a
warm, shared-host, single-process measurement, not a cold-storage or sustained
load test. Do not infer universal scaling from the earlier 500-document run.

## Long-run harness hardening

Readiness review found shortcomings in the soak runner, separate from the FYLO
engine. Native handshake, request, shutdown, build, and resource-sampling waits
are now bounded. Owned processes are stopped and reaped before fixture deletion;
cleanup continues after individual failures. An observed RSS breach now stops
the workload immediately, while disk-growth enforcement occurs at checkpoints.
Interruption, including during final cleanup, cannot produce passing evidence.
Failed gates retain the fixture by default.

The runner copies the executable into its private workspace, records its SHA256,
and rechecks that copy before every open. It uses an isolated application.json,
not repository configuration or secrets. Monotonic workload duration excludes
build, seeding, and final cleanup. Release qualification still requires at least
72 hours, 100,000 operations, and a verified restart; the shorter smoke profile
does not satisfy those release gates.

Restart checks now read the prior sequence/parity, SQL-updated document, restored
document, and metadata before any new mutation can mask stale state. A black-box
regression demonstrates that the original runner incorrectly passed a simulated
stale post-restart read and the new runner rejects it. Other tests exercise real
child processes that hang, fail to start, exit abnormally, or ignore termination;
they also cover resource failure and termination during final cleanup. The fake
executable is a process-boundary fixture, not evidence of database correctness.

Timeout controls are `--operation-timeout-ms` (30,000 default),
`--shutdown-timeout-ms` (5,000), `--build-timeout-ms` (600,000), and
`--sample-timeout-ms` (5,000). Version-2 reports distinguish preparation,
monotonic workload duration, wall-clock duration, source binary, and tested hash.
Resource measurements remain sampled, not continuous peak guarantees. These
changes do not certify physical power-loss durability, uninterruptible storage
I/O, or Windows process cleanup on this macOS host.

The final bounded native rerun completed 1,000 iterations, 7,560 machine
operations, 9,600 assertions, and 40 verified restarts in 60.391 seconds of
workload time, after 0.558 seconds of preparation. Sampled peak native RSS was
12,779,520 bytes; final root growth was 4,519 bytes. All smoke gates passed.
The tested debug binary SHA256 remained
`e35c3f68c92c8f6c24912374ffa58c38024f387252bb4a260d2b80d052cafaaf`.
Raw evidence remains ignored at
`benchmarks/tin-poc/results-soak-hardened-final-20260926.json`.

The final targeted suite passed 21 tests with 133 assertions (15 lifecycle/process
tests and six existing CI-contract tests). Formatting, typechecking, and diff
checks passed. The sparse-checkpoint RSS breach and late-cleanup interruption
regressions were each observed failing before their fixes, then passing after.
One repeat using a one-second fixture operation budget failed before the resource
cases reached their first completed iteration; isolated and full reruns passed.
Non-hanging fixtures now allow five seconds, while deliberate handshake/request
timeout cases retain one second. Resource assertions and production timeout
defaults were not weakened. The final 21-test rerun passed with these allowances.
The native rerun overlapped lightweight verification and is correctness evidence,
not an isolated latency benchmark. The 72-hour campaign has not been started.

## Six-hour background attempt: interrupted, not qualified

The user-authorized six-hour run started at 2026-09-26 04:00:13 UTC and was due
to finish around 10:00:14 UTC. It used the smoke profile, the same verified debug
binary, an isolated database, and a one-shot background job. It was not a
72-hour release-profile run.

The last checkpoint actually verified in this chat was at 06:45:28 UTC:
9,914,871 ms elapsed (about 2 hours 45 minutes), 1,762,152 machine operations,
2,238,390 model assertions, and 238 verified restarts. All reported smoke gates
were true, the binary hash matched, logs were empty, and the process was running.
Later heartbeat requests do not by themselves establish observed progress.

At the next executed inspection, the host boot time was 08:32:14 UTC (04:32:14
Toronto time), before the required finish. The one-shot job and original `/tmp`
manifest, report, and logs were absent; no retained final evidence was found.
Therefore neither a six-hour pass nor successful final exit can be claimed.
This does not establish an engine defect or the reason for the reboot.

An audit record at `benchmarks/six-hour-soak-20260926/interruption.json` preserves
the observed checkpoint from the chat's tool output and the current boot/job
checks. It is explicitly not a recovered original or final report. Monitoring
was paused, and no replacement run was launched. Any future authorized attempt
should retain checkpoints outside temporary storage so evidence survives reboot.

## Remaining release gates

1. Run Linux and Windows native CI and all SDK/browser jobs on an exact committed
   candidate. Existing remote results at `e2361625` do not validate this dirty
   worktree. The scheduled fuzz failure was reproduced and corrected locally;
   the remote run remains failed until a new candidate is verified.
   A local probe found Docker installed but its daemon unavailable, with no active
   local Linux/Windows environment established; no VM/container service was started.
2. Keep the full browser matrix green on the committed candidate using the
   isolated WebKit profile fixture. The local OPFS blocker is resolved; private
   browsing/ephemeral contexts remain outside the qualified surface.
3. Measure cold storage on an isolated test volume, representative datasets beyond
   the 10,000-document local fixture (including above-budget real-document indexes),
   sustained memory pressure, and sustained multi-process load. Bounded contention
   and above-admission snapshot checks passed locally, but do not establish these
   longer-running properties. Global OS caches were deliberately not purged on
   the shared machine.
4. Run the 72-hour release soak, sustained fault campaigns, and signed
   release-artifact tests.
   Process-kill failpoints do not establish physical power-loss durability.
5. Retain representative end-to-end baselines before setting performance release
   thresholds or promoting LIKE/range/BM25/SIMD experiments.
6. Decide whether to unify the existing JavaScript/Rust coercion semantics, with
   explicit compatibility tests and migration policy; this task did not silently
   change those public predicates.

See [index reuse](QUERY_INDEX_REUSE.md) for the cache safety contract and
[the PoC report](TIN_INSPIRED_SEARCH_POC.md) for experimental results and gates.
