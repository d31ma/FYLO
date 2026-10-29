# Running segment collections inside a `.fxdb` file

Status: design for implementation, 2026-10-29. Applies ADR 0015 (segments)
to ADR 0016 (the `.fxdb` container). Where this note and ADR 0016 differ, this
note records the simpler first implementation and ADR 0016 keeps the end
state.

## Goal

The segment code (`segments/` and `write/commit_log/segment_*.rs`) works on a
collection directory: numbered `.seg` files, `state.slots`,
`locations.wal` and `locations.snapshot`. A `.fxdb` collection keeps the same
records, the same commit protocol and the same recovery rules, but stores
them in extents of one file. The plan is one seam with two
implementations, so the commit, recovery, compaction and query code is
shared and tested once.

## The seam: `SegmentMedium`

Every place the segment code touches a path goes through one trait object
per collection. The directory implementation is today's code moved behind
it; the container implementation maps each call onto `ContainerFile`.

| Operation | Directory medium (today) | Container medium |
| --- | --- | --- |
| list segment numbers | `read_dir` of `segments/` | the map's `segment` extents owned by the collection, plus the active extent the generation slot names, by sequence number |
| read a span of segment *n* | `pread` on `NNNNNNNNNN.seg` | `read_payload` on the extent whose sequence is *n*; a missing or reused extent is `ConcurrentWrite` (moved generation), as a missing file is today |
| length of segment *n* | file length | active: the slot's committed length; sealed: a scan to the last valid frame, cached. A reused extent is cleared (hole punch, else zeros) before its header is written, so a scan never reads a former owner's frames |
| create segment *n* | create file, write `LogSegmentHeader` | allocate a `segment` extent (its sequence *is* *n*), write the same `LogSegmentHeader` at payload offset 0 |
| append + sync | `write` + `fdatasync` | `write_payload` at the tracked length + `sync` |
| truncate a torn tail | `set_len` | nothing to cut: the slot's committed length bounds every read; recovery zeroes the torn bytes |
| remove segment *n* | `remove_file` | `release`, and a map checkpoint, after which the extent may be reused |
| adopt a compacted segment | rename a temporary file | the new extents were allocated under their final sequence numbers; adoption is the compaction record |
| read / publish / create slots | `state.slots` | the directory block's generation slot pair |
| read / replace the location snapshot | `durable_replace` of `locations.snapshot` | `write_snapshot` → `record_checkpoint` → `release_snapshot(old)` |
| read / replace the schemas snapshot | `segments/schemas.snapshot` | a `schemas` run the checkpoint slot names; see "Schema versions" |
| location log | `locations.wal` | none; see below |
| maximum segment bytes | 1 GiB | the extent's payload capacity (8 MiB − 4 KiB by default) |

**Segment numbers are extent sequence numbers.** A `SegmentLocation`
keeps `(segment, offset, length)` unchanged. Because a sequence number is
never reused, a location into a released and reused extent fails the header
check and the reader retries, which is ADR 0016 section 5's rule without
new fields. The container medium keeps a `sequence → extent index` table,
rebuilt at open from the map and updated on allocate, take-back and
release. A number the table lacks may belong to a writer whose map is newer
than a reader's: the reader rereads the map when the superblock generation
moved (`ContainerFile::refresh`), looks the number up there, and else takes
back, in memory only, the active extent the generation slot names.

**Slots.** The container's `ContainerGenerationSlot` carries the generation,
`writing`, the active extent and its sequence, the committed length,
`next_lsn` and the record count. `SlotRecord`'s `locations_length` has no
meaning without a location log; `table_epoch` becomes the collection's
checkpoint number from its checkpoint slot. The medium converts in both
directions, so the commit code keeps using `SlotRecord`.

## Location deltas without `locations.wal`

A directory collection writes each commit's location rows to
`locations.wal`, and readers lay the log over the snapshot. In `.fxdb` the
rows are not written twice. The location table is:

