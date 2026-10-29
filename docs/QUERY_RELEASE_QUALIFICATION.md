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
  of a large finite JSON number after encoding and reparsing. The
  `serde_json/float_roundtrip` feature, enabled by `fylo-format` and so present in
  every build that parses documents, fixes this. Regression tests cover the exact
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

## Develop baselines, 2026-10-11

These are the retained end-to-end baselines for `develop`. The harness binaries
were built from a tree whose `crates/`, `Cargo.toml`, and `Cargo.lock` match
`1b64fbdf` byte for byte. The only later engine change before this record,
`2d350d66`, touches `fylo-machine`, which none of the harness crates depend on.
Host: Apple M4 (10 cores), 16 GiB, macOS 26.6.2, Rust 1.97.1, release
opt-level 3. Fixtures lived on an APFS sparse image stored on the external ANNEX
SSD.

**This was not a quiet machine.** A long-running Tachyon probe used about two
cores throughout, and other builds came and went. The one-minute load average
was 4.4 to 7.7 at the start and end of each run. The owner chose to measure
under that load rather than keep waiting. The 5,000-document run was repeated
three times to show the noise. Reads stayed within about 10% between runs, but
write medians varied up to threefold. Treat write numbers as upper bounds.

Each run used two collections, a 912-byte body per document, 3 warmups, and 30
timed samples. Full identifier and body oracles passed every run, as did the
post-mutation checks.

| Operation (ms) | 5k p50 | 5k p95 | 25k p50 | 25k p95 |
| --- | ---: | ---: | ---: | ---: |
| Selective equality (50 / 250 rows) | 29.336 | 32.711 | 390.619 | 418.980 |
| Same, parsed cache evicted | 52.884 | 57.852 | 507.743 | 517.451 |
| Broad equality (2,500 / 12,500 rows) | 1,413.771 | 1,870.106 | 25,602.660 | 33,544.294 |
| Range fallback scan (5,000 / 25,000 rows) | 2,966.787 | 3,531.461 | 44,017.625 | 46,134.963 |
| Engine open plus selective query | 38.099 | 57.137 | 390.103 | 405.730 |
| Alternating collections, two queries | 103.621 | 157.000 | 1,031.379 | 1,039.192 |
| Durable put | 10.407 | 13.861 | 11.163 | 14.283 |
| Durable patch | 20.487 | 48.708 | 18.728 | 21.435 |
| Durable delete | 13.230 | 27.211 | 15.282 | 19.334 |

The 5k columns are the median of three runs' p50 and p95. The ranges across
runs were: selective p50 28.979–31.722, broad p50 1,400.441–1,426.519, range
p50 2,892.763–3,245.993, and put p50 7.998–25.457. The 25k column is a single
run.

**Reads scale worse than linearly.** Five times the data made selective queries
13.3x slower, range scans 14.8x slower, and broad queries 18.1x slower. Write
latency stayed flat. Building the fixture took 223 s for 10,000 documents and
4,010 s for 50,000, an 18x increase. The 25,000-per-collection process peaked
at 1,497,284,608 bytes (1.39 GiB) of RSS, against 106–117 MB for each 5k run.
The snapshot was 20,293,586 bytes, below the 64 MiB admission budget. Any
release threshold should be set per dataset size, not extrapolated from these
two points.

**Cold reads, 25k root, 12 samples.** The image was detached and re-attached
before every sample, and each sample ran in a fresh process. OS-wide caches
were not purged, because that needs sudo. Each sample read 22.9 MB from the
image device, so the reads did reach storage.

| Cold sample (ms) | p50 | p95 |
| --- | ---: | ---: |
| Engine open | 1.202 | 8.579 |
| First selective query (250 rows) | 721.214 | 767.421 |
| Second selective query | 382.604 | 494.737 |
| Process wall time | 1,116.507 | 1,284.496 |

The first cold query cost about 1.85x a warm one. Peak RSS was 169.8 MB.

**Bounded multi-process load (30 s).** Two leased writers and two read-only
processes ran against one root.
- Writes: 1,178 committed writes and 58 rebuilds.
- Reads: 19,278 coherent successful reads.
- Expected rejections: 11,180 lease rejections, 7,784 exhausted concurrent-write
  retries, and 492 changed-path rejections.
- Settled checks: all 100 passed, with zero duplicate owners and zero lost
  writes.
- Above-budget snapshot (379,273 indexed documents): five oversized loads
  took 95.2–182.7 ms, and none was retained.
