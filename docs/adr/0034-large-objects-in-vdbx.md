# ADR 0034: Large objects in `.vdbx` (images, videos and other files)

- Status: **Accepted, not yet built** (2026-11-21). The user chose to design
  it now (N7: "Design it now"), through the hub. Slices L1-9a (native) and
  L1-9b (WebAssembly, mobile, encryption, pack and unpack) build it. It ships
  in the layout release of ADR 0033, **after the next BYCKR release** (the S5
  release).
- Release changed (2026-11-21): the user decided "there should be no later BYCKR release"; this ADR now ships in **the next BYCKR release**, together with ADR 0033 and [ADR 0035](0035-fylos-own-sdk-in-the-store-shape.md). Where this ADR says "after the next BYCKR release", read "in the next BYCKR release".
- Date: **2026-11-21**
- Owners: **BYCKR maintainers**
- Extends: **ADR 0016** (single-file `.vdbx`). Section 1 of ADR 0016 left
  "buckets, which need large-object extents that span more than one extent"
  to a follow-up ADR. This is that ADR.
- Related: ADR 0015 (records, compaction), ADR 0021 (protected files), ADR
  0022 (IndexedDB backend), ADR 0025 (browser encryption with a host key),
  ADR 0026 (mobile builds), ADR 0033 (files beside documents; one `.vdbx` per
  namespace).

## Context

ADR 0033 puts files (images, videos, any bytes) in the same collection as
documents, as `<TTID>.<ext>` files. A namespace can be a folder or one `.vdbx`
file (ADR 0033 section 10). Today a `.vdbx` holds documents only: an extent
holds records of one collection, records are small (a document request is at
most 1 MiB), and `fylo pack` refuses buckets with `ELAYOUTUNSUPPORTED`.

The user's answer to N7, quoted exactly: "Design it now". So a namespace with
files must be able to live in one `.vdbx`, in the browser and on phones too.

Facts that decide the design:

- **Extents** are fixed-size per file, 1 MiB to 64 MiB, 2 MiB by default.
  Each begins with a 64-byte header (magic, kind, owning collection id,
  sequence number); the rest of its first 4 KiB page is zero, so the
  extent's payload starts at byte 4,096. Free extents are reused lowest number first, only after
  a checkpoint has written a map that shows them free (ADR 0016 section 6).
- **A video can be hundreds of MiB.** It must never sit whole in memory: not
  in the native engine, and not in the WebAssembly engine, whose memory is
  small.
- **A player seeks.** Video in a browser or a phone app reads byte ranges.
- **Protected files are encrypted** with AES-256-GCM, a fresh random nonce
  per sealed unit (ADR 0016 section 9). A range read must decrypt only the
  bytes it needs.
- **The bucket source limit is 512 MiB** today (README, "Raw Files").

## Decision

### 1. An object is a record plus a run of extents

- A file stored in a `.vdbx` is an **object**. Its bytes live in **object
  extents** (a new extent kind in the extent header). An object extent
  belongs to exactly one object.
- The object's **record** is an ordinary record in its collection's active
  extent (ADR 0016 section 4), with the file's TTID and metadata (name, key,
  content type, custom `meta`, as for a file on disk) plus:
  - `length`: the object's size in bytes;
  - `sha256`: of the object's plain bytes;
  - `chunkSize`: see section 2;
  - `runs`: a list of extent runs, each `[first extent number, extent
    count]`, in object order;
  - `salt`: 32 random bytes, only in an encrypted file (section 4).
- **Runs.** The writer takes free extents lowest number first, as today, and
  prefers contiguous ones, so most objects have one run. A list holds at
  most 4,096 runs; an object that would need more is refused with
  `ELAYOUTUNSUPPORTED` and a message that names `fylo compact --shrink`.

### 2. Chunks of about 1 MiB

- An object is written as **chunks**. A chunk frame is a small header
  (object TTID hash, chunk index, payload length, CRC-32C of what follows)
  and the payload (ciphertext and a 16-byte tag when the file is
  encrypted).
- **Chunk size is fixed per object**, chosen so that a whole number of
  frames fill an extent's payload, which starts at byte 4,096: with an
  extent size `E`, `k = E / 1 MiB` frames per extent (at least 1), each in a
  slot of `floor((E - 4096) / k)` bytes, and a chunk payload of that slot
  minus the 32-byte frame header (and the 16-byte tag when encrypted). So a
  chunk holds just under 1 MiB (two chunks per default 2 MiB extent). The
  last chunk is shorter.
- So **chunk `i` is found by arithmetic**: run position `floor(i / k)`, slot
  `i mod k`. No chunk table is stored, and a range read needs no extra read.
- A frame never crosses an extent boundary. Object chunks are **not
  compressed**: images and videos are already compressed, and a fixed size
  keeps the arithmetic simple.

