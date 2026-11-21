# ADR 0030: Tiered read cache (temp-disk documents, in-memory query results)

- Status: **Proposed** (2026-11-14). The user answered decisions D1 to D6 the
  same day. Slice R (fewer file calls per read, section "Slice R") is built;
  the temp tier waits for the user's decision on its numbers.
- Date: **2026-11-14**
- Owners: **FYLO maintainers**
- Amends: ADR 0014 (the hot set also keeps query results; the lookup order
  gains a temp-disk tier).
- Related: ADR 0003 (one owner per root, caches are derived), ADR 0010 (EFS
  profile), ADR 0011 and 0028 (invocation), ADR 0013 (write-ahead log end),
  ADR 0021 (identities on protected `.vdbx`), ADR 0027 (layout ids, "correct
  by construction"), ADR 0029 (no network roots except EFS),
  `docs/QUERY_INDEX_REUSE.md` (why a generation alone is not a cache key).
- The planning notes (spec, slice plan, benchmark) are kept outside the
  repository, in the maintainers' planning folder `tiered-cache-plan`.

## Context

The user asked for three read tiers: the root's disk (the true copy), a disk
cache in the OS temp folder for documents read often, and memory for query
results.

What FYLO has today:

- **Hot set** (ADR 0014): parsed documents, index snapshots, id lists and
  collection headers in memory, only in exclusive sessions, cleared around
  every write frame, filled only after a stable read at a known generation
  and log end.
- **Index cache** (`index_cache/`): one process-wide parsed index snapshot,
  reused only when the snapshot and log bytes read again are byte for byte
  the same. It saves parsing, not file reads.
- **Segment location tables** (ADR 0015, 0027, `segments/table_cache.rs`):
  keyed by medium identity, layout id and table epoch.
- **Word-index freshness** (ADR 0023): one answer per collection and
  generation.
- **Checksum xattr cache** (README): a raw file's hash, stamped with size and
  mtime.

Facts that decide the design:

1. A temp folder is faster than the root only when the root is slow and the
   temp folder is local. The only slow root FYLO admits is EFS through the
   explicit EFS profile (ADR 0029 refuses other network roots). That profile
   uses the files layout only: one JSON file per document plus a mandatory
   `.fylo-attrs` manifest, no write-ahead log, no segments, no `.vdbx`.
2. On AWS Lambda each request is one `fylo exec --invocation` process
   (ADR 0028). Memory dies with the process; `/tmp` lives as long as the warm
   container. Other containers write the same EFS root at the same time.
3. Folder roots have no per-actor access since POSTIX was removed (ADR 0021):
   access is the OS user's file permissions. Per-identity access exists only
   on protected `.vdbx` databases, which are always local.
4. `$encrypted` fields are stored as AES-GCM ciphertext inside the document
   file. Decoding a document decrypts them.

## Decision

### 1. The tiers and the lookup order

| Tier | Holds | Lives in |
| --- | --- | --- |
| Memory: hot set (ADR 0014, extended) | parsed documents, index snapshots, id lists, headers, **and now query results** | process memory |
| Temp disk (new) | **stored** document records and stored index bytes, exactly as read from the root, never decoded | `<temp folder>/fylo-cache-v1...` |
| Primary | everything | the root |

A document read looks in the hot set, then the temp tier, then the primary.
A `findDocs` looks for a query result in the hot set first; on a hit it
reads only the listed documents (through the same three tiers). On a miss it
plans as today, reading the index from the hot set, then the temp tier, then
the primary.

A temp-tier hit is handed to the same decode path a primary read uses
(`build_record`: schema decode, decryption, signature and key checks). So a
hit returns exactly what a primary read would return, and every per-read
check still runs.

### 2. Which modes use which tier

| Mode | Hot set (today) | Query results in memory | Temp tier |
| --- | --- | --- | --- |
| `exec --loop --exclusive-root`, `FrameSession` with the lease | yes | **yes** | yes, when active (section 4.1) |
| `exec --loop` without the exclusive lease | no | no (decision D3) | yes, when active |
| `exec --invocation`, `exec --request`, CLI one-shots | no | no (the process ends) | yes, when active |
| Library opens (`NativeRoot`, `ReadOnlyEngine` without a hot set) | no | no | yes, when the host passes a temp cache |
| `.vdbx` roots, any mode | as today | as the hot set | **never** |
| Browser engine | no | no | **never** (OPFS is its derived store, ADR 0003) |

Reasons:

- **Query results only where the hot set is.** The exclusive lease means
  every write passes through the session, which clears the tier around it.
  That needs no extra check per query. The user chose (D3) to keep both
  memory tiers in exclusive sessions only.
- **The temp tier in every mode, with a check.** Its main user is Lambda,
  where every request is a new process without the exclusive lease. So every
  temp entry is checked against the root's current state on every use. An
  exclusive session runs the same check once, when it reads a collection's
  header.
- **Never for `.vdbx`.** A database is always on a local disk, so the tier
  cannot be faster, and protected databases hold per-identity encrypted
  collections that must not be copied in any form.

### 3. How a cached item is checked before use

#### 3.1 The collection stamp

Every read already opens `state.json` before and after it reads, to answer
from one stable generation. The **collection stamp** is what those reads see,
plus the opened file's identity:

| Part | Source | What it catches |
| --- | --- | --- |
| generation, status, write-ahead log end (segment nonce, length, next sequence number, record count) | `state.json` bytes | every commit by any release |
| `state.json` file identity: device, inode (NFS fileid), size, mtime and ctime in nanoseconds | `fstat` of the handle already open | a replaced file, also one with the same generation (restore, branch checkout, a copied collection, an older release) |
| collection incarnation id | new small file, written once per incarnation (section 3.3) | a re-created collection whose `state.json` happens to get the same inode and ctime |
| root key | canonical root path and filesystem id | another root at another path |

Rules:

- A stamp is read only from an **opened** `state.json`, with `fstat` on the
  open handle, never with a bare `stat`. On NFS, opening forces the client to
  revalidate (close-to-open); a bare `stat` may return cached attributes.
- A collection with no `state.json` and no incarnation id is **not
  cacheable**: nothing is written to or read from the temp tier for it. (A
  missing state file reads as generation 0, which a re-created collection
  also reads.)
- The `writing` status is never cacheable.
- No extra file call: the stamp comes from the reads the engine does today.

#### 3.2 The document stamp

Each temp document entry stores the collection stamp it was read under and a
**document stamp**: the identity (device, inode, size, mtime ns, ctime ns) of
the document file and, in the EFS profile, of its `.fylo-attrs` manifest.

- **Collection stamp equal** → the entry is the document at this exact state.
  It is used with **no** file call on the root.
- **Collection stamp different** (someone wrote the collection since) → the
  entry is revalidated: open the document file (and the manifest) on the
  root, `fstat` the handles, compare with the document stamp. Equal → hit,
  and the entry's collection stamp is refreshed. Different or missing → miss;
  the read goes to the primary and the entry is replaced or deleted.
- Revalidation runs inside the same stable-read attempt, so the result obeys
  the same before-and-after generation rule as a primary read.

#### 3.3 How each kind of change is caught

| Change | Caught by |
| --- | --- |
| a write by this or another process, any release | generation and log end in `state.json` |
| restore, rollback, branch checkout, merge, `restoreCommit` | `state.json` file identity (a new file) and a new incarnation id |
| a generation number that comes back to the same value | file identity and incarnation id (QUERY_INDEX_REUSE's objection) |
| drop and re-create under the same name | new incarnation id; no `state.json` and no id means not cacheable |
| `rebuildCollection`, reshard, layout move | generation bump (structural journal) and file identity |
| a document changed while the collection stamp moved | document stamp (new file or new ctime) |
| an older FYLO release writing the root | it rewrites `state.json` (new identity), even though it does not know the incarnation id |
| a torn or corrupt temp entry | the entry's checksum (section 4.4) |
| an edit to the root's files by a tool other than FYLO | see 3.4 |

**Incarnation id.** Like ADR 0027's layout id: a TTID clock tick of the
writing process, in a new file `.fylo-transactions/<namespace>/<name>/incarnation`
(16 bytes: magic, id, CRC-32C), written when a collection is created and by
every operation that replaces a collection's content in place (checkout,
restore, merge, rollback, replace). It is a second guard; the file identity
is the first. `state.json` is parsed with `deny_unknown_fields`, so the id
cannot go there without breaking older readers; a separate file is ignored
by older releases.

#### 3.4 Out-of-band edits

An edit to a document file by a tool other than FYLO does not change
`state.json`. While the collection stamp is unchanged, the temp tier will
not see it. This is the same limit the hot set accepted in ADR 0014, and
such edits are unsupported. The user chose **collection** validation (D2).
A **strict** mode (every hit opens the document file and compares the
document stamp) was considered and is not built.

#### 3.5 Query results

- A query result is the ordered list of ids of **all** documents that match
  the query's predicates, before `limit`, paging and projection are applied.
- Key: the collection, the hot set's header for it (its stamp), and the
  query's canonical form (`$ops`, `$match`, `$sort` and their values, JSON
  keys sorted). Not part of the key: `limit`, page, projection, request id.