- Peak RSS was 420.2 MB.

This repeats the 2026-09-26 bounded check. It is still not a sustained-load
qualification.

Raw results, logs with load samples, runner scripts, and harness hashes are kept
in the ignored `benchmarks/baselines-20261011/`. Binary SHA256 values:
- `end-to-end`: `f9c437facebe8c5c1ec59b5f1b13c5cfb67496c32a46292acbbaa82562a84f8d`
- `bounded-load`: `0b90455ac66219bce4b72245a4c7848082fa575b3b290e4d747524620291805d`

This harness copy differs from `benchmarks/tin-poc` in three ways:
- document ids start at `4VRNF520000`
- it is capped at 79,000 documents
- it adds `FYLO_BENCH_KEEP_ROOT` and a `cold` mode

An earlier attempt on 2026-10-10 was discarded. Its quiet-machine check matched
only build tools, so it missed the probe that skewed it.

## Read scaling after one-call path checks, 2026-10-28

**Cause of the superlinear reads.** Before every document read,
`NativeRoot::verify_path` walked each component below the root. For each one it
listed the whole parent directory to confirm the exact spelling, ran `lstat`,
and finally ran `realpath`. A document read ran that check three times: before
the open, after the open, and after reading the bytes. Because each check listed
the shard directory, the cost per row grew with the number of documents in a
shard. A file-call interposer counted 63.7 calls per row for a 100-row query on
a 200-document root, including 15 directory listings.

**Change.** On Apple platforms a path is now opened with `O_NOFOLLOW_ANY`. That
single call refuses a symbolic link in any component, so the kernel checks the
whole path at the moment of the open. Only a complete success takes this path.
Any failure falls back to the old component walk, which reports the same errors
as before. Canonical spelling is checked against a process-wide cache of
directory listings. A cache miss rereads the listing, and a change in the
collection's generation drops every cached listing under that collection.
Document reads try each shard candidate with that one open, and they read a
file of known size in one `read`. The post-read identity check guarded only the
gap between check and open that the atomic open removes, so it is skipped for
native records. The EFS profile keeps it, because its sidecar is a second file.
Linux, Windows and WebAssembly keep the component walk, but they now get the
cached spelling check instead of listing directories.

**File calls per row.** Each read now makes 6 calls: open, fstat, flistxattr,
one `lstat` for the attribute-sidecar fallback (a record with no FYLO xattrs
may carry them in a `.fylo-attrs` file), read, and close. The sidecar `lstat`
stays because a sidecar can hold the access descriptor.

| Query (interposer) | Before calls | After calls | Before per row | After per row |
| --- | ---: | ---: | ---: | ---: |
| Broad, 100 rows, 200-doc root, local APFS | 6,368 | 663 | 63.7 | 6.6 |
| Broad, 2,500 rows, 5k root, APFS image on ATLAS | 185,197 | 15,054 | 74.1 | 6.0 |
| Broad, 250 rows, 500-doc root, raw ATLAS SMB share | 17,176 | 1,551 | 68.7 | 6.2 |

**Mac, local APFS.** Measured on an Apple M4 with 16 GiB, macOS 27.2, Rust
1.97.1, release opt-level 3, with fixtures on the internal disk. This used the
same 2026-10-11 harness, with `end-to-end.rs` SHA256 `e65d46c0…`. Before and
after binaries were built from `fe2f1b7` and this change, and their 5k runs were
interleaved. The one-minute load was 3.8 to 17 at the start and end of the runs,
because other builds came and went, so treat writes as upper bounds. Full
identifier and body oracles and the post-write checks passed in every run.

| Operation (ms, p50) | 5k before | 5k after | 25k 2026-10-11 | 25k after |
| --- | ---: | ---: | ---: | ---: |
| Selective equality (50 / 250 rows) | 33.255 | 1.419 | 390.619 | 10.729 |
| Same, parsed cache evicted | 56.652 | 24.859 | 507.743 | 123.843 |
| Broad equality (2,500 / 12,500 rows) | 1,643.998 | 59.413 | 25,602.660 | 307.580 |
| Range fallback scan (5,000 / 25,000 rows) | 3,278.193 | 124.254 | 44,017.625 | 801.453 |
| Engine open plus selective query | 33.743 | 1.451 | 390.103 | 11.457 |
| Alternating collections, two queries | 114.036 | 50.186 | 1,031.379 | 264.434 |
| Durable put | 6.880 | 7.498 | 11.163 | 16.255 |
| Durable patch | 8.934 | 7.710 | 18.728 | 15.457 |
| Durable delete | 8.089 | 8.058 | 15.282 | 15.889 |

