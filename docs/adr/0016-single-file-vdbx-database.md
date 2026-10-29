# ADR 0016: Single-file `.vdbx` databases

- Status: **Accepted** (Samuel, 2026-10-29; spike results are added as they
  arrive and may only settle the codec and the lock qualification list)
- Date: **2026-10-29**
- Owners: **FYLO maintainers**
- Builds on: ADR 0015 (append-only document segments), ADR 0013 (the record
  format and commit protocol), ADR 0014 (the hot set's log-end check) and
  ADR 0008 (the portable WebAssembly engine).
- Amends: ADR 0008's browser storage, for databases held in a `.vdbx` file.
- Design notes and options considered: "A single-file format for FYLO
  databases" (https://claude.ai/artifact/R83hiCJU8CFmXNi2mS4LDM).

## Context

A FYLO root is a directory tree. Even with ADR 0015's segments, a collection is
a folder of segment files, a slot file, two location files, a prefix index and
lock files, and a root holds one such folder per collection plus a catalog and
a transaction journal. Samuel asked for a database to be **one file** with its
own extension that:

1. holds the database's documents and indexes;
2. only the `fylo` engine reads;
3. is compressed but read in place, without extracting it first;
4. grows and shrinks with the data, as a sparse disk image does.

Two measurements point the same way. On the raw ATLAS SMB share the cost of a
query is round trips: an open is about 2.2 ms and an `flistxattr` about 3 ms,
so a 250-row query over per-document files took 1.8 s. Inside an APFS sparse
image stored on the same share, which is one file to the SMB client, a
2,500-row query took 114 ms (both with the hot set off, 2026-10-28,
`docs/QUERY_RELEASE_QUALIFICATION.md`). The image needs mounting, only works on
macOS, and wedged with error 45 under `hdiutil attach`. A file format of our
own gets the one-file benefit on every platform without a mount.

The browser is the second reason. Fylo Explorer still runs the JavaScript
engine over a folder picked through File System Access, which only Chromium
offers. The WebAssembly engine (ADR 0008) calls its host synchronously, and
browser folder operations are asynchronous, so today's WebAssembly path
bridges them through a second Worker and a `SharedArrayBuffer`. That needs
the page served cross-origin isolated, which static hosts such as GitHub Pages
cannot do. An engine that needs exactly one file needs no bridge: a Worker can
read a picked `File` synchronously and can hold one synchronous OPFS access
handle.

## Decision

A FYLO database can be a single **`.vdbx` file**: a header followed by
fixed-size **extents**, each holding one of the files an ADR 0015 collection
keeps today. Records inside are ADR 0015 records, compressed one at a time,
and optionally encrypted. The same Rust code reads and writes it natively,
under WASI and in the browser.

### 1. Scope

Version 1 covers **document collections** of a root, on the `Native` profile,
on macOS, Linux and Windows natively, and in the WebAssembly engines.

Not in version 1, and refused with `ELAYOUTUNSUPPORTED` when packing a root
that uses them:

- buckets (raw-file collections), which need large-object extents that span
  more than one extent; a follow-up ADR;
- document version control, for ADR 0015's reason;
- the embedded queue (ADR 0009). The `EfsSidecarV1` profile (ADR 0010) has no
  `.vdbx` form and needs none: a `.vdbx` file uses no extended attributes or
  hard links, which are what that profile works around;
- transactions that span several collections. One file makes them possible
  (section 6), but they are a separate decision.

A directory root stays fully supported. `.vdbx` is opt-in per database.

### 2. Name and identity

- Extension **`.vdbx`**, a virtual database image, read the way `.vhdx` is
  read for a virtual disk. `.fx` was rejected because DirectX and HLSL effect
  files already use it.
- 2026-10-29: renamed from .fxdb to .vdbx at the owner's request, to read as a
  virtual disk image for databases (like .vhdx). .vdb was rejected because
  OpenVDB, Teiid and antivirus definitions already use it.
- Magic: the 8 bytes `89 56 44 42 58 0D 0A 1A` (`\x89VDBX\r\n\x1a`), PNG's
  pattern, so a file mangled by a text-mode transfer or truncated at a high
  bit fails the magic check instead of parsing.
- Media type `application/vnd.d31ma.vdbx`; macOS type `com.d31ma.vdbx`.
- The format version is a major number and three feature-flag sets, as in
  ext4: an unknown **incompatible** flag refuses the file, an unknown
  **read-only-compatible** flag opens it read-only, and an unknown
  **compatible** flag is ignored. Encryption and the sealed mode are
  incompatible flags; a new codec is an incompatible flag. Read-only-compatible
  bit 0, `READ_ONLY_COMPAT_SCHEMAS`, is reserved for schemas stored in the
  database (ADR 0017).

### 3. File layout

| Offset    | Content                                                                 |
| --------- | ----------------------------------------------------------------------- |
| 0         | superblock copy A, 4 KiB                                                |
| 4 KiB     | superblock copy B, 4 KiB                                                |
| 8 KiB     | reserved, zero, to the first extent boundary                            |
| 1 extent  | extent 1, the collection directory                                      |
| n extents | extents 2 and up: segments, snapshots, dictionaries, maps or free space |

**Extent size** is fixed per file at creation: 8 MiB by default, a power of
two from 1 MiB to 64 MiB. Every extent begins with a 64-byte extent header:
magic, kind, owning collection id, **sequence number**, and a CRC-32C. The
sequence number comes from a counter in the superblock and is never reused.

**Superblock.** Magic, format version and feature flags (including which
codecs appear), extent size, extent count, superblock generation, the next
extent sequence number, the first directory extent, the extents that hold the
current extent map, the encryption header (section 9), and a CRC-32C. A checkpoint writes the copy that does not hold the newest
valid generation and flushes; a reader takes the valid copy with the higher
generation. This is ADR 0015's slot rule at file scale.

**Collection directory.** One 4 KiB block per collection: a static part
written when the collection is created (its name, id and catalog descriptor,
with its own CRC), two **checkpoint slots** (the checkpoint LSN and position,
and the extents, lengths and SHA-256 of its current location, index and
schemas snapshots and dictionary) and two **generation slots** (ADR 0015 section 6,
moved in from `state.slots`, naming the active segment extent and its
committed length). Each slot pair is written A/B with its own CRC, so an
in-place slot write never touches the static part. One block per collection keeps two collections from
ever writing the same sector. An 8 MiB directory holds 2,047 collections (its first 4 KiB is the extent header) and
chains to a further directory extent past that.

**Extent map.** 32 bytes per extent: state (free, owned, sealed), kind, owner,
sequence number and live bytes. A 1 TiB file at 8 MiB extents has a 4 MiB
map. The map is written copy-on-write into free extents at each checkpoint,
and the superblock names the current one. Between checkpoints it is rebuilt
from the records that change it (section 6).

**Extent kinds:**

| Kind         | Holds                                                             | Source of truth? |
| ------------ | ----------------------------------------------------------------- | ---------------- |
| `directory`  | collection blocks                                                 | yes              |
| `segment`    | ADR 0015 records for one collection                               | yes              |
| `dictionary` | a compression dictionary for one collection                       | yes              |
| `locations`  | ADR 0015 `locations.snapshot`, possibly a run of extents          | no, derived      |
| `index`      | the prefix index snapshot (`index/keys.snapshot`), possibly a run | no, derived      |
| `schemas`    | the schemas snapshot (ADR 0017 section 3), possibly a run         | no, derived      |
| `map`        | an extent map copy                                                | no, derived      |
| `free`       | nothing                                                           | n/a              |

A dictionary is part of the source of truth because the records compressed
with it cannot be read without it. It is never freed while a record names it.

**Magics.** Every fixed structure starts with its own magic, so a reader that
lands on the wrong bytes fails a check instead of parsing them. The last row
is not stored: it opens the associated data of every sealed unit (section 9).

| Structure                                     | Magic (ASCII)      | Bytes |
| --------------------------------------------- | ------------------ | ----- |
| superblock                                    | `\x89VDBX\r\n\x1a` | 8     |
| extent header                                 | `VDXEXTNT`         | 8     |
| collection block, static part                 | `VDXCOLL\0`        | 8     |
| collection block, sealed static part          | `VDXCOLLS`         | 8     |
| checkpoint slot                               | `VDXP`             | 4     |
| generation slot                               | `VDXG`             | 4     |
| extent map copy                               | `VDXMAP\0\0`       | 8     |
| dictionary header                             | `VDXC`             | 4     |
| sealed image header (`fylo pack --sealed`)    | `VDXSEALD`         | 8     |
| associated data of a sealed unit (not stored) | `VDXSEAL\x01`      | 8     |

### 4. Records

The record frame is ADR 0015's (sync marker, length, CRC-32C, body) with one
added **frame header byte** for the codec, and a 4-byte dictionary id when a
dictionary was used. When the file is encrypted, the frame also carries a
12-byte nonce and a 16-byte tag (section 9), and the CRC covers the
ciphertext so that `fylo verify` can check integrity without the key.

The manifest, the entry fields and their meaning are ADR 0015 section 3
unchanged, so every rule that rebuilds locations, indexes or the per-document
tree from records still holds.

### 5. Reads

A process opens the file once and keeps the handle. Opening reads the
superblocks, the directory and the extent map, then the location snapshot of
each collection it touches: memory-mapped natively, read into memory in
WebAssembly. Records written since the snapshot are found by replaying each
collection's active segment from the checkpoint to the committed length in
its slot. That tail is bounded by the checkpoint interval, so it costs a few
reads, and it replaces ADR 0015's `locations.wal` and `keys.wal` files: the
deltas live only in memory.

A point read is a location lookup and **one `pread`**. A query that hydrates
many rows sorts locations by offset and reads up to 1 MiB at a time (ADR 0015
section 5). Each read checks the frame CRC, decrypts if needed, decompresses
one record, and checks the body SHA-256.

A reader holding a location re-checks the target extent's header sequence
number when it reads. A mismatch means the extent was freed and reused since
the location was resolved; the reader treats it as a moved generation and
retries, which is today's protocol.

### 6. Writes and extents

The commit protocol is ADR 0015 section 6 with the active segment file
replaced by the collection's active segment extent:

1. under the collection's write lock, build the record;
2. publish `writing` in the collection's slot;
3. append the record to the active extent and flush the file. **This is the
   commit point.**
4. publish `stable`.

A flush covers the whole file, so group commit can retire waiting writers of
several collections with one flush.

**Taking a new extent.** When the active extent cannot hold the next record,
the writer takes the database's allocation lock, picks the lowest-numbered
free extent (or extends the file by one extent), writes its extent header with
a new sequence number, and appends a **seal record** to the old extent that
names the new extent and its sequence number. The seal record and the new
header are flushed with the commit that first uses the new extent. Recovery
follows seal records from the checkpointed active extent, so an extent taken
after the last checkpoint is found without the map.

**Compaction** is ADR 0015 section 7. The live entries of mostly-dead sealed
extents are copied into new extents, a compaction record in the collection's
active extent names the new extents and the ones they replace, and the
replaced extents become free. A freed extent is **reused only after the next
checkpoint** has written a map that shows it free. Readers in other processes
that still hold locations into it are protected by the sequence-number check
in section 5.

**Shrinking.** Free extents are reused first, lowest number first, so data
drifts toward the front of the file. Space is returned to the disk two ways:

- **hole punching** of free extents where the filesystem supports it:
  `F_PUNCHHOLE` on APFS, `fallocate(FALLOC_FL_PUNCH_HOLE)` on Linux,
  `FSCTL_SET_ZERO_DATA` on a sparse NTFS file. Support is probed at open on a
  free extent and remembered for the life of the handle; a failure only means
  the space is kept;
- **tail truncation**, on every filesystem, including those with no hole
  punching (NFS 4.1, FAT, exFAT): `fylo compact --shrink` relocates the live extents nearest the
  end into free extents nearer the front, through the same copy, record and
  checkpoint steps, then truncates the file after the last owned extent.

### 7. Locking and processes

- **Embedded, like SQLite** (Samuel, 2026-10-29). There is no FYLO server
  and no `fylo serve`: every process that uses a database opens the file
  itself, as it would a `.db` or `.sqlite` file. A server that reads a
  `.vdbx` file is just another such process, and FYLO runs one only in tests.
- **Several local processes** coordinate with OS byte-range locks on a
  reserved range far beyond any data offset (2⁶² and up): one lock byte for
  the database lease, one for extent allocation, and one per collection
  (indexed by its directory block) for writers. Every process holds the lease
  byte shared while it has the file open; an operation that needs the file to
  itself (`compact --shrink`, `pack --sealed` in place) takes it exclusive.
  Readers take no other lock; they read through the slots. `fcntl` open-file-description locks (`F_OFD_SETLK`)
  on Linux, `fcntl` on macOS, `LockFileEx` on Windows. These replace the lock
  files and the `hard_link` path for `.vdbx` databases.
- Byte-range locks are enabled on a filesystem only after the multi-process
  load harness passes on it. The qualification list starts with APFS and NTFS
  locally, the ATLAS SMB share, and the local NFS 4.1 server.
- **Any other filesystem still works** (Samuel, 2026-10-29: FYLO must not
  depend on EFS or any one filesystem). Where locks are unqualified, or the
  filesystem refuses them (`ENOLCK`, `EOPNOTSUPP`), one process at a time
  writes the database. It holds a numbered lease file beside it,
  `<name>.vdbx.lease.<generation>`, made with exclusive create
  (`O_CREAT | O_EXCL`, `CREATE_NEW`), which every filesystem FYLO targets
  honours; the highest generation owns the database. The owner rewrites its
  lease on a heartbeat. A contender that sees the lease unchanged for the
  takeover time (30 s by default, timed on its own clock, so hosts need not
  agree on the time) creates the next generation; exclusive create lets only
  one contender win, and the winner deletes the older files. An owner checks
  before each commit that its generation is still the highest. A clean close
  marks the lease free instead of deleting it, so generations only grow and
  the next writer takes it at once.
- Under the lease, **readers are never blocked**: any number of processes,
  on any machine, read through the slots exactly as they do with locks. A
  second writer waits for the lease up to its busy timeout (5 s by default,
  as SQLite's `busy_timeout`) and then fails with `EBUSY`; a process can also
  open the file read-only and never ask for it. A crashed writer holds up
  other writers for the takeover time, never readers; a lease last written
  more than twice the takeover time ago, by its writer's clock, is taken over
  at once.
- **A crashed writer on the same host is taken over at once** (2026-10-29).
  On the ATLAS share a writer killed with `kill -9` held the others up for
  65 s: a contender gives up after its 5 s busy timeout, long before it has
  watched the lease for the 30 s takeover time, so only the twice-the-takeover
  rule on the writer's clock let the next process in. The lease owner now
  also holds an exclusive byte-range lock on a marker file named after its
  owner token in the system temporary directory, taken before the lease
  names the token. The OS lets that lock go when the process stops, however
  it stops, so a contender that finds the marker in its own temporary
  directory and gets its lock takes over without waiting. The marker is used
  only on a filesystem whose locks are qualified (the temporary directory is
  local: APFS, ext4, tmpfs or NTFS); an owner on another host, one with
  another temporary directory, or one that wrote no marker is waited out as
  before. The lease file's format is unchanged: the owner token it already
  holds names the marker. A process-id check (`kill(pid, 0)`, `OpenProcess`)
  was considered and not taken: it is only sound with a host identity that
  tells machines, boots and PID namespaces apart (hostnames collide, and a
  boot identifier needs `sysctl` on macOS and unsafe FFI on Windows, a new
  entry in the ADR 0004 inventory), and a reused process id or an unreaped
  zombie reads as alive; the marker lock has none of these problems and uses
  the byte-range lock code already qualified for `.vdbx` files.
- **The takeover time stays 30 s** (2026-10-29). Across hosts the 65 s is
  set by the twice-the-takeover rule, so a shorter takeover time is the
  only lever there: 15 s would bring it to about 30 s. Renewing is cheap
  (a directory listing and a write and flush of about 10 ms on SMB, once per
  third of the takeover time while writing), so the cost is not I/O but
  margin: a
  writer renews only from its writes, so a commit has two thirds of the
  takeover time after its last check (20 s today, 10 s at 15 s) before a
  paused or very slow writer can be taken over under it, and the clock-skew
  tolerance of the wall-clock rule halves with it. A background renewing
  thread would not help: a paused process stops its thread too, which is the
  case the margin is for, and it would keep an idle writer's lease alive,
  where today an idle writer's lease lapses and another writer may take it.
  Shortening the takeover time is left to the owner.
- **Choosing the mode.** A writer uses byte-range locks when its filesystem
  is qualified and grants a probe lock, and the lease otherwise. A caller can
  also ask for **exclusive mode**, the lease everywhere, as SQLite's
  `locking_mode=EXCLUSIVE`. The modes exclude each other: a lease writer
  whose filesystem grants locks at all also holds the lease byte exclusive,
  and a lock writer waits while any lease is held, taking over one a crash
  left. Unsupported: one host whose filesystem refuses every lock sharing a
  file with another host that writes through locks.

**Filesystem requirements.** A `.vdbx` database needs only positional reads
and writes, resizing the file, flushing the file, and exclusive create for the
fallback lease. It does not need atomic rename, directory flushes after
creation, extended attributes, hard links, sparse files, hole punching or
byte-range locks; each is used when present and probed, never assumed. On a
filesystem without sparse files, growing by an extent writes its zeros. On
FAT32, files stop at 4 GiB and growth past that fails with `EFBIG`; exFAT has
no such limit.

### 8. Compression

Each record body is compressed on its own, so a point read decompresses one
document. A **dictionary** is trained per collection from its first 1 MiB of
documents at the first checkpoint that has them, stored in a `dictionary`
extent, and named by id in each frame. A later dictionary is trained at
compaction when it improves the ratio by at least 10%; records keep the
dictionary they were written with until compaction rewrites them.

**Codecs (decided by Samuel, 2026-10-29, from spike 4).** Each frame's codec
byte names how it was compressed: `0` none, `1` zstd, `2` LZ4.

- Native writers use **zstd level 3** with the collection's dictionary,
  through the C library (`zstd` crate). It needs an ADR 0004 dependency
  review before it lands.
- The browser cannot carry the C library: compiled to WebAssembly it adds
  433 KB, more than the engine's remaining 224 KB of budget. The browser reads
  zstd frames with **`ruzstd`** (pure Rust, +16 KB gzipped).
- The browser writes **LZ4** frames with **`lz4_flex`** (pure Rust, +6 KB
  gzipped), using the same dictionary's raw bytes. `ruzstd` cannot compress
  with a dictionary.
- Native compaction rewrites LZ4 frames as zstd.

On three 5,000-document test sets, zstd level 3 with a 32 KiB dictionary
stored documents at 2.9x to 3.4x and LZ4 with a dictionary at 2.1x to 2.6x;
every codec decoded a document in under 10 µs
(`vdbx/spike-4-codec.md` in the project files).

A **sealed** file (section 11) recompresses its extents in 64 KiB blocks for
a better ratio; a point read then decompresses one block.

### 9. Encryption

Encryption is **opt-in per database and off by default**. The documentation
and the website say so plainly wherever `.vdbx` is introduced, and
`fylo inspect` reports `encrypted: false` for a plain file.

When enabled at creation (`fylo create db.vdbx --encrypt`, or
`encryption: { file: true }` in `application.json`):

- the file key is derived with HKDF-SHA256, built on the existing `hmac` and
  `sha2` crates, from the secret named by `fylo.encryption.key` and a random
  32-byte salt stored in the superblock;
- every record frame, the collection directory, dictionaries and snapshot
  extents are sealed with **AES-256-GCM** (the `aes-gcm` crate the engine
  already uses for `$encrypted` fields), compressed first;
- each sealed unit takes a **fresh random 96-bit nonce**, never one derived
  from the LSN, because compaction and packing rewrite the same LSN with
  different bytes;
- the associated data binds a frame to its collection id, LSN and entry index,
  so a frame cannot be moved into another collection or replayed as another
  record unnoticed;
- the superblock keeps a key-check value, so a wrong key is reported as
  `EKEY` at open, not as corruption.

Without the key, only the superblock and extent headers are readable: sizes,
counts and the format version, not collection names or documents. A process
without the key opens the file **locked**: `fylo inspect` reports
`encrypted: true, locked: true` and lists no collections, and every write
fails with `EKEY` until the file is unlocked. A collection block's static part
is sealed with its block number as associated data, so a block copied to
another position is refused; its slots stay in the clear, since they hold only
counts and positions. A lost key
loses the database; `fylo unpack` to a directory root stays the way to keep a
plain copy. Key rotation rewrites the file through compaction and is a
follow-up.

### 10. WebAssembly and Fylo Explorer

The container lives in the Rust storage crates and does all its I/O through
the `fylo-vfs` seam, so `fylo.wasm` (WASI) and `fylo-browser.wasm` run the same
code. It needs six host operations on one file, and the `HostVfs` table
already has all of them (`open`, `stat`, `read_at`, `write_at`, `truncate`,
`flush`), so the host ABI does not change. Hosts:

| Host                              | How it opens the file                                      | Writes                   |
| --------------------------------- | ---------------------------------------------------------- | ------------------------ |
| Browser, a picked or dropped file | a `File` read with `FileReaderSync` in the engine Worker   | none, read-only snapshot |
| Browser, a working copy           | one OPFS `FileSystemSyncAccessHandle`, opened before start | in place                 |
| Browser, a hosted sealed file     | synchronous ranged `XMLHttpRequest` in the engine Worker   | none                     |
| iOS and Android shells            | a native `HostVfs` over the app's file handle              | in place                 |
| WASI runtime                      | a preopened file                                           | in place                 |

None of these needs the `SharedArrayBuffer` bridge or cross-origin isolation.
A browser cannot write a user-picked file in place (File System Access only
replaces a file whole), so Explorer edits a picked file through an OPFS
working copy and writes it back with **Save** or downloads it. A working copy
is owned by one tab through Web Locks. A browser cannot lock a file against a
native process, so a picked file is read as a snapshot: the slots and
committed lengths read at open, re-read when the file's size or modification
time changes.

Explorer gains **Open database** (file picker, file input and drag and drop),
which works in every current browser, not only Chromium. Explorer's folder
view stays on the JavaScript engine until it is retired; this ADR does not
port it.

### 11. Packing, unpacking and operations

- `fylo pack <root> --out db.vdbx` builds a `.vdbx` from a directory root (file
  or segment layout), with ADR 0015's staging, oracle and commit steps.
  `--sealed` produces a read-only, block-compressed file with no free extents.
- `fylo unpack db.vdbx --out <root>` writes a directory root in the file
  layout, byte-identical to ADR 0015's export.
- `fylo compact db.vdbx [--shrink]`, `fylo verify db.vdbx`,
  `fylo salvage db.vdbx --out <root>` (stepping over damage by sync marker and
  extent header) and `fylo inspect db.vdbx` (extent counts by kind, free and
  dead bytes, flags, encryption).
- Every API that takes a root takes a `.vdbx` path. A root path that names an
  existing regular file is opened as a container; one that names a directory,
  or a path ending in `/`, is a directory root. `Fylo.open('orders.vdbx')`
  and every CLI command follow the same rule.

### 12. Compatibility

An older binary given a `.vdbx` path finds a regular file where it expects a
root directory and fails before writing anything; the verification suite
pins the exact error for the current release. A newer binary refuses an unknown
incompatible flag with `EFORMATUNSUPPORTED`. Downgrading a database means
`fylo unpack` first.

## Spikes before implementation

These run on the Mac Mini after the timed ATLAS benchmarks finish. Their
results are added here as they arrive.

| Spike | Question                                                                                    | Decides                                          |
| ----- | ------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| 1     | Cold `pread` latency on one long-open file on the raw ATLAS share, with and without caching | whether a cold read under 1 ms is possible there |
| 2     | Byte-range locks between two machines on CITADEL, and on the local NFS 4.1 server           | **Partly done 2026-10-29:** see below            |
| 3     | Whether `F_PUNCHHOLE` from macOS frees space on the ATLAS share                             | **Done 2026-10-29:** it does not; see below      |
| 4     | `lz4_flex` and zstd with a dictionary on the 5k fixture: ratio, decode time and code size   | **Done 2026-10-29:** section 8's codecs          |

**Spikes 2 and 3 on CITADEL from macOS 27.2 (SMB 3.1.1, Samba, one client).**
Every `fcntl` lock call on an `smbfs` mount, `F_SETLK`, `F_SETLKW` and
`F_OFD_SETLK` alike, fails with `ENOTSUP`, so the probe in section 7 finds no
locks and a macOS writer on the share uses the lease. The only server-side
byte-range lock the macOS client offers is its own `fsctl`, which is exclusive
only, held per handle, mandatory (other handles cannot read the locked bytes)
and released when the process closes any other read-write handle on the file;
each operation costs a round trip, 8 to 25 ms median while the ATLAS
benchmarks ran. FYLO does not use it. `F_PUNCHHOLE` also fails with `ENOTSUP`
and frees nothing (on local APFS it freed exactly the punched 8 MiB), so a
`.vdbx` on the share returns space only by tail truncation (`fylo compact
--shrink`). Still open: locks between two client machines on the share, and
the local NFS 4.1 server. Scripts and the full report are in
`vdbx-spikes/REPORT.md` on HAVEN.

## Consequences

Benefits:

- a database is one file to copy, back up, mail or open in Explorer;
- one open and one header read per database instead of several files per
  collection, which is what matters on network filesystems;
- JOINs read both collections from the same moment, through one handle;
- the browser and mobile builds open databases with no bridge and no special
  hosting headers, in every browser;
- compressed records, and real protection when encryption is on.

Costs:

- an extent allocator, a free map and a reuse check that ADR 0015's
  directory layout does not need;
- damage to both superblock copies or the directory loses access to the whole
  database until `fylo salvage` recovers its records;
- one OS owner and mode for the whole database: per-collection OS permissions
  are gone (per-record access descriptors still apply, as in ADR 0015
  section 10);
- a flush covers all collections, so a heavy import can lengthen another
  collection's commit. Imports write and flush their extents in chunks
  before their commit records to keep that small;
- byte-range locks are new to FYLO and need their own qualification per
  filesystem.

## Alternatives considered

- **A page-based file with B-trees, like SQLite or LMDB.** Compressed pages
  need their own offset map, every commit writes the log and later the page,
  and it replaces ADR 0013 and ADR 0015 with a new engine.
- **SQLite as the container.** A C dependency and a format FYLO does not own,
  and SQLite's documentation warns against its locking on network
  filesystems, which is ATLAS and EFS.
- **A band bundle, like a macOS sparse bundle.** The safest on network storage
  (a freed band is a deleted file), but it is a folder, not one file.
- **One `.vdbx` per collection.** No consistent snapshot across collections
  for JOINs, an open per collection on a cold start, and no path to atomic
  multi-collection writes. An application that wants that isolation can still
  use one `.vdbx` per collection.
- **Whole-file compression (zip, gzip).** Needs extraction to read.

## Verification

- `fylo-format` unit and fuzz targets for the superblock, directory, extent
  header, map and frame parsers: truncation at every byte, bit flips, both
  superblocks damaged, a reused extent with a stale sequence number, unknown
  flags of each class.
- The ADR 0015 conformance suites run against a `.vdbx` database as a third
  layout.
- **Crash matrix:** new failpoints `after-extent-header`, `after-seal-record`,
  `after-superblock-write`, `after-map-write`, `after-shrink-relocate`,
  `after-shrink-truncate`, `after-pack-staging`, plus ADR 0015's, under
  `abort`, `enospc` and `edquot`.
- **Reuse race:** readers holding locations while a compactor frees and a
  writer reuses the same extents, for 10 minutes, with every read checked.
- **Pack oracle:** directory root → `.vdbx` → directory root on the 5k and 25k
  fixtures, identical by ADR 0015's oracle; and the same through `--sealed`.
- **Encryption:** wrong key, swapped frames between collections, a replayed
  frame, and a flipped tag bit are each detected.
- **Browser:** Playwright opens a packed `.vdbx` read-only from a file input,
  edits an OPFS copy and saves it, with no cross-origin isolation; the
  WebAssembly size budget still passes; the artifact interchange script runs
  the same NDJSON against `fylo` and `fylo.wasm` on a `.vdbx` database.
- **Multi-process:** the load harness with byte-range locks on each
  filesystem before it is qualified (section 7).
- **Benchmarks:** point, selective, broad and range reads and durable writes
  at 5k and 25k on the Mac, in the APFS image on ATLAS, on the raw ATLAS
  share and from the Surface, against the ADR 0015 layout, with load recorded.

## Operations

- `fylo pack`, `unpack`, `compact [--shrink]`, `verify`, `salvage` and
  `inspect`, as above.
- On network storage, open the file directly from each machine. Where the
  filesystem's byte-range locks are qualified, writers on several machines
  share it; elsewhere they take turns through the lease. Readers on any
  machine are never blocked.
- Back up a live database with `fylo pack --sealed` (a consistent copy), or
  copy the file while no writer has it open.