1. the snapshot named by the newest checkpoint slot, and
2. rows derived from the records between the checkpoint record's position
   and the slot's committed length, replayed in sequence order.

`LocationTable::extend` gets a second source: instead of reading log bytes
from `wal_length` to the new length, it replays records from the last
position it applied to the slot's position, using the same row derivation
`SegmentRebuild` uses. `TableCache` keys a container table by
`(file identity, collection id)` and its position by `(sequence, offset)`.
The replayed tail is bounded by the checkpoint interval, as ADR 0016
section 5 says.

### Damage after the snapshot

Without a location log, a record after the snapshot that cannot be read
stops the table: readers get `ENATIVE_DOCUMENT`, as they do for any damaged
record they reach. `fylo verify` and `fylo salvage` instead build the table
as far as the records allow (`LocationTable::load_salvaging`): the replay
stops at the damaged frame and marks the table degraded there. Verify
reports where the locations stop; both then name the documents a damaged
document-segment record at or after that point held, unless a readable
record after it writes them again, which is what a directory collection's
location log lets salvage report as lost.

The difference: a directory collection's log names those documents
whatever the damage, while here they are read from the damaged record's
manifest, unchecked, keeping only identifiers that are canonical TTIDs. A
record whose manifest is damaged too names nothing, and salvage reports only
its byte range. Verify also differs in what it lists: a directory
collection reports every location that disagrees with a rebuild (the
rebuild stops at the damage too, so that includes documents written after
it), while a container reports where its table stops and the documents
lost there.

## Taking a new extent (simplification of ADR 0016 section 6)

ADR 0016 has the old extent end with a seal record naming the new one, so
recovery can follow extents allocated after the last map checkpoint. The
first implementation avoids a new record kind:

- when the segment code rolls to a new extent, the medium writes the new
  extent's opening bytes and then runs a map checkpoint, before any slot
  names the extent. Every segment a slot can name is therefore in the
  durable map, even when both generation slots are lost; a stop before the
  checkpoint leaves an extent no slot names, which the map shows free;
- the generation slot also names the active extent, so a file whose map
  predates its active extent (one written before this rule) still finds it:
  an open takes it back.

Every other change to the map is checkpointed before the allocation byte
is let go ("Locks and write access"): compaction's output extent when it
is taken, before the compaction record that adopts it, and the release of
a segment or a snapshot run, so the release outlives the open that made
it. A released extent is therefore reusable at once, and a reader still
holding a location into it fails the header check and retries.

A finished segment is **sealed** in the map with the bytes the location
table still refers to in it: the full one when the segment code rolls to
the next (the seal is in the map checkpoint that taking the next extent
runs, or that a refresh reads again first), and a compaction's copy when
it is adopted, with a checkpoint of its own. Shrinking moves only
sealed extents, and `fylo inspect` counts them; the live bytes are those at
sealing and are not updated as later writes supersede documents.

The cost is a map write per rotation, and one per other change: a
compaction's copy taken and adopted, a released segment, and the two of a
location fold (its new runs, then the release of the old). The map is 32
bytes per extent, so with 8 MiB extents the extra bytes per rotation are
`file size / 2 TiB` of the rotated data: 0.5% at 10 GiB, 5% at 100 GiB.
Seal records replace this when files that large are in use.

## Locks and write access

**One open per file.** A process keeps one `ContainerOpen` per `.fxdb`
file, shared by every medium over it: the one collection of a hybrid
collection directory's `segments.fxdb` (`with_segment_container`), and every
collection of a database root ("Root integration"). It also keeps one
medium per collection (`ContainerMedium::attach`). Both registries hold
them weakly: a medium closes when the last operation holding it ends, and
the file when its last medium and its last root handle drop; the next
access opens it afresh.

**Readers** open the file with `open_reader` and take no lock. They keep
a segment table per collection, built from the map and kept as extents are
taken and released. A segment number the table lacks may belong to a
writer whose map is newer: the reader rereads the map and the directory
when the superblock generation moved (`ContainerFile::refresh`), and else
takes back, in memory only, the active extent the generation slot names. A
collection another process added is found the same way.