The 5k columns are the median of three runs. Across the three runs, broad p50
was 1,637.0–1,706.7 before and 58.0–60.4 after. Range p50 was 3,248.9–3,316.6
before and 121.8–128.1 after. The 25k after column is a single run. The 25k
before column is the 2026-10-11 baseline on a different host load and disk,
because no same-day 25k run was taken before the change. The 25k fixture took
919 s to build (4,010 s on 2026-10-11), and the process peaked at 1.44 GiB RSS.

**Scaling.** Five times the data now makes broad queries 5.2x slower, range
scans 6.5x slower, and evicted selective queries 5.0x slower, all close to
linear. The 2026-10-11 ratios were 18.1x, 14.8x and 9.6x. Warm selective queries
grow 7.6x. They return five times the rows and also scan a five-times-larger
index snapshot. Writes were not the target of this change. A durable write still
lists a shard once after each generation change, and there is no same-day 25k
write baseline.

**ATLAS.** Read-only checks ran on copies of one fixture with the same before
and after binaries, under a one-minute load of 25 to 42 from other builds. These
are p50 values with 30 samples on the image and 10 on the raw share.

| Read check (ms, p50) | Before | After |
| --- | ---: | ---: |
| APFS image on ATLAS, 5k: selective, 50 rows | 102.759 | 1.516 |
| APFS image on ATLAS, 5k: broad, 2,500 rows | 5,256.582 | 124.257 |
| Raw SMB share, 500: selective, 5 rows | 24.767 | 17.941 |
| Raw SMB share, 500: broad, 250 rows | 1,855.649 | 1,803.560 |

On the raw share the path checks were already answered from the SMB client's
attribute cache, so removing them barely moved the wall time. What remains is
one open (about 2.2 ms) and one `flistxattr` (about 3 ms) per row, and both go
to the server. Removing those needs the engine next to its disk, or packed
segment files, not fewer path checks.

Raw results, traces, logs with load samples, and the run scripts are in
`/Volumes/FORGE/Logs/FYLO/fylo-path-verify/`.

## Write latency after the write-ahead log, 2026-10-28

Document writes now commit through the collection write-ahead log
([ADR 0013](adr/0013-collection-write-ahead-log.md)): one append and one
`fsync` per commit instead of 23 to 28. Same harness as the 2026-10-11
baselines (`end-to-end.rs` SHA-256 `e65d46c0…`), release build, 30 samples per
operation. Logs, load samples and raw JSON are in
`/Volumes/FORGE/Logs/FYLO/fylo-wal/`.

**Mac internal APFS, 5,000 documents per collection, median of three runs
(load 3.5 to 4.7):**

| Operation (ms) | develop fe2f1b7 p50 | log p50 | develop p95 | log p95 |
| -------------- | ------------------: | ------: | ----------: | ------: |
| Durable put    |                6.42 |    0.70 |        6.83 |    0.88 |
| Durable patch  |                8.44 |    0.61 |       11.59 |    0.75 |
| Durable delete |                7.75 |    0.60 |        8.86 |    1.06 |

The fixture build (10,000 puts) dropped from 82 s to 8.4 s. A traced put makes
72 file calls instead of 413, and one `fsync`. The log branch also includes
the one-call path checks, which account for part of the patch and delete
gain; on the log alone, before rebasing, put measured 0.93 ms, patch 1.54 ms
and delete 1.42 ms.

**APFS sparse bundle on ATLAS, 5,000 documents, one run:**

| Operation (ms) | develop p50 | log p50 | develop p95 | log p95 |
| -------------- | ----------: | ------: | ----------: | ------: |
| Durable put    |        7.62 |    0.75 |       37.65 |    0.90 |
| Durable patch  |       12.85 |    0.66 |       25.84 |    0.71 |
| Durable delete |       12.04 |    0.63 |       80.55 |    0.89 |

The fixture build took 30 s instead of 638 s. The image was attached with
`diskutil image attach`; images attached with `hdiutil attach` wedged with
error 45 within a minute, twice.

**Correctness under load.** The crash matrix interrupted 408 mutations across
24 failpoints (macOS) and 378 (Windows), all recovered. The repeated
concurrency suite passed 10 of 10 runs on macOS and 8 of 8 on Windows. The
bounded multi-process load (30 s) committed 1,297 writes, served 18,458
coherent reads, and passed all 100 settled checks with no lost writes or
duplicate owners.

