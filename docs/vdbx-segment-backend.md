# Running segment collections inside a `.vdbx` file

Status: design for implementation, 2026-10-29. Applies ADR 0015 (segments)
to ADR 0016 (the `.vdbx` container). Where this note and ADR 0016 differ, this
note records the simpler first implementation and ADR 0016 keeps the end
state.

## Goal

The segment code (`segments/` and `write/commit_log/segment_*.rs`) works on a
collection directory: numbered `.seg` files, `state.slots`,
`locations.wal` and `locations.snapshot`. A `.vdbx` collection keeps the same
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
`locations.wal`, and readers lay the log over the snapshot. In `.vdbx` the
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

A checkpoint folds its location and index runs under a slot published
`writing`. One that fails in this process, as on a full disk, rolls back
before it returns: a checkpoint record it appended is cut off, a next
extent it took is given back and the segment it sealed for it unsealed,
and the slot is published stable again where the checkpoint started, so
nothing is left for recovery. Runs it already wrote are derived from the
records up to there; a checkpoint slot names them only once they are whole,
and the next claim releases those none names.

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

**One open per file.** A process keeps one `ContainerOpen` per `.vdbx`
file, shared by every medium over it: the one collection of a hybrid
collection directory's `segments.vdbx` (`with_segment_container`), and every
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

**A write a writer left.** A reader that finds a collection's slot
`writing` in a database root (`NativeCollection::generation`) asks who is
behind it, without waiting (`ContainerOpen::slot_writer`): a writer holds
the collection's writer byte for each write, and a lease writer or a
shrink the lease byte exclusive, and the OS lets both go when their process
stops. When one is held the writer is running and publishes in a moment, so
the reader is told the collection is being written and retries, as in a
directory root. When neither is, the writer stopped, and the reader
finishes its write as the next writer would, by recovering the collection,
as a directory root's reader recovers a stranded write; it does so only
where that keeps no writer out and waits for none: it writes the file
already, or would share it under byte-range locks and no lease a stopped
writer left needs taking over. Otherwise (under the lease, without locks,
on a file it may not write, or when the recovery fails, as on a full disk)
it reads the collection as the slot last published it. That is whole: the
records up to the position a `writing` slot names stay as they are, a
checkpoint slot names new snapshot runs only once they reach that
position, and a compaction releases the segments it replaced only after a
slot names its record. The next writer of the collection settles the
write. A reader that finished a write keeps the file open to write, as a
writer does, until its open closes.

**Writers.** The first write through an open reopens the file with
`ContainerFile::open_writer` (ADR 0016 section 7). Where byte-range locks
are qualified and granted, the writer holds the lease byte shared beside
writers in other processes; elsewhere, or in exclusive mode, the
single-writer lease lets one process write and every other writer fails
with `EBUSY` (`EROOTLOCKED`) after its busy timeout, while readers carry
on. The writer checks its right (`assert_writable`) before every write
through the medium and renews a lease from those same calls once a third of
its takeover time has passed; one that lost its lease fails with
`EVDBX_LEASE_LOST` (`EROOTLEASELOST`), and its open goes back to reading,
so the next write opens the file and claims its collections afresh.

**Claiming a collection.** A medium's first write claims its collection.
Where writers share the file, the claim takes the collection's writer byte
(`LockByte::collection_writer` of its directory block) exclusive, waiting up
to the busy timeout, and holds it until the last write through the medium in
this process ends, so the bytes this process writes stay its own until it
publishes them; under the lease the process is the only writer and takes
none, and keeps its claims while the medium is open. The claim then settles what a
writer that stopped left in the collection, with the map read afresh: an
active segment extent its slot names but the map does not show is taken
back, and `locations` and `index` runs no checkpoint slot names are
released. Writers of different collections run side by side; those of one
collection take turns, write by write.

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

## Compaction and shrinking

