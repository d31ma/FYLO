# ADR 0015: Append-only document segments

- Status: **Accepted** (approved by Samuel, 2026-10-28)
- Date: **2026-10-28**
- Owners: **FYLO maintainers**
- Amends: the "one canonical JSON file per document" layout in `README.md`
  and `CLAUDE.md`, for document collections that opt in. Builds on ADR 0013
  (the record format and commit protocol) and keeps ADR 0014's hot-set check
  working.

## Context

After path verification and the write-ahead log, a document read on local
APFS costs 6 file calls (open, fstat, flistxattr, the sidecar lstat, read,
close) and a put costs one log append and one flush plus the apply step, which
still creates a temporary file, renames it over the document and sets its
extended attributes. Local numbers are good: 5k broad equality in 59 ms, a
durable put in 0.70 ms.

On the raw ATLAS SMB share the per-row calls are the whole cost. Each row is
one open (about 2.2 ms) and one `flistxattr` (about 3 ms), both round trips to
CITADEL, so 250 rows take 1.8 s, and path verification could not move it
(`docs/QUERY_RELEASE_QUALIFICATION.md`, "ATLAS"). The same per-file shape
costs 6 calls per row on every filesystem, a directory entry and an inode per
document, and a temporary file and a rename per write.

The fix is to stop storing one file per document. A collection that keeps its
documents in a few large append-only files can answer a read with one `pread`
on an already open handle, a broad query with a handful of large sequential
reads, and a write with one append to a file that already exists.

## Decision

A document collection can use the **segment layout**: its documents live in
append-only segment files made of ADR 0013 log records. The segment the
writer appends to is also the collection's write-ahead log, so a commit is the
append itself and there is no apply step for document bodies. Every document
record holds the document's canonical JSON byte for byte, with its complete
FYLO attribute set inline, so documents remain the source of truth: every
derived structure rebuilds from the records alone, and the per-document file
tree can be exported exactly.

### 1. Scope

The first implementation covers **document collections on the `Native`
profile, on Unix and Windows**. It is opt-in per collection: new collections
still default to the file layout until the segment layout has passed a
release. A root-level default (`fylo.layout` in `application.json`) is added
but defaults to `"files"`.

These stay on the file layout and refuse the segment layout with
`ELAYOUTUNSUPPORTED`:

- raw-file collections (buckets), whose payloads are large blobs that gain
  nothing from packing;
- the `EfsSidecarV1` profile;
- the WASI and browser builds (the format is in `fylo-format`, so they can
  follow later without a format change);
- roots with document version control initialized. VCS commit, checkout,
  restore and merge read and write the per-document tree. Moving them to the
  logical record view (same blob hashes, so history is unchanged) is a
  follow-up ADR. Until then `fylo vcs init` refuses a root with a segment
  collection, and migration refuses a root with VCS.

### 2. Files

```text
<root>/.collections/<collection>/
  segments/
    0000000001.seg        sealed, immutable
    0000000002.seg        sealed, immutable
    0000000003.seg        active: the only file a commit appends to
    state.slots           generation slots (section 6), 128 bytes
    locations.snapshot    sorted id -> record location table (derived)
    locations.wal         location changes since the snapshot (derived)
  index/                  prefix index, unchanged (derived)
  events/  locks/         unchanged
<root>/.fylo-transactions/<namespace>/<collection>/
  state.json              fixed segment marker (section 9)
```

There is no `docs/` or `.deleted/` tree. Segment numbers only grow. The active
segment is sealed and a new one started when it passes 64 MiB, so a write
creates a file only once per 64 MiB written.

### 3. Segment and record format

Defined in `fylo-format` beside the log types, so the browser kernel, the
salvage tool and offline readers share one parser.

**Segment header** (32 bytes): ADR 0013's header with magic `FYLOSEG\0`,
format version `1`, flags `0`, the segment number, and the LSN of its first
record, with the same CRC-32C. A segment written by compaction sets flag bit 0
("compacted") and its first-LSN field is the lowest LSN it holds.

**Record frame**: ADR 0013's frame with a 4-byte sync marker `F5 59 52 01` in
front: `marker, body length, CRC-32C of body, body`. The marker costs 4 bytes
per record and lets the salvage scan find the next record after a damaged one,
so one bad sector loses the documents in that record, not the rest of the
segment. A candidate frame is accepted only if its checksum, length bound and
LSN all hold.

**Record body**: unchanged from ADR 0013 (LSN, kind, generation, manifest,
blobs). Kinds `1` commit, `2` abort, `3` checkpoint, plus `4` compaction
(section 7). A commit manifest is `"format": "fylo.segment-commit.v1"` and
lists one entry per document the mutation touched, in the order applied:

