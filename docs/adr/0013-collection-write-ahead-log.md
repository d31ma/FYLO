# ADR 0013: Collection write-ahead log with group commit

- Status: **Accepted** (approved by Samuel, 2026-10-28)
- Date: **2026-10-28**
- To be amended by [ADR 0035](0035-fylos-own-sdk-in-the-store-shape.md) (accepted, not yet built): a client's one-collection transaction (`storeTransaction`, `transactionBegin`/`Commit`) commits as one log record.
- Amended by the namespace log for transactions over several collections
  (Amendment 3, one commit over several collections, BYCKR-TXN2, accepted
  2026-11-21).
- Owners: **BYCKR maintainers**
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

The 2026-10-28 syscall trace (report "BYCKR on ATLAS: tests, speed and the
latency gap") counted, per operation:

| Operation | File calls | fsync | rename |
| --------- | ---------: | ----: | -----: |
| Put       |        489 |    23 |      9 |
| Patch     |        659 |    25 |      9 |
| Delete    |        627 |    28 |     11 |

Flushing was 53 to 90% of write time in the APFS image on ATLAS, where one
flush costs 0.86 ms. On the Mac's internal SSD a flush is cheaper, and the
cost moves elsewhere. The 2026-10-28 rerun on develop fe2f1b7 (internal APFS,
load 4 to 9, logs in `/Volumes/FORGE/Logs/BYCKR/fylo-wal/bench-before-mac`)
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
`fsync`, not `F_FULLFSYNC`, so BYCKR survives process death and kernel panics
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
review, and moving it is a later ADR. (Amendment 2 puts sidecar-store
document collections on the log.)

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
  blob, file mode, owner and group when set, and the complete BYCKR metadata
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

Clarified 2026-11-12: two `fylo exec` processes never write one root at the
same time. Every `fylo exec` session, with or without `--exclusive-root`,
holds the root's kernel lease from its first request until it exits (ADR
0003; "Root owner" in `README.md`). A second `fylo exec` on the same root gets
`EROOTLOCKED` for every request until the first exits; it does not wait for
the collection lock.

### 6. Failure in a live writer

- **Validation fails** (step 2): nothing was written. The error is returned.
- **Append or flush fails** (step 3), for example ENOSPC or EDQUOT: the
  segment is truncated back to its length before the append, the error is
  returned, and the log is closed for writes in this process until the
  collection is reopened. BYCKR never retries a failed flush on the same
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
  the snapshot, clamped between 64 KiB and 16 MiB. That is the compaction
  rule, which the write path without a log uses too. The 16 MiB ceiling is a
  quarter of the 64 MiB that readers accept for `keys.wal` (2026-11-07):
  without it a snapshot over 64 MiB let the log grow past what readers read,
  and the collection failed every read and write with `ENATIVE_FILE_SIZE`.
  Compaction reads the log with the snapshot's 256 MiB limit, so recovery and
  the next write fold such a log. A clean close is
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

## Amendment 1 (2026-11-21): the Windows start time comes from the process handle

- Status: **Accepted** (2026-11-21: the user's "Allow it (Rec.)" for one
  small unsafe Windows function with `OpenProcess` and `GetProcessTimes`,
  Q-WIN1, following ADR 0032's `windows_disks.rs` pattern; and the user's
  "Allow it (Rec.)" for the synchronize right and one zero-wait
  `WaitForSingleObject` in the same function, Q-WIN1b)

On Windows, `process_identity` asked WMI for a process's start time by
starting PowerShell (`Get-CimInstance Win32_Process`). On the ARM64 Windows
test VM one call took 1.0 to 3.3 s, 10 s right after boot, and 47.7 s for a
process that no longer exists. Section 4 computes the writer's own identity
once per process, so every `fylo` process paid it on its first lock, which
comes with its first write: the first `putData` took 1.6 to 5.0 s on
26.45.07 while later ones took 6 to 60 ms. A host that stops `fylo` one
second after its last request (TACHYON) could cut that write off. Checking
another process's lock record paid it again, uncached.

