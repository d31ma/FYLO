# FYLO's per-document JSON files against Apache Parquet

Date: 2026-11-07. FYLO at `develop` `f27df4c`. Branch `bench/parquet`, not merged.

## The answer

**Keep FYLO's per-document JSON files as the source of truth. Do not replace them with
Parquet. Parquet is worth adding only as a derived, rebuildable columnar snapshot for
scans, analytics and read-only sharing, the same way FYLO treats its indexes.**

Parquet is far more efficient for what it was designed for: bulk loads (190x to 2,200x
faster), scans, filters and projections (250x to 1,200x faster), and size (17x to 180x
smaller on disk). On an SMB share it is the only one of the two that works at all.

But FYLO's workload is a document store: one durable write at a time, point reads by
id, and documents that must survive on their own. There Parquet gives back most of
its lead or loses:

- every durable single-document write in Parquet creates a new file, and the store
  then needs a compaction that rewrites the whole collection (0.45 s per million small
  documents locally, about 10 s on ATLAS) to stay fast;
- point reads, patches and deletes of 3 KB documents are 2.5x to 10x slower in
  Parquet, because one row means decoding a page of every column;
- a torn tail on a Parquet file loses every document in it; the same damage to FYLO
  costs one document;
- a Parquet reader and writer for the browser is larger (839 KB gzipped) than FYLO's
  whole browser engine budget (800 KB gzipped).

The biggest gaps in the query numbers are not FYLO's file format but four defects
in FYLO's engine that this benchmark found (section 8). Fixing those closes most of
the query gap without changing the format.

## 1. What was compared

**FYLO, per-document JSON files** (the default `files` layout; `.vdbx` and the
segment layout were not part of this comparison), confirmed from the code:

- one canonical JSON file per document at
  `<root>/.collections/<collection>/docs/<shard>/<TTID>.json`, sharded by the
  trailing character of the TTID's creation segment (ADR 0006, default width 1);
  metadata (identity, checksum, access) in extended attributes;
