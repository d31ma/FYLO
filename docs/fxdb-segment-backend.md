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
| remove segment *n* | `remove_file` | `release`; reused only after the next map checkpoint |
| adopt a compacted segment | rename a temporary file | the new extents were allocated under their final sequence numbers; adoption is the compaction record |
| read / publish / create slots | `state.slots` | the directory block's generation slot pair |
| read / replace the location snapshot | `durable_replace` of `locations.snapshot` | `write_snapshot` → `record_checkpoint` → `release_snapshot(old)` |
| location log | `locations.wal` | none; see below |
| maximum segment bytes | 1 GiB | the extent's payload capacity (8 MiB − 4 KiB by default) |

**Segment numbers are extent sequence numbers.** A `SegmentLocation`
keeps `(segment, offset, length)` unchanged. Because a sequence number is
never reused, a location into a released and reused extent fails the header
check and the reader retries, which is ADR 0016 section 5's rule without
new fields. The container medium keeps a `sequence → extent index` table,
rebuilt at open from the map and updated on allocate and release.

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

## Taking a new extent (simplification of ADR 0016 section 6)

ADR 0016 has the old extent end with a seal record naming the new one, so
recovery can follow extents allocated after the last map checkpoint. The
first implementation avoids a new record kind:

- the generation slot already names the active extent, so one unmapped
  segment extent per collection is always findable;
- when the segment code rolls to a new extent and the current active extent
  is not yet in the durable map, the medium runs a map checkpoint first.

So at most one segment extent per collection is ever missing from the map,
and it is the one the slot names. Compaction allocates its output extents,
then checkpoints the map before it writes the compaction record.

The cost is a map write per rotation when no checkpoint happened since the
last one. The map is 32 bytes per extent, so with 8 MiB extents the extra
bytes per rotation are `file size / 2 TiB` of the rotated data: 0.5% at
10 GiB, 5% at 100 GiB. Seal records replace this when files that large are
in use.

## Locks

The collection write lock becomes a byte-range lock on the collection's
lock byte in the `.fxdb` file (or process-local under the lease, which
`set_process_local` already arranges). The allocation lock guards
`allocate`, `release` and map checkpoints. Readers take no lock, as today.

## Encryption and codecs

Record frames gain ADR 0016 section 4's codec byte and, in an encrypted
file, a nonce and tag sealed with `ContainerSealContext::Record`. Both
are behind the frame encode/decode the medium owns, so the directory medium
keeps today's frames byte for byte.

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