`fylo compact db.vdbx [--shrink]` (`NativeWriteRoot::compact_database`)
compacts every collection of the database, one after another, as ADR 0015
section 7 compacts a collection directory's segments; `fylo compact
<collection> --root db.vdbx` and the machine's `compactCollection` compact
one.

**The compactor byte.** A second lock byte per collection,
`LockByte::collection_compactor` of its directory block (2³² past the
writer bytes), keeps one compaction of a collection at a time across
processes and threads; it replaces the directory root's `compaction.lock`.
A compaction takes it, waiting up to the busy timeout (`EBUSY` as
`EROOTLOCKED` after it), and holds it from preparation to adoption. It
takes the collection's writer byte only to prepare (seal the active segment,
choose the segments, retrain the dictionary, take the copy's extent) and to
adopt (append the compaction record, release what it replaces); the copy
runs between, holding neither, so writers of the collection, in this
process or another, commit while it copies, and readers never wait.
Recovery leaves the segment extents nothing names alone while another
holder has the compactor byte, since one may be that compaction's copy,
and dropping a collection takes the byte first. Under the lease the
process is the only writer, and the byte is decided inside it.

**Shrinking** runs after the compactions, with the file held as its only
writer: where writers share the file, the lease byte goes from shared to
exclusive, waiting up to the busy timeout for the other writer processes to
close the file (`EBUSY` if they do not), and back to shared after; under
the lease this process is the only writer already. Readers take no lock, so
they read on throughout: a location into a moved extent fails the header
check and the reader retries with the newer map, as after a compaction. The
shrink then repeats, after a map checkpoint that makes the extents released
so far free:

- the last extent in use, when a free extent lies before it, moves into the
  lowest free one, each move committed as it would be on its own: a
  segment by a compaction of that segment alone (the active one is sealed
  first, so a new active extent is taken), a `locations`, `index` or
  `schemas` run by a copy that a new checkpoint slot names while carrying
  the other runs (resealed for the new checkpoint in an encrypted file), a
  dictionary by a copy under the same id, which the slot names when it
  named the original, and the map by the next checkpoint;
- it stops when no free extent lies before the last in use, a move would not
  free anything (the directory, a dropped collection), or three moves in a
  row leave the end where it was.

Then the free extents at the end are cut off (`ContainerFile::shrink_free_tail`,
which checkpoints a map that no longer shows them before the file is cut),
and holes are punched over the free extents left. The map is written copy
on write, so a shrunk file keeps one free extent where the previous map
was; it is punched where the filesystem punches holes. Support is probed
by the punch itself on each shrink, not at open as ADR 0016 section 6 has
it. A stop after any move leaves a whole database: a copy no slot names is
released by the next claim of its collection (a run) or the next
compaction (a segment or a dictionary), and a stop before the cut leaves a
longer file, which opens as before. The failpoints `after-shrink-relocate`
and `after-shrink-truncate` stop between the steps.

In an encrypted database the compaction re-encodes each copied record with
its own LSN, so the copy is sealed and bound exactly as a fresh write of
that record, and the shrink's copies are sealed for their new checkpoint.
A handle opened without the key is refused with `EKEY` before anything is
locked or written.

**Salvage.** `fylo salvage db.vdbx --out <root>`
(`NativeWriteRoot::salvage_database`) writes every readable document and
schema version of every collection as a new directory root in the file
layout, as `fylo unpack` does, and changes nothing in the database. It
steps over a damaged record by its sync marker and a segment extent whose
header no longer reads by that header, reporting each as a damaged range
and the documents they held as lost; a collection's registry keeps the
schema versions up to the first one missing. The superblock, the map and
the directory must still read: a file whose structures are damaged is not
opened, and scanning its extents without them is left for later. An
encrypted database needs its key and is salvaged as a plain root.

## Encryption and codecs

Record frames gain ADR 0016 section 4's codec byte and, in an encrypted
file, an LSN, a nonce and a tag sealed with `ContainerSealContext::Record`.
Both are behind the frame encode/decode the medium owns, so the directory medium
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
  (`ENATIVE_UNSUPPORTED` with `EVDBX_VERSION` in the message), never taken for
  a torn tail: a scan that stops there fails rather than let recovery cut the
  segment.
- A damaged frame still names its documents to salvage when its stored body
  decompresses, as when only its checksum was hit.

**Dictionaries (landed).** `segments/container_medium/dictionaries.rs` and
`container_file/dictionary.rs`, ADR 0016 section 8:

- A collection writes without a dictionary until its records reach 1 MiB.
  The first location checkpoint that finds them trains a 32 KiB zstd
  dictionary on those records (natively only: the trainer is the C
  library's), writes it to a `dictionary` extent and names it in the
  checkpoint slot (extent, id, stored length). Records written after that
  checkpoint use it: zstd natively, LZ4 with the same bytes as its prefix in
  the browser. A collection whose records cannot train one is not retried
  until the writer reopens.
- `fylo pack` trains each collection's dictionary the same way, on the
  first 1 MiB of its documents in identifier order, but before it writes
  them, so the packed records use it from the start rather than wait for a
  compaction.
- A `dictionary` extent holds a 32-byte header (`VDXC`, the id, whether the
  bytes are sealed, their length and CRC-32C) and the dictionary, sealed in
  an encrypted file with `ContainerSealContext::Dictionary` for its
  collection and id. It never changes, so a checkpoint that keeps naming it
  carries the slot field and needs no copy.
- Every record keeps the dictionary it was written with until compaction
  rewrites it, so a reader loads whichever dictionary a frame names from the
  collection's `dictionary` extents the first time it meets it. A frame
  naming a dictionary the file does not hold, or holds damaged, fails as
  `EVDBX_CORRUPT` and is never taken for a torn tail: recovery refuses the
  collection rather than cut records off.
- Compaction trains a candidate on the newest 1 MiB of records and keeps it
  only when it stores them at least 10% smaller than the current one. A new
  dictionary is named by its own checkpoint before the active segment is
  sealed, so the copy and every later record use it. After the compacted
  segments are removed, every dictionary that is neither current nor named
  by a remaining record is released; when any segment cannot be read to its
  end, none is.
- A crash between writing a dictionary and the slot naming it leaves one no
  record names. It is harmless: the next dictionary takes a higher id,
  `fylo verify` warns of it (`dictionaryUnused`), and the next compaction
  releases it.
- `fylo verify` checks every `dictionary` extent's bytes against its header,
  the dictionary each slot names, and that every frame's dictionary id
  resolves (`dictionaryDamaged`, `dictionaryMismatch`, `dictionaryMissing`),
  all without the key. `fylo inspect` reports each collection's current
  dictionary and how many it holds.

**Codec feature bits (landed).** A writer's first claim of a collection sets
the superblock's `INCOMPAT_CODEC_ZSTD` (natively) or `INCOMPAT_CODEC_LZ4`
(the browser) through a map checkpoint under the allocation byte, the way
schema versions set theirs, and `fylo inspect` lists them under `codecs`.

**Sealed records (landed).** ADR 0016 section 9. Encryption is opt-in per
database and off by default: `fylo create db.vdbx --encrypt`, `fylo pack
--encrypt`, or `fylo.encryption.file: true` in `application.json` creates an
encrypted file, keyed by `fylo.encryption.key` (see
[configuration](configuration.md#encrypted-databases)). Every other database
is plain, and its frames are as above.

- A sealed frame's stored body is the header byte with `0x20` set, the
  dictionary id when one was used, the record's 8-byte LSN in the clear,
  then a random 96-bit nonce, the compressed body sealed with AES-256-GCM
  and the 16-byte tag: compress, then encrypt. The key is HKDF-SHA256 of
  the secret and the superblock's salt, checked against its key-check value
  when the file opens.
- The associated data is `ContainerSealContext::Record`: the collection's
  name and the record's LSN (one frame holds one whole record, so its entry
  is 0). A frame moved into another collection, or copied over another
  record, does not open; it fails as `EVDBX_CORRUPT` and is never taken for
  a torn tail. Replay also requires each record's LSN to follow the one
  before it, across incremental replays, and a point read checks the LSN its
  location names, so a valid older frame copied over a newer one is refused
  rather than served.
- The CRC-32C covers the stored, sealed bytes, so `fylo verify` and salvage
  check frames without the key. Only the key tells a frame moved whole from
  its place.
- `segments/container_frames.rs` seals and opens frames with the cipher the
  process-wide open holds; a writer's upgrade and a reader's reopen after a
  lease loss unlock their file with it.
- Opened without the key, a database is **locked**: it lists no
  collections, and a read of one and every write fail with `EKEY`,
  leaving the file untouched. A wrong key fails with `EKEY` when the root
  opens. `fylo inspect` reports `encrypted` and `locked`, and lists the
  collections only with the key.
- The generation slots and the superblock stay in the clear (ADR 0016
  section 9), so a copy of the whole file put back in place of a newer one
  is not detected.

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
5. **Root integration**: a root path ending in `.vdbx` opens a container;
   `fylo pack` and `fylo unpack` convert between a directory root and a
   file ("Packing and unpacking").

## Root integration

Step 5's first half: ADR 0016 section 11's rule that a root path naming an
existing regular file is opened as a container, for document collections in
the segment layout. `NativeRoot`, `NativeWriteRoot` and the engines take a
`.vdbx` path wherever they take a root directory. Nothing is created beside
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
directory root before it runs a command; a path ending in `.vdbx`, or naming
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
`EfsSidecarV1` profile and a separate worktree root have no `.vdbx` form.

**The machine protocol and the CLI** admit a database root without a root
lease: the database is owned through its own file (ADR 0016 section 7), so
a session checks it by opening it, and creates it when it is missing, as it
creates a missing directory root. Every document operation, SQL included,
then runs as it does on a directory root, and `fylo <command> --root
app.vdbx` works the same way.

**Not yet** (they fail with `ENATIVE_UNSUPPORTED` or are left for later):
setting a collection's validation mode (a rewrite of its block's static
part), the root's own write options (exclusive mode and timeouts are the
process defaults), and sealing a database in place (`fylo pack --sealed`
needs `--out`). Compaction, shrinking and salvage are in "Compaction and
shrinking"; sealed files are in "Sealed packs".

## Packing and unpacking

ADR 0016 section 11, as `NativeWriteRoot::pack_database` and
`unpack_database` and the CLI's `fylo pack` and `fylo unpack`. Like
`fylo export`, they take an output path and so are not machine protocol
operations.

**`fylo pack <root> --out db.vdbx`** copies every document collection of
a directory root, in the file or the segment layout, into a new database.

- It refuses with `ELAYOUTUNSUPPORTED`, naming the feature, a root that
  uses buckets (any entry in `.buckets/`), document version control
  (`.fylo-vcs/`) or the embedded queue (any entry in `.fylo-queue/`), and a
  root on the EFS sidecar profile. It refuses an existing `--out` with
  `ENATIVE_UNSUPPORTED`, and a database root without `--sealed`: a plain
  pack takes a directory.
- It takes every collection's lock, in name order, and recovers each, as
  `fylo export` does, and holds them all until the database has its name,
  so nothing writes the source meanwhile. A collection created during a
  pack is not in it.
- It builds the database in a staging file beside `--out`,
  `.<name>.pack-staging`, first removing one a stopped pack left and the
  lease files beside it. Each collection becomes a directory block whose
  descriptor keeps the source's shard width and validation mode. Its
  schema versions are appended first, as schema records in ordinal order
  (setting `READ_ONLY_COMPAT_SCHEMAS` first), then each document as a commit
  record of its own in identifier order, with its body, attributes, mode
  and modification time: the records a layout migration to segments writes
  (`SegmentImport`). When the next record does not fit in the active
  extent, the extent is sealed with the bytes it holds and the next opens
  with a checkpoint record, as a checkpoint's rotation does. A closing
  checkpoint then folds the location, index and schemas snapshots from the
  records.
- The oracle is ADR 0015's: every collection of the staging file has the
  same live and deleted identifiers as the source, the same bytes,
  attributes, mode and modification time for every document, and the same
  schema registry. Records naming another owner need
  `--allow-engine-only-access`, as `fylo layout` does.
- The commit: the staging file is closed, flushed, and passes the
  `after-pack-staging` failpoint; then it is linked to `--out` (a link
  fails on an existing name, so a file that appeared meanwhile is never
  replaced; where links are missing, a rename after a check), the staging
  name is removed and the directory flushed. A stop at any point leaves no
  `--out`, and at most the staging file, which nothing reads and the next
  pack to the same name removes. A failure the process survives removes it
  at once.

**`fylo unpack db.vdbx --out <root>`** writes a new directory root in the
file layout into an absent or empty `--out`. For each collection, under its
lock: the catalog descriptor (the source's shard width and validation
mode), the tree `fylo export` writes (`docs/`, `.deleted/`, `index/`, with
each document's attributes, mode, owner and modification time), and, for a
collection with schema versions, the registry files, the fenced generation
state and the descriptor's head, in a registry commit's order. The written
files are checked against the documents with the same oracle. An unpack that
fails removes what it wrote; one that stops leaves a partial root, which a
second unpack refuses until it is removed.

**`fylo pack <root|db.vdbx> --out sealed.vdbx --sealed [--encrypt]`**
writes a sealed file: a read-only image of the database, for distribution
or backup (ADR 0016 sections 8 and 11). See "Sealed packs".

**Not carried:** the record of the one-time `fylo.schema` import, which a
database has no place for ("Schema versions"); the earlier widths of an
unfinished reshard, since every document is read wherever it is and
written at the collection's current width; and record ownership, which a
database does not give records (ADR 0016 "Costs").

## Sealed packs

ADR 0016 sections 8 and 11, as `NativeWriteRoot::pack_sealed_database`,
`container_file/sealing.rs` (writing) and `container_file/sealed_image.rs`
(reading).

- **Source.** A directory root, packed as above, or a live `.vdbx`
  database, so a sealed pack is also a consistent backup: each collection
  is locked and recovered as `fylo export` does, and its documents and
  schema versions are read through the engine. Either way the records go
  to a plain staging file, `.<name>.pack-staging`, stored uncompressed and
  with no dictionary trained, so the blocks compress them whole.
- **Renumbering.** The staging container's in-use extents keep their order
  and get new numbers, its free extents and its map are dropped, and a new
  map follows the last of them, so the image has no free extent. Each
  extent header's number and every slot's references follow; the newest
  slot of each pair becomes the first and the second is zeroed.
- **Layout.** Superblocks A and B stay in the clear at 0 and 4 KiB and carry
  `INCOMPAT_SEALED` (with `INCOMPAT_ENCRYPTED`, its salt and key check, in
  an encrypted file). `ContainerSealedHeader` is at 8 KiB: the magic
  `VDXSEALD`, the image's length, the block count and where the block index
  is and its CRC-32C. Blocks start at 12 KiB. Each covers 64 KiB of the
  image and stores a codec byte and the block at zstd level 19, or the
  block as is when that is smaller; a block of zeros stores nothing. The
  index after the last block gives each block's offset, length and CRC-32C,
  so a point read opens the one block that holds its record. The image's
  first 8 KiB, the superblocks, are read from the file itself.
- **Encryption.** Under `--encrypt`, or when the source database is
  encrypted (its key, `fylo.encryption.key`, then also seals the output),
  each block is sealed with AES-256-GCM, its number the associated data
  (`ContainerSealContext::Block`), so a block moved to another place does
  not open. The records inside are plain: the blocks carry the seal.
  Without the key the file is locked as any encrypted database is.
- **Check.** Before it has its name, the sealed staging file,
  `.<name>.seal-staging`, is opened as a reader and checked with the oracle
  against the plain staging file; then the plain one is removed and the
  commit is a pack's.
- **Reading.** Every read path opens it: the engine, the machine protocol,
  `get`, `find`, `sql`, `inspect` (which reports `blocks`), `verify` and
  `unpack`, and the browser engine. A few decoded blocks are cached, since a
  scan reads a block's records in turn.
- **Writing.** Every write is refused before any lease is taken, with
  `EROFS` (`ENATIVE_UNSUPPORTED` through the machine protocol): an open for
  writing, a document or SQL write, creating or dropping a collection,
  compaction, shrinking and rebuild. `fylo unpack`, or a pack without
  `--sealed`, gives a database that takes writes.
- **Verify.** `fylo verify` checks every block's CRC-32C without the key and,
  with it, that each opens and decompresses to its 64 KiB; a bad block is
  `sealedBlockDamaged` at the extent it falls in, and the rest of the check
  then stops. A read of a damaged block fails with `EVDBX_CORRUPT`.
- **Not yet:** sealing a database in place (the database's exclusive lease
  byte, then the image written over it); `--out` is required. Sealing runs
  natively only: the browser reads a sealed file but cannot write one.

## Schema versions

ADR 0017 section 3 in a `.vdbx` file. A schema version is a schema record
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
  `ENATIVE_UNSUPPORTED`.
- Compaction copies the schema records into the segment it writes, as it
  does for the container of a collection directory ("Compaction and
  shrinking").