| Field            | Meaning                                                                                                                         |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `id`             | canonical TTID                                                                                                                  |
| `state`          | `live`, `deleted` (soft-deleted, restorable) or `absent`                                                                        |
| `body`           | blob offset, length and SHA-256 of the canonical JSON; omitted for `absent`                                                     |
| `attributes`     | the complete FYLO attribute set (`user.fylo.*` names to base64 values), exactly what `flistxattr` returns today, sorted by name |
| `modifiedMillis` | the document's modification time, which `updatedAt` reads today from the file's mtime                                           |
| `mode`           | the file mode the file layout would give the document                                                                           |

Every entry is a full post-image, so replaying a record twice gives the same
result, and a SQL mutation that touches many documents is still one atomic
record. A soft delete is an entry with `state: deleted` and the same body. It
copies the body (documents are small), which keeps every record readable on
its own and keeps compaction simple. `absent` is not produced by any
operation today. It exists so a future hard delete or VCS checkout has a
tombstone to write.

Inline attributes replace the per-row `flistxattr` and the sidecar `lstat`.
The access descriptor (`user.fylo.access`), record identity, checksum and
custom metadata (`user.fylo.meta.*`) are all in the set.

### 4. Locations

`locations.snapshot` is a sorted table of fixed-width entries: id (16 bytes,
zero padded), segment number, offset, record length, entry index within the
record, state and LSN. It is memory-mapped and searched like
`index/keys.snapshot`. `locations.wal` holds the entries written since the
snapshot. A reader takes the newer of the two for an id, and for equal LSNs
the higher segment number.

Both files are derived. The rule that defines them is: for each id, the entry
with the highest LSN in any segment the collection currently holds. They are
rebuilt by scanning segments, so damage to either one costs a rescan, never a
document.

`document_ids()` and `deleted_document_ids()` become one read of the location
table instead of a listing of every shard directory.

### 5. Reads