- a zero-payload prefix index per collection, `index/keys.snapshot` (sorted,
  mmap'd) plus `index/keys.wal`, with keys for every field: exact (`eq`), numeric
  (`n`, `nr`), forward and reversed prefixes, and trigrams; it rebuilds from the
  documents;
- every mutation commits through the collection's write-ahead log
  (`.fylo-transactions/.../log/segment.log`, ADR 0013): one checksummed append and
  one `fsync`, then the document, metadata and index are applied, and a checkpoint
  every 4,096 records makes them durable and empties the log.

FYLO was driven in-process through its own engine (`NativeWriteRoot`,
`WriteEngine`, `ReadOnlyEngine`), exactly as the `fylo` crate calls it: the root's
lease held for the session, one engine open per call, durability `process`
(`fsync`). Opening a root runs `recover_collection`, as the machine protocol does
(and `fylo::Root::open` does since `d7187b0`).

**Parquet** (Rust `parquet`/`arrow` 60.1.0, in a separate cargo workspace, so
nothing entered FYLO's `Cargo.lock`), modelled the way table formats such as Delta
Lake and Iceberg do it:

- one typed Arrow schema per data shape, with nested structs and lists, plus `_id`,
  `_seq` and `_deleted` columns;
- **every commit writes one new immutable file**: a temporary name, `fsync`,
  rename, and a directory `fsync`. A batch insert is one file; a single insert,
  patch or delete is a one-row file. A patch writes the full new version of the
  row; a delete writes a tombstone row;
- the newest version of an id wins. On open, the store reads the `_id` and
  `_deleted` columns of every file into an in-memory map from id to its newest row:
  this is the only index;
- compaction rewrites the live rows into one file sorted by id, then deletes the
  old files;
- snappy compression, 65,536-row row groups, 1,024-row pages, page-level statistics
  and the page index (so a point read decodes only the needed pages, and range
  queries skip row groups by min/max).

A whole-file rewrite per change was not modelled: it is the compaction cost below,
paid on every write.

## 2. Method

- Machine: Apple M6, 12 cores, 24 GiB, macOS, internal APFS SSD. ATLAS: the CITADEL
  SMB 3 share (`//iyor@CITADEL.local/ATLAS`), from the same Mac.
- One job at a time. This Mac has no `fylo-locks` root for `mac-heavy`, so jobs were
  run strictly in sequence instead.
- Data: generated with a fixed seed, so both engines store identical documents.
  - **small**: 9 flat fields, about 200 bytes of JSON;
  - **medium**: the same 9 fields plus a title, a category, three nested objects,
    two arrays of scalars and a 2 KB text body, about 3 KB of JSON. (FYLO refuses
    arrays of objects, so there are none.)
- Scales: 10,000 for both shapes; 50,000 small for both engines; Parquet also at
  1,000,000 small and 100,000 medium. FYLO cannot hold more than about 101,000
  small or 24,000 medium documents (section 8.1), so it has no 1M or 100k run.
- Per workload: 1,000 point reads and 1,000 `getLatest` (300 on ATLAS); 30
  equality and 30 range queries at 10k (5 at 50k and above, 10 on ATLAS); 5 filter
  and 5 scan queries at 10k (3 at larger scales); 200 single inserts, patches and
  deletes (100 on ATLAS). Bulk loads are batches of 1,000 documents (10,000 for
  Parquet's 1M and 100k loads on ATLAS).
- Every query's row count matched between the engines.
- p50 and p95 are per operation; "speed-up" is FYLO's p50 over Parquet's p50.
- Queries:
  - equality: `bucket = k`, 0.1% of documents;
  - range: `k <= score < k+1`, about 1%;
  - filter + projection: `age >= 90` (10%), returning `name`, `city`, `score`;
  - full scan + projection: every document, returning those three fields.
  FYLO has no projection in its query API, so it returns whole documents and the
  harness keeps three fields: that is FYLO's real cost.

Raw results are in `results/*.jsonl` (`tables.py` prints the tables); full logs are
in `~/Library/Logs/local/FYLO/parquet-bench/run-1` to `run-5` on this Mac.

## 3. Results on the local disk

### small documents, 10,000

| Workload | FYLO p50 / p95 | Parquet p50 / p95 | Faster |
| --- | ---: | ---: | --- |
| Batch insert, 1,000 docs | 1.60 s / 2.14 s | 721 us / 1.99 ms | Parquet 2,200x |
| Insert one doc, durable | 656 us / 974 us | 140 us / 186 us | Parquet 4.7x |
| Patch one doc, durable | 638 us / 919 us | 393 us / 439 us | Parquet 1.6x |
| Delete one doc, durable | 524 us / 765 us | 393 us / 436 us | Parquet 1.3x |
| Point read by id | 250 us / 539 us | 237 us / 259 us | even |
| `getLatest` | 217 us / 480 us | 236 us / 256 us | even |
| Equality query (0.1%) | 3.90 ms / 23.44 ms | 448 us / 519 us | Parquet 8.7x |
| Range query (1%) | 705 ms / 1.19 s | 592 us / 784 us | Parquet 1,190x |
| Filter 10% + 3 fields | 609 ms / 656 ms | 678 us / 868 us | Parquet 900x |
| Full scan + 3 fields | 663 ms / 737 ms | 2.44 ms / 2.72 ms | Parquet 270x |
| Open, with recovery | 143 ms | 0.46 ms | Parquet |
| Compaction after 600 single writes | none needed | 39.8 ms | FYLO |

### medium documents, 10,000

| Workload | FYLO p50 / p95 | Parquet p50 / p95 | Faster |
| --- | ---: | ---: | --- |
| Batch insert, 1,000 docs | 912 ms / 1.38 s | 4.76 ms / 5.63 ms | Parquet 190x |
| Insert one doc, durable | 780 us / 1.15 ms | 218 us / 273 us | Parquet 3.6x |
| Patch one doc, durable | 807 us / 1.14 ms | 2.06 ms / 2.29 ms | **FYLO 2.5x** |
| Delete one doc, durable | 635 us / 895 us | 2.02 ms / 2.17 ms | **FYLO 3.2x** |
| Point read by id | 178 us / 202 us | 1.81 ms / 1.91 ms | **FYLO 10x** |
| `getLatest` | 177 us / 202 us | 1.81 ms / 1.90 ms | **FYLO 10x** |
| Equality query (0.1%) | 585 ms / 596 ms | 5.12 ms / 6.01 ms | Parquet 114x |
| Range query (1%) | 604 ms / 641 ms | 10.68 ms / 10.84 ms | Parquet 57x |
| Filter 10% + 3 fields | 606 ms / 623 ms | 654 us / 973 us | Parquet 930x |
| Full scan + 3 fields | 624 ms / 653 ms | 2.46 ms / 2.69 ms | Parquet 250x |
| Open, with recovery | 59 ms | 0.47 ms | Parquet |
| Compaction after 600 single writes | none needed | 115 ms | FYLO |

With 64-row pages instead of 1,024, Parquet's medium point read drops only to
1.41 ms, and its equality query improves to 2.6 ms: a Parquet point read pays to
seek and decode a page in every one of the 25 leaf columns, whatever the page size.

### small documents, 50,000

| Workload | FYLO p50 / p95 | Parquet p50 / p95 | Faster |
| --- | ---: | ---: | --- |
| Batch insert, 1,000 docs | 894 ms / 1.50 s | 673 us / 1.07 ms | Parquet 1,300x |
| Insert one doc, durable | 769 us / 1.12 ms | 163 us / 280 us | Parquet 4.7x |
| Patch one doc, durable | 738 us / 950 us | 1.08 ms / 1.13 ms | **FYLO 1.5x** |
| Delete one doc, durable | 577 us / 763 us | 1.07 ms / 1.11 ms | **FYLO 1.9x** |
| Point read by id | 210 us / 293 us | 921 us / 992 us | **FYLO 4.4x** |
| Equality query (0.1%) | 603 ms / 611 ms | 1.93 ms / 2.06 ms | Parquet 310x |
| Range query (1%) | 3.18 s / 3.30 s | 2.64 ms / 2.73 ms | Parquet 1,200x |
| Filter 10% + 3 fields | 3.58 s / 3.76 s | 3.29 ms / 3.85 ms | Parquet 1,090x |
| Full scan + 3 fields | 3.14 s / 3.14 s | 12.98 ms / 13.48 ms | Parquet 240x |
| Open, with recovery | 226 ms | 2.2 ms | Parquet |
| Bulk load | 49.5 s (1,010 docs/s) | 0.07 s (716,000 docs/s) | Parquet |

### Parquet alone at 1,000,000 small and 100,000 medium

FYLO cannot store these collections (section 8.1).

| Workload | 1M small, p50 | 100k medium, p50 |
| --- | ---: | ---: |
| Bulk load | 1.4 s (700,000 docs/s) | 1.0 s (100,000 docs/s) |
| Insert one doc, durable | 156 us | 221 us |
| Patch / delete one doc | 1.26 ms / 1.25 ms | 3.59 ms / 3.57 ms |
| Point read by id | 1.12 ms | 3.37 ms |
| Equality query (0.1%) | 37.7 ms (973 rows) | 49.3 ms (105 rows) |
| Range query (1%) | 53.3 ms | 104 ms |
| Filter 10% + 3 fields | 95 ms | 6.7 ms |
| Full scan + 3 fields | 292 ms | 26.8 ms |
| Open (builds the id map) | 59 ms | 4.6 ms |
| Compaction after 600 single writes | 460 ms | 552 ms |

Parquet's point reads and single writes get slower as the collection grows
(1.1 ms at 1M), because each one decodes a page and a patch must read before it
writes; FYLO's stay near 0.2 ms at every size it can reach.

## 4. Results on ATLAS (SMB)

### FYLO's per-document files cannot run on the share

- **Writes fail.** A live root on ATLAS takes 40 durable puts, then fails with
  `ENATIVE_IO: Operation not supported (os error 45)`. Probes of the mount show
  that macOS `smbfs` refuses `F_FULLFSYNC`, hard links and `fcntl` byte-range
  locks. FYLO's checkpoint and index-compaction paths flush with `std`
  `sync_all`, which is `F_FULLFSYNC` on Apple platforms; the failing call was not
  traced, but it is most likely the first index compaction.
- **A copied root cannot be opened either.** Opening runs recovery, which flushes
  the same way and fails with the same `ENOTSUP`. On current `develop` (since
  `d7187b0`, when `fylo::Root::open` started recovering too) FYLO cannot open a root
  on the ATLAS share at all. It needs a supported local filesystem, as its README
  says.
- **Copying a 10k root onto the share** took 702 s for small documents and 1,116 s
  for medium ones (10,006 files, about 9 to 14 files a second).
- **Reading a copied root without recovery** (the `fylo` crate's open before
  `d7187b0`, kept here only to show the per-file cost of SMB reads):

| Workload, 10k, ATLAS | FYLO small | FYLO medium | Parquet small | Parquet medium |
| --- | ---: | ---: | ---: | ---: |
| Point read by id, p50 / p95 | 5.28 / 14.7 ms | 8.28 / 11.5 ms | 0.30 / 0.61 ms | 2.05 / 2.34 ms |
| Equality query (0.1%) | 43 ms | 691 ms | 0.43 ms | 5.89 ms |
| Range query (1%) | 73 s | 116 s | 0.61 ms | 11.5 ms |
| Filter 10% + 3 fields | 74 s | 111 s | 0.68 ms | 0.87 ms |
| Full scan + 3 fields | 71 s | 75 s | 2.41 ms | 3.16 ms |
| Open | 192 ms | 136 ms | 2.9 ms | 0.6 ms |

### Parquet on the share

| Workload, ATLAS, p50 / p95 | 10k small | 10k medium | 1M small | 100k medium |
| --- | ---: | ---: | ---: | ---: |
| Insert one doc, durable | 29.6 / 65.4 ms | 30.2 / 81.6 ms | 31.4 / 76.9 ms | 28.7 / 63.2 ms |
| Patch one doc, durable | 43.2 / 107 ms | 39.2 / 74.9 ms | 52.4 / 115 ms | 59.6 / 112 ms |
| Delete one doc, durable | 45.4 / 120 ms | 46.5 / 109 ms | 57.0 / 137 ms | 59.1 / 135 ms |
| Point read by id | 0.30 / 0.61 ms | 2.05 / 2.34 ms | 1.12 / 1.18 ms | 3.34 / 4.47 ms |
| Equality query (0.1%) | 0.43 ms | 5.89 ms | 38.0 ms | 47.2 ms |
| Full scan + 3 fields | 2.41 ms | 3.16 ms | 297 ms | 26.8 ms |
| Bulk load | 0.3 s | 0.9 s | 9.4 s | 5.8 s |
| Compaction after the single writes | 6.7 s | 7.4 s | 10.5 s | 9.9 s |

Reads on the share run at local speed once a file is open: the SMB client caches
it. Each durable write costs 30 to 60 ms (create, flush, rename and directory flush
are round trips to CITADEL), with p95 to 300 ms. The first 1M load, with one file
per 1,000 documents, stalled on SMB for up to 100 s per file and was stopped and
rerun with 10,000-document files.

## 5. Storage size

| Collection | FYLO files, allocated (logical) | Parquet snappy | Parquet zstd(3) | Parquet uncompressed |
| --- | ---: | ---: | ---: | ---: |
| 10k small | 64.2 MB (24.4 MB) | 0.36 MB | 0.19 MB | 0.84 MB |
| 10k medium | 141 MB (128 MB) | 8.4 MB | 6.0 MB | 24.7 MB |
| 50k small | 300 MB (102 MB) | 1.7 MB | 0.82 MB | 4.1 MB |
| 1M small | not storable | 33.4 MB | 15.6 MB | 82.9 MB |
| 100k medium | not storable | 81.7 MB | 57.9 MB | 244 MB |

- Of FYLO's 10k medium root, about 39 MB is documents and 88 MB is the prefix
  index (keys for every field, including trigrams of the 2 KB body).
- Small documents cost 6.0 to 6.4 KB each on APFS, mostly the 4 KB block per file
  and the index. On ATLAS the same 10k small root allocated 33 MB.
- FYLO's per-document layout has no compression option.

## 6. Crash safety, damage and rebuild

**Kill mid-write** (`SIGKILL` at a random moment while a child process writes in a
loop, then reopen and read every acknowledged id; 10 rounds each, small documents):

| Engine | Acknowledged writes | Lost | Open after the kill | Left behind |
| --- | ---: | ---: | ---: | --- |
| FYLO files | 15,107 | 0 | 164 to 354 ms (recovery) | nothing |
| Parquet, writes | 35,415 | 0 | 188 ms to 1.05 s | 5 temporary files in all, deleted on open |
| Parquet, kills during compaction | 25,839 | 0 | 2 to 4 ms | 7 temporary files in all, deleted on open |

Both are crash-safe for acknowledged writes. Parquet's open time grows with every
uncompacted file (1 s at about 36,000 one-row files), because it reads every
footer and id column.

**Damage** to the largest data file of a 10k medium collection, compacted:

| Damage | FYLO files | Parquet |
| --- | --- | --- |
| Torn tail (last half of the largest document file, last 4 KiB of the Parquet file) | 9,999 of 10,000 readable | **0 readable**: "Corrupt footer", the store will not open |
| 4 KiB of garbage in the middle (half of the largest document for FYLO) | 9,999 of 10,000 readable | 9,511 readable; the 489 rows in the damaged pages fail snappy decoding |

FYLO's unit of loss is one document; Parquet's is a page, a row group or, if the
footer is hit, the whole file. Parquet can mitigate this with smaller files (losing
its size and scan advantage) and with page checksums, but its data is not
self-describing without the footer.

**Rebuild of derived data:** deleting FYLO's `index/` directory and running
`rebuild_collection` took 0.66 s (10k small) and 2.07 s (10k medium), and the
queries returned the same rows. Parquet has no derived data on disk; its id map is
rebuilt on every open (above).

## 7. Small files and the browser

**Small-file overhead.** 10k documents are 10,006 files for FYLO and one file for
compacted Parquet. Allocation per small document is 6 KB against 34 bytes. FYLO's
scans touch every file (about 60 us each locally, about 7 ms each over SMB).
Parquet's problem is the reverse: one file per durable write until compaction.

**Browser and OPFS.** FYLO's browser engine is Rust compiled to WebAssembly
(`crates/fylo-wasm`), over OPFS synchronous access handles in a Worker (ADR 0008),
within a budget of 2,432 KiB raw and 800 KiB gzipped
(`scripts/verify-browser-wasm-budgets.mjs`).

- A minimal Parquet and Arrow write-and-read module for `wasm32-unknown-unknown`
  (`wasm-probe/`, snappy only, `opt-level = "s"`, LTO, then `wasm-opt -Oz`) is
  3.37 MB raw and 839 KB gzipped. That alone is over FYLO's whole engine budget,
  before any FYLO code; zstd would add a C dependency or a pure-Rust decoder.
- OPFS can hold Parquet files: a reader needs random access to the footer, which a
  sync access handle gives. But every durable single-document write becomes a new
  OPFS file plus later compaction in the browser, and the document-per-file model
  FYLO's browser storage, access descriptors and version control are built on
  (ADR 0003, ADR 0008) would have to be redesigned.

## 8. FYLO defects found along the way

These explain most of FYLO's worst numbers and are worth fixing whatever is decided
about Parquet:

1. **A collection stops accepting writes after about 101,000 small or 24,000
   medium documents.** `keys.wal` is compacted into the snapshot only once it
   outgrows the snapshot (`write.rs:2819`, `log_checkpoint.rs:131`), but readers
   refuse a log larger than `MAX_INDEX_WAL_BYTES` = 64 MiB (`lib.rs:123`, `2581`).
   Once the snapshot passes 64 MiB, the log grows past the limit before compaction
   runs: `ENATIVE_FILE_SIZE: keys.wal contains 91425817 bytes; limit is 67108864`
   (`results/ceiling.log`).
2. **Index cache budget.** The shared index cache keeps at most 64 MiB
   (`MAXIMUM_RETAINED_BYTES`, `index_cache/index_cache.rs:14`). A larger index,
   about 10k medium or 50k small documents here, is reloaded and parsed on every
   query, so an equality query jumps from about 4 ms to about 600 ms.
3. **Range, filter and scan queries read every document.** The native engine
   plans only equality through the index (`EqualityPlan`,
   `crates/fylo-engine/src/lib.rs:1581`); `$gte`, `$lt` and the rest hydrate every
   document. The README's "Query Strategy" table says numeric ranges use the `n`
   index, which the Rust engine does not do.
4. **No writes or opens on macOS SMB.** The 41st put, and recovery on open, fail
   with `ENOTSUP` on `smbfs`, most likely from `F_FULLFSYNC` (`std` `sync_all`)
   in the checkpoint or index-compaction path; the `.vdbx`
   lease already falls back to `fsync` (`0a65795`). FYLO documents that a live
   root needs a local filesystem, but a copied root cannot even be opened read-only
   for inspection.
5. **Fixed during this work:** `fylo::Root::open` did not run `recover_collection`
   after a crash, so a read hit `EENGINE_CONCURRENT_WRITE` until a write recovered
   the collection (fixed on `develop` in `d7187b0`).
6. Minor: a missing `index/` directory is reported as `ENATIVE_UNSAFE_PATH ...
   non-canonical spelling` rather than as a missing index.

## 9. Fairness

**What favours Parquet here:**

- queries ran against a compacted store, Parquet's best state; between
  compactions, every single-document write adds a file that each query and open
  must read;
- the in-memory id map is built at open and kept in memory (about 59 ms and tens
  of MB at 1M); FYLO keeps nothing in memory between calls except its index cache;
- the projection and filter workloads are columnar by nature; FYLO has no
  projection in its API;
- Parquet has no per-document metadata, access descriptors, schema validation,
  version control or event journal; FYLO writes all of these on every put;
- macOS `fsync` does not flush the drive cache, so durable writes are cheap for
  both; Parquet's write path is otherwise minimal (one file, one flush, a rename).

**What favours FYLO here:**

- a FYLO durable write is one log append and one `fsync`, applied later; Parquet
  writes and flushes a whole new file per write;
- the Parquet model is a benchmark harness, not a tuned engine (no bloom filters,
  no secondary indexes, no caching of decoded pages);
- FYLO's own defects (section 8) are counted against it as measured, not as they
  would be once fixed.

**Not measured:** `.vdbx` and FYLO's segment layout (out of scope by request),
power-loss durability (`F_FULLFSYNC`), concurrent readers and writers, and Linux
or Windows.

## 10. Recommendation for FYLO

1. **Keep the per-document JSON files as the source of truth.** They fit FYLO's
   principles: documents are truth, writes are synchronous and durable one at a
   time, damage is contained to one document, and the browser engine stays small.
   Parquet as primary storage would trade those for a compaction pipeline, file
   churn per write and whole-file failure modes.
2. **Fix the engine first** (section 8, items 1 to 3): the index-log ceiling, the
   index-cache budget and range planning. They cause FYLO's 600 ms to 3 s queries
   and its collection-size limit, and Parquet is not needed to fix them.
3. **If analytics, exports or SMB sharing matter, add Parquet as a derived
   snapshot,** not as storage: a per-collection `fylo export --parquet` (or a
   background snapshot beside the index) written from the documents, rebuildable
   at any time, and read-only. That is where Parquet's 250x to 1,200x scan and
   projection speed and 17x to 180x smaller size pay off, and on a share like
   ATLAS one Parquet file is readable at local speed where FYLO's files are not.
   Keep it native-only: in the browser it would cost more than FYLO's whole engine
   budget.

## Reproduce

```bash
cd bench/parquet
CARGO_TARGET_DIR=$HOME/Developer/Builds/parquet-bench CC=/usr/bin/clang cargo build --release
./run.sh ~/Library/Logs/local/FYLO/parquet-bench/run-N local10k crash damage atlas10k atlasfylo local-mid variant atlas-top
python3 tables.py ~/Library/Logs/local/FYLO/parquet-bench/run-N/results.jsonl
```

Data goes to `~/Developer/Builds/parquet-bench-data` and
`/Volumes/ATLAS/FYLO/parquet-bench` (`BENCH_LOCAL`, `BENCH_ATLAS`). The `wasm-probe/`
crate builds with `cargo build --release --target wasm32-unknown-unknown`.