### 3. Streaming writes and streaming, range reads

- **Writes stream.** `putFile` (and `putData` with a file) reads its source
  one chunk at a time, writes each frame, and computes the SHA-256 on the
  way. Memory holds about two chunks, whatever the object's size.
- **Reads stream.** `getFileData` returns the object chunk by chunk, with
  positioned reads (`pread`, or the host's `read_at`). Each chunk's CRC is
  checked (and its tag, when encrypted). A whole read also checks the
  object's SHA-256 at the end.
- **Range reads.** `getFileData` gains an optional byte range (`start`,
  `end`). BYCKR reads only the chunks that cover it. This is what a video
  player's seek needs. A range read checks each chunk it reads, not the whole
  object's SHA-256. How bytes cross the machine protocol does not change.

### 4. Encryption per chunk (protected and encrypted files)

- Each object gets its own **object key**: HKDF-SHA256 of the file key, with
  the object's random 32-byte `salt` and the label `fylo.vdbx.object.v1`.
- Each chunk is sealed with **AES-256-GCM** under the object key. The
  **nonce is the chunk number** (96 bits, big-endian).
- The **associated data** binds the chunk to the collection id, the object
  TTID, the chunk number, the chunk count, and a "last chunk" flag. A chunk
  cannot be moved to another object or position, and an object cannot be cut
  short, without the tag check failing.
- **Why this keeps ADR 0016 section 9's rule.** ADR 0016 forbids nonces
  derived from the log position, because compaction and packing rewrite the
  same position with other bytes under the same key. Here the key is new for
  every object write (a fresh salt), and an object's chunks are never
  encrypted twice under one object key: compaction copies the sealed frames
  unchanged (section 6), and `fylo pack` and `fylo rekey` give every object a
  new salt. So a (key, nonce) pair is used once.
- **A key is never used twice with the same nonce.** Overwriting, appending
  to or compacting a large object gives it a new key (a new salt) or a new
  object id; BYCKR never encrypts a chunk number again under the old key.
- A range read decrypts only the chunks it reads.
- The browser engine with a host key (ADR 0025) and protected files (ADR
  0021) use the same frames. A policy that may not read the collection may
  not read its objects.

### 5. Crash order: chunks first, record last

1. Under the database's allocation lock, take the object's extents and write
   their headers (kind `object`, a new sequence number).
2. Write every chunk frame. Flush the file.
3. Under the collection's write lock, append the object's record to the
   collection's active extent and flush. **This is the commit point**, as for
   every record (ADR 0016 section 6).

A crash before step 3 leaves extents that no record names:

- extents taken after the last checkpoint are free in the checkpointed map,
  so the next open simply reuses them;
- extents that a checkpoint recorded as used while the write was still
  running are **orphans**. `fylo verify` reports them, and `fylo compact`
  returns them to the free list.

A reader never sees a half-written object: it finds the object only through
its record.

### 6. Deleting, replacing and compacting

- **Delete** appends a delete record, as for a document. The object's
  extents become free **at the next checkpoint**, and are reused only after
  it (ADR 0016's rule). A reader that still holds the object's runs sees a
  changed extent sequence number and gets "not found", as for a moved
  generation.
- **Replace** writes a new object and record, then frees the old object's
  extents the same way. Soft-deleted files keep their extents until they are
  purged.
- **`fylo compact --shrink`** moves live object extents nearer the front of
  the file: it copies the sealed chunk frames **unchanged** into lower free
  extents, appends a relocation record with the new runs, and frees the old
  extents at the next checkpoint. The associated data does not name an
  extent position, so the copied frames stay valid. Hole punching of free
  extents works as in ADR 0016.
- **`fylo pack --sealed`** copies object extents as they are; it does not
  block-compress them (section 2), so range reads stay direct in a sealed
  file.

### 7. The size limit

**512 MiB per object**, the same as the bucket source limit today. A larger
object is refused with today's limit error before anything is written. The
limit is a constant of the format version, not a setting; raising it later is
its own decision.

### 8. Browser and mobile engines

- The same Rust code runs in `fylo.wasm`, `fylo-browser.wasm` and the mobile
  builds, through the `fylo-vfs` seam. It needs only the six host operations
  ADR 0016 section 10 lists (`open`, `stat`, `read_at`, `write_at`,
  `truncate`, `flush`), so the host ABI does not change.
- Memory stays bounded: one chunk buffer for a read, about two for a write.
- Per host: an OPFS working copy reads and writes in place; a picked file is
  read with `File.slice` per chunk (read-only); a hosted sealed file uses a
  ranged request per chunk; the IndexedDB backend (ADR 0022) stores object
  extents as pages like any other; iOS and Android use the app's file handle.
- So the browser and phones gain files in `.vdbx`. Today they refuse
  buckets.

### 9. Pack and unpack

- `fylo pack` accepts a namespace with files: each `<TTID>.<ext>` file and
  its metadata becomes an object. The pack oracle compares every object's
  SHA-256 and metadata.
- `fylo unpack` writes each object back as `<TTID>.<ext>` (or `<TTID>.bin`,
  ADR 0033 section 7), byte for byte, with its metadata in extended
  attributes or sidecar files, as the target's recorded probe says (ADR 0033
  section 6).
- `fylo salvage` steps over damaged chunks by the frame's CRC and the extent
  header, and writes the objects it can read whole.

### 10. Compatibility

- A file gains a new **incompatible feature flag** in its superblock,
  `large-objects`, when its first object is written. An older binary refuses
  such a file with `EFORMATUNSUPPORTED` (ADR 0016 section 12) before it
  writes. A file with no objects stays readable by older binaries.
- `fylo inspect` reports object extents as their own kind, with their count
  and bytes, and orphan extents if any.
- The storage contract `v2` (ADR 0033 section 13) describes the object
  record fields. This ADR adds no machine operation; `getFileData` gains the
  optional range (additive, machine protocol stays `v1`).

### 11. Decisions made while building (L1-9a)

Accepted by the BYCKR Build Coordinator on 2026-11-21.

- **A `stamp` in the record.** The object record also carries `stamp`: the
  sequence number of the first extent the write took. Sequence numbers are
  never reused, so the stamp names this one write. The frame header's object
  hash is SHA-256 over a label, the collection id, the stamp and the TTID.
  A reader that still holds the runs of an older write of the same TTID
  gets "not found" when a later write reuses those extents, never the
  later write's bytes. Relocation keeps the stamp, so moved frames stay
  valid.
- **The payload starts at byte 4,096.** Section 2's arithmetic uses the
  real payload offset of ADR 0016's extents (the header takes 64 bytes, the
  rest of the first page is zero). With 2 MiB extents a slot is
  1 MiB - 2 KiB.