Decision: read the start time with `OpenProcess`
(`PROCESS_QUERY_LIMITED_INFORMATION` and `PROCESS_SYNCHRONIZE`) and
`GetProcessTimes`, in one function (`ProcessStartTime::creation` in
`crates/fylo-storage-native/src/process_start_time.rs`, Windows only). Windows
still opens a process that has exited while another process holds a handle
to it, for example the host that killed it, and WMI lists it no longer. So the
function first asks `WaitForSingleObject` with a zero timeout: a process that
has exited reads as gone, as with WMI, and its stale lock is reclaimed. The recorded value does not change: WMI's `CreationDate`
keeps whole microseconds, so the .NET ticks are
`creation FILETIME / 10 * 10 + 504911232000000000`. On the test VM this
equalled WMI's value for 12 of 12 processes, including the System process
and `lsass.exe`, so lock records written by older versions still match. A
process that does not exist reads as gone, as before. When Windows refuses to
open a process (for example, access denied), BYCKR still asks WMI, so a live
owner is never read as gone. The two unsafe calls are listed in
`docs/security/unsafe-inventory.md` (ADR 0004).

## Amendment 2 (2026-11-21): one engine, two metadata stores

Accepted 2026-11-21 (BYCKR Build Coordinator). BYCKR-PERF1.

**Context.** ADR 0033 decided that the write-ahead log is BYCKR's one engine,
and that a record's metadata is stored where the file-system probe (L1-4)
records it: in extended attributes, or in a `<payload>.fylo-attrs` sidecar
file (ADR 0010's format). Section 1 above still kept the
`EfsSidecarV1` profile, today's sidecar store, on the undo journal. A network
share has no usable extended attributes (on macOS NFS a `setxattr` writes an
AppleDouble `._` file that other machines do not see), so a share gets
sidecars, and Mesail's production root on ATLAS is such a share. Measured on
ANVIL's NFS mount, one sidecar-store put on the journal made 1,053 file
calls (180 opens, 27 flushes, 11 renames, 11 removals); a put on the log
makes about 110 calls with one flush. A send of 25 documents took 25 such
puts, because a batch needs the log.

**Decision.** Section 1's scope covers both metadata stores. On Unix the
native engine commits sidecar-store document collections through the log;
the sidecar is written by the log's apply step, and recovery redoes it from
the log exactly as it redoes document files.

- The plan writes a record's sidecar as a step of its own in the same log
  record: a created record gets a `create` step for its sidecar; a rewritten
  payload gets a sidecar rewrite with the new binding; a metadata change
  rewrites only the sidecar (the payload too only when its ownership
  changes); a move moves both files.
- The plan builds the sidecar bytes from the bytes it writes: the binding is
  the SHA-256 of the planned payload, so no step reads a file back.
  Metadata is never read from or written to extended attributes in the
  sidecar store; pre-images of payload and sidecar are plain files.
- A create refuses an orphan sidecar at the target, and a move refuses a
  sidecar at the destination or a missing one at the source, with the
  journal's `CorruptMetadata` errors.
- A new payload and every sidecar get mode `0600`, as the journal gives them.
- `commit_document_writes` (a batch in one record) works in both stores.
- Raw-file collections stay on the journal in both stores (section 1's
  implementation note), and so does every operation section 1 lists as out
  of scope.
- Windows: the sidecar store does not run there today. Binding a payload
  and checking file owners need POSIX calls, so every sidecar-store write on
  Windows is refused with `ENATIVE_UNSUPPORTED`, on the log or on the
  journal. Until that changes, only Unix sidecar roots use the log; when the
  store runs on Windows it uses the log too.
- Transactions: the storage engine's one-collection batch
  (`commit_document_writes`, the commit of a `storeTransaction`) works on a
  sidecar root and commits as one log record, as on an xattr root. A sidecar
  root therefore no longer needs to refuse a one-collection transaction with
  `ETXNLAYOUT`; the machine layer's check (BYCKR-SDK-3) changes from "native
  profile only" to "a profile whose documents use the log" (native, and the
  sidecar store on Unix). A transaction over a raw-file collection, or on
  Windows with the sidecar store, is still refused.

Recovery, checkpoints, group commit and the durability promise are those of
sections 4 to 9: the log record is flushed before any file changes, apply is
redo-only, and a checkpoint flushes every file the steps name, sidecars
included. A crash between a document's rename and its sidecar's leaves a
record the log replays. The journal's sidecar failpoints
(`after-record-payload-rename`, `after-record-sidecar-rename`) stay on the
journal paths that still use them.