## Hot set, 2026-10-28 (measured in the since-removed fylo serve)

`fylo serve` was removed on 2026-10-29: FYLO is embedded, and the hot set now
runs in sessions that hold the root's exclusive lease
(`exec --loop --exclusive-root`), where reads run one at a time. The numbers
below are kept as measured.

**Change.** `fylo serve` keeps parsed documents, index snapshots, id lists and
collection headers in memory under a byte budget (ADR 0014, default 64 MiB).
Reads share the executor; any other frame runs alone and clears the hot set.
Separately, `getDoc` and `findDocs` no longer list the whole collection to
learn its kind, with or without the hot set.

**Setup.** Apple M4, macOS 27.2, Rust 1.97.1, release opt-level 3. Before is
develop at `cf52abf` (with the write-ahead log); after is this change, with the
hot set on (64 MiB) and off (`--hot-set-bytes 0`). Fixtures follow the
2026-10-11 workload (two collections, `group = n % 100`, 912-byte body): 5,000
documents per collection in an APFS sparse image stored on ATLAS, and 500 per
collection copied to the raw ATLAS SMB share. The one-minute load was 2.2 to 6
during the in-process runs and 2.8 to 8 during the network runs. Every sample
checked its row count. Raw results, load samples and the bench sources are in
`/Volumes/FORGE/Logs/FYLO/fylo-hotset/`.

**In process.** Time for `FrameSession::answer`: request parsing, dispatch,
the read and the encoded response. 200 samples per run (30 on the raw share),
three warmups; p50, with the range across runs.

| Read (p50) | Before | After, hot set off | After, hot set on |
| --- | ---: | ---: | ---: |
| Image 5k: `getDoc` | 5.20 ms | 245 µs | 2.9 µs |
| Image 5k: selective, 50 rows | 7.63 ms | 2.71 ms | 77.8 µs (76.1–79.6) |
| Image 5k: broad, 2,500 rows | 114 ms | 116 ms | 4.08 ms |
| Image 5k: engine `find` only, 50 rows | 2.37 ms | 2.51 ms | 28.9 µs |
| Raw share 500: `getDoc` | 14.2–24.2 ms | 16.3–18.8 ms | 3.0 µs |
| Raw share 500: selective, 5 rows | 39.2–43.3 ms | 79.3–86.6 ms | 11.2–11.4 µs |
| Raw share 500: broad, 250 rows | 1.82–2.28 s | 1.40–1.86 s | 366–387 µs |

The warm 50-row read is under 0.1 ms in process (p95 81–85 µs); the engine
itself takes 29 µs and the rest is encoding 51 KB of JSON. Before the change a
`getDoc` listed every document and tombstone in the collection to learn its
kind, which is most of its 5.2 ms. On the raw share with the hot set off, the
selective read is about twice develop's; the likely reason is that develop's
listing warmed the SMB client's cache for the opens that followed. This was not
investigated further, because the hot set is on by default.

**From the Surface.** The Surface is on Wi-Fi. Each sample is paired with an
empty round trip (`handshake`) taken just before it, reported as RTT. 100
samples (30 for broad), two runs each, p50. These times include a network hop
to `fylo serve` that no longer exists; an embedded app does not pay it.

| Read (p50, ms) | Before | RTT | Hot set off | RTT | Hot set on | RTT |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Image 5k: `getDoc` | 12.45 / 12.25 | 5.2 / 5.1 | 5.24 / 5.21 | 4.9 / 4.9 | 4.20 / 4.38 | 4.1 / 4.4 |
| Image 5k: selective, 50 rows | 14.99 / 14.80 | 5.3 / 4.9 | 9.47 / 11.86 | 5.3 / 5.3 | 5.71 / 5.33 | 5.2 / 4.4 |
| Image 5k: broad, 2,500 rows | 185.5 / 185.1 | 4.5 / 4.5 | 149.5 / 147.1 | 4.5 / 4.1 | 67.8 / 72.0 | 3.8 / 4.5 |
| Raw 500: `getDoc` | 22.82 / 19.77 | 5.9 / 5.2 | 7.93 / 13.78 | 5.2 / 5.5 | 4.22 / 4.31 | 4.4 / 4.7 |
| Raw 500: selective, 5 rows | 36.27 / 45.24 | 5.7 / 5.1 | 20.76 / 43.29 | 5.6 / 5.7 | 4.08 / 4.46 | 4.1 / 4.5 |
| Raw 500: broad, 250 rows | 1,476.7 / 1,353.3 | 6.6 / 6.7 | 1,408.2 / 1,382.3 | 6.8 / 7.1 | 19.6 / 19.3 | 5.7 / 5.1 |