- **One run for a new object.** A writer takes the lowest run of free
  extents that holds the whole object, or new extents at the end of the
  file. So a new object has exactly one run; more runs come only from
  `fylo compact --shrink`, which moves an object one extent at a time. The
  4,096-run cap still guards every record.

## Consequences

- A namespace with images and videos can be one file, on a server, in a
  browser and on a phone.
- Video seeking reads only the chunks it needs, encrypted or not.
- Memory use does not grow with object size.
- Object extents waste space at the end of each object (at most one extent
  minus its last chunk). Small files (a few KiB) each take a whole extent;
  for many small files a folder namespace is smaller. A later decision may
  store small files inline in their record.
- Deleted objects keep their space until the next checkpoint, and the file
  shrinks only with `fylo compact --shrink` or hole punching.

## Alternatives considered

- **Objects as ordinary records**, split over several records in the
  collection's extents. No new extent kind, but every object would be mixed
  with document records, compaction would rewrite video bytes, and seeking
  would need a chunk table. Rejected.
- **Variable-size chunks with a chunk table** in the record. Fills extents
  better, but needs an extra lookup per range read. Rejected for the fixed
  arithmetic.
- **A random nonce per chunk**, like ADR 0016's frames. Safe too, but an
  object could not prove that no chunk was dropped or reordered without
  extra fields. The per-object key with the chunk number as nonce gives
  both.
- **Refuse files in `.vdbx`** (N7 choice a). The user chose to design it now.

## Verification

- **L1-9a (native):** write and read objects of 0 bytes, 1 byte, one chunk,
  one extent, and 512 MiB with bounded memory; every range that starts or
  ends on and next to a chunk and an extent boundary; a crash at every step
  of section 5, then open, read and `fylo verify`; delete, checkpoint and
  reuse; `compact --shrink` with a reader that holds old runs; a file over
  the limit is refused before writing; an older binary refuses a file with
  `large-objects`.
- **L1-9b (WebAssembly, mobile, encryption, pack):** the same reads in the
  browser through OPFS, a picked file, a hosted sealed file and IndexedDB;
  encrypted objects: a moved, dropped or reordered chunk fails the tag
  check, and a range read decrypts only its chunks; `fylo rekey` gives every
  object a new salt; `fylo pack` then `fylo unpack` of a namespace with
  files is byte-identical, metadata included.

## Review

- Decision to design it now: the user, 2026-11-21 (N7), through the hub.
- Reviewed by the hub on 2026-11-21: details 1-5 accepted; detail 5 with the
  key/nonce rule above (a key is never used twice with the same nonce, section
  4). The details are the fixed chunk size arithmetic, the per-object key with
  the chunk number as nonce, the 4,096-run cap and the `large-objects` flag
  (this ADR), and those listed in ADR 0033's review.