- Admitted only when the engine evaluated every candidate (a query cut short
  by `limit` is not admitted) and the stable check after the read passed.
- Cleared with the hot set: before and after every frame that is not a read.
- On a hit, each listed document is read through the tiers and passes every
  per-read check, then `limit`, paging and projection are applied as today.
- Never written to disk. An id list for a predicate on an encrypted field
  says which documents hold a value; it must not leave memory.

### 4. The temp tier

#### 4.1 When it is active

`--temp-cache <off|auto|on>`, default **`auto`** (decision D1):

- `off`: nothing is read from or written to the temp folder. This is the
  switch to turn the tier off, per process, without a release.
- `auto`: FYLO decides once, when the session opens its root. The tier is
  active only when **all** of these hold:
  1. the root is a folder root (not `.vdbx`) opened with
     `--filesystem-profile efs-sidecar-v1`. Today this is the only way a
     root may sit on a remote filesystem (ADR 0029); the handshake shows it
     as `rootLock.remote: true`;
  2. the cache folder passes the private-folder checks (4.2);
  3. ADR 0029's filesystem check reports the cache folder's filesystem as
     **local** (not NFS, SMB, WebDAV, FUSE, virtiofs, 9p, and not
     `unknown`);
  4. the cache folder is on another device than the root (`st_dev`
     differs);
  5. the cache folder's filesystem has at least 64 MiB free.
  There is **no speed probe**. Timings change with load (the benchmark saw
  30 to 60% swings), so a probe would make the same root cache in one run
  and not in the next. The filesystem class is a fixed fact; the EFS
  profile is the user's own statement that the root is remote.
  When `auto` stays inactive, the handshake says which rule failed
  (`root is local`, `vdbx`, `unsafe folder`, `cache folder is remote`,
  `same device`, `low space`).