With the hot set a warm `getDoc` or small query costs one round trip; the
50-row read adds about 0.5 ms to move 51 KB over Wi-Fi, and the broad read is
mostly the 2.5 MB transfer. The first query of a new connection, which fills
the hot set, took 39–64 ms on the image and 212–298 ms on the raw share
(1,460–1,645 ms before). One first query, on the freshly created image, took
14.8 s and did not repeat.

**Concurrency and budget.** `crates/fylo-machine/tests/hot_set_server.rs`
ran, over TCP at the time, four rounds of three readers and two writers and
checked every row for coherence and for going backwards, and every acknowledged write with a
settled `getDoc` and `findDocs` on another connection: 768 settled checks, no
stale or incoherent row, also under a 4 KiB budget. On the real binary, six
readers and one writer ran for 30 s against the image root, sampling the hot
set every 50 ms from another connection:

| Load (30 s) | Reads, hot set off | Reads, hot set on | Peak resident | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| 1 write/s, 64 MiB | 50,518 | 584,631 and 679,433 | 14.7 MB | 88–91 MB either way |
| 50 writes/s, 64 MiB, interleaved | 43,363 / 30,619 / 37,295 | 27,158 / 52,194 / 30,945 | 5.9 MB | 76–216 MB |
| 50 writes/s, 4 MiB | | 46,701 | 1.46 MB | 215 MB |

No run had an error or an incoherent row, and resident bytes never exceeded
the budget. Under 50 writes a second every write empties the hot set, so it
gives no gain, and the spread between runs is wider than the difference. A
first version that opened the collection twice on refill read 22,546 rows
against 37,104 with the hot set off; refills now cost the same file calls as a
plain read. Per-document invalidation from the write-ahead log's commit
records is the step that would keep a write-heavy hot set warm.

**Windows.** On the Surface (native Windows) the workspace tests pass,
including the hot-set suites. The one failure there,
`write_ahead_log.rs::a_commit_leaves_a_v2_state_and_a_checkpoint_returns_to_v1`,
is a develop race in that file's tests under parallel threads and passes alone.

## Cloud Run and Azure Functions, local only, 2026-10-11

`bun run cloudrun:local` and `bun run azure:local`
(`scripts/verify-serverless-local.mjs`) run the EFS-profile crash suite, the
two-instance root lease drill, and the HTTP invocation adapter
(`crates/fylo-machine/examples/http_invocation.rs`) as two instances on each
platform's shared storage. Run on an arm64 WSL2 host with Docker 29.8:

| Mount | Crash suite | Lease across instances | Two service instances |
|---|---|---|---|
| Cloud Run, Filestore (NFSv3, no-lock) | pass (288 + 43) | fail: both admitted | fail: concurrent writers get `EROOTLEASELOST` |
| Cloud Run, Cloud Storage (gcsfuse) | fail: no hard links | fail: both admitted | fail: no hard links |
| Azure Files, SMB, Samba stand-in (`nobrl` or not) | fail: link refused | `nobrl`: fail, both admitted; byte-range locks: second refused with `EROOTLOCKED`, writes then fail on links | fail: link refused |
| Azure Files, NFS 4.1 | pass (288 + 43) | pass | pass |

- **No hard links.** Collection locks are taken by hard-linking a temporary
  file into place (`try_create_lock` in `write.rs`). The fallback for missing
  links only catches `Unsupported`. gcsfuse, the real client, returns `EPERM`,
  so every write fails on Cloud Storage. The Samba stand-in returns `EACCES`
  with the same result. Real Azure Files may answer with a different errno; if
  it is `EOPNOTSUPP`, FYLO would take the exclusive-create fallback instead.
  That is not tested here. gcsfuse also needs `file-mode=600,dir-mode=755`,
  or the profile descriptor is refused as unsafe.
- **No cross-instance locks.** Cloud Run mounts NFS in no-lock mode, and gcsfuse
  and `nobrl` keep locks local. Two instances both get the root lease. The
  owner metadata catches this afterwards as `EROOTLEASELOST`; no acknowledged
  write was lost. On SMB with byte-range locks, cifs reports a held lock as
  `EACCES`. The root lease used to surface that as `ENATIVE_IO`, so invocations
  failed instead of waiting their turn. It now reports `EROOTLOCKED`; the
  Samba drill shows the second instance refused with it and taking over once
  the holder is killed. Invocations waiting their turn end to end is not shown
  here, because the stand-in still refuses the hard links writes need.