**Writers.** The first write through an open reopens the file with
`ContainerFile::open_writer` (ADR 0016 section 7). Where byte-range locks
are qualified and granted, the writer holds the lease byte shared beside
writers in other processes; elsewhere, or in exclusive mode, the
single-writer lease lets one process write and every other writer fails
with `EBUSY` (`EROOTLOCKED`) after its busy timeout, while readers carry
on. The writer checks its right (`assert_writable`) before every write
through the medium and renews a lease from those same calls once a third of
its takeover time has passed; one that lost its lease fails with
`EFXDB_LEASE_LOST` (`EROOTLEASELOST`), and its open goes back to reading,
so the next write opens the file and claims its collections afresh.

**Claiming a collection.** A medium's first write claims its collection.
Where writers share the file, the claim takes the collection's writer byte
(`LockByte::collection_writer` of its directory block) exclusive, waiting up
to the busy timeout, and holds it while the medium is open, so the bytes
this process wrote but has not published stay its own; under the lease the
process is the only writer and takes none. The claim then settles what a
writer that stopped left in the collection, with the map read afresh: an
active segment extent its slot names but the map does not show is taken
back, and `locations` and `index` runs no checkpoint slot names are
released. Writers of different collections run side by side; those of one
collection take turns, each from its first write until its medium closes.

**Changing the map.** The allocation byte (`LockByte::ALLOCATION`) guards
the extent map: taking, taking back and releasing extents, writing snapshot
runs, adding and dropping collections, and checkpoints. A writer takes it,
rereads the map if another process checkpointed it since, makes its change,
and checkpoints before it lets the byte go, so its map never holds an
allocation another process cannot see, and no two processes hand out the
same extent. A seal waits for the next checkpoint and is made again after a
refresh. Under the lease the bytes are decided inside the process
(`set_process_local`), so the same path serves both modes. Readers take no
byte.

Inside a process, the commit queue keeps one writer per collection at a
time, as it does for a directory root. A database root opens a medium per
operation, so each write claims its collection again: a lock byte, the
allocation byte, a superblock read and a scan of the collection's runs in
the map, and a checkpoint only when a stopped writer left something.

## Encryption and codecs

Record frames gain ADR 0016 section 4's codec byte and, in an encrypted
file, a nonce and tag sealed with `ContainerSealContext::Record`. Both
are behind the frame encode/decode the medium owns, so the directory medium
keeps today's frames byte for byte.

**Codecs (landed).** `SegmentMedium::frame_bodies` says how a medium's frames
store a record's body: as is for a directory (`fylo_format::PlainBody`), and
for a container as `segments/container_frames.rs` does it. Every frame the
segment code writes or reads goes through it: appends, segment openings,
compaction copies, scans, replays, salvage and point reads. A container frame
is the directory frame (sync marker, length, CRC-32C) around a stored body: the
header byte (codec in the low four bits, `0x10` a dictionary id follows,
`0x20` sealed), the 4-byte dictionary id when one was used, then the record
body compressed on its own. The length and CRC cover the stored bytes, so
`fylo verify` and salvage find damage without decompressing.

- Native writers write zstd level 3, the browser LZ4, and a body that would
  not shrink is stored as is (codec 0). Every build but WASI reads all three;
  WASI reads no segments and carries neither library.
- Compaction re-encodes each copied record, so LZ4 records come out as zstd.
- A codec or flag this build does not know is refused as a newer format
  (`ENATIVE_UNSUPPORTED` with `EFXDB_VERSION` in the message), never taken for
  a torn tail: a scan that stops there fails rather than let recovery cut the
  segment.
- A damaged frame still names its documents to salvage when its stored body
  decompresses, as when only its checksum was hit.