- `on`: the same rules without 1 and 4, for any folder root (for example a
  root on a USB hard disk with the temp folder on an SSD, and for tests).

Never active for `.vdbx` roots, raw-file collections, segment collections
(always local) or collections that are not cacheable (3.1). The handshake
says why it is inactive.

#### 4.2 Location and permissions

| OS | Base folder | Cache folder |
| --- | --- | --- |
| macOS | `$TMPDIR` (per-user, already private) | `$TMPDIR/fylo-cache-v1` |
| Linux, Android | `$TMPDIR`, else `/tmp` (on Lambda: `/tmp`) | `<base>/fylo-cache-v1-<uid>` |
| Windows | `%TEMP%` (per-user) | `%TEMP%\fylo-cache-v1` |

`--temp-cache-dir <path>` replaces the base folder.

- Folders are created with mode `0700`, files with `0600`, whatever the
  umask. Windows: a protected DACL that grants only the current user.
- An existing cache folder is used only if it is a real folder (not a
  symbolic link or reparse point), owned by the process's effective user,
  and (Unix) has no group or other permission bits. Otherwise the tier is off
  for this process and the handshake reports `unsafe folder`. FYLO never
  changes or deletes a folder it does not own.
- Every file under it is opened relative to the opened folder handle with
  no-follow flags, so a swapped link cannot redirect a read or write.
- One cache folder per OS user. Users never share one.

#### 4.3 Layout

```text
fylo-cache-v1[-<uid>]/
  sweep.stamp                        mtime = last sweep (one stat to read)
  <root key, 32 hex>/
    <collection key, 16 hex>/
      index-<stamp hash>.fyc         stored index snapshot and log bytes
      <first 2 hex of id hash>/
        <SHA-256 of id, 32 hex>.fyc  one stored document record
```

The root key is a hash of the canonical root path and its filesystem id; the
collection key a hash of namespace and name. File names never contain a
caller's id or collection name, so no caller string becomes a path.

#### 4.4 Entry format and crash safety

One file per entry: a fixed header (magic `FYLC`, format version, kind,
root key, collection and id as stored, collection stamp, document stamp,
lengths, CRC-32C of the header), then the stored payload bytes and the
stored metadata bytes (native: the attributes `read_document` returns; EFS:
the verified `.fylo-attrs` manifest), then a CRC-32C of the body.

- Written as `<name>.partial-<pid>-<random>` (`O_CREAT|O_EXCL`, `0600`), then
  renamed over the final name. No `fsync`: it is a cache.
- A reader checks magic, version, both checksums, that the collection and id
  in the entry are the ones asked for, and the stamps (3.1, 3.2). In the EFS
  profile it also checks the manifest's record-identity digest against the
  payload, as a primary read does (ADR 0010). Any failure deletes the entry
  and counts `rejected`; the read goes to the primary.
- Two processes writing one entry: the last rename wins; both are valid for
  their own stamps.
- Written only after the primary read passed its stable check, the same
  admission rule as the hot set. Documents larger than 1/64 of the budget
  are not written.

#### 4.5 Size, eviction, cleanup

- `--temp-cache-bytes <n>`, default 256 MiB, never more than half of the free
  space on the cache folder's filesystem at start (Lambda's default `/tmp`
  is 512 MB).
- **Sweep**: at most once per 30 seconds across all processes (the
  `sweep.stamp` mtime), and when this process wrote more than 10% of the
  budget since its last sweep. It lists the entries, deletes the least
  recently used (oldest mtime) until the total is at most 90% of the budget,
  and deletes `.partial-*` files older than 10 minutes.
- **Recency**: a hit sets the entry's mtime to now when it is older than
  60 seconds (one `futimens` on the open handle), so often-read documents
  stay.
- **Full disk**: a failed write deletes its partial file, counts `errors`,
  and stops this process's writes until the next sweep.
- **Process end**: nothing to clean. Entries are meant to serve the next
  process. A crash leaves at most `.partial-*` files, which the next sweep
  removes.
- **Wiped temp folder** (OS cleaner, Lambda recycle, a user): reads miss, and
  the folders are created again with the same checks.
- `fylo cache clear` deletes this user's cache folder; `fylo cache status`
  prints its size and entry count.

#### 4.6 What is cached on disk, and what is not