- **Alternating writers on NFS, fixed.** About a third of writes failed with
  "private copy source timestamp changed" when the previous write came from
  another client. The before-image capture read the timestamp with a path
  stat, which NFS answered from a stale attribute cache, then compared it with
  the opened file. The EFS profile now takes the metadata from the opened
  file, which close-to-open consistency revalidates. That affected EFS too:
  its NFS lane now alternates writers between two clients. That step failed
  with this error before the fix and passes after it.
- **Lock file outside the mount, fixed.** The root lease lock sits beside the
  root, not inside it. The Floci lane used to set `FYLO_ROOT=/mnt/state`, the
  mount point itself, so the lock landed in the container's own `/mnt`.
  Invocations in separate containers then never contended on a shared lock,
  and on real Lambda `/mnt` is presumably read-only (inferred, not checked).
  The root is now `/mnt/state/root`. The Lambda rehearsal creates it on cold
  start, because the EFS profile needs an existing root and nothing else
  provisions one. The lane asserts that the lock file is on the access
  point's volume.
- **Emulator limits.** No official emulator exists for either platform.
  - Cloud Run is the container run with its contract: `PORT`, `K_*`, 512 MiB,
    and SIGTERM with a 10 s grace period.
  - Azure is the Functions host from Core Tools 4.15.2. It runs on arm64,
    unlike the MCR images, and uses Azurite for `AzureWebJobsStorage`.
  - The storage stand-ins:
    - Filestore: a kernel NFS server.
    - Cloud Storage: fake-gcs-server. It has no `objects.move`, so atomic
      rename is off.
    - Azure Files: Samba on FAT, so links are refused. The errno and the mount
      options Azure applies are unconfirmed.
  - Instances mount for themselves in privileged containers.
  - Cloud Run gen1 (gVisor) is not covered, because volume mounts need gen2.
  - Once, a second NFS 4.1 client failed to mount with `Cannot allocate
    memory`; the rerun passed.

## Remaining release gates

1. Passed on `develop` at `65d774b` (CI run `38083366120`): Linux, macOS, and
   Windows 2022/2025 native CI, Miri, coverage, SBOM, dependency policy, the
   compiled binary and published clients, and the EFS profile on local NFS 4.1
   with Lambda in Floci. Floci does not enforce IAM, VPC isolation, or access-point
   identities, so those properties still need a real AWS run.
2. Passed in the same run: Chromium and Firefox on ubuntu-24.04 and WebKit on
   macos-15 (Linux WebKit builds expose no `navigator.storage`). Private
   browsing/ephemeral contexts remain outside the qualified surface.
3. Measure above-budget real-document indexes, sustained memory pressure, and
   sustained multi-process load. Cold reads on an isolated image volume and a
   50,000-document fixture were measured on 2026-10-11 (see "Develop baselines"
   above). Bounded contention
   and above-admission snapshot checks passed locally, but do not establish these
   longer-running properties. Global OS caches were deliberately not purged on
   the shared machine.
4. Run the 72-hour release soak, sustained fault campaigns, and signed
   release-artifact tests.
   Process-kill failpoints do not establish physical power-loss durability.
5. Baselines retained 2026-10-11 for 5,000 and 25,000 documents per collection
   (see "Develop baselines" above), measured under background load. Setting
   performance release thresholds still needs a quiet-host rerun, and so does
   promoting the LIKE/range/BM25/SIMD experiments. The superlinear read
   scaling was explained and removed on 2026-10-28 (see "Read scaling after
   one-call path checks").
6. Decided 2026-10-10: the browser JavaScript engine follows the native Rust
   semantics. Rust predicates and joins now coerce strings with the index
   writer's JavaScript `Number` rules (hex/binary/octal accepted, signed radix
   and non-finite values rejected). Arrays and objects never loosely equal a
   scalar, and predicate paths do not index into arrays. The divergent inputs
   are pinned in `tests/fixtures/rust-predicate-v1.json`, which both engines run.
   No reindex is needed: stored index keys were already identical.

See [index reuse](QUERY_INDEX_REUSE.md) for the cache safety contract and
[the PoC report](TIN_INSPIRED_SEARCH_POC.md) for experimental results and gates.