**Still to come.** Per-collection dictionaries: training on the first 1 MiB
of documents at a checkpoint, a `dictionary` extent the checkpoint slot names,
loading every dictionary a collection's records name, retraining at
compaction, and their checks in `fylo verify`. The frame header, the
dictionary id and a codec per dictionary are in place; a collection has no
dictionary yet, so a frame naming one is refused as damage. Sealing records
in an encrypted file. Setting the superblock's codec feature bits, which
`fylo inspect` reports.

## Order of work

1. **Seam refactor** (directory medium only): introduce `SegmentMedium`,
   move every path operation in the segment code behind it, no behaviour
   change. The existing segment tests, crash matrix and migration round
   trip must pass unchanged.
2. **Location source**: give `LocationTable` the record-replay source and
   use it in a test-only medium that has no location log.
3. **Container medium**: implement the table above over `ContainerFile`,
   then run the segment test suite against both media.
4. **Frames**: codec byte, dictionaries, sealing.
5. **Root integration**: a root path ending in `.fxdb` opens a container;
   `fylo pack` and `fylo unpack` convert between a directory root and a
   file.

## Root integration

Step 5's first half: ADR 0016 section 11's rule that a root path naming an
existing regular file is opened as a container, for document collections in
the segment layout. `NativeRoot`, `NativeWriteRoot` and the engines take a
`.fxdb` path wherever they take a root directory. Nothing is created beside
the file except the lease that ADR 0016 section 7 allows.

**Opening and creating.** `NativeRoot::open` on a regular file opens the
file as a database, over the process's one open of it ("Locks and write
access"), which every root handle and every collection in it share. A
reader takes no lock and follows what writers in other processes add,
reading the map again when their checkpoints moved it and taking back the
active extent a collection's generation slot names when the map predates
it, as after a writer stopped. A writer (`NativeWriteRoot`) opens the file
to write on its first write, as every container does: with byte-range
locks where they are qualified, so writer processes of different
collections write one database side by side and those of one collection
take turns, and under the lease elsewhere, so one process writes at a time
and a second writing process gets `EROOTLOCKED`. Opening to write gives back
the extents a dropped collection still owns; claiming a collection settles
what a stopped writer left in it. `NativeRoot::close_database` forgets the
process's open, its media and its cached tables and index, checkpointing a
writer's map first, so the next open reads the file as a new process would;
handles opened before keep the old open, and its right to write, until they
drop. A database is created with `NativeRoot::create_database`
(`ContainerFile::create`, 8 MiB extents). The CLI creates a missing
directory root before it runs a command; a path ending in `.fxdb`, or naming
a regular file, is never created as a directory (`NativeRoot::names_database`).

**Collection catalog.** A collection is its directory block. The block's
static part holds its name, its id and the catalog descriptor that
`.fylo-catalog/collections/<name>.json` holds for a directory root
(`kind`, `shardWidth`, `layout: "segments"`, `versioned`), plus a `created`
name, as a hybrid collection's descriptor has, so a collection created
again, or a file created again at the same path, never shares a cached
table with the one before; the file needs no catalog of its own. Adding a
collection checkpoints the map, so other processes find it. A dropped
collection's block is rewritten with the dropped flag and every extent the
collection owns is released and checkpointed; its block is never reused, so
a block number names one collection for the life of the file. A new
collection of the same name takes the next block. A collection in a
database has no directory: its `path` is `<file>/.collections/<name>`, which
only keys in-process caches and queues, and is never opened.

**Collection write locks.** A collection's write lock is its claim ("Locks
and write access"), in place of `locks/collection.lock`: the collection's
medium, which holds the writer byte of its directory block where writers
share the file, kept for the length of the write.

**Prefix index.** A container collection keeps its prefix index in the file,
as it does its locations, and writes no `index/` files. The hybrid
collection directory's container (`with_segment_container`) does the same.

- The **index snapshot** is an `index` snapshot run named by the checkpoint
  slot, next to the location run. Both reach the slot's position.
- **Deltas** are not written twice. A commit record already lists the index
  keys it removes and adds (`indexRemoved`, `indexAdded`), so the keys past
  the snapshot are replayed from the records between the checkpoint slot's
  position and the published slot's end, skipping what an abort record
  cancelled, exactly as location rows are. A reader caches the replayed lines
  per collection and checkpoint and extends them, as `TableCache` does.
