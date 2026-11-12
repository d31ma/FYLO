# ADR 0027: Segment layout ids

- Status: **Accepted** (accepted by the user, 2026-11-12)
- Date: **2026-11-12**
- Owners: **FYLO maintainers**
- Amends: ADR 0015 section 2 (`state.slots` grows from 128 to 144 bytes) and
  section 6 (a reader's cached location table is keyed by layout id too).

## Context

A process keeps the location table of each segment collection it reads in a
process-wide cache (`TableCache`). The key was the collection's directory
path and the slot's table epoch. A collection dropped and created again at
the same path, or moved to segments again, is a new layout at the same path.
If its epoch equals the one a reader cached, the reader takes the old table
for the new layout. FYLO's entry check then fails the read with
`segment entry does not match its location`: no wrong data, but a failed
read.

Commits `f869c2c` and `769f6f7` made this rare: a new layout starts at a
random 32-bit epoch, and a drop forgets the dropping process's own table. In
another process the risk stayed 1 in 2^32 per re-creation. FYLO prefers
correctness by construction over chance.

The `.vdbx` media are not affected: their cache key already holds a digest
of the collection's descriptor, which is new for each new collection.

## Decision

### 1. Each directory layout has a layout id

- When a directory segment layout is created (a new collection, or a move
  to segments), FYLO gives it a **layout id**: the next TTID clock tick of the
  creating process (100-nanosecond units since the Unix epoch, above every
  tick that process drew before, the same sequence TTIDs use).
- The id lives in `segments/state.slots`, in a 16-byte block after the two
  64-byte slots, at bytes 128 to 143:

  | Bytes  | Field                                  |
  | ------ | -------------------------------------- |
  | 0..4   | magic `FYLI`                           |
  | 4..12  | layout id, unsigned 64-bit, little end |
  | 12..16 | CRC-32C of bytes 0..12                 |

- The block is written once, in the same durable write that creates the
  slot file. A publication writes only its slot, so the id never changes for
  the life of the layout.

### 2. The cache key holds the layout id

- A reader reads both slots and the block in one `pread`, as before (144
  bytes instead of 128).
- A cached table is used only when the medium identity, the layout id and
  the table epoch all match.

### 3. Why this is unique by construction

Two layouts at one path exist one after the other: a new layout is created
only where no slot file exists, so the old layout's slot file was written
durably, and later removed by a drop or a move, before the new one is
created. The new layout's id is a clock tick drawn after
that, so it is later than the old layout's id. Ids at one path therefore
only grow, and a reader's cached id can never belong to a newer layout.

This holds while the wall clock does not step back across a drop and a
re-creation. FYLO's TTIDs rely on the same clock. If the clock does step
back, the new id must still land on exactly the old 100-nanosecond tick (1
millisecond in a browser), and the random table epoch must match too.

### 4. Collections written before layout ids

- An older `state.slots` file ends after the two slots. Its layout id reads
  as **unknown**, and FYLO reads and writes the collection as before. FYLO
  does not add the block to an existing file.
- A block that is missing, short, or fails its CRC reads as unknown.
- An unknown id is never equal to a real one, so a layout created now never
  takes a table cached for an older layout. Two layouts with unknown ids can
  only come from an older release; the random epoch still guards them.
- Older releases read only the first 128 bytes and ignore the block. A
  publication by an older release writes only its slot, so it keeps the
  block.

### 5. What stays the same

- The random starting epoch of `769f6f7` stays, as a second guard.
- `drop_collection` still forgets the dropping process's own table.
- `.vdbx` databases and the test container media keep their keys; their
  identity already names the collection's creation. Their format does not
  change.

## Consequences

- The slot file is 16 bytes longer, and a reader reads 16 more bytes in the
  same call. No new file and no new system call.
- A cross-process test forces the old epoch collision (it rewrites the new
  layout's slots to the old epoch) and shows the reader now loads the new
  table: `tests/segment_recreate_cache.rs`.