**Fencing.** A writer reads the owner record once at the start of a commit
under the collection lock, reuses that check until a commit point, and reads
it again right before the log append (ADR 0033: one small read per commit).
The undo journal reads it again at its start, at its commit marker and at a
rollback. Before this amendment a sidecar-store put read it about 55 times.

**Apply.** The applier sets a file's time and mode through the handle that
wrote it. An NFS client flushes written data before it sets times, so the
time the server keeps is the one the step names (checked on ANVIL from the
server side for 100 B to 3 MB files).

## Amendment 3 (2026-11-21): one commit over several collections (BYCKR-TXN2)

- Status: **Accepted 2026-11-21 (BYCKR Build Coordinator)**
- Asked for by: the user ("Yes, several"), for TACHYON and Mesail: one
  commit per user action, such as a message and its outbox entry. Mesail's
  largest action (a chat send) writes 6 to 8 collections and about 25
  documents in one namespace.
- Contract: ADR 0035, Amendment "Several collections".

#### Problem

Every collection has its own log, and one commit is one record in one
collection's log. A transaction over collections A and B therefore needs one
point at which all of its writes become durable together, and a recovery
that never applies only part of it. On a network share (Mesail's production
data on ATLAS) each flush costs about 30 to 230 ms, so the number of flushes
per commit decides the cost.

#### Options compared

| | (i) One record in a namespace log | (ii) Parts in each collection log, then one commit marker |
| --- | --- | --- |
| Commit point | one append and **one flush** of the namespace log | each part appended to its collection log and **flushed** (N flushes, they may run in parallel), then a marker appended and flushed (one more) |
| Flushes per commit (N collections) | 1, plus one per collection whose log segment is new since its last checkpoint | N + 1 |
| Where a part lives until checkpoint | the namespace record; a copy is appended to the collection log without a flush | the collection log, flushed |
| Recovery | each collection replays its own log as today, then applies the parts of namespace records it lacks, after its last record | recovery must read the marker log first; a prepared part with no marker is skipped in every collection, so each collection's replay depends on another file |
| Undecided parts | none: a part exists in a collection log only after the commit point | a prepared part sits in a collection log between its flush and the marker; a single-collection commit after it must not be blocked or re-ordered by it, and a checkpoint must not fold an undecided part into the files |
| Checkpoints | unchanged, plus one mark record in the namespace log before a collection removes its log | a checkpoint must wait for, or roll back, every undecided part it covers |
| Locks | collection locks in name order, namespace log mutex last | the same |
| Readers during apply | every collection is `writing` from before the commit point to after its apply | the same |
| New files | one log per namespace, created on the first multi-collection commit, removed when settled | one marker log per namespace |
| L1-3b | lives in the namespace folder under the namespace lease; does not use L1-3b's undo journal, which costs about 10 durable writes per operation | the same |

**Decision: (i).** It is the only option with one flush per action, and
each collection's own replay stays independent of the namespace file except
for one final step. (ii) costs N + 1 flushes (8 to 9 for Mesail's chat send,
about 0.3 to 2 s on ATLAS) and adds undecided state to every collection log.
Building on L1-3b's namespace journal (the undo journal for catalog
operations) was also rejected: an undo journal needs before-images and
flushes per step, which is what ADR 0013 replaced.

#### Files

```text
<namespace root>/.fylo-transactions/.namespace/log/segment.log
```

The segment format, record frame and checksums are ADR 0013's. The segment
opens with a checkpoint record whose manifest is
`{"format": "fylo.log-namespace-mark.v1", "collection": null}`. Record
kinds:

- **commit**, manifest `fylo.log-namespace-commit.v1`: a list of parts. A
  part names its collection, the generation it publishes in that collection,
  its blob range inside the record, and the collection's own commit manifest
  (`fylo.log-commit.v1` steps and index delta).
- **abort** (`fylo.log-abort.v1`): a commit whose apply was rolled back.
- **checkpoint mark** (`fylo.log-namespace-mark.v1` with a collection): that
  collection checkpointed its own log at a generation; every part of it at
  or below that generation is durable in its files.

A transaction that writes one collection never touches the namespace log.

#### Commit protocol

1. Lock every collection in **collection-name order**: its in-process
   commit queue exclusively (other writers of this process wait), then its
   collection lock file. Recovery of a collection runs here, as today.
2. Plan each collection's part with the same planners as a one-collection
   batch. Check the index room and the record limit (64 MiB in all).