A point read looks the id up in the location table, then makes **one
`pread`** of the record (or just the entry's blob and manifest) from a segment
handle the collection keeps open for the duration of the stable read. It
checks the frame CRC and the body SHA-256 before returning bytes.

A query that hydrates many rows sorts their locations by segment and offset,
merges neighbours into reads of at most 1 MiB, and issues those. A broad query
over most of a collection becomes a sequential scan of each segment. On the
raw SMB share that turns 250 rows from about 500 round trips into a few.

Per query, the fixed cost is reading `state.slots`, opening the location files
and opening each segment touched once.

### 6. Commit protocol and generation slots

The commit is ADR 0013's protocol with the segment in place of the log:

1. Under the collection lock (or the group-commit busy period), validate the
   mutation and build its record exactly as today.
2. Publish `writing`.
3. Append the record to the active segment with one `write` and flush it with
   `sync_handle` (`F_FULLFSYNC` under `durability: "power-loss"`). **This is
   the commit point.**
4. Append the location entries to `locations.wal` and the index delta to
   `index/keys.wal`, without flushes.
5. Publish `stable` and release the lock.

Group commit, failure handling (cut off a failed append, rewind a failed
flush, abort record for a failed apply) and the durability promise are those
of ADR 0013 section 5, 6 and 9 and its implementation notes, unchanged.

**Generation slots.** Publishing `writing` and `stable` through `state.json`
costs a temporary file and a rename each. A segment collection instead keeps
its live generation in `segments/state.slots`: two 64-byte slots, each holding
a magic, the generation, the state (`stable` or `writing`), the active segment
number, its committed length, the next LSN, the record count and a CRC-32C. A
publication overwrites the slot that does not hold the newest valid
generation, in place, with one `pwrite` and no flush. A reader reads both slots
with one `pread` and takes the valid one with the highest generation. A torn
slot fails its CRC, so the reader sees the previous generation, and the
committed-length check then sends it to recovery, exactly as the v2 length
check does today.

So a steady-state put is one append, one flush, two small appends and two
in-place slot writes: **no file is created and nothing is renamed**.

The slot's (active segment, committed length, next LSN) triple is the "log
end" that ADR 0014's hot set compares, so its check keeps working.

Readers in other processes see a committed record as soon as `stable` is
published, because bytes before the committed length never change again.
Segment handles are opened with sharing that allows a concurrent appender on
Windows (`FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`).

### 7. Compaction and space reclaim

A record is dead when every entry in it has been superseded by a higher LSN.
A sealed segment's dead bytes are counted from the location table. A segment
is compacted when at least half of it is dead, and segments smaller than
16 MiB are merged with their neighbours. Triggers: at checkpoint time, and
`fylo compact <collection>`.

1. **Copy, without the lock.** Sealed segments never change, so the compactor
   reads the chosen segments and writes the live entries into a new segment,
   numbered above every existing one, sorted by id so that id-range scans read
   sequentially. Each live entry keeps its original LSN, attributes, time and
   body. The new file is flushed, then its directory.
2. **Publish, under the lock.** Take the collection lock, append a compaction
   record to the active segment naming the new segment, its SHA-256 and the
   segments it replaces, and flush it. This is the compaction's commit point.
   Then update `locations.wal` for every entry that still points into a
   replaced segment and whose LSN still wins, publish a new `stable`
   generation and release the lock. Entries that a writer superseded during
   step 1 are simply dead in the new segment.
3. **Remove.** Delete the replaced segments and flush the directory. On
   Windows a segment another process still has open cannot always be deleted.
   It is then left and retried at the next checkpoint. Readers ignore any
   segment that a compaction record has replaced.

Recovery after a crash:

- before step 2's flush: the new segment is named by no compaction record, so
  it is an orphan and is deleted;
- after it: the compaction record is replayed, and any replaced segment still
  present is deleted.

Because a copied entry keeps its LSN, a segment that exists twice (old and
compacted) holds identical entries, and the location rule picks the same
document either way. A reader that resolved a location into a segment that
compaction then removed gets "not found", treats it as a moved generation and
retries, which is today's protocol.

`absent` tombstones are dropped only by a compaction whose inputs include
every sealed segment older than the tombstone.

### 8. Checkpoints and recovery

A checkpoint flushes `locations.wal`, `index/keys.wal` and the slots, folds
the location and index logs into their snapshots when they are large (today's
compaction rule for keys), and appends a checkpoint record to the active
segment that records the location and index log lengths and the snapshot
hashes. It no longer flushes document files, because there are none.

Recovery is ADR 0013 section 7 with the segment as the log: find the last
checkpoint record, cut the active segment at its first invalid record (an
unacknowledged torn append), truncate `locations.wal` and `keys.wal` to the
checkpointed lengths, replay commit and compaction records after it that no
abort names, delete orphan and replaced segments, and publish `stable`. If a
snapshot hash does not match, the location table or the prefix index is
rebuilt from segments.

A full rebuild (`fylo rebuild`, or recovery that cannot trust any derived
file) scans every segment in number order, keeps the highest-LSN entry per
id, and writes both the location table and the prefix index from those
documents. This is today's "index rebuilds iterate documents", with records in
place of files.

**Damaged records.** `fylo verify` checks every frame CRC and body SHA-256.
`fylo salvage <collection> --out DIR` exports every readable document to the
file layout, using the sync marker to step past damage, and lists the ids it
could not recover. That keeps the property that one damaged file costs one
document: here one damaged record costs the documents in that record.

### 9. Migration, export and compatibility

**Catalog.** The collection descriptor gains `"layout": "segments"`. Absent
means files. The descriptor is parsed with `deny_unknown_fields`, so an older
binary refuses the collection instead of reading an empty `docs/` tree. As a
second fence, `.fylo-transactions/.../state.json` is written once as
`fylo.collection-segments.v1` with `"layout": "segments"`, which older
binaries already refuse with `ENATIVE_CORRUPT_METADATA`.

**Migration** is `fylo layout <collection> --to segments|files` (and
`db.<collection>.layout(...)` in the shims). It is a structural operation on
the transaction journal, like `reshard`:

1. Take the collection lock and checkpoint the write-ahead log.
2. Build the target layout in `<collection>/.layout-staging/`. To segments:
   read every live and deleted document in TTID order with its attributes,
   mode and time, and write them as compacted segments plus location and
   index files. To files: write each record's body to its shard path with its
   attributes, mode and modification time, and the prefix index.
3. Verify the staging copy against the source with the full oracle: the same
   identifier sets (live and deleted), and for every document the same body
   bytes, attribute set, mode and modification time.
4. Commit: a durable journal marker, then the catalog descriptor change, then
   rename the staging tree into place.
5. Remove the old layout's files and the journal transaction.

A crash before the marker rolls back by deleting the staging tree. After it,
recovery rolls forward. A round trip files → segments → files reproduces the
original tree's bodies, attributes, modes and times exactly. Owners are
reproduced when the process may `chown`.

**Export.** `fylo export <collection> --layout files --out DIR` writes the
per-document tree, byte-identical to what migration would produce, without
changing the collection. It works for backups, grep and diff, and it is the
supported way to get "one JSON file per document" out of a segment
collection. `getDoc` and every API still return the canonical JSON of one
document.

**Reshard** on a segment collection only records the new width for export and
migration. No data moves, because segments are not sharded.

### 10. Access control (POSTIX)

The access descriptor travels inline and the engine evaluates it on every read,
write and query row, exactly as it does today, including for virtual
identities over the machine protocol.

What a segment collection cannot do is give each record its own operating
system owner and mode, because records share files. Segment files, slots and
location files are created `0600` and owned by the root owner. Two
consequences:

- `.as({ uid, gid, mode })` on a segment collection stores the descriptor and
  is enforced by FYLO, but no `chown` or `chmod` happens, so a put no longer
  fails when the process lacks `chown` permission;
- a local OS user who is not the root owner can read no record at all
  directly, where today they could read the records whose mode allowed it.

A collection that relies on per-record OS ownership for processes outside
FYLO should stay on the file layout. `fylo layout --to segments` warns and
requires `--allow-engine-only-access` when the collection holds any record
whose descriptor names a uid or gid other than the root owner's.

## Implementation notes (2026-10-29)

The first implementation corrects these details. None changes the decision:
documents in append-only segments made of log records, the active segment as
the write-ahead log, inline attributes, derived location and index files that
rebuild from segments, and an exact export back to one file per document.

- **Code.** The format is in `fylo-format` (`LogSegmentKind`, the marked
  frame, `LogSalvage`, record kind `4` compaction). The read side is
  `crates/fylo-storage-native/src/segments/`. The write side lives beside the
  write-ahead log in `src/write/commit_log/segment_*.rs`, and the log writer
  is generic over the plan (`LoggedPlan`), so file and segment collections
  share the append, flush, abort and busy-period code.
- **Location rows** are 72 bytes, not 48: TTIDs can be 36 characters, so the
  identifier field is 40 bytes. The location log is applied in file order and
  the last row wins. An undo row (state `absent`, sequence number 0) hides an
  identifier a rolled-back put had added. A checkpoint folds the location log
  into the snapshot once it passes 64 KiB, whatever the snapshot's size,
  which keeps a writer's in-memory overlay small.
- **Generation slots** are 64 bytes: magic, version, state, generation,
  active segment, committed length, next sequence number, records since the
  last checkpoint, location log length, a table epoch and a CRC-32C. Slots
  order by generation, then segment, then length, because a checkpoint
  publishes again at the same generation. The table epoch changes whenever
  the location files are rewritten rather than appended to (a fold, a
  recovery, a rebuild), so a process that caches a table extends it only
  while the epoch holds.
- **Reads** check each frame's CRC-32C. The body's SHA-256 is checked by
  `fylo verify-segments`, not on every read, so a broad query does not hash
  every document twice. A query reads candidates in batches of 256 from a
  segment collection, and one at a time from a file collection as before.
- **Older binaries.** The catalog descriptor is not parsed with
  `deny_unknown_fields`, so an older release would ignore `"layout"`. The
  fences are the fixed `fylo.collection-segments.v1` state file (ADR 0017
  took the `v3` generation format for its schema fence), which
  older releases refuse, and a plain file named `docs`, where they expect a
  directory.
- **Compaction** runs only on request (`fylo compact`, `compactCollection`).
  There is no automatic trigger yet; running it inside a writer's checkpoint
  would stall that writer. A request seals the active segment first if it
  holds documents. The copy is written as `compacting-<pid>.tmp` under a
  separate `locks/compaction.lock` and renamed to its segment number under
  the collection lock. Compacted records hold one document each, in
  identifier order. Recovery and compaction remove every segment no location
  refers to, which covers a compaction's inputs and an unadopted output.
- **Recovery** refuses to cut the active segment when readable records follow
  the first damaged one. That is damage inside committed data, not a torn
  tail, and it asks for `fylo salvage`.
- **Migration** does not use the transaction journal, because the journal
  writes `state.json`, which a segment collection keeps as its fixed marker.
  It builds the copy in `.layout-staging`, verifies it with the full oracle,
  writes `layout-migration.json` beside the state file, and swaps through
  `.layout-old`. Every step checks what is already done. A writer or recovery
  that finds the marker finishes the swap and returns `ConcurrentWrite`, so
  its caller retries against the new layout. Readers see `writing` for the
  whole swap.
- **Errors and names.** A request the segment layout does not support
  returns `ENATIVE_UNSUPPORTED` rather than a new `ELAYOUTUNSUPPORTED` code.
  `fylo verify` already re-hashes raw files, so the segment check is `fylo
verify-segments`.
- **One location per document.** A put whose identifier is soft-deleted is
  refused with "document identifier already exists". The file layout allowed
  a live and a deleted copy of one identifier at once.
- **Version control.** Commit, checkout, restore and merge refuse a root
  that holds a segment collection, and migration to segments refuses a root
  with `.fylo-vcs`. There is no separate `fylo vcs init` to guard: the first
  commit initializes the repository, so the commit check covers it.
- **Shims.** The Node client has `createCollection(name, kind, { layout })`,
  `migrateLayout`, `compactCollection` and the facade's `layout()` and
  `compact()`. The other language clients send the same machine requests
  without helpers for now.
- **Failpoints** added: `after-compaction-copy`, `after-compaction-sync`,
  `after-compaction-record`, `after-compaction-remove`,
  `after-layout-staging`, `after-layout-marker` and `after-layout-swap`.
  Segment commits reuse the log's `before-log-append`, `after-log-append`,
  `before-log-sync`, `after-log-sync`, `after-log-apply-step` and
  `after-checkpoint-flush`.

## Consequences

Benefits:

- a point read is one `pread` and a broad query a few large reads, on every
  filesystem; on the raw SMB share that removes about 500 round trips per
  250 rows;
- a put creates no file and renames nothing, and drops the apply step for
  the body, so it is cheaper than ADR 0013's 0.70 ms;
- no directory entry or inode per document, and no shard directory listings;
- compacted segments are in id order, so id-range scans are sequential;
- documents stay the source of truth, records are self-describing and
  checksummed, and the file layout is always one export away.

Costs:

- two layouts to maintain until buckets, EFS, VCS and the browser move or the
  file layout is retired;
- dead versions take space until compaction, which rewrites live data;
- per-record OS ownership is gone for segment collections (section 10);
- a damaged record loses every document in it, where today a damaged file
  loses one (a SQL mutation can put many documents in one record);
- you can no longer inspect or edit a document with a text editor; editing
  the file tree by hand was already unsupported without a rebuild.

## Alternatives considered

- **Keep per-document files and add a packed read cache.** A cache must be
  invalidated on every write and still pays the file write. It moves the
  problem instead of removing it.
- **Separate log and data segments** (keep ADR 0013's log, and apply into
  data segments). Every byte would be written twice and the apply step would
  stay. Using the data segment as the log is the standard log-structured
  design and is simpler.
- **An embedded key-value engine (an LSM tree such as RocksDB or sled).** A
  large native dependency against ADR 0004, a second format FYLO does not own,
  and no way to keep the browser kernel on the same format.
- **One file per shard instead of segments.** Rewrites a whole shard file per
  write, or needs the same append-and-compact machinery per shard.
- **Keep `state.json` renames for segment collections.** Simpler, but each
  write would still create two temporary files.

## Verification

- `fylo-format` unit and fuzz targets for the segment parser: truncation at
  every byte, bit flips, a damaged record in the middle (salvage resumes at
  the next marker), oversized lengths, LSN gaps and unknown kinds.
- Engine conformance: the existing document, query, SQL, access, schema,
  encryption and machine-protocol suites run against both layouts.
- **Migration oracle on the 5k fixture:** files → segments → files, with the
  full identifier oracle (live and deleted sets) and body oracle (bytes,
  attributes, modes, times) at each step, and the final tree equal to the
  original.
- **Crash matrix:** new failpoints `after-segment-append`,
  `after-segment-sync`, `after-location-append`, `after-slot-publish`,
  `after-compaction-copy`, `after-compaction-sync`,
  `after-compaction-record`, `after-compaction-remove`,
  `after-layout-staging`, `after-layout-marker` and `after-layout-swap`,
  declared in `FAILPOINTS`. Every mutation scenario plus compaction and
  migration runs at each under `abort`, `enospc` and `edquot`, with the
  existing assertions and the before-or-after oracle.
- Rebuild equivalence after every checkpoint and compaction in the suites:
  the location table and prefix index equal a rebuild from segments.
- The concurrency suite and the multi-process load harness, with a compactor
  running beside writers and readers.
- **Benchmarks:** selective, broad and range queries and durable
  put/patch/delete at 5k and 25k on the Mac, in the APFS image on ATLAS and on
  the raw ATLAS share, before and after, with the 2026-10-11 harness and the
  load average recorded around each run.

## Operations

- `fylo layout <collection> --to segments|files`, `fylo export`,
  `fylo compact`, `fylo verify` and `fylo salvage`, as above.
- `fylo inspect` reports the layout, segment count and sizes, dead-byte ratio
  and the last checkpoint.
- Before a downgrade, migrate segment collections back to files with
  `fylo layout --to files`; older binaries refuse them otherwise.
