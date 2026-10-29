# ADR 0013: Collection write-ahead log with group commit

- Status: **Accepted** (approved by Samuel, 2026-10-28)
- Date: **2026-10-28**
- Owners: **FYLO maintainers**
- Amends: the collection transaction journal described in `README.md`
  ("`.fylo-transactions`") for ordinary record mutations. The journal stays for
  the operations listed under "Out of scope".

## Context

Every record mutation today runs an undo journal. A put creates a transaction
directory, writes a manifest, publishes a `writing` generation, copies a
before-image of every file it will touch, writes the record with
write-temp-fsync-rename-fsync-parent, appends the index log, writes a commit
marker, publishes a `stable` generation and removes the transaction directory.
Each of those steps is made durable on its own, and the collection lock adds
two more flushes.

The 2026-10-28 syscall trace (report "FYLO on ATLAS: tests, speed and the
latency gap") counted, per operation:

| Operation | File calls | fsync | rename |
| --------- | ---------: | ----: | -----: |
| Put       |        489 |    23 |      9 |
| Patch     |        659 |    25 |      9 |
| Delete    |        627 |    28 |     11 |

Flushing was 53 to 90% of write time in the APFS image on ATLAS, where one
flush costs 0.86 ms. On the Mac's internal SSD a flush is cheaper, and the
cost moves elsewhere. The 2026-10-28 rerun on develop fe2f1b7 (internal APFS,
load 4 to 9, logs in `/Volumes/FORGE/Logs/FYLO/fylo-wal/bench-before-mac`)
measured a durable put at 6.0 to 6.6 ms p50 on a 5,000-document root, patch
at 8.4 to 8.8 ms and delete at 7.7 to 8.0 ms. One traced put (7.1 ms) breaks
down as:

| Cost                                              | Time per put |
| ------------------------------------------------- | -----------: |
| 413 file calls, all kinds                         |      3.65 ms |
| of which 23 fsyncs                                |      0.36 ms |
| of which 76 directory opens (path verification)   |      0.83 ms |
| spawning `ps` to stamp the collection lock record | about 2.2 ms |
| everything else (parsing, hashing, allocation)    | about 1.2 ms |

The `ps` spawn is `process_identity` in `CollectionWriteLock::acquire_at`,
which runs on every lock acquisition. On Windows the same probe starts
PowerShell, which costs far more. So a sub-millisecond put needs three
things: the log in this ADR, which removes the transaction directory and
about 250 file calls with it; computing the process identity once per process
(section 4); and the path-verification work in the parallel
`fylo-path-verify` change, which removes most of the directory opens.

The flushes are ordering barriers for an undo protocol: each before-image must
be durable before the file it protects changes. A redo log inverts that. The
commit becomes one durable append that already holds the complete result, and
the files are brought up to date afterwards without their own barriers,
because a crash at any later point can be repaired by replaying the log.

`sync_handle` sets today's durability floor. On Apple platforms it is
`fsync`, not `F_FULLFSYNC`, so FYLO survives process death and kernel panics
there but not power loss. Elsewhere it is `File::sync_all` (`fsync` on Linux,
`FlushFileBuffers` on Windows), which does survive power loss on a disk that
honours flushes.

## Decision

Each collection gets a write-ahead log. An ordinary record mutation commits by
appending one checksummed record that holds the mutation's complete result and
flushing the log once. Documents, metadata and indexes are then applied from
that record with no flushes. A checkpoint later makes the applied files durable
and truncates the log. Documents stay the source of truth: a checkpointed root
has the same layout as today and every index rebuilds from documents alone.

### 1. Scope

Through the log (the `Native` filesystem profile):

- `put_document`, `put_raw_file`
- `patch_document`, `patch_document_fields`
- `delete_document` (soft delete) and `restore_document`
- `set_record_metadata`, `replace_record_metadata`, `set_record_access`
- `execute_sql_mutation` (all matched records in one record)

Out of scope, still on the undo journal: `create_collection`,
`drop_collection`, `reshard_collection`, `rebuild_collection`, version-control
commit, checkout, restore and merge, and the queue. Each of these first takes
the collection lock and checkpoints the log, so the two mechanisms never
overlap on one collection. The `EfsSidecarV1` profile also stays on the
journal: its close-to-open, sidecar-pair and owner checks need their own
review, and moving it is a later ADR.

### 2. Files

The log lives in the collection's existing transaction directory, which
document enumeration already ignores:

```text
<root>/.fylo-transactions/<namespace>/<collection>/
  state.json                 generation marker (unchanged meaning)
  log/
    0000000000000007.log     current segment, named by sequence number
```

There is exactly one live segment after a checkpoint completes. A second
exists only between "new segment durable" and "old segment removed".

### 3. Record format

Defined and versioned in `fylo-format` (`LogSegment`, `LogRecord`), so the
browser kernel and offline tooling share one parser. All integers are little
endian. The checksum is CRC-32C, implemented in `fylo-format` with a
known-answer test, so no dependency is added.

Segment header, 32 bytes:

| Offset | Size | Field                                         |
| -----: | ---: | --------------------------------------------- |
|      0 |    8 | magic `FYLOLOG\0`                             |
|      8 |    2 | format version, `1`                           |
|     10 |    2 | flags, `0` (non-zero is refused)              |
|     12 |    8 | segment sequence number                       |
|     20 |    8 | log sequence number (LSN) of the first record |
|     28 |    4 | CRC-32C of bytes 0 to 27                      |

Record frame:

| Size | Field                                               |
| ---: | --------------------------------------------------- |
|    4 | body length `n` (at most 64 MiB; larger is refused) |
|    4 | CRC-32C of the body                                 |
|    n | body                                                |

Body:

| Size | Field                                           |
| ---: | ----------------------------------------------- |
|    8 | LSN, one more than the previous record          |
|    1 | kind: `1` commit, `2` abort, `3` checkpoint     |
|    8 | generation this record publishes                |
|    4 | manifest length `m`                             |
|    m | manifest, canonical JSON, `deny_unknown_fields` |
|  ... | blobs, concatenated in manifest order           |

A commit manifest (`"format": "fylo.log-commit.v1"`) names the operation and
lists steps in apply order. Each step is a full post-image, never a delta, so
replaying it twice gives the same result:

- `write`: collection-relative path, blob index and length, SHA-256 of the
  blob, file mode, owner and group when set, and the complete FYLO metadata
  attribute set.
- `move`: from and to paths (soft delete and restore), with the post-image
  attributes of the target.
- `remove`: a path that must be absent afterwards.
- `index`: the prefix-index keys removed and added, in the existing
  `-\tkey` / `+\tkey` line form.

Raw files larger than 1 MiB are not copied into the log. The payload is written
to a scratch file beside its target and flushed before the commit record is
appended, and the step names the scratch path and its SHA-256. That is the only
case with two flushes per commit.

An abort manifest (`fylo.log-abort.v1`) names one earlier LSN that must not be
replayed. A checkpoint manifest (`fylo.log-checkpoint.v1`) records the LSN it
covers, the generation, the byte length of `index/keys.wal` and the SHA-256 of
`index/keys.snapshot` at that point.

### 4. Commit protocol

Under the collection lock, a writer:

1. Recovers the collection if `state.json` says `writing`.
2. Validates the mutation and builds its record: reads the current record
   (the pre-image), checks the identifier, access and schema rules exactly as
   today, computes the post-image and the index delta. The pre-image stays in
   memory for in-process rollback. It is not logged.
3. Appends the record with one `write` call and flushes the segment with
   `sync_handle`. **This is the commit point.**
4. Publishes `writing` (generation + 1) in `state.json` by write-temp and
   rename, without a flush.
5. Applies each step without a flush: write-temp and rename for payloads, then
   attributes, mode and owner; rename for moves; append to `index/keys.wal`.
6. Publishes `stable` (generation + 2) the same way and releases the lock.

Readers keep today's protocol unchanged: they read against one `stable`
generation and retry while it is `writing` or when it moves. Rename keeps each
file atomic for readers in other processes, and they see applied files through
the shared page cache straight away.

Flushes per commit: **one** (two for a raw file over 1 MiB). Renames per put:
three (payload, two `state.json` publications), plus one to release the
collection lock on a shared root, down from nine. The collection lock no
longer flushes: mutual exclusion comes from the atomic link or exclusive
create, not from durability. A lock file can then be left torn by power loss,
so one rule is added: an unreadable lock record whose file predates the
current boot is stale and is reclaimed. Today an unreadable record blocks the
collection until an operator removes it. The writer's own process identity
(boot and start time) cannot change while it runs, so it is computed once per
process instead of by spawning `ps` or PowerShell on every acquisition.
Checking another process's identity, which only happens when a lock looks
stale, keeps today's probe.
When the root is opened with `--exclusive-root`, the kernel lease already
excludes other processes, so the collection lock becomes an in-process mutex
and costs no file calls at all.

### 5. Group commit

Writers to one collection inside one process share a commit queue. A writer
enqueues its mutation and either becomes the leader or waits. The leader takes
the lock, takes every waiting mutation up to 256 records or 8 MiB, validates
them in arrival order against an in-memory overlay of the earlier ones (so a
patch after a put of the same identifier in one batch sees the put), appends
all records with one `write`, flushes once, applies them, publishes one
`writing`/`stable` pair and wakes every member with its own result. A member
that fails validation gets its own error and does not affect the others.

There is no timer and no artificial delay. A writer that arrives while a flush
is in flight waits for that flush and then for its own batch, so its latency
is bounded by two lock-flush-apply cycles plus queueing for the 256-record cap.
A lone writer pays exactly one cycle. `batchPutData` submits its items to the
same queue, so a batch shares flushes while each item stays its own atomic
commit, as today.

Writers in different processes do not batch with each other: they serialize
on the collection lock as they do today. A process that holds the root's
exclusive lease (`exec --loop --exclusive-root`) is where concurrent writers
meet one queue.

### 6. Failure in a live writer

- **Validation fails** (step 2): nothing was written. The error is returned.
- **Append or flush fails** (step 3), for example ENOSPC or EDQUOT: the
  segment is truncated back to its length before the append, the error is
  returned, and the log is closed for writes in this process until the
  collection is reopened. FYLO never retries a failed flush on the same
  handle, because the kernel may already have dropped the dirty pages.
- **Apply fails** (step 5): the record is durable but not acknowledged. The
  writer restores each touched file from its in-memory pre-image, rewrites the
  index log to its length before the batch, appends an abort record for the
  failed LSN, flushes, publishes `stable` and returns the error. Nothing is
  left for recovery, which is what the crash matrix requires for `enospc` and
  `edquot`.
- **Apply fails and so does the rollback or the abort append**: `state.json`
  stays `writing` and the error says both failed. The next opener recovers by
  replaying the record, so the write ends up applied. Today the same case is
  rolled back by the next opener, so this is the one observable difference: in
  both designs the collection recovers to one valid state.

### 7. Recovery

Recovery runs under the collection lock: on the first write that finds
`writing`, in `recover`, and when a reader finds a stranded `writing` state
whose owner is gone (the existing 2d350d6 path). In order:

1. If `state.json` names a journal transaction, run today's journal recovery
   and stop. The two never coexist on one collection.
2. Open the newest segment whose header checksum is valid. Its first record
   must be a valid checkpoint. If no segment qualifies, rebuild the index
   from documents (step 6) with an empty log.
3. Read records forward, checking length bounds, checksum and that each LSN is
   one more than the last. Stop at the first record that fails; truncate the
   segment there. That is the torn tail of an interrupted append, and none of
   it was acknowledged.
4. Truncate `index/keys.wal` to the length the checkpoint recorded and check
   the snapshot hash. A mismatch means the index cannot be trusted, so go to
   step 6 after replay.
5. Replay every commit record after the checkpoint that no abort names, in LSN
   order: perform its steps and append its index delta. Steps are post-images,
   so records that were already applied are rewritten to the same bytes.
6. If step 2 or 4 found the index untrustworthy, rebuild it from documents,
   exactly as recovery does today.
7. Checkpoint (section 8) and publish `stable` at a generation above every
   generation in the log and in `state.json`.

A second recovery finds a clean checkpoint, an empty tail and `stable`, and
reports `recovered: false`.

### 8. Checkpoint and truncation

A checkpoint runs under the collection lock when the segment passes 16 MiB or
4,096 records, when the root closes cleanly, before any journal operation from
section 1, and at the end of recovery. In order:

1. Collect every path the records since the last checkpoint touched. The set
   comes from the log, so any process can checkpoint, not only the one that
   wrote.
2. Flush each of those files and their parent directories, then
   `index/keys.wal`.
3. Fold the index log into the snapshot if it is large (today's
   `compact_index_if_large` rule, moved here from the write path), with the
   existing durable replace.
4. Write a new segment, sequence + 1, whose first record is a checkpoint, and
   flush it with a full drive flush (`F_FULLFSYNC` on Apple, `sync_handle`
   elsewhere), then flush the log directory.
5. Remove the old segment and flush the directory.

A crash between 4 and 5 leaves two segments; recovery uses the newer. The cost
is one flush per touched file per checkpoint instead of several per write, and
one full drive flush per checkpoint.

### 9. Durability promise

An acknowledged write is in the collection log, and the log has been flushed
with `sync_handle`.

- **Process death:** unchanged. Every acknowledged write survives, on every
  platform.
- **Power loss on Linux and Windows:** unchanged. Every acknowledged write
  survives on a disk that honours flushes.
- **Power loss on Apple, default:** better than today. Everything up to the
  last checkpoint survives, because the checkpoint ends with `F_FULLFSYNC`.
  Writes after it may be lost, and a document written after it is either
  whole or absent (rename). Today nothing on Apple is promised across power
  loss.
- **Power loss on Apple, opt in:** a new root setting,
  `durability: "power-loss"` (`RootConfig::durability`, application.json
  `durability`), flushes each group commit with `F_FULLFSYNC`. Every
  acknowledged write then survives. It costs one full drive flush (about
  5.7 ms measured) per batch, so it does not meet the 1 ms target. The default
  stays `"process"`, which keeps today's per-write promise.

### 10. Compatibility and rollback

`state.json` keeps `fylo.collection-generation.v1` whenever the log is empty
after a checkpoint. While a collection has records since its last checkpoint,
`state.json` is written as `fylo.collection-generation.v2` with
`"log": true`. Older binaries already refuse an unknown format with
`ENATIVE_CORRUPT_METADATA`, so they fail closed instead of ignoring writes they
cannot see. A clean close checkpoints, so a root closed by the new binary
opens in the old one. The released-rollback gate
(`verify-rust-released-rollback.mjs`) gets a case for this, and the operations
runbook says to close or run `fylo recover` before a downgrade.

The browser and Wasm builds use the same code through `fylo-vfs`. Where the
host has no flush, the log is exactly as durable as today's files are there.

## Implementation notes (2026-10-28)

The first implementation corrects these details. None changes the decision:
one append and one flush per commit, redo-only recovery, documents as the
source of truth, and the durability promise above.

- **Scope.** Document collections on the `Native` profile, on Unix and
  Windows, go through the log. Raw-file collections (`put_raw_file` and their
  metadata, access, delete and restore) stay on the journal for now, so the
  out-of-line raw blob path is not built yet. WASI and browser builds also stay
  on the journal. `set_record_access` uses the log on Unix only.
- **One segment, no rotation.** The segment is `log/segment.log`. A checkpoint
  makes every applied file durable, publishes the v1 `state.json` durably and
  removes the log directory. The next commit creates a fresh segment whose
  first record is the checkpoint: the `keys.wal` length, the snapshot digest,
  and the generation the segment was opened at. The header's sequence field
  holds a random segment identity.
- **Steps.** There are two kinds. `update` either creates a file or rewrites
  one in place, with optional bytes, attribute set, mode and owner, and always
  an explicit modification time, so replay sets the same time. Rewriting in
  place keeps the owner and inode, as the journal path does. `move` covers
  soft delete and restore: a destination that exists means the move already
  happened. The index delta lives in the manifest (`indexRemoved`,
  `indexAdded`), not as a step.
- **Order of publication.** `writing` is published before the append, not
  after it, so a crash between the append and the publication cannot leave a
  durable record behind a `stable` state. The v2 `state.json` carries
  `log: {nonce, length, nextLsn, records}`. A reader compares the segment's
  length with it and treats a mismatch as writing. That routes a commit whose
  publication power loss dropped to recovery.
- **Group commit** is a shared busy period, not batches a leader validates.
  Writers plan and append in turn under the queue's mutex. One of them
  flushes for everything appended so far, and each applies its own commit. A
  plan that touches a file another member has not applied yet waits for it.
  Readers see `writing` for the whole busy period. `batchPutData` is
  unchanged, because the machine session runs one request at a time.
- **Failures.** A failed append is cut off. A failed flush rewinds the segment
  to its last durable point, or removes a segment with nothing durable in it,
  and fails every writer whose record it covered. New writers wait for the
  busy period to end, and the next one reopens the segment. A failed apply is
  undone from the in-memory pre-images and the inverse index delta is
  appended, instead of cutting `keys.wal` back, because other members may have
  appended to it. Then the abort record is appended and flushed.
- **Leftover log beside v1.** If the segment was opened at a generation at or
  above the one `state.json` records, a publication was lost and recovery
  replays. Otherwise a checkpoint finished and died before removing the
  directory, and only the directory is removed.
- **Checkpoints** run at 4,096 records, at 16 MiB, or when `keys.wal` outgrows
  the larger of 64 KiB and the snapshot. That is the compaction rule, moved
  from the write path to here. A clean close is
  `NativeWriteRoot::close_logs()`, which the write CLI and `fylo exec` (single
  request and `--loop`) call on exit.
- **Locks.** There is no `--exclusive-root` shortcut: a process started
  without that flag never checks the lease, so the file lock stays. A native
  lock is one exclusive create instead of link-then-check, because the hard
  link alone was a third of a put on APFS. EFS keeps link-then-check with its
  flushes. An unreadable native lock record is stale once it is older than the
  five-minute lock TTL. Boot time is not used.
- **Failpoints.** The new ones are `before-log-append`, `after-log-append`,
  `before-log-sync`, `after-log-sync`, `after-log-apply-step`,
  `after-checkpoint-flush` and `after-checkpoint-remove`. A failpoint inside
  the abort path is not declared, because a run injects one failpoint and
  cannot also fail the apply that leads there. The crash matrix gains raw-file
  `set-access`, `delete` and `restore` scenarios, which keep the journal's
  access and move failpoints covered.
- **Setting.** The setting is `fylo.durability` in `application.json`:
  `"process"` (the default) or `"power-loss"`.

## Consequences

Benefits:

- one flush per commit instead of 23 to 28, and three renames per put instead
  of nine;
- concurrent writers in one process share flushes;
- a power-loss promise on Apple at checkpoint granularity, and an opt-in one
  per write;
- recovery is redo-only, idempotent and bounded by the checkpoint size;
- the checkpointed layout is the layout readers and rebuild tooling already
  know, which keeps segment storage (the next ADR) a separate decision.

Costs:

- a second write path beside the journal until the remaining operations move;
- a crash leaves up to 16 MiB of log to replay before the next write;
- every record is written twice between checkpoints (log, then file), and raw
  files over 1 MiB pay a second flush;
- a write whose apply and rollback both fail is completed by recovery rather
  than undone.

## Alternatives considered

- **Drop flushes from the journal and keep undo.** Without the barriers an
  undo journal is unsafe: a changed file can reach disk before its
  before-image.
- **Apply before the commit flush.** Avoids the in-memory overlay for group
  commit, but after power loss an applied file can be on disk while its record
  is not, with nothing to undo it.
- **One log per root.** Batches across collections, but every writer process
  would serialize on one root lock, and generations are per collection.
- **Undo images in the log.** Needed only for in-process rollback, which
  already has the pre-images in memory. Leaving them out halves record size.

## Verification

- `fylo-format` unit and fuzz targets for the segment and record parser:
  truncation at every byte, bit flips, oversized lengths, LSN gaps and
  unknown kinds all stop the scan without panicking.
- New failpoints, declared in `FAILPOINTS` so the crash matrix must reach
  them: `before-log-append`, `after-log-append`, `after-log-sync`,
  `after-log-apply-step`, `before-log-abort`, `after-checkpoint-flush`,
  `after-checkpoint-segment`, `after-checkpoint-remove`. Every scenario in
  section 1 runs at each, under `abort`, `enospc` and `edquot`, with the
  existing assertions: the first read after a crash sees an agreeing index,
  recovery is idempotent, and a live failure leaves nothing to recover.
- An oracle check after each crash case: the collection equals either the
  state before the mutation or the state after it, byte for byte, for
  documents, attributes and modes.
- A rebuild-equivalence check after every checkpoint in the test suites: the
  index files equal a rebuild from documents.
- The repeated concurrency suite (`index_reuse_concurrency`, settled-read
  checks) and the bounded multi-process load harness, with no lost or
  duplicated writes.
- Benchmarks on the Mac's APFS with the 2026-10-11 harness: durable put under
  1 ms p50, with patch and delete reported beside it, and the syscall trace
  showing one flush per commit. The ATLAS sparse-bundle write numbers are
  re-measured against the report's table.

## Operations

- `fylo recover --collection <name>` replays and checkpoints.
- `fylo inspect` reports the log's LSN range, segment size and checkpoint age.
- Before a downgrade, close every writer or run `fylo recover` on each
  collection.