3. From here on, work in **request order** (the order of each collection's
   first write). Publish `writing` in each collection's `state.json`,
   creating its log segment if it has none.
4. Append the one record to the namespace log and flush it once
   (`F_FULLFSYNC` with `durability: "power-loss"`). **Commit point.**
5. Apply each part, in request order.
6. Append each part to its collection's log as an ordinary commit record,
   without a flush. A segment created in step 3 is flushed once here, so its
   name is durable before a later commit relies on it.
7. Publish `stable` in each collection, in request order, and release the
   locks.

A failure before step 4 writes nothing. A failed apply in step 5 undoes
every part from its in-memory pre-image, in reverse order, and appends an
abort record (one more flush). If the undo or the abort fails, every
collection stays `writing` and recovery completes the committed write, as
ADR 0013 section 6 does for one collection.

#### Why the unflushed copy is safe

The copy of a part enters a collection's log under that collection's lock,
before any later commit to the collection. A later commit flushes the whole
log file, which makes the earlier copy durable too. So after a crash, a part
missing from a collection's log is never followed by a durable record in
that log: applying it after the log's last record is the commit order.

#### Recovery order

A collection's recovery (unchanged triggers: `writing`, a log longer than
`state.json` records, a leftover log):

1. Replay the collection's own log (ADR 0013 section 7).
2. Compute what is durable in the collection: a v1 `state.json`, the
   generation its segment opened at, its replayed records, and the highest
   checkpoint mark for it in the namespace log. A v2 `state.json` is never
   trusted for this, because it is published without a flush.
3. Apply every part of a namespace commit record (not aborted), in LSN
   order, whose generation is above that. Append its index delta when the
   index log is trusted; otherwise rebuild the index from documents.
4. Checkpoint and publish `stable`, as today.

**Namespace settling.** A power cut can lose the `writing` state of a
collection whose part is only in the namespace log; nothing would then
trigger step 1. So before the first write of a process takes any collection
lock, the process settles the namespace once: when a namespace log exists,
it locks every collection the log names (name order), recovers each one that
lacks a part and checkpoints the others, then removes the log, unless a newer
commit arrived meanwhile. A clean close (`close_logs`) does the same, so a
closed namespace has no namespace log and opens in an older release.

#### Checkpoints

A collection checkpoint is unchanged except for one step: after its files
are flushed and before its log is removed, when the namespace log names the
collection, it appends a checkpoint mark and flushes it. Without the mark, a
later segment of that collection lost in a power cut would let recovery
re-apply an old part over newer writes. The namespace log itself is removed
only by settling (above).

#### Locks

Collection locks are taken in name order by every transaction, and a
one-collection writer takes one lock, so no cycle can form. The namespace
log's mutex is held only for an append and its flush, and is always the last
lock taken. Two transactions over disjoint collections serialize only on
that flush; they do not group-commit with each other in this version.

#### Readers during apply

- There is **no read snapshot across collections.** A reader without the
  transaction handle may see collection A's new state before collection B's.
- Every collection of the transaction is `writing` from step 3 until its own
  `stable` in step 7, and readers of a `writing` collection wait, as today.
- **Order guarantee:** parts are applied and collections become `stable` in
  the order of each collection's first write in the request, not the lock
  order. An application that writes its pointer last (an outbox entry, a
  head row) can rely on this: once a reader sees the pointer, every
  collection it points into has its new state or is still `writing`, where
  the reader waits. It never finds the pointed-to document missing.

#### Fencing

Every append to the namespace log (the commit record in step 4, an abort
record, a checkpoint mark) first checks the namespace lease with a fresh
read of its record, as each collection-log append does under L1-3b. A writer
that lost the lease fails with `EROOTLEASELOST` before the append, so it
never reaches the commit point, and nothing it planned is applied.

#### Network roots: close-to-open reads

On a network root (NFS, SMB) the client may answer a `stat` from cached
attributes. So every decision of settling and recovery is made from a fresh
open and read, never from cached attributes: whether the namespace log
exists (an open of its segment, not a `stat` of its folder), its records and
marks (read whole after an open), and each collection's `state.json` and log
segment (read after an open, as today). This is the rule L1-3 follows for the
lease record.

An open and a read are not enough on macOS NFS: within the attribute cache
time the client returns data up to 5 s old (1.6 s with `F_NOCACHE`), as
BYCKR-L1-3 measured. So the namespace log, its marks and every `state.json`
that settling and recovery read go through L1-3's fresh-read helper, the one
the lease record uses. There is one helper; this slice adds no second copy.