- **Folding.** Every location fold, which a container collection makes at
  every checkpoint, folds the index too: the old snapshot with the replayed
  deltas becomes the new run, recorded in the same checkpoint slot. Replaying
  a delta over a snapshot that already holds it changes nothing, because each
  delta sets a key's presence, so a rebuild may write an index that reaches
  past its slot's position.
- An index run that cannot be read is folded into none: the checkpoint slot
  then names no index, and readers report `ENATIVE_INDEX` until
  `fylo rebuild` writes one from the documents.

**Refused with `ELAYOUTUNSUPPORTED`** (a new error code; ADR 0015's
implementation notes used `ENATIVE_UNSUPPORTED` for the directory layout):
creating a file (bucket) collection, creating a collection with the file
layout, an explicitly versioned collection and every version control
operation that writes or needs the repository (`fylo status` reports
version control as off), moving a collection between layouts or resharding
it, and a root lease on a database path, so the embedded queue, which
holds one, and `fylo exec --exclusive-root` refuse a database. The
`EfsSidecarV1` profile and a separate worktree root have no `.fxdb` form.

**The machine protocol and the CLI** admit a database root without a root
lease: the database is owned through its own file (ADR 0016 section 7), so
a session checks it by opening it, and creates it when it is missing, as it
creates a missing directory root. Every document operation, SQL included,
then runs as it does on a directory root, and `fylo <command> --root
app.fxdb` works the same way.

**Not yet** (they fail with `ENATIVE_UNSUPPORTED` or are left for later):
compaction (it needs a second per-collection lock byte), salvage, setting a
collection's validation mode (a rewrite of its block's static part), an
encrypted database (a root takes no key yet), the root's
own write options (exclusive mode and timeouts are the process defaults),
and `fylo pack` and `fylo unpack`.

## Schema versions

ADR 0017 section 3 in a `.fxdb` file. A schema version is a schema record
(kind 5) appended through the collection's commit path, the same code as in
a collection directory (`commit_segment_schema`), and recovery replays it
the same way.

- The **schemas snapshot** is a `schemas` run (extent kind 7) named by a
  third reference in the checkpoint slot, beside the location and index
  runs. The reference takes slot bytes 172 to 227, which a slot written
  before it left zero, so an older slot names none. A slot that names none
  reads as no snapshot: a writer rebuilds the registry from the records
  (`SegmentSchemas::load`) and its next checkpoint writes the run, and a
  reader takes the collection as having no registry. The empty registry is
  kept as no run, so a collection without schema versions takes no extent
  for it. A run that cannot be read is rebuilt from the records.
- Replacing the snapshot writes a new run and records a checkpoint slot
  that keeps the newest slot's position and its location and index runs,
  then releases the run it replaces; a location fold or an index rebuild
  keeps the schemas run. A plain file's run does not depend on the
  checkpoint that names it, so later slots name the same run; an encrypted
  file seals every run for its checkpoint, so a new checkpoint writes a copy
  of each run it keeps. A run no slot names is released at the next claim
  of the collection. Readers keep the decoded snapshot while the slot's
  length and SHA-256 for the run stay the same.
- Before the first schema record is written, the writer sets
  `READ_ONLY_COMPAT_SCHEMAS` (bit 0) in the superblock through a map
  checkpoint, so a build that predates schema records opens the file
  read-only. This build knows the bit and writes such a file.
- A collection's descriptor is its directory block's static part, which is
  not rewritten, so it names no head (`schemaHead`): the records alone are
  the head, and recovery has no descriptor to bring up to them.
- **Not yet:** the record of the one-time import of a `fylo.schema` folder
  has no place in the file, so reading or writing it is refused with
  `ENATIVE_UNSUPPORTED`. Compaction, which copies the schema records into
  the segment it writes, is not run in a database yet; it already does so
  for the container of a collection directory.
