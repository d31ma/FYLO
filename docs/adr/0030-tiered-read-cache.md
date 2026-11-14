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