#### Readers after a power cut

A power cut (of the writer, or of the storage server of a network root) can
leave a part only in the namespace log, with its collection's `state.json`
showing an older stable generation. Until a writer settles the namespace, a
reader must not serve that older state. The rule:

- A namespace log names, per collection, the highest generation of its
  parts. A reader of a collection the log names whose `state.json` is
  `stable` at a lower generation treats the collection as `writing`: it
  retries as for a collection log with an unapplied record, then answers
  `EENGINE_NAMESPACE_UNSETTLED` (below). The next writer settles the
  namespace and the reader then sees the new state.
- Each reader process reads the namespace log (open and read) at its first
  read of the namespace, again whenever it sees a collection's generation go
  backwards (only a lost write on the storage can cause that), and again
  whenever the log's identity (segment nonce and length) differs from what it
  cached. While a live writer holds the namespace, the collections it
  commits are `writing` until their parts are applied and copied, so a
  reader never needs more than this.
- Collections the log does not name pay nothing; a collection it names pays
  one integer comparison per read while the cached log is current.
- **The wait is bounded.** A reader does not settle the namespace itself: it
  may hold no lease and no write access, and a read-only tool must not write.
  After the normal read retries it fails with `EENGINE_NAMESPACE_UNSETTLED`
  (retryable), whose message says that a writer must open the root (any
  write, or `fylo recover`) and that the read can then run again. A
  collection the log does not hold back reads normally meanwhile.

#### Limits (answers for Mesail and TACHYON)

- **Collections per transaction:** no limit of its own. The transaction's
  limits bound it: 1,000 writes, 8 MiB of documents, and one record of at
  most 64 MiB. Each extra collection costs two `state.json` renames and one
  unflushed append (plus one flush the first time after its checkpoint).
- **Time limit of an open handle:** the same for any number of collections:
  default 30,000 ms, at most 300,000 ms. Writes are held in memory until
  commit; locks are taken only at commit. Other writes to a collection the
  handle has written, in the same session, answer `ETXNBUSY`.
- **`createCollection` inside a transaction:** not allowed (`ETXNSCOPE`).
  It is a journal operation with its own catalog update and checkpoint, so
  joining it to the record is not cheap. Create collections before `begin`.

#### Windows

- The namespace log uses the same segment code, flush and directory flush as
  collection logs; Windows runs it in the Windows lane.
- A segment handle is closed before its folder is removed (Windows cannot
  remove a folder holding an open file): settling drops the namespace log's
  handle before removing it, as `end_busy` does for collection logs.
- A reader that loses the race with a `state.json` rename sees access
  denied on Windows; this is already read as "the generation moved" and
  retried.

#### Failpoints and tests

New failpoints: `before-namespace-log-sync`, `after-namespace-log-sync`,
`after-namespace-apply-part`, `before-namespace-collection-append`. The crash
matrix gains a namespace transaction scenario that reaches each of them.
Tests in `crates/fylo-storage-native/tests/namespace_transaction.rs`: two
and three collections, a failing write writes nothing, raw-file and repeated
collections refused, a part lost from a collection log is recovered,
overlapping transactions in three threads never deadlock, a crash at each
failpoint in a child process leaves all or nothing, a checkpoint mark stops
an old part from returning, and a reader that follows the last write never
finds its target missing.

#### Costs

- A namespace log appears on the first multi-collection commit and stays
  until settling or a clean close.
- A clean close of a namespace with a namespace log flushes one mark per
  collection it names before removing it.
- Recovery reads the namespace log once per collection it recovers.
- Measured (release build, 25 writes, 20 rounds; 2026-11-21): one
  transaction over 7 collections costs about the same as 7 separate commits
  (internal disk p50 14.9 ms against 12.0 ms; ANVIL NFS p50 179 ms against
  157 ms). The gain is all or nothing and the stated order, not speed: per
  collection there are still two `state.json` renames, and each part's
  bytes are written twice (namespace record and collection copy).

#### Possible follow-ups (BYCKR-TXN3, after the release)

- Collection copies that point at the namespace record instead of
  repeating its blobs, so a part's bytes are written once before the
  checkpoint.
- Fewer publication renames per collection in a transaction.