Cached: document records of files-layout document collections (native and
EFS profile), and the stored index bytes of such collections (keyed by the
full collection stamp only; on a hit they feed the existing content-verified
index cache instead of the root's index files).

Not cached: raw files (`getFileData`), tombstones and deleted documents,
query results, decoded documents, authorization answers, schema and catalog
files, anything of a `.vdbx`.

### 5. Encryption and access

- **No decrypted byte reaches the temp disk.** The tier stores records as
  they are stored on the root: `$encrypted` fields stay AES-GCM ciphertext.
  Decryption happens after a hit, in memory, through the normal decode path,
  and fails closed (`EDECRYPTFAILED`) exactly as a primary read does. A
  reader of the cache folder learns nothing a reader of the root could not;
  the cache folder is private to the OS user as well.
- **Index bytes** hold the same HMAC blind-index keys as the root.
- **Authorization is never cached.** Folder roots: access is the OS user's
  permission on the root's files. A process can only find entries it wrote
  itself or that another process of the same OS user wrote after reading the
  root. After any FYLO write (stamp changed), revalidation opens the root's
  file, so the kernel checks permission again. Protected `.vdbx`: the temp tier is never used; the query
  tier is per session, a session has one identity, and every listed document
  still goes through the per-read signature, key-epoch and grant checks.
- **Encrypted collections** work the same as plain ones in every tier. The
  query tier keeps results of predicates on encrypted fields only in memory.

### 6. Fit with the existing caches

- **Hot set**: extended, not replaced. It gains one resident kind, query
  results, under the same budget, the same second-chance clock, the same
  epoch and clear rules. One budget (`--hot-set-bytes`) covers both (decision
  D4). In exclusive sessions only (D3).
- **Index cache**: unchanged. The temp tier only changes where the bytes come
  from (temp instead of the root) when the collection stamp matches.
- **Segment tables, word-index freshness, checksum xattrs**: unchanged.
  Segments are never on a remote root, so the temp tier does not touch them.
- Order for a document: hot set → temp tier → primary. A temp hit is admitted
  to the hot set exactly like a primary read.

### 7. Configuration and reporting

| Setting | CLI | `application.json` / `ApplicationConfig` | Default |
| --- | --- | --- | --- |
| temp tier mode | `--temp-cache off\|auto\|on` | `fylo.tempCache.mode` / `temp_cache.mode` | `auto` (decision D1) |
| temp tier size | `--temp-cache-bytes <n>` | `fylo.tempCache.bytes` | 256 MiB, capped at half the free space |
| temp tier folder | `--temp-cache-dir <path>` | `fylo.tempCache.dir` | the OS temp folder |
| query results | under `--hot-set-bytes` | `hot_set_bytes` | on wherever the hot set is on |

All `exec` modes accept the temp flags (`--loop`, `--request`,
`--invocation`). A request can never change them (like `root` and
`schemaDir`).

Every handshake reports:

- `capabilities.tempCache`: `false` when the mode is `off`; otherwise
  `{ mode, active, reason, dir, budgetBytes, usedBytes, entries, hits,
  misses, revalidations, rejected, writes, evictions, errors }`. `usedBytes`
  and `entries` are from the last sweep.
- `capabilities.hotSet` gains `queries`, `queryHits`, `queryMisses`.

Counters are for operators; results never depend on them.

### 8. Failure policy

A cache can never fail a request. Any temp-tier problem (unsafe folder, I/O
error, full disk, corrupt entry, wiped folder) is counted and the read goes
to the primary. A primary error is returned as today; a cache hit never
hides one that a primary read would have raised at the same state.

## Consequences

- Lambda with an EFS root: a warm container answers repeated reads of an
  unchanged collection without opening document or manifest files on EFS.
  Only the stable `state.json` reads remain (two per read, as today).
- Measured on two NFS servers (the planning benchmark): the document read is about
  half of a `getDoc` (FYLO makes about 35 NFS calls for a document and its
  manifest). A temp hit should save about 50 to 55% of a `getDoc` in a long
  session, and about 12 to 17% of a one-request invocation process, where
  process start, recovery and the lease cost more. The hot set stays far
  faster (0.3 ms per `getDoc` on every root).
- Write-heavy collections gain little: after each write the stamp moves, and
  each cached document costs one open on EFS to revalidate.
- Local roots: no change in `auto` mode. Exclusive sessions gain the query
  tier.
- A small format addition (the incarnation file); older releases ignore it.
- More code in the read path; it is covered by differential tests against
  tiers-off results.

## Risks

| Risk | Mitigation |
| --- | --- |
| A stale read from a stamp that should have changed but did not | two independent guards (file identity, incarnation); differential and cross-process tests for every change kind in 3.3 |
| NFS or EFS returns cached attributes or coarse timestamps | stamps only from opened handles; EFS timestamp and close-to-open behavior become an item of the EFS qualification plan |
| A cache bug returns another document | entry carries collection, id and stamps, all checked; EFS identity digest checked |
| Temp disk fills on Lambda | budget capped at half the free space; sweep; writes stop on `ENOSPC` |
| Another local user reads or plants entries | private folder rules (4.2), no-follow opens |
| Sweeps cost time in short Lambda requests | at most one sweep per 30 s across processes |
| The gain is smaller than hoped | the benchmark (the planning benchmark) and a real Lambda + EFS measurement gate the default |

## Alternatives rejected

- **Rely on the OS page cache and the NFS client cache.** They keep the
  bytes, but every read still opens the document and its manifest, and on NFS
  each open is a round trip (close-to-open). Measured: the second read of a
  document in a shared session costs the same as the first (8 ms on fast NFS,
  55 to 90 ms on slower NFS). The temp tier removes those opens when the
  collection is unchanged.
- **Only cut FYLO's NFS calls per read, with no new cache.** Not rejected:
  it is decision D6. It helps every read, needs no stamp, and may give much
  of the same gain. It does not give memory-speed reads.
- **Generation number as the key.** Unsafe (QUERY_INDEX_REUSE).
- **Cache decoded documents on disk.** Writes decrypted fields to disk.
- **Temp tier only in exclusive sessions.** Misses Lambda, the only case
  where it helps.
- **One pack file or SQLite database for the temp tier.** Needs byte-range
  locks between processes; some `/tmp` filesystems (container overlayfs) do
  not offer them, and a pack needs its own compaction. One file per entry
  uses only rename and needs no lock.
- **Query results on disk** (so Lambda keeps them between requests). Writes
  "which documents match" for predicates on encrypted fields to disk. Kept as
  decision D5 in case the user wants it for unencrypted fields only.
- **A memory-backed temp folder (tmpfs, `XDG_RUNTIME_DIR`).** Takes memory
  the function or app needs; a user can still choose it with
  `--temp-cache-dir`.
- **Write-through on writes.** Writes would pay a second write; reads refill
  the tier anyway.

## Slice R: fewer file calls per read (built 2026-11-14)

Decision D6 put this first. A call trace (an interposed library that logs
every path-based file call of `fylo exec --loop`) showed one `getDoc` on an
EFS-profile root making about 254 path-based calls in a shared session and
130 in an exclusive one. About half re-checked root ownership: the owner
record was read about 48 times, the profile descriptor 20 times, and the
root path was resolved about 6 times per request.

**Change.** A read-only machine frame (the hot set's read list) runs inside a
`ReadScope` (`fylo-storage-native`, one per thread). Inside it a root's
ownership and profile checks, and the canonical form of a path, are worked
out once and reused for the rest of the frame. Write frames, recovery
between read attempts, and every `NativeWriteRoot` check every time as
before: a writer's root never reuses a check, and writer-side lease checks
call `RootLease::assert_owned_fresh`. The per-file identity checks of the
EFS profile are unchanged.

**Measured** on 2026-11-14 (Mac load 8 to 16; p50 / p95 in ms; 1,000
documents of about 1 KB; `findDocs` returns 50; NFSv3 over Thunderbolt and
NFSv4 over Ethernet, both with the EFS profile):

| Case | before | after |
| --- | --- | --- |
| path-based file calls per `getDoc`, shared session (NFSv3 / NFSv4 root) | 253 / 226 | 114 / 102 |
| path-based file calls per `getDoc`, exclusive session | 129 | 54 |
| NFS calls per `getDoc`, shared: NFSv3 lookups / NFSv4 round trips | about 70 / 114 | about 55 / 63 |
| `getDoc`, shared session, NFSv3 | 7.2 / 23.0 | 5.8 / 9.0 |
| `getDoc`, shared session, NFSv4 | 35.5 / 49.9 | 21.8 / 47.0 |
| `getDoc`, shared session, repeat read, NFSv4 | 44.3 / 86.2 | 20.3 / 30.3 |
| `getDoc`, exclusive session, first read, NFSv4 | 27.3 / 43.9 | 18.0 / 28.6 |
| `getDoc`, one invocation process, NFSv4 | 98 / 212 | 86 / 180 |
| `findDocs`, shared session, NFSv3 | 201 / 284 | 144 / 157 |
| `findDocs`, shared session, NFSv4 | 896 / 1,803 | 488 / 914 |
| `getDoc`, shared session, local APFS | 0.74 / 0.97 | 0.49 / 0.64 |

NFS call counts come from `nfsstat -c` with other sessions' traffic
subtracted, so they are rough. After the change the document read is still
55 to 63% of a `getDoc` on NFS (an existing id against a deleted one in the
same session), so a temp-disk hit would still save about half of a
shared-session `getDoc`. Whether to build the temp tier is the user's next
decision. The rest of the calls are the EFS profile's own identity checks
(each file is opened again after it is read to prove it was not swapped,
and each document's manifest binding is verified); they are a separate,
safety-sensitive change.

## Slice S0: collection stamp (built 2026-11-14)

`NativeCollection::collection_stamp()` returns a `CollectionStamp`: the
generation state (with the write-ahead log end), the `FileStamp` of the
opened `state.json`, and the collection's `CollectionIncarnation`. A new
files-layout collection gets a fresh incarnation, a TTID clock tick in the
16-byte file `.fylo-transactions/<namespace>/<name>/incarnation` (magic
`FYLN`, the tick, a CRC-32C); older releases ignore the file, and a missing
or damaged one reads as unknown. `is_cacheable()` is true only for a
files-layout collection of a folder root at a stable generation that has a
`state.json` or a known incarnation. No reader uses the stamp yet, so no
read path changed.

On Windows the `FileStamp` has no change time (std offers none without
unsafe code, which ADR 0004 confines to two modules); it uses a hash of
the volume serial number and file index, the size, and the last-write and
creation times. `auto` never turns the temp tier on there.

Tests (`crates/fylo-storage-native/tests/collection_stamp.rs`): an
inserted, a changed and a deleted document, a collection dropped and
created again, the same `state.json` written again by rename and in place,
a restored commit, a rebuild and a reshard each give another stamp; a
collection without its incarnation is still cacheable by its `state.json`;
one with neither file, and a segment collection, are not; the EFS profile
behaves the same.

## Slice S1: query match lists in the hot set (built 2026-11-14)

An exclusive session's hot set keeps, per collection and generation, the
complete match list of each unsorted `findDocs` query it evaluated to the
end: the ids of every matching document, in order. The key is the query
without its `$limit`, so one list answers every limit by its first ids. A
query cut short by its limit is not kept. A list is admitted only after the
read's stable check, shares `--hot-set-bytes` (decision D4), its clock and
epoch, and is cleared with the hot set before and after every write frame.
A list larger than the whole budget is answered and not kept (counted as a
refusal). Nothing survives the session: a later session, or one opened
after another process wrote the root, starts empty. The handshake's
`hotSet` gains `queries`, `queryHits` and `queryMisses`.

Measured (2026-11-14, busy Mac; 500 documents of about 1 KB, ten queries
of 50 matches each, warm = five repeats; p50 in ms, S0 build against S1):

| Root | warm `findDocs` before | warm `findDocs` after | with `$limit` 5 |
| --- | --- | --- | --- |
| local APFS | 16.9 | 13.8 | 2.1 / 2.2 |
| NFSv3, EFS profile | 15.8 | 9.7 | 2.1 / 2.2 |
| NFSv4, EFS profile | 11.5 | 23.3 (noise) | 2.3 / 2.4 |

Every repeat was a query hit (100 of 100). The gain is small because an
exclusive session's warm query already reads nothing from the root: the
hot set holds the index and the documents, and most of the time is spent
building the 50-document answer. S1 removes only the planning and the
predicate checks. It was merged because the user asked for this memory
tier by name; the network cost is the temp tier's target (S2 to S5).

Tests: `crates/fylo-engine/tests/hot_set_queries.rs` (every limit of a
kept query equals storage, a cut query is not kept, changed, deleted and
inserted documents and a re-created collection after a clear, the budget
bounds the lists, an over-budget list is not kept, a zero budget keeps
none) and `crates/fylo-machine/tests/hot_set_query_writes.rs` (insert,
update and delete frames are seen by the next query in an exclusive
session; a new session sees what another writer did after the last one
ended; query hits are counted).

## Decisions (the user, 2026-11-14)

- **D1. Default mode: `auto`.** The tier turns itself on only under the rules
  of 4.1 (EFS profile, local cache folder on another device, no speed probe).
  `--temp-cache off` turns it off.
- **D2. Validation: `collection`.** No file call while the collection is
  unchanged; out-of-band edits are not seen, as with the hot set. No strict
  mode.
- **D3. Memory tiers: exclusive sessions only.**
- **D4. One memory budget:** query results share `--hot-set-bytes`.
- **D5. Query results never on disk.**
- **D6. Order of work: cut FYLO's NFS calls per read first** (PLAN slice R),
  measure again, and build the temp tier (S0, S2 to S5) only if it still
  pays. The user decides that after the new numbers.

## Rollback

`--temp-cache off` turns the temp tier off without a release, and
`--hot-set-bytes 0` the memory tiers. `fylo cache clear` removes every temp
entry. Removing the code leaves only the incarnation files, which older and
newer releases ignore.

## Verification (planned)

Per slice, in short:

- differential tests: every read operation with tiers on equals tiers off,
  on plain, encrypted and `.vdbx` roots;
- cross-process tests for every row of 3.3, with a forced reused generation;
- crash tests (kill during an entry write), wipe tests, budget tests;
- permission tests per OS (owner, mode, links, Windows DACL);
- opt-in network tests on NFS mounts with the EFS profile
  (`FYLO_TEST_NETWORK_ROOTS`, as ADR 0029);
- a benchmark gate on NFS here, and a real Lambda + EFS gate in PaaS before
  `auto` becomes the default.

## Slice S2: temp cache folder manager (built 2026-11-14)

`fylo-storage-native` gains `TempCache` and its parts (`temp_tier/`): the
per-OS location (4.2), `PrivateFolder` (creation with mode `0700`, the
checks of an existing folder, and no-follow opens of files below it,
relative to the open folder handle on Unix), `CacheSweep` (4.5: least
recently used entries first down to 90% of the budget, partial files older
than 10 minutes, `sweep.stamp` with the last totals), and `CacheBudget`
(256 MiB, at most half the free space). The machine protocol reports
`capabilities.tempCache` (section 7), and the CLI takes `--temp-cache`,
`--temp-cache-bytes` and `--temp-cache-dir`, `fylo.tempCache` in
`application.json`, and `fylo cache status|clear`.

There is no read path yet, so the tier is never active: when every rule of
4.1 passes, the reason is `read path not built`. `auto` creates nothing for
a root that is not opened with the EFS profile.

**Windows.** Setting and checking a protected DACL needs Win32 calls the
standard library does not offer, so `temp_tier/windows_folder.rs` is a new
unsafe boundary under ADR 0004, decided in ADR 0031 and listed in
`docs/security/unsafe-inventory.md`.

## Slice S3: temp tier read path (built 2026-11-15)

A session that starts the tier (`TempCache::start`) gets a
`TempCacheTier` when every rule of 4.1 passes, and hands it to the roots it
opens with `RootOpenOptions::with_temp_tier`. In this slice only roots
opened with the **native** profile read through it, so it is active only
under `--temp-cache on`; an EFS-profile root still reports `read path not
built` (slice S4), and `auto` keeps every local root on the root.

**Where the stamp comes from.** A collection handle that reads through the
tier reads `state.json` from an open handle on every generation read (the
reads `read_stable` and the hot set already make before and after each
attempt) and records the digest of the collection stamp (3.1) in the
handle's read window. The incarnation is read once per handle. No other
file call is added. A clone of a handle starts with an empty window, and a
handle whose last generation read failed, saw `writing`, or saw a stamp
that is not cacheable has no stamp, so its reads go to the root.

**Hit, revalidation, miss.** A document read looks up its entry (4.3): an
entry read under the recorded stamp is used with no file call on the root;
one read under another stamp is used only when the document file the shard
lookup opens is the file the entry names, with the same file stamp (3.2),
and is then kept again under the current stamp. Anything else (a missing
entry, a damaged, foreign or stale one) is a miss: the entry is deleted and
counted as `rejected`, and the read goes to the root. A hit is handed to
the same decode path as a root read (`build_record`), so decryption,
schema migration and every engine check still run.

**Writers never read through it.** A writer's root (`NativeWriteRoot`)
drops the tier: a writer changes documents after its last generation read,
so its handle's recorded stamp would no longer describe the files it reads
back (for example the "after" index keys of an SQL `UPDATE`).

**What a same-stamp hit does not re-check.** A hit under the current stamp
makes no file call on the root, so it does not repeat the per-file checks of
a root read (the content checksum attribute, the no-follow open, the
`legacyAccess` notice). It relies on the stamp: every FYLO change moves it.
A file changed, swapped or deleted by another tool while `state.json` stays
the same is not seen, the limit 3.4 accepts (decision D2). A stamped
generation read that fails clears the window and answers as a handle
without the tier does, so the tier adds no error of its own.

**Admission.** A document read from the root waits in the handle's read
window. The next generation read writes only the entries read under a stamp
equal to the one it sees, and drops the rest, so an entry reaches the disk
only after the read passed its closing check, under a stamp no commit came
between. An entry is written as a partial file of the process and renamed
over its name (4.4); the entry holds the stored payload, the document's
path below `docs/`, its modification time, both stamp digests and two
CRC-32C checksums. The document's file stamp is taken right after the file
is opened and must be the same after it is read, so an entry never pairs
bytes with a later file stamp. A failed write pauses this process's writes
until the next sweep is due (at most 30 seconds); partial names carry the
pid, the time and a serial, so a restarted process with the same pid never
meets an old partial file. A payload over 1/64 of the budget is not kept, and a
read attempt holds back at most a tenth of the budget.

**Tests.** `fylo-machine/tests/temp_tier_reads.rs` (17): every read equals
the read without the tier, cold and warm, on plain and encrypted
collections; encrypted values never appear in any cache file; a write, a
delete, a drop and re-create, a rebuild and a `state.json` put back to an
older generation by another session are all seen; truncated, flipped,
foreign-document, foreign-collection and older entries are refused;
oversized documents are not kept; `auto` on a local root and `.vdbx` roots
create nothing. `fylo-cli/tests/temp_tier_processes.rs` (3): separate
`fylo` processes see each other's writes, and a process stopped in the
middle of an entry, or before renaming it
(`FYLO_RUST_TEMP_CACHE_FAILPOINT`), leaves only a partial file.

**What it does not do yet.** No NFS gain can be measured in this slice: the
native profile refuses a network root (ADR 0029), so the tier's real target,
an EFS-profile root, comes with S4. **S4 owns the 30% go rule** (the
coordinator's decision, 2026-11-15): it is the first slice that can read an
EFS-profile root, so it measures the NFS gain and stops if it is under 30%.
For S3 the check is that a local root in `auto` reads as fast as before.

**Measured on a local root** (2026-11-21, APFS, release build, 1,000
documents of about 1 KB, two shared `exec --loop` processes one after the
other, each reading 200 random documents and 10 queries of 50 matches;
p50 in ms, two rounds; Mac load 10 to 17):

| Mode | `getDoc`, first process | `getDoc`, second process | `findDocs`, first | `findDocs`, second |
| --- | --- | --- | --- | --- |
| `off` | 0.84 / 0.89 | 0.71 / 0.78 | 18.5 / 21.2 | 17.7 / 18.8 |
| `auto` (inactive: root is local) | 0.76 / 0.68 | 0.82 / 0.62 | 17.4 / 18.4 | 19.5 / 17.5 |
| `on` | 1.28 / 1.42 | 0.94 / 1.05 | 35.8 / 39.4 | 21.8 / 19.3 |

`auto` on a local root is the same as `off` within noise (AC17). With `on`
a local root is slower: the first process writes every entry (596 writes),
and the second answers all 700 reads from the tier but each hit still costs
about 0.25 ms more than reading the local disk. This is why `auto` keeps
local roots off; on NFS a hit saves about 18 ms (section "Slice R"), which
S4 measures.

## Slice S4: EFS-profile roots and `auto` (built 2026-11-21)

A root opened with the EFS profile now reads through the temp tier, and
`auto` turns the tier on for it when rules 2 to 5 of 4.1 hold. The reason
`read path not built` is gone.

**Entries (format v2).** An entry also keeps the record's verified manifest
(the `.fylo-attrs` file as the root read decoded it), after the payload and
under the same body checksum; it is empty in the native profile. Version 1
entries (slice S3) are refused and replaced.

**Hit.** As 3.2, and in the EFS profile the hit also checks, from the entry
alone, that the manifest binds the payload to this record (the
record-identity value: format, namespace, collection, id and the payload's
SHA-256), as a root read does (ADR 0010). It makes no file call on the
root.

**Document stamp.** In the EFS profile it is the digest of the payload's
and the manifest's file stamps. Both are taken right after each file is
opened, before it is read, and must be the same once the read passed its
checks, or no entry is kept. Revalidation opens both files on the root and
compares the digest.

**One place for names.** The tier keys a collection's entries by
`NativeCollection::cache_name` (namespace and name) and its stamp reads
`generation_state_path` and `incarnation_path`, so a later change to what
names a collection touches only those helpers. `auto`'s first rule reads
one function, `TempCacheRoot::is_remote_or_sidecar` (today: the root was
opened with the EFS profile), so a later layout change can take that fact
from another source there alone. Two values depend on today's root layout
and fail safe if it changes (old entries are refused and the tier refills):
an entry's path below the collection's `docs/` folder, and the record
identity (namespace, collection, id) the EFS hit check rebuilds.

**The 30% go rule** (measured 2026-11-21, release build, EFS-profile root,
1,000 documents of about 1 KB, two shared `exec --loop` processes one after
the other, each reading 200 random documents and 10 queries of 50 matches;
p50 in ms, two rounds; Mac load 18 to 22; the `auto` rows are the second
process, all reads answered from the tier):

| Root | `getDoc` off | `getDoc` auto | `findDocs` off | `findDocs` auto |
| --- | --- | --- | --- | --- |
| ANVIL, NFSv3 over Thunderbolt | 18.6 / 14.8 | 9.9 / 10.2 (35 to 45% less) | 416 / 357 | 111 / 66 (70 to 80% less) |
| CITADEL, NFSv4 over Ethernet | 27.4 / 17.5 | 13.8 / 9.0 (about 50% less) | 1,093 / 801 | 185 / 163 (about 80% less) |

Both pass the 30% rule. A hit still costs about 9 to 14 ms on NFS: the
per-request work (the lease, `state.json`, the collection open) remains.
A real Lambda + EFS run is still needed before this ADR is Accepted.

**Tests.** The machine tests of S3 now also run on an EFS-profile root (16
more); the CLI tests add an EFS process test, an `exec --invocation` test,
and an opt-in test on real network mounts
(`FYLO_TEST_NETWORK_ROOTS`, `--run-ignored only`) where the default `auto`
turns the tier on; it passed on both mounts above. Unit tests cover the
binding check and entry format v2.

## Slice S5: index bytes in the temp tier (built 2026-11-21)

**What.** A files-layout collection's stored index (`index/keys.snapshot`
and `index/keys.wal`, exactly as read from the root) is kept in one entry
per collection, `<root key>/<collection key>/index.fyc` (kind 2, the same
header and checksums as a document entry), under the digest of the
collection stamp it was read at. A newer stamp replaces it in place, and an
entry of an older stamp is removed when it is found (a stamp never comes
back). It is used only under an equal stamp, with no revalidation, and still
goes through the content-verified index cache, so a hit costs a local read
instead of two files on the root. A range read (`index_snapshot_in_ranges`)
uses the entry too; on a miss it reads the whole snapshot once, when it is
small enough to keep, and keeps it. Larger indexes are read in place as
before.

**Every index change moves the stamp or keeps the index logically the
same.** Writers never read through the tier. Transaction writes change the
index while `state.json` says `writing`, which is not cacheable; logged
writes publish their log position before they append to the index log;
`rebuildCollection`, `reshardCollection`, and the word-field changes (`set_word_fields`, `repair_word_fields`) run
a rebuild, which moves the generation; compaction exists only for the
segment layout, which never uses the tier. A log checkpoint keeps the index
logically the same but replaces the snapshot and then empties the log, so a
reader could see the old snapshot beside an emptied log. Every index read
therefore reads the log first and the snapshot second: replaying a log over
a snapshot that already holds it gives the same keys, so a read during a
checkpoint never misses one (a review found this before merge; it also
fixes the same race for reads without the tier).

**Measured** (2026-11-21, ANVIL NFSv3, EFS-profile root, same benchmark as
S4; `auto`, second process): repeated `findDocs` p50 52 ms (S4: 66 to 111
ms; `off`: 300 to 333 ms); `getDoc` unchanged (9.2 to 9.7 ms). There is no second NFS measurement:
HAVEN and ATLAS are production volumes and are not used for tests (the
user's rule, 2026-11-21); a later measurement needs a test export on ANVIL
or HANGAR.

**Tests.** `fylo-machine/tests/temp_tier_reads.rs` adds index tests on
native and EFS roots: queries read the index from the tier; a document
written after the index was kept is found; a damaged index entry is refused;
an index over its share of the budget is not kept; and `$match` answers the
same after word keys are added (a rebuild) once the index was kept. Unit
tests cover the index entry format.
