# An index that is not read whole

Status: proposal, not implemented (2026-11-07).

## Problem

A file-layout collection's prefix index is two files: `keys.snapshot`, every
key sorted one per line, and `keys.wal`, `+`/`-` lines appended by each write.
Several paths read and merge both files whole, into one in-memory set:

- **Compaction** (`LogCheckpoint::fold_index`) merges the whole index and
  rewrites the whole snapshot each time the log passes its threshold (at most
  16 MiB).
- **Unplanned reads**: `scan_index`, `index_snapshot`, `verify_index` and the
  hot-set cache merge the whole index. Planned queries already read only
  their key ranges from the snapshot (`index_snapshot_in_ranges`), but still
  pass over the whole log.
- **Validation**: `IndexSnapshot::from_bytes` checks every key of a snapshot
  it is handed.

The merged index is capped at 256 MiB (`IndexLimit::INDEX_BYTES`), so memory
and latency stay bounded. That is roughly 150,000-200,000 small or
35,000-48,000 medium documents per collection (1.4-1.8 KB and 5.6-7.6 KB of
keys per document). Writes now stop at 240 MiB with `EINDEX_FULL`. Raising the
cap only moves the wall: memory, and the cost of each fold, grow with the
index.

The goal is an index whose reads and writes cost what they touch, not what
the collection holds, while keeping FYLO's rules: documents are the truth and
the index can always be rebuilt from them; writes are synced; no server;
the browser engine (OPFS / File System Access) reads the same files.

## Option A: paged prefix files

Split the sorted keys into pages of a fixed size (for example 64 KiB), each a
file `index/pages/<n>.keys`, with a small `index/pages.json` listing every
page's first key, length and checksum. A reader binary-searches the directory
and reads only the pages its ranges cover. Compaction applies the log page by
page: a page whose keys changed is written as a new file, split when it grows
past the page size, and a new directory is published with one atomic rename.

- **Reads:** cost follows the ranges read. A full scan streams page by page.
- **Writes:** a fold rewrites only the pages the log touched, so write
  amplification follows the log, not the index.
- **Crash safety:** copy-on-write pages plus one directory rename; an old
  page is deleted once no directory names it.
- **Costs:** a new on-disk format the browser engine must learn; many small
  files, which network and sync filesystems handle badly (FYLO already
  refuses those for a live root); the directory itself grows with the index
  (16,000 entries at 1 GiB of keys).

## Option B: range reads and streaming merges on today's files

Keep `keys.snapshot` and `keys.wal` exactly as they are, and make every
reader stream:

- Unplanned reads merge the snapshot and the log as two sorted streams (the
  log sorted once in memory, it is at most 16 MiB) instead of building a set
  of the whole index.
- Compaction writes the merged stream to a new snapshot file and renames it
  into place, holding one buffer, not the index.
- Validation checks each key as it streams past.

- **Reads:** planned reads are already ranged; scans become constant-memory.
- **Writes:** each fold still rewrites the whole snapshot. With a 16 MiB log
  threshold, a 1 GiB index rewrites 1 GiB for every 16 MiB of keys written,
  about 64 times write amplification, and a fold takes seconds.
- **Format:** unchanged, so the browser engine and existing roots keep
  working, and the read cap becomes a memory budget per operation instead of
  a collection size.
- **Costs:** the least code; the fold cost becomes the next limit.

## Option C: a small LSM tree of sorted runs

Turn the log fold into flushes. When `keys.wal` passes its threshold it is
sorted and written as an immutable run, `index/runs/<level>-<n>.keys`, each
with a sparse block index (one key per 64 KiB) at its end and a checksum. A
manifest lists the live runs. Runs of similar size are merged in the
background of the next write (size-tiered), streaming, so each key is
rewritten about `log(index / 16 MiB)` times instead of once per fold. Readers
merge the runs' ranges with the log as sorted streams; removals are
tombstones until the oldest run absorbs them.

- **Reads:** a range read touches each run once (up to about 10 runs at
  1 GiB); bloom filters per run could skip runs for `$eq`.
- **Writes:** amplification grows with the logarithm of the index; folds
  stay small.
- **Crash safety:** immutable runs plus one manifest rename, the same rule
  as today's snapshot replace.
- **Costs:** the most code (manifest, merge policy, tombstones); a new format
  for the browser engine; reads pay a merge across runs.

## Recommendation

Do **B first**, then **C** when collections outgrow it.

B removes the hard wall with no format change: memory no longer grows with
the index, the browser engine and existing roots are untouched, and
`EINDEX_FULL` can move from "the index is too large to read" to an explicit,
configurable size budget. Its remaining cost, rewriting the snapshot on each
fold, is linear and predictable, and stays below a second up to a few
hundred MiB of keys.

C is the right long-term structure: today's snapshot plus log is already a
two-level LSM tree, and C generalises it with the same crash rule (write
immutable files, publish one rename). B's streaming merge and streaming
validation are exactly C's building blocks, so B is not thrown away.

A is not recommended: per-page files multiply small files, which FYLO's own
storage rules avoid, and copy-on-write pages buy little over C's runs for an
append-mostly workload.

## Also affected

- The segments layout on a directory uses the same `index/` files and the
  same limit. A `.vdbx` database keeps the index in its container medium,
  merged the same way; `EINDEX_FULL` does not yet guard it, and the redesign
  should cover it.
- A collection already past 256 MiB (written before `EINDEX_FULL`) cannot be
  read or compacted. B makes it readable again; until then, rebuilding its
  index is not possible either, because the rebuilt snapshot has the same
  size.
