<p align="center">
  <strong style="font-size: 2em;">FYLO</strong><br/>
  <em>A single-binary document store with zero-payload prefix indexes and language shims for Python, Ruby, Node, Go, Rust, C#, Java, PHP, and Dart, plus local-first browser, mobile (iOS/Android), and Flutter clients.</em>
</p>

<p align="center">
  <a href="https://github.com/d31ma/Fylo/releases/latest"><img src="https://img.shields.io/github/v/release/d31ma/Fylo?label=latest&color=blue" alt="Latest Release"></a>
  <a href="https://github.com/d31ma/Fylo/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/d31ma/Fylo/ci.yml?branch=main&label=CI" alt="CI Status"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/license-MIT-green" alt="License: MIT"></a>
  <a href="https://github.com/d31ma/Fylo/stargazers"><img src="https://img.shields.io/github/stars/d31ma/Fylo?style=flat" alt="GitHub Stars"></a>
</p>

<p align="center">
  <strong>One canonical file per document. Key-only indexes. No monolithic caches.</strong><br/>
  A single <code>fylo</code> binary, driven from 9 languages via thin shims.
</p>

---

## Table of Contents

- [Why FYLO?](#why-fylo)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [Browser access](#browser-access)
- [Configuration](#configuration)
- [CRUD Operations](#crud-operations)
- [Querying](#querying)
- [Schema Versioning](#schema-versioning)
- [Encryption](#encryption)
- [Access Control](#access-control-application-identities)
- [Serverless Queue](#serverless-queue)
- [Remote Access](#remote-access)
- [CLI & Machine Interface](#cli--machine-interface)
- [Recovery & Rebuild](#recovery--rebuild)
- [EFS metadata candidate](#efs-metadata-candidate)
- [Limitations](#limitations)
- [License](#license)

---

## Why FYLO?

FYLO trades complexity for clarity. Documents are plain JSON files on disk. Indexes are zero-byte key entries that accelerate queries without duplicating data. If the index ever drifts, FYLO rebuilds it from the documents — the files are always the source of truth.

| Principle                    | Implementation                                                                       |
| ---------------------------- | ------------------------------------------------------------------------------------ |
| **Documents are truth**      | One `.json` file per document, sharded by trailing TTID creation characters          |
| **Indexes are accelerators** | Zero-payload prefix keys in a sorted catalog file                                    |
| **Rebuildable, not sacred**  | `fylo.<collection>.rebuild()` reconstructs indexes from data                         |
| **Self-contained core**      | Rust storage, query, recovery, and machine protocol in one native executable         |
| **Filesystem-only**          | One local engine; mount, snapshot, and back up the root with filesystem-native tools |
| **Brokerless queues**        | Durable local topics, leases, retries, consumer groups, and dead letters             |
| **Browser: local-only**      | OPFS engine in the browser — each device owns its own store, fully offline           |

---

## Quick Start

FYLO ships as a single self-contained `fylo` binary via
[GitHub Releases](https://github.com/d31ma/Fylo/releases) — not npm. Install it
onto your `PATH`. [CHEX](https://github.com/d31ma/CHEX) schema validation and
[TTID](https://github.com/d31ma/TTID) identifiers are built in, so there is
nothing else to install:

```bash
# fylo (macOS / Linux)
curl -fsSL https://github.com/d31ma/Fylo/releases/latest/download/install.sh | sh
```

Set `FYLO_VERIFY_PROVENANCE=1` before running the installer to require signed
GitHub artifact provenance in addition to the release checksum. This opt-in
requires an authenticated GitHub CLI and fails closed before installation; see
the [release provenance runbook](docs/operations/release-provenance.md).
Maintainers can reproduce the pull-request gates from the
[CI qualification runbook](docs/operations/ci-qualification.md).

Then drive it from your language through a thin, dependency-free
[client shim](clients/). Node example:

```ts
import { Fylo } from './clients/node/fylo.mjs' // spawns the `fylo` binary

const db = new Fylo('/mnt/fylo')
await db.createCollection('users')

const id = await db.putData('users', { name: 'Ada', role: 'admin' })
const doc = await db.getLatest('users', id)
console.log(doc) // { <id>: { name: 'Ada', role: 'admin' } }

await db.close()
```

Shims ship for Node, Python, Ruby, Go, Rust, C#, Java, PHP, and Dart (see
[`clients/`](clients/)). Browsers, mobile apps (iOS/Swift, Android/Kotlin), and
Flutter use local-only clients that embed the engine on-device (OPFS) — see
[Browser access](#browser-access) and [`clients/`](clients/).

### Web Applications

The marketing website and Fylo Explorer are separate Tachyon applications.
Run either one from its own root:

```bash
git clone https://github.com/d31ma/Fylo.git
cd Fylo/website && bun install --frozen-lockfile && bun run serve
cd ../explorer && bun install --frozen-lockfile && bun run serve
```

---

## Architecture

### Three artifacts, one engine

The same engine ships three ways. They share the source, the on-disk format,
and the NDJSON machine protocol, and differ only where a platform cannot supply
something — the handshake capabilities report which.

| Artifact            | Target                   | Storage                         | Driven by                         |
| ------------------- | ------------------------ | ------------------------------- | --------------------------------- |
| `fylo`              | native                   | `std::fs`                       | a spawned process                 |
| `fylo.wasm`         | `wasm32-wasip1`          | `std::fs` over WASI preopens    | any WASI runtime                  |
| `fylo-browser.wasm` | `wasm32-unknown-unknown` | a host table the embedder fills | a browser Worker, or any embedder |

`fylo` and `fylo.wasm` are **interchangeable**: a shim spawns either, writes
NDJSON to stdin, reads NDJSON from stdout, and cannot tell which it got.
WebAssembly is not a browser technology — `wasm32-wasip1` has real files and
real stdio, so the same engine runs under any WASI runtime on a server.

The `fylo-browser.wasm` build owns its storage rather than delegating to a
JavaScript engine, which is why an embedder can carry version control, schema,
and encryption into a browser with it. The browser loader that apps use today
(`fylo.js` / `fylo-web.mjs`) and the mobile clients that host it are a separate
JavaScript engine: in this release they do not support raw-file buckets,
schemas, or field encryption (see [Browser access](#browser-access)). See the
[artifact matrix](docs/releases/artifact-matrix.md) for what each artifact can
and cannot do, and why.

Document collections live under `.collections`; buckets (for raw files) live
under `.buckets`. The two are structurally identical — only the top-level
directory differs. Collections hold `Record` values; buckets hold `Blob`/`File`
values:

```text
<root>/.collections/<collection>/   ← documents (Record)
<root>/.buckets/<bucket>/           ← raw files (Blob / File), same internal layout:
  docs/                    ← one file per document/object (TTID-named)
    W/                     ← shard: trailing characters of the creation segment
      4UUB32VGUDW.json
  .deleted/                ← soft-deleted payloads (hidden sibling of docs/)
    W/
      4UUB32VGUDW.json
  index/                   ← local filesystem prefix index catalog
    manifest.json          ← format version marker
    keys.snapshot          ← sorted index keys, mmap'd for O(log n) lookup
    keys.wal               ← append-only mutation log (compacted once it outgrows the snapshot, or 16 MiB)
                             (the two together hold at most 240 MiB of keys; see Limitations)
  events/                  ← written only by the browser engine (fylo-web.mjs)
    <collection>.ndjson    ← its change journal; the native engine never writes it
  locks/                   ← advisory file locks
```

`events/` belongs to the JavaScript browser engine. The native engine and
`fylo-browser.wasm` neither write nor read it: their change feed comes from
the segments layout's location table (see [Change feed](#change-feed)). When
the browser engine opens a root the native engine also writes, it adds the
native writes to its journal before it answers `changes`.

FYLO keeps the logical transaction journal outside collection trees so it is
not mistaken for a document, indexed, versioned, or copied as record payload:

```text
<root>/.fylo-transactions/<namespace>/<collection>/
  state.json               ← stable/writing generation marker
  log/segment.log          ← write-ahead log since the last checkpoint
  <transaction-id>/
    transaction.json       ← operation, commit phase, before-image manifest
    before/                 ← linked/copied files needed for rollback
```

Document mutations commit through the collection's write-ahead log
([ADR 0013](docs/adr/0013-collection-write-ahead-log.md)): one checksummed
append and one flush per commit, after which the document, its metadata and
the index are brought up to date without further flushes. Recovery replays the
log; a checkpoint flushes the applied files and removes the log, leaving the
layout below exactly as an older release wrote it. Raw-file collections and
structural operations (create, drop, reshard, rebuild, version control) use
the transaction journal.

A document collection can instead keep its documents in append-only segment
files ([ADR 0015](docs/adr/0015-append-only-document-segments.md)): set
`fylo.layout` to `"segments"` for new collections, or move one with
`fylo layout <collection> --to segments`. Each segment record holds one
document's exact JSON and all its metadata, the segment being appended to is
the write-ahead log, and a read is one positioned read:

```text
<root>/.collections/<collection>/
  segments/
    0000000001.seg         ← sealed, immutable
    0000000002.seg         ← active: commits append here
    state.slots            ← generation slots, updated in place
    locations.snapshot     ← id → record location (derived)
    locations.wal
  index/                   ← prefix index, unchanged (derived)
  docs                     ← a plain file, so older releases refuse the collection
```

`fylo export` writes the one-file-per-document tree back out byte for byte,
and `fylo compact` reclaims the space old versions take.

Every transactional collection mutation publishes an odd `writing` generation
before changing records and the next even `stable` generation after commit.
Readers materialize against one stable generation and retry if it changes. On
startup or first access, an active transaction is rolled back; a transaction
with a durable committed marker is rolled forward. Index files remain derived
and are rebuilt when recovery needs them.

A collection's kind is recorded once in its catalog descriptor
(`.fylo-catalog/collections/<name>.json`); names are unique across both
namespaces, so `db.<name>` is unambiguous.

When document version control is initialized, FYLO also writes hidden repository
metadata beside `.collections`:

```text
<root>/.fylo-vcs/
  HEAD                     ← active branch ref
  refs/heads/<branch>.json ← branch metadata and latest commit id
  branches/<branch>/       ← hidden working tree for non-main branches
    .collections/...
  commits/<commit-id>/     ← commit metadata and root tree hash
  objects/<hh>/<hash>      ← verified content-addressed blobs and tree nodes
  staging/<transaction>/   ← durable restore/merge recovery transactions
    transaction.json
```

`main` uses the root `.collections` tree. Other branches use hidden working
trees under `.fylo-vcs/branches/`, so `fylo checkout -b feature` isolates
subsequent reads and writes without changing the base document layout. Commits
reference content-addressed trees. Unchanged blobs and subtrees are shared by
hash, and object hashes are verified before restore or merge. Restore and merge
materialization uses a durable staging transaction; startup deterministically
rolls an interrupted swap backward or forward before collections are opened.

**Index keys** are slash-delimited — field path, kind, value, doc ID:

```text
name/f/alice/4UUB32VGUDW
name/r/ecila/4UUB32VGUDW
age/n/c03e000000000000/4UUB32VGUDW
age/nr/3fc1ffffffffffff/4UUB32VGUDW
```

- `f` = forward prefix (LIKE 'ali%')
- `r` = reversed prefix (LIKE '%ice')
- `n` / `nr` = sortable numeric (range queries)
- `eq` = exact match
- `g3` = trigram (contains queries)
- `w` = a normalized word of a word field (`$match`), only for fields named by
  `setWordFields`
- `wv` = a word witness: one per `eq` key of a word field, so FYLO can tell
  when a release without word fields wrote to it

### Index

The prefix index is always **local**: an mmap'd sorted file + WAL (binary
search, zero JS heap). Documents are truth; the index is a local accelerator
that can always be rebuilt from them. Remote copies and sync hooks are never in
the query or transaction path.

---

## Browser access

The browser client is **local-only**: a bundled OPFS engine (`fylo-web.mjs`,
released as an asset) that reads and writes a browser-local store directly.
There is no network access and no backend — each browser (and each mobile app
hosting the engine in a WebView) owns its own database.

Add the version-pinned loader to the document head:

```html
<script src="https://d31ma.github.io/FYLO/version/26.33.02/fylo.js"></script>
```

```ts
const db = await Fylo.open()

await db.users.create()
const id = await db.users.put({ name: 'Ada', role: 'admin' })
await db.users.put(id).metadata({ source: 'browser', reviewed: false })
const metadata = await db.users.get(id).metadata()
const doc = await db.users.latest(id)
```

Create a collection before the first `put`; writing to a collection that does
not exist fails with `Collection not found`.

**Not supported in the browser or on mobile in this release:** raw-file buckets
(`kind: 'file'`), schemas, and field encryption. The browser loader
(`fylo.js` / `fylo-web.mjs`) and the iOS, Android, and Flutter clients that
embed it fail such requests with an error instead of ignoring them. Use the
native `fylo` binary, through any binary-backed client, for those features.

### An encrypted `.vdbx` in the browser, with a key from the page

The single-file engine (`fylo-browser.wasm` with `dist-web/vdbx-worker.js`)
opens and writes an encrypted `.vdbx` database with a key the page supplies
([ADR 0025](docs/adr/0025-browser-vdbx-encryption-with-a-host-key.md)). It
needs no cross-origin isolation (no COOP or COEP headers).

```js
import { SingleFileDatabase } from './vdbx-client.mjs'

// Either the secret itself, at least 32 bytes (the same secret native
// `fylo.encryption.key` names, so files move between native and the browser)…
const key = new TextEncoder().encode(secretFromYourServer)
// …or a non-extractable HKDF CryptoKey, so the secret never sits in script.
const hkdfKey = await crypto.subtle.importKey('raw', key, 'HKDF', false, ['deriveBits'])

const db = await SingleFileDatabase.open(
    { kind: 'opfs', name: 'device.vdbx' },
    { workerUrl, moduleUrl, encryption: { key: hkdfKey }, create: true }
)
```

- `create: true` creates a missing OPFS working copy, **always encrypted**
  with that key. Without a key the browser creates nothing
  (`EBROWSER_UNSUPPORTED`).
- A database created this way is **unprotected**: it has no identities and no
  access policy (ADR 0021). Anyone with the key reads and writes all of it.
  This is a recorded exception to ADR 0021's "protected by default" rule,
  agreed by the user on 2026-11-11.
- A key shorter than 32 bytes, or a wrong key, fails with `EKEY`.
- An AES-GCM `CryptoKey` is refused with `EBROWSER_UNSUPPORTED`: using it for
  every record would need a cross-origin isolated page.
- `encryption` together with `identity` is refused; a protected database uses
  its identity's keys.
- Keep the key away from the device's storage. A key kept in `localStorage`
  or beside the file protects nothing. The folder engine (`fylo.js`) still
  refuses encryption.

Use `https://d31ma.github.io/FYLO/version/latest/fylo.js` to track the newest
successful release. Direct ESM consumers can import
`https://d31ma.github.io/FYLO/version/26.33.02/fylo-web.mjs` instead.

Treat the loader as executable supply-chain input: it runs with the embedding
origin's authority, including access to that origin's FYLO data. Production
applications should use the immutable version URL, not the mutable
`version/latest/` alias. For higher-assurance deployments, download the loader
and every adjacent module, worker, and Wasm asset from the matching release,
verify them against `SHA256SUMS`, and serve them from your own origin. Restrict
the permitted script, module, worker, and asset origins with CSP; integrity on
`fylo.js` alone does not cover the modules it imports dynamically.

The browser index scanner also has an opt-in Wasm prototype. It keeps the
existing OPFS snapshot + WAL format: Wasm scans the immutable snapshot inside
the worker, while JavaScript applies live WAL additions/removals and falls back
automatically if the module cannot load. Build all adjacent browser assets with:

```bash
bun run build:web:wasm
```

The build re-encodes `fylo-browser.wasm` with binaryen's `wasm-opt` (the
version in `.binaryen-version`). It runs no optimization pass: it only writes
the module's numbers in their shortest form, which the Rust linker pads. Install
it with `brew install binaryen` or `sh scripts/install-binaryen.sh`.

The page bundle `dist-web/fylo.mjs` is built with Bun's `--minify-whitespace`:
spaces, line breaks and comments are removed, and names and code are kept.

```ts
const db = createBrowserClient({ storage: 'opfs', worker: true, wasm: true })
await db.ready()
```

Chromium can instead mount a user-selected FYLO root directly:

```ts
const handle = await showDirectoryPicker({ mode: 'readwrite' })
const db = createBrowserClient({
    storage: { type: 'fsa', handle, access: 'readwrite' },
    worker: true,
    wasm: true
})
await db.ready()
```

Set `access: 'overlay'` for read-only exploration. Reads fall through to the
selected directory while generated indexes, journals, and other writes remain
in memory.

Pass `wasm: { url: 'https://example.test/fylo-index.wasm' }` to override the
adjacent module URL. `collection.inspect()` reports `indexAcceleration` as
`active`, `fallback`, or `off`; the feature remains opt-in while the integration
prototype is benchmarked on representative stores.

### Fylo Explorer

The Explorer is a browser UI over a **real FYLO root on your disk** — no
server, no protocol. It opens the folder through the File System Access API:
pick the root once in the OS dialog, and later visits reopen it automatically
(the handle persists; Chromium's "Allow on every visit" makes it zero-click).
Chromium-only — Firefox and Safari do not implement real-folder access.

```bash
cd explorer && bun run seed     # optional: demo root at explorer/db (gitignored)
cd explorer && bun run serve    # http://localhost:8080
cd explorer && bun run bundle   # production bundle at explorer/dist/web
```

Every GitHub release also includes a versioned, checksum-covered
`fylo-explorer-<CalVer>.zip` containing the contents of `explorer/dist/web`.
Extract it directly into the root of a static host:

```bash
VERSION=26.33.02
curl -fLO "https://github.com/d31ma/Fylo/releases/download/v${VERSION}/fylo-explorer-${VERSION}.zip"
mkdir "fylo-explorer-${VERSION}"
unzip "fylo-explorer-${VERSION}.zip" -d "fylo-explorer-${VERSION}"
python3 -m http.server 8080 --directory "fylo-explorer-${VERSION}"
```

`index.html` is at the ZIP root. Production hosts should use a dedicated HTTPS
origin, serve the extracted tree at `/`, and preserve the generated MIME types
(especially `application/wasm` for `.wasm` files). `localhost` is suitable for
local evaluation.

- **Read-only by default.** Reads go straight to the folder; the engine's own
  writes (index rebuilds, journals) land in an in-memory overlay, so the root
  is never modified — indexes are accelerators, rebuilt in RAM per session.
- **Browse and query.** A sidebar split into **Collections** (documents) and
  **Buckets** (files), a document list, a JSON viewer, and a filter bar accepting
  SQL `WHERE` expressions (`role = 'admin' AND age >= 30`). Buckets browse as macOS-Finder-style Miller
  columns built from the plain-text key index (object keys live in xattrs, which
  browsers can't read — the index mirror makes them visible anyway), with image
  preview and byte download. A SQL console runs `SELECT` statements read-only
  (full SQL once writes are enabled).
- **Writes are opt-in.** "Enable writes" re-arms the folder as readwrite and
  drops the overlay: create/edit/delete/restore go through the engine into the
  real root (compat is tested in both directions — desktop reads what the
  browser wrote and vice versa). Buckets accept uploads into the current folder;
  the bytes and a `key` index entry are written immediately, but the
  key/checksum xattrs can't be set from a browser — a desktop `rebuild` or
  `verify` re-derives them. A banner warns when the root has live lock files;
  there is no cross-process locking from a browser, so concurrent writes are
  last-writer-wins.

The Explorer rejects oversized work before reading it into memory:
previews are limited to 32 MiB, imports to 16 MiB and 10,000 records, exports
to 64 MiB and 10,000 records, and bucket uploads to 64 MiB. Use the CLI for
larger operations. Explorer is a standalone Tachyon app under `explorer/`; it
builds directly at `/` for its dedicated origin and is not part of the marketing
website bundle. Tachyon's generated component runtime currently uses `eval` for
bindings and event dispatch, so the deployment CSP must retain `unsafe-eval`
until Tachyon offers a CSP-safe compiler mode. FYLO's own runtime import does
not use `eval` or `new Function`.

For production deployments, serve Explorer from a dedicated origin that hosts
no unrelated application code. A CSP limits what a compromised page can load,
but browser directory-handle grants and origin storage are shared by every
script on the same origin; the application cannot enforce DNS/hosting
separation in code. The marketing site may keep linking to that origin, but it
should not share its JavaScript execution boundary.

---

## Configuration

Native runtime settings live in `application.json` under the `fylo` key. Each
underscore in a former `FYLO_*` setting becomes a nested key; for example,
`FYLO_SHARD_WIDTH` becomes `fylo.shard.width`. Environment settings are no longer
implicit overrides.

```json
{
    "fylo": {
        "root": "./data",
        "schema": "./schemas",
        "strict": true,
        "shard": { "width": 1 },
        "encryption": { "key": "${FYLO_ENCRYPTION_KEY}" },
        "cipher": { "salt": "${FYLO_CIPHER_SALT}" }
    }
}
```

Keep only secrets in an adjacent `.env`, and explicitly reference them using a
whole-value `${NAME}` string. Injected environment secrets take precedence over
the adjacent file; unrelated variables are ignored. Omit encryption and cipher
entries when they are not needed. [application.example.json](application.example.json)
is a minimal starting point that does not require secrets.

The CLI discovers `application.json` in its working directory, or accepts
`--config /path/to/application.json`. Every binary-backed SDK shim can select a
configuration file for its own FYLO child:

```js
const db = new Fylo('/mnt/fylo', {
    binary: '/opt/fylo/bin/fylo',
    config: './config/mail/application.json'
})
```

Explicit constructor roots and CLI flags win over JSON settings, followed by
engine defaults. Paths in JSON resolve relative to that file. An absent default
file uses defaults; an explicitly selected missing file, invalid configuration,
or unresolved secret reference fails initialization. Loading configuration never
mutates the process environment.

Treat configuration and binary paths as trusted bootstrap input. Keep secret
files private and out of source control. Browser/mobile clients continue using
explicit storage-host options and never implicitly load local JSON or `.env`
files. See the [configuration and migration guide](docs/configuration.md) and
[SDK shim examples](clients/README.md).

## CRUD Operations

Collections must be created explicitly before reads, writes, updates, deletes,
imports, rebuilds, or queries. Use `db.<collection>.inspect()` when you want a
safe existence check; it returns `exists: false` instead of throwing. Every
other operation on a missing collection fails with the machine error code
`ENATIVE_NOT_FOUND`.

### Create

```ts
await db.users.create()

const id = await db.users.put({
    name: 'Jane Doe',
    age: 29,
    team: 'platform'
})
```

### Read

```ts
const doc = await db.users.get(id)
```

### Update (preserves the document TTID)

```ts
const sameId = await db.users.patch(id, { team: 'core-platform' })
```

### Delete

```ts
await db.users.delete(sameId) // moves payload to .deleted/W/4UUB32VGUDW.json
```

Soft-deleted files retain their TTID filename, use file `mtime` as `deletedAt`,
and become read-only (`0444`). They are excluded from ordinary queries.

### Recover Deleted Documents

```ts
const deleted = await db.findDeletedDocs('users', {
    $deleted: { $gte: Date.parse('2026-05-01T00:00:00Z') }
})

await db.users.restore(sameId)
```

Restore preserves the TTID, moves the payload back into `docs/`, restores
writable file permissions (`0644`), rebuilds its indexes, and records the
restoration as a live insert event. A tombstoned TTID cannot be written
directly; it must be restored.

### Raw Files (Buckets)

A **bucket** stores raw files: create a collection with kind `file`, then store
a file by its local path or a URL. FYLO copies the bytes in; it keeps no link to
the source. Buckets are stored on disk under `.buckets/<name>/` (documents live
under `.collections/<name>/`); the two share an identical internal layout.
Databases written by older FYLO versions, where file collections lived under
`.collections/`, are migrated to `.buckets/` automatically the first time the
engine opens them.

Buckets need the native `fylo` binary. The browser loader and the iOS, Android,
and Flutter clients do not support them in this release and reject
`kind: 'file'` with an error.

The examples use the Node shim. It has a dedicated `putFile`; every other shim
stores a file by sending the same `putData` request, with a `file` field,
through its raw `request` method (see the machine requests below), and reads
it with `getFileData` the same way.

```js
import { Fylo } from './fylo.mjs'

const db = new Fylo('/mnt/fylo')

await db.assets.create('file')

const id = await db.assets.putFile(
    { path: '/uploads/summary.pdf' },
    { key: '/reports/2026/summary.pdf' },
    { rating: 5, starred: true }
)

const manifest = await db.getDoc('assets', id) // { [id]: manifest }, no bytes
const { result } = await db.request({ op: 'getFileData', collection: 'assets', id })
const bytes = Buffer.from(result.data, 'base64')
```

`putFile(collection, file, fileOptions, meta)` takes `{ path }` or `{ url }` as
the file, and the collection facade drops the collection argument. Every file
has a slash-delimited logical `key`. Without one, the key is `/` followed by the
source file name; an exact key is preserved as supplied:

```js
const exact = await db.assets.find({
    $ops: [{ key: { $eq: '/reports/2026/summary.pdf' } }]
})

const reports = await db.assets.find({
    $ops: [{ key: { $like: '/reports/%' } }]
})
```

Keys always begin with `/`, may be at most 1024 UTF-8 bytes, and cannot contain
backslashes, control characters, or `.` / `..` path segments. A key is logical
metadata, not a local filesystem path; the raw bytes still use the TTID
filename shown below. FYLO stores a key exactly as supplied and does not make
keys unique, so two files can share one; pick unique keys in your application
when you need them. To change a file's key, store the file again under the new
key and delete the old record.

Checksums are cached in a `user.fylo.checksum` xattr stamped with (size, mtime),
so listings and reads do not re-hash file contents; the hash is recomputed
automatically whenever the stamp no longer matches the file. The cache trusts
its stamp, so silent corruption that preserves both size and mtime is invisible
to the fast path. `verifyCollection` is the stamp-ignoring audit that closes the
gap: it re-hashes the full contents of every file (active and soft-deleted),
freshens matching stamps, and reports mismatches without touching the corrupt
file's original claim:

```js
const { result: report } = await db.request({ op: 'verifyCollection', collection: 'assets' })
// { collection, filesScanned, verified, stamped, corrupt: [{ id, namespace, expected, actual }] }
```

The audit reads every byte, so it is slow by design: run it as a scheduled
background job, not per request. The CLI equivalent exits non-zero when
corruption is found, so a cron line is all a weekly audit needs:

```cron
# Weekly integrity audit, Sunday 03:00 — mail/alert fires on non-zero exit
0 3 * * 0  fylo verify assets --root /mnt/fylo --json || notify "fylo: corruption detected"
```

FYLO stores the bytes unchanged at:

```text
.buckets/assets/docs/<shard>/<TTID>.<original-extension>
```

**That path is internal and versioned, not an interface.** `<shard>` is derived
from the identifier by a rule that has already changed once
([ADR 0006](docs/adr/0006-shard-records-by-the-trailing-creation-characters.md))
and may change again. Read content through `getFileData` (below); do not
compute the path.

Metadata is derived from the stored file, with the logical `key` stored as a
`user.fylo.key` extended attribute (xattr) on the file itself, so it travels
with the bytes across moves: `name`, `key`, `extension`, `contentType`,
`contentLength`, `etag`, `checksumSHA256`, `createdAt`, and `lastModified`.
These fields use the normal prefix index and can be queried with `findDocs`.

Every timestamp FYLO returns is a **whole** number of epoch milliseconds.
`createdAt` is decoded from the TTID; `lastModified` — like `updatedAt`,
`mtime`, and `deletedAt` on documents — is the filesystem modification time,
truncated to the millisecond.

Developer-defined metadata rides along the same way, as `user.fylo.meta.*`
xattrs on the document or raw file. Pass it as the `meta` argument of `putFile`
(the top-level `meta` field of `putData`), bulk-edit it with `setMeta` (`null`
removes an entry), and read it with `getMeta`, which returns the complete
canonical record plus custom metadata. Every record includes `id`, `mtime`,
`updatedAt`, and `createdAt`; raw files also include their stored file
descriptor:

```js
await db.setMeta('assets', id, { rating: 4, starred: null }) // update + remove
await db.getMeta('assets', id)
// { id, name, key, extension, contentType, contentLength, etag, checksumSHA256,
//   lastModified, mtime, updatedAt, createdAt, rating: 4 }
```

Canonical fields take precedence if a custom metadata key uses the same name.
Every shim exposes `getMeta` and `setMeta`, with language-specific casing and
collection-scoped forms documented in [`clients/README.md`](clients/README.md).

The record must be a plain object. Names are 1-64 characters of letters, digits,
`.`, `_`, or `-`, starting with a letter or digit. Each value must be
JSON-serializable and at most 60 KiB after UTF-8 JSON encoding; strings, numbers,
booleans, arrays, and objects round-trip with their types. A top-level `null`
value is a deletion marker, not a storable metadata value. FYLO validates the
whole mutation before writing it and rolls back a filesystem xattr batch if a
later write fails. On file collections, metadata is indexed under `meta/`, so it
can be queried, including numerically:

```js
await db.assets.find({ $ops: [{ ['meta/starred']: { $eq: true } }] })
await db.assets.find({ $ops: [{ ['meta/rating']: { $gte: 4 } }] })
```

Metadata survives soft delete, restore, and version-control restores because it
is snapshotted with each commit. A file's key and metadata live only in its
xattrs, so copy a store directory only with a tool that preserves extended
attributes; a raw file whose xattrs were stripped cannot be indexed again from
its bytes alone, so restore it from version control or a backup instead.
Filesystem-backed document and file collections use native xattrs on
macOS/Linux and an NTFS Alternate Data Stream manifest on Windows.
A filesystem without extended attributes (a Docker Desktop VirtioFS bind mount
on macOS, a FAT volume) can still hold documents, but not their metadata or a
bucket's files. There, creating a bucket fails at once, and a write that stores
metadata fails, with an `ENATIVE_IO` error that names the call, the attribute
and the file, such as `setxattr user.fylo.meta.starred on <path>: filesystem
does not support extended attributes`. To keep metadata and buckets, use a
local filesystem with extended attributes, the `efs-sidecar-v1` filesystem
profile (Unix only), or a `.vdbx` database. Before creating a bucket, the native writer
checks the filesystem by setting and removing `user.fylo.probe` on a
short-lived `.fylo-xattr-probe-*.tmp` file at the top of the root.
Metadata is per-version on filesystem-backed JSON documents. The machine ops
`getMeta` and `setMeta` cover it from every client shim; native metadata remains
an engine implementation detail rather than a package-level byte API.

A `url` source (`file:`, `http:`, or `https:`) is fetched once, at write time,
with a 30-second timeout and a 512 MiB limit.

The machine request behind `putFile` takes the file as a tagged absolute path
(or `url`), with an optional `key`:

```json
{
    "op": "putData",
    "root": "/mnt/fylo",
    "collection": "assets",
    "file": {
        "path": "/uploads/greeting.txt",
        "key": "/incoming/greeting.txt"
    }
}
```

`getDoc` answers a file collection with the manifest and no bytes. `getFileData`
returns the content, in either of the two shapes the handshake advertises as
`documentBuckets.getOutputs`:

```json
{ "op": "getFileData", "root": "/mnt/fylo", "collection": "assets", "id": "4VU2UX8MC3K" }
```

```json
{
    "id": "4VU2UX8MC3K",
    "contentLength": 12,
    "checksumSHA256": "a8359ee3…",
    "encoding": "base64",
    "data": "aGVsbG8gYnVja2V0"
}
```

Adding an absolute `path` writes the content there and returns only the
receipt, which is how an object larger than one response frame is read. The
path must not already exist — `getFileData` never replaces a file it did not
create — and an inline read that would not fit the frame is refused with
`EFRAME_RESPONSE_TOO_LARGE` naming `path` as the alternative.

```json
{
    "op": "getFileData",
    "root": "/mnt/fylo",
    "collection": "assets",
    "id": "4VU2UX8MC3K",
    "path": "/tmp/greeting.txt"
}
```

---

## Querying

FYLO queries use prefix indexes first, then hydrate only matching documents.

```ts
// Exact match
const results = await db.users.find({
    $ops: [{ name: { $eq: 'Alice' } }]
})

// Range query (numeric fields)
const adults = await db.users.find({
    $ops: [{ age: { $gte: 18 } }]
})

// Contains (array membership)
const engineers = await db.users.find({
    $ops: [{ tags: { $contains: 'engineering' } }]
})

// OR across conditions
const privileged = await db.users.find({
    $ops: [{ role: { $eq: 'admin' } }, { role: { $eq: 'owner' } }]
})
```

### Word search and sorted pages

Name the fields you search by word, once per collection. FYLO rebuilds the
collection's index and keeps a key for every word of those fields from then on
([ADR 0023](docs/adr/0023-word-index-and-sorted-queries.md)):

```ts
await db.messages.words(['body', 'subject']) // machine op: setWordFields
```

```bash
fylo index words messages body subject
```

`$match` takes words. A document matches when each word you give is the start
of a word in the field, so `meet tom` finds "Meeting with Tomás". Words are
lower-cased, and accents on Latin letters are removed (`é` becomes `e`,
`ß` becomes `ss`). There is no ranking, stemming or phrase search, and text
written without spaces (Chinese, Japanese) is one word per run. `$match`
combines with every other operator:

```ts
const found = await db.messages.find({
    $ops: [{ conversation: { $eq: 'c-42' }, body: { $match: 'meet tom' } }]
})
```

`$match` works on any field; on a field without word keys it reads every
document. Word keys cost index space (a 2 KB message body adds roughly 6 to
12 KB of keys), and they share the collection's 256 MiB index limit.

`$sort` orders by one field's number, from the index, and `$after` takes the
cursor of the previous page:

```ts
let page = await db.messages.page({
    $ops: [{ conversation: { $eq: 'c-42' } }],
    $sort: { sentAt: 'desc' },
    $limit: 50
}) // { items: [{ id, doc }], nextCursor }
page = await db.messages.page({ ...query, $after: page.nextCursor })
```

- The sort value is the field's JavaScript `Number()` value: numbers, numeric
  strings, booleans and `null`. **A document whose sort field has no number
  (missing, a word, an ISO date string, an empty array) is left out of a
  sorted query.** Store timestamps as epoch milliseconds to sort by them.
- An array sorts by its smallest number (`asc`) or largest (`desc`). Ties go
  by document id.
- The cursor is stateless: it needs no `exec --loop` and never expires. A
  cursor from a different filter or sort is refused with `EINVALIDCURSOR`.
  Documents written between pages appear on a later page only when they sort
  after the cursor.
- A machine `findDocs` with `$sort` answers `{ "items": [{ "id", "doc" }],
"nextCursor" }` (ids only with `$onlyIds`); it cannot be combined with the
  loop's `page` option.

### SQL Support

```ts
const db = new Fylo('/mnt/fylo')

await db.sql`CREATE TABLE posts`
const id = await db.sql`INSERT INTO posts (title, published) VALUES (${'Hello'}, ${true})`
const posts = await db.sql`SELECT * FROM posts WHERE published = ${true}`
```

`CREATE TABLE <name>` creates a document collection and `DROP TABLE <name>`
drops one.

`UPDATE` and `DELETE` statements are atomic within their collection: either
every matched document, its native metadata, and index entries commit, or the
statement restores all before-images.

Use `EXPLAIN` to inspect the selected access path without executing the
statement, or `EXPLAIN ANALYZE` to execute it and include elapsed time and the
result:

```ts
const plan = await db.executeSQL("EXPLAIN SELECT * FROM posts WHERE title = 'Hello'")
// { operation: 'SELECT', collection: 'posts', access: [...], executed: false }
```

`EXPLAIN ANALYZE` returns the same fields plus `executed: true`, `elapsedMs` and `result`; `EXPLAIN` never executes, even for INSERT, UPDATE, DELETE, CREATE or DROP.

The CLI accepts the same syntax:

```bash
fylo sql "EXPLAIN SELECT * FROM posts WHERE published = true" --root /mnt/fylo
```

### Access control: application identities

FYLO grants access per **application identity**, on **protected `.vdbx`
databases**, and denies by default
([ADR 0021](docs/adr/0021-application-identities-and-signed-policies.md)).
An identity is a P-256 key pair one application holds. A protected database
is encrypted with a key per collection, and each key is wrapped only for the
identities a signed policy grants it, so the rules hold for any process that
reads the file, on every platform and in the browser.

```bash
# One key per application; the database's admin gets its own.
fylo identity create --store file:admin.pem
fylo identity create --store keychain:myapp-web
fylo identity show keychain:myapp-web --json   # id (fid1:...) and publicKey (fpk1:...)

# A new database is protected unless created with --unprotected.
fylo create app.vdbx --admin file:admin.pem

# The admin grants read, write (includes read) or admin on a collection,
# every collection, or the database.
fylo access grant app.vdbx --grantee web --key fpk1:p256:... \
    --actions read --collection users \
    --identity file:admin.pem --trust-anchor fid1:...
fylo access revoke app.vdbx --rule web-collection:users \
    --identity file:admin.pem --trust-anchor fid1:...
fylo access list app.vdbx --identity keychain:myapp-web --trust-anchor fid1:...
fylo access settle app.vdbx --identity file:admin.pem --trust-anchor fid1:...
```

- **Opening.** A process opens a protected database as one identity:
  `fylo.identity` and `fylo.trustAnchor` in `application.json`,
  `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR`, or `--identity` and
  `--trust-anchor`. The trust anchor is the `fid1:` identifier of the admin
  that created the database; an open without one fails with `EPOLICY`
  unless the caller opts in to `--trust-on-first-use`, which warns.
- **Denials.** Without an identity the database is locked (`ENOIDENTITY`).
  An identity with no grant on a collection gets `EACCES`; a record not
  signed by an identity that could write it is `ESIGNATURE`.
- **Writes are signed.** Every record carries its writer's ECDSA signature,
  which readers check, so a process that only holds a read key cannot add
  records others accept.
- **Revocation** writes the next policy version and starts a new key epoch
  for the collections the rule covered; records written before stay readable
  to an identity that kept the old key until a rekey rewrites them. It also
  rotates the catalog key, which seals collection names and the policy. If
  a revocation stops part way, `fylo access report` and `fylo verify` say
  a catalog rotation is pending, and `fylo access settle app.vdbx` (as an
  admin) finishes it; an admin's next write also does. `fylo access audit`
  lists every signed policy change, settles included.
- **Every entry point** takes the identity: the CLI, `fylo exec --loop`
  (its handshake reports the identity and its grants), the `fylo` crate
  (`OpenOptions::with_identity`), `fylo link` for Turso SDKs, SQL over
  `.vdbx`, and the browser (`identity: { name, workerUrl, trustAnchor }`
  with a `webcrypto:` key).
- **Directory roots** stay an unprotected layout for development and
  tooling: anyone who can read the folder reads every record and every
  indexed value.

End-user rules (which person may see which record) stay in your
application. Per-record POSTIX access (`uid`, `gid`, `mode`, `.as()`,
`--uid`, `--groups` and the machine `access` field) was removed in this
release. Roots that carry such descriptors still open, with the descriptors
ignored and a one-time warning; `fylo access report <root>` lists them and
`fylo pack --protect` moves the root into a protected database. See
[Migrating to protected databases](docs/migrating-to-protected-databases.md).

### Query Strategy

Both engines use the prefix index only to choose which documents to read,
then check every one against the whole query. A query whose every `$ops`
entry has at least one field the index can answer reads only those
documents; otherwise it reads every document.

| Operator                     | Native engine (CLI, clients, Rust)               | Browser engine                               |
| ---------------------------- | ------------------------------------------------ | -------------------------------------------- |
| `$eq`                        | Exact match key (`eq`) and coerced numbers (`n`) | Exact match key (`eq`)                       |
| `$gt`, `$gte`, `$lt`, `$lte` | One run of sortable numeric keys (`n`)           | Sortable numeric keys (`n`/`nr`)             |
| `$contains`                  | Reads every document                             | Exact match on array members                 |
| `$like "ali%"`               | Reads every document                             | Forward prefix (`f`)                         |
| `$like "%ice"`               | Reads every document                             | Reversed prefix (`r`)                        |
| `$like "%lic%"`              | Reads every document                             | Trigram (`g3`) → hydrate → verify            |
| `$match` (word field)        | One run of word keys (`w`) per word              | One run of word keys (`w`) per word          |
| `$match` (other field)       | Reads every document                             | Reads every document                         |
| `$sort`                      | Walks `n` (asc) or `nr` (desc) keys in order     | Walks `n` (asc) or `nr` (desc) keys in order |

Both engines read every document for comparisons an empty array satisfies
(such as `$gte: 0`): an empty array compares as `0` but has no index key.
The native engine also reads every document for `$eq: null` and for a
comparison whose bound is not a number. It reads only the index keys a query needs from `keys.snapshot`, plus the
write-ahead log, rather than loading the whole index.

---

## Schema Versioning

Schemas live under `fylo.schema` in a per-collection layout:

```text
<fylo.schema>/
  <collection>/
    manifest.json          ← { current, versions: [{v, sha256?, addedAt?}] }
    history/
      v1.schema.json       ← chex regex schema
      v2.schema.json       ← head is whichever manifest.current points at
    migrations/
      v1-to-v2.json        ← { "steps": [ ... ] }, run when moving v1 → v2
```

`manifest.json`:

```json
{
    "current": "v2",
    "versions": [
        { "v": "v1", "addedAt": "2026-04-01T00:00:00Z" },
        { "v": "v2", "addedAt": "2026-04-27T00:00:00Z" }
    ]
}
```

Chex regex schemas (`history/v2.schema.json`):

```json
{
    "id": "^[0-9]+$",
    "title": "^.+$",
    "body": "^.+$",
    "slug": "^[a-z0-9-]+$"
}
```

Migrations are JSON, not code (ADR 0018). `migrations/v1-to-v2.json` lists
the steps that turn a v1 document into a v2 one:

```json
{
    "steps": [{ "slugify": "slug", "from": "title" }]
}
```

Steps run in order on the document with `_v` removed, then `_v` is stamped
with the target version. A document several versions behind runs each step
file along the manifest history, in order. Each step is an object with exactly
one operation key, whose value is the field it works on, plus that operation's
parameters. Fields are slash-separated paths through nested objects
(`payload/verifier`); writing a path creates missing parent objects.

| Step                                                 | Effect                                                                                 |
| ---------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `{"rename": "a", "to": "b"}`                         | move the field                                                                         |
| `{"copy": "a", "to": "b"}`                           | copy the field                                                                         |
| `{"default": "a", "value": 0}`                       | set the field only if it is missing or `null`                                          |
| `{"set": "a", "value": 0}`                           | set the field unconditionally                                                          |
| `{"remove": "a"}`                                    | delete the field                                                                       |
| `{"convert": "a", "to": "number"}`                   | convert to `"string"`, `"number"` or `"boolean"` (`"true"`/`"false"`, `1`/`0`)         |
| `{"map": "a", "values": {"y": true}, "else": false}` | replace the value by its entry in `values`, else by `else` (if given)                  |
| `{"wrap": "a"}`                                      | turn a value that is not an array into a one-element array                             |
| `{"lower": "a"}`, `{"upper": "a"}`                   | change a string's case (Unicode full case mapping)                                     |
| `{"trim": "a"}`                                      | remove leading and trailing whitespace                                                 |
| `{"replace": "a", "find": "x", "with": "y"}`         | replace every occurrence of literal text (no regex)                                    |
| `{"template": "a", "value": "{first} {name/last}"}`  | set a string built from other fields; `{{` and `}}` are literal braces                 |
| `{"slugify": "a", "from": "title"}`                  | lower-case `from`, join runs of ASCII letters and digits with `-`, `untitled` if empty |

A step whose source field is missing does nothing, except `set`, `template`
and `slugify`, which always write their field. `default`, `convert` and the
string steps treat `null` as missing; a missing or `null` placeholder or
`slugify` source reads as empty text. A field of the wrong type (for example
`lower` on a number) fails the migration. A step with an unknown parameter or
two operation keys is rejected.

FYLO never runs JavaScript from a schema folder. A version step that has only
an old `upgraders/v1-to-v2.js` file fails `schemaMaterialize` with an error
pointing at ADR 0018; rewrite it as steps. A change the steps cannot express is
an application update in two versions: add a version whose schema accepts both
the old and the new shape, rewrite the documents with `patchDocs` or SQL
`UPDATE`, then add a version that accepts only the new shape.

Behavior:

- Documents carry `_v` (version label)
- In strict mode, every write that stores a document body validates the whole
  resulting document against the head schema with the built-in CHEX validator
  and stamps `_v`: `putData`, `batchPutData`, `importBulkData`, `patchDoc`,
  `patchDocs`, and SQL `INSERT` and `UPDATE`. A patch is validated as the
  merged document, not as the fields it sends
- Every read returns documents in the head shape (ADR 0017 section 5):
  `getDoc`, `findDocs`, joins, SQL `SELECT`, deleted-document reads and the
  hot set. A document stored below the head is decrypted at the version it was
  written at and migrated in memory by the JSON steps of each later version;
  its stored bytes do not change until something writes it. Queries and SQL
  `UPDATE` and `DELETE` match the migrated document, so a query on a renamed
  field gives the same answer before and after `fylo schema migrate`. While a
  collection has documents below the head, a query that uses a field a
  migration writes or renames reads every document instead of the index;
  queries on other fields keep it
- A write that patches an old document (`patchDoc`, `patchDocs`, SQL
  `UPDATE`) migrates it to the head first, so the stored document moves to the
  head shape
- A document missing `_v` is read, materialized and counted as the oldest
  version; one whose `_v` the schema does not list fails the read with
  `ESCHEMA`
- `fylo schema inspect <collection>` (`schemaInspect`) reports the
  collection's validation mode, its head, how many live documents are stored
  at each version (`versions[].documents`, plus `unversioned` and
  `unknownVersions`) and `converged`: whether every document is at the head.
  Once it has converged, a read costs one comparison of `_v`. For a registry
  collection the writer keeps the count in
  `<root>/.fylo-schemas/<collection>/counts.json` (one file per branch
  worktree), stamped with the collection's generation, and recounts by scan
  whenever a crash, a recovery or another process leaves it stale. A
  collection whose versions come only from the `fylo.schema` folder is counted
  by scan when asked
- `schemaMaterialize` (`fylo schema materialize`) migrates one given document
  to the head shape in memory
- `fylo schema migrate <collection>` (`schemaMigrate`) rewrites every document
  below the head version at the head, validating and encrypting each as a
  write would, and is safe to run again. Documents without `_v` are skipped
  and counted, because outside strict mode they may already be in the head
  shape; `--include-unversioned` migrates them as the oldest version
- A document is decrypted with the `$encrypted` list of the version in its
  `_v`, so a later version may rename or drop an encrypted field
- Any collection directory under `fylo.schema` that contains a `manifest.json` is auto-created on `new Fylo(...)`; await `fylo.ready()` if you need the bootstrap to settle before issuing reads from a synchronous probe (mutation/query methods await internally)
- FYLO documents cannot hold arrays of objects — declare each nested object as its own collection. A write of a document with `items: [{ name: '...' }]` fails with `EARRAYOFOBJECTS`, even if its CHEX schema describes the array. Arrays of scalars and nested objects (as fields) are fine.

### Schema registry: `fylo schema push` and `pull`

The folder is where schemas are written and kept in source control. A
directory root also keeps its own copy, the **schema registry** (ADR 0017
section 3): each committed version of a collection's schema is one immutable
file, `<root>/.fylo-schemas/<collection>/<ordinal>.json`, holding its label
(the `_v` value), the CHEX schema with its `$encrypted` list, its SHA-256,
its JSON migration from the previous version, the compiled validator and when
and by what it was committed. The collection's catalog descriptor records the
head version.

```bash
fylo schema push article --schema-dir ./schemas --root /mnt/fylo   # commit new versions
fylo schema pull article --out ./schemas-copy --root /mnt/fylo     # write the folder layout back out
```

- `fylo schema push` (`schemaPush`) commits, in manifest order, each version of
  the folder that the registry does not have yet. Before committing it
  migrates the collection's documents (all of them up to 10,000, an evenly
  spaced sample above that) to the new head and validates them there; if any
  fail it commits nothing and names the first failures. A label already
  committed with different content is refused with `ESCHEMACONFLICT`:
  committed versions never change, so give the change a new label. It also
  refuses a migration with a JavaScript upgrader (ADR 0018), a top-level `$`
  key other than `$encrypted`, and an array whose items are objects
  (`EARRAYOFOBJECTS`). A version with a pattern that has no DFA form (a
  Unicode `\b`) is committed with a warning that builds without a regex
  compiler cannot validate it.
- `fylo schema pull` (`schemaPull`) writes the registry out as a
  `fylo.schema` folder: manifest, history and migrations.
- Once a collection has a registry, validation, `_v` stamping, `$encrypted`
  lists, `schemaMaterialize`, `schemaMigrate`, `schemaInspect` and
  `schemaDoctor` read it, whether or not `fylo.schema` is configured, and edits
  to the folder take effect at the next push. A collection without one keeps
  reading the folder, as before.
- The WebAssembly builds (`fylo.wasm`, `fylo-browser.wasm`) have no regex
  compiler. They validate with the compiled validator a native push stored in
  the registry, which gives the same results and messages as the native
  binary. Where they cannot validate (a head version that is not portable, a
  record whose compiled validator is missing or refused, or a collection with
  no registry whose schema is in a `fylo.schema` folder or whose descriptor
  records `strict` or `warn`) they refuse `strict` and `warn` writes with
  `ESCHEMAUNAVAILABLE` rather than store an unvalidated document; `off` writes
  as before. Push schemas with a native build.
- `"schema": { "path": "./schemas", "autoPush": true }` in `application.json`
  pushes every folder collection the root has whenever a write opens the root,
  which keeps the "edit the file, the next write uses it" workflow for
  development. It is off by default; `"schema": "./schemas"` is the same as
  `{ "path": "./schemas" }`.
- The first time a write opens a root with a `fylo.schema` folder, every
  existing collection the folder versions is imported into the registry once,
  without a dry run, and `<root>/.fylo-schemas/import.json` records it, so
  upgrading does not silently drop validation. Collections created later wait
  for a push.
- A collection with a registry also writes its generation state in a format
  older releases refuse with `ENATIVE_METADATA`, so a binary that cannot read
  the registry cannot write unvalidated documents to it.
- Push and pull work on the `main` branch: the registry is shared by every
  branch, and a push checks the documents of the checked-out one.

---

## Encryption

Fields declared in `$encrypted` arrays are stored with AES-GCM. Equality queries use HMAC blind indexes — lookups work without decrypting, but an attacker with index access can count repetitions.

Field encryption needs the native `fylo` binary. The browser loader and the
mobile clients do not support schemas or field encryption in this release and
fail such requests with an error.

```json
{
    "$encrypted": ["ssn", "email", "payload/verifier"],
    "id": "^[0-9]+$",
    "name": "^.+$",
    "email": "^.+$",
    "ssn?": "^[0-9-]+$",
    "payload?": { "verifier?": "^.+$" }
}
```

`$encrypted` describes how fields are stored; it is not a document property,
so it works with strict mode and a document never carries it.

Nested fields use a **slash-separated** path (`payload/verifier`), matching the
index field-path format. A dotted path such as `payload.verifier` is accepted
as a literal field name, matches nothing, and silently leaves the field
unencrypted — check the stored document if a field is not being encrypted.

Requirements:

- The secret referenced by `fylo.encryption.key` must contain at least 32 UTF-8 bytes
- `fylo.encryption.key` and `fylo.cipher.salt` must be configured together as secret references; literal credentials in JSON are rejected
- Process-global: one key for all collections

Decryption is schema-driven and does not depend on write history: a read-only
process loads a collection's `$encrypted` registration on its first read, so
replicas, reporting jobs, and event-sourced startup replay decrypt correctly
without writing first.

Reads fail closed. If a field the schema declares `$encrypted` cannot be
decrypted — missing key, wrong key, a value that fails authentication, or a
value stored before the field was encrypted — the operation fails with
`EDECRYPTFAILED` naming the collection and field. FYLO never returns a stored
value to a caller as if it were plaintext when it cannot verify it. Adding a
field to `$encrypted` therefore requires rewriting the documents that already
hold a plaintext value for it.

---

## Remote Access

There is none, by design. FYLO is an embedded library, like SQLite: it opens
the database files directly in your app's process and speaks no network
protocol. The CLI and language shims drive the `fylo` binary against a local
root, and the browser/mobile clients embed the engine over an on-device OPFS
store (see [Browser access](#browser-access)).

When the database lives on network storage, run your app server next to the
files and have clients talk to your app, rather than opening a live root over
SMB or NFS from another machine.

The PostgREST-style filter grammar (`role=eq.admin&age=gte.30`) lives on as a
query front-end: `queryFromSearch` in `src/query/postgrest.js` translates it
into a `findDocs` query.

---

## CLI & Machine Interface

### CLI

```bash
# Query
fylo "SELECT * FROM posts WHERE published = true"
fylo sql "SELECT * FROM posts" --page-size 25

# Admin
fylo inspect posts --root /mnt/fylo --json
fylo rebuild posts --root /mnt/fylo
fylo reshard posts --width 2 --root /mnt/fylo --json  # move to a wider shard layout
fylo verify assets --root /mnt/fylo --json  # integrity audit; exits 1 on corruption
fylo get posts 4UUB32VGUDW --root /mnt/fylo --json
fylo deleted posts --root /mnt/fylo --json
fylo restore posts 4UUB32VGUDW --root /mnt/fylo --json

# Document version control
fylo checkout -b feature/docs --root /mnt/fylo
fylo commit -m "snapshot feature docs" --root /mnt/fylo
fylo branch --root /mnt/fylo
fylo log --root /mnt/fylo
fylo status --root /mnt/fylo
fylo diff --root /mnt/fylo
fylo restore-commit 4UUB32VGUDW --root /mnt/fylo --force
fylo merge feature/docs -m "merge feature docs" --root /mnt/fylo
fylo checkout main --root /mnt/fylo

# Schema
fylo schema inspect article --schema-dir ./schemas --json
fylo schema doctor article --schema-dir ./schemas
fylo schema validate article @article.json --schema-dir ./schemas --json
fylo schema mode article warn --root /mnt/fylo        # off, warn or strict
fylo schema check article --schema-dir ./schemas --json  # documents the head rejects
fylo schema push article --schema-dir ./schemas          # commit new versions to the registry
fylo schema pull article --out ./schemas-copy            # write the registry back out
```

Each collection has a schema validation mode, kept in its catalog descriptor
so every process that opens the root uses it. `strict` refuses a document the
head schema rejects with `ESCHEMA` and writes nothing. `warn` writes it anyway,
logs a warning to stderr and counts it; `inspectCollection` reports the mode
and this process's count as `validation` and `schemaViolations`. `off` checks
nothing. Every mode still stamps `_v` and encrypts `$encrypted` fields, and the
mode applies to every write that stores a document body: `putData`,
`batchPutData`, `importBulkData`, `patchDoc`, `patchDocs` and SQL `INSERT` and
`UPDATE`. A collection with no recorded mode follows `fylo.strict`
(`--strict-schema`): `strict` when it is on, `off` otherwise. Use `warn` while
adopting a schema over existing data, and `fylo schema check` to find the
documents to fix before switching to `strict`.

`status` and `diff` compare document payloads only (`docs/` and `.deleted/`),
so rebuilt indexes, event journals, lock files, and mtime-only changes do not
create noisy diffs.

Document writes are auto-committed by default. `put`, `patch`, `delete`, and
`restore` create commit snapshots after the local filesystem write succeeds;
failed writes and no-op mutations do not create empty commits.

Commit storage is content-addressed: each document version is stored once as a
deduplicated blob, so commits share unchanged bytes across history and branches
instead of copying whole collections. Bulk operations coalesce — `put.batch`,
`patch.many`, `delete.many`, and `import` each record a single commit covering
every document they touch, so large ingests stay fast. Prefer these over
per-document writes when loading data.

This release has no option to turn auto-commit off or to exclude a collection
from history: neither the clients nor the machine protocol accept one. Version-control snapshots keep a
full content-addressed copy of every raw file, which doubles disk and write
bandwidth for large media collections, so budget for that when you store large
files in a bucket.

`restore-commit` refuses to overwrite uncommitted working tree changes unless
`--force` is passed; commit snapshots themselves remain immutable. `merge`
supports fast-forward and three-way document-payload merges. If both sides
changed the same TTID payload differently, FYLO reports conflicts and leaves
the current branch untouched.

## Serverless Queue

FYLO includes a durable, brokerless queue in the Rust engine. It requires no
Redis, SQS-compatible service, queue daemon, account, or network connection.
Messages and consumer state live under the same filesystem root:

```text
<root>/.fylo-queue/v1/
  manifest.json
  receipt-key.json
  topics/<encoded-topic>/Q00000000000000000001.json
  consumers/<encoded-group>/<encoded-topic>.json
  dedupe/<encoded-topic>/<sha256-key>.json
  dead-letter/<encoded-group>/<encoded-topic>/<message-id>.json
```

The queue provides at-least-once delivery, publication-ordered scanning,
independent consumer groups, visibility leases, lease extension, delayed
publication and retries, bounded attempts, and group-specific dead letters.
Messages are immutable. A claim is persisted before its receipt is returned;
if the worker exits without acknowledging, the message becomes available after
the visibility timeout.

Every native SDK shim also provides an idiomatic consumer decorator, annotation,
or callable wrapper. Each invocation processes one bounded batch and
automatically acknowledges successes or negative-acknowledges handler failures,
which makes the same queue suitable for serverless functions and scheduled
workers without embedding an infinite polling loop. Automatic consumers store
the generic reason `queue handler failed`; they never persist exception text.
Trusted code that needs a diagnostic reason can pass one explicitly to
`queueNack`.

```js
const published = await db.queue.publish(
    'email.welcome',
    { userId: 'u-7' },
    { idempotencyKey: 'welcome:u-7' }
)

const deliveries = await db.queue.claim('email.welcome', 'email-service', {
    maxMessages: 10,
    visibilityTimeoutMs: 30_000,
    maxAttempts: 5
})

for (const delivery of deliveries) {
    try {
        await sendWelcomeEmail(delivery.payload)
        await db.queue.ack('email.welcome', 'email-service', delivery)
    } catch {
        await db.queue.nack('email.welcome', 'email-service', delivery, {
            delayMs: 5_000,
            reason: 'queue handler failed'
        })
    }
}
```

Publication is safe to retry only with the same non-empty `idempotencyKey`,
topic, and byte-equivalent JSON payload. A key reused with different content
returns `EQUEUE_INVALID`. Claiming is intentionally not retry-safe: a lost
claim response leaves leases that expire normally. Recent acknowledgement
retries are idempotent only with the exact receipt that completed the delivery;
each group retains the 1,000 most recently acknowledged ID/receipt pairs for
that validation.
“Serverless” means embedded and brokerless, not distributed multi-writer
storage. FYLO still enforces one owning engine process per root. Concurrent
worker tasks share that process and consumer group; another process can take
over after the owner exits and the filesystem lease is released. There is no
built-in forever-running worker—the application or its serverless invocation
polls, handles, and acknowledges deliveries.

Current limits are 127 UTF-8 bytes per topic or consumer-group name, 1 MiB per
stored message, 1,000 deliveries per claim, an 8 MiB aggregate claim or
dead-letter response budget, and a 64 MiB aggregate message scan budget per
queue request,
10,000 pending delivery states and 1,000 acknowledged ID/receipt pairs per
consumer group, 100 attempts, a 24-hour
visibility lease, a 30-day delay, and 1,000 dead letters returned per read.

### Machine Interface (cross-language)

```bash
echo '{"op":"inspectCollection","root":"/mnt/fylo","collection":"posts"}' | fylo exec --request -
```

```json
{
    "protocolVersion": 1,
    "ok": true,
    "op": "inspectCollection",
    "durationMs": 4,
    "result": { "collection": "posts", "exists": true }
}
```

Before sending ordinary operations, supervisors can identify the exact runtime
and discover its framing contract:

```bash
fylo --version
fylo version --output json
printf '%s\n' '{"op":"handshake"}' | fylo exec --loop --root /mnt/fylo
```

`version --output json` and the `handshake` result share one stable identity:
FYLO runtime and protocol versions, immutable release commit and target,
effective frame limits, and supported capabilities. The `schemaValidation`
capability names the built-in CHEX validator and the CHEX release it follows;
WebAssembly builds add `compiledOnly: true`, because they validate only with
the compiled validators in a root's schema registry. Source and locally
compiled development executions explicitly report an unknown commit; release
builds embed the immutable source revision and build target. A handshake is
side-effect-free and does not create the configured root or initialize a
collection.

Capability records are versioned contracts. `documentBuckets.version === 1`
advertises raw-file collections (`kind: "file"`), their supported machine
operations, path/URL ingestion, `getFileData`'s base64 and path outputs, and
full-content SHA-256 verification.
The handshake's top-level `identity` reports the application identity the
session opens protected databases with, and, when its root is one, the grants
that identity holds there (`null` without an identity; see
[api/machine/v1](api/machine/v1/README.md)).
`serverlessQueue.version === 1` advertises the embedded filesystem queue,
including its operation names, delivery model, message/claim limits, consumer
groups, visibility leases, delayed delivery, idempotent publication, and
group-specific dead letters.

Supported operations: `handshake`, `executeSQL`, `createCollection`, `dropCollection`, `inspectCollection`, `rebuildCollection`, `setWordFields`, `reshardCollection`, `migrateLayout`, `compactCollection`, `verifyCollection`, `getDoc`, `getFileData`, `getLatest`, `getMeta`, `setMeta`, `findDocs`, `findDeletedDocs`, `changes`, `restoreDoc`, `joinDocs`, `putData`, `batchPutData`, `patchDoc`, `patchDocs`, `delDoc`, `delDocs`, `importBulkData`, `checkout`, `branch`, `commit`, `log`, `status`, `diff`, `restoreCommit`, `merge`, `schemaInspect`, `schemaCurrent`, `schemaHistory`, `schemaDoctor`, `schemaValidate`, `schemaMaterialize`, `schemaMode`, `schemaCheck`, `schemaMigrate`, `schemaPush`, `schemaPull`, `queuePublish`, `queueClaim`, `queueAck`, `queueNack`, `queueExtend`, `queueStats`, `queueDeadLetters`, `inspectDatabase`, `verifyDatabase`.

Requests name no caller: a request that carries the removed `access` object
is refused with `EBADREQUEST`. Access comes from the session's identity.

#### Bounded NDJSON frames

Persistent loops use one UTF-8 JSON object per LF-delimited line. The secure
defaults are 1 MiB per request and 8 MiB per response; the LF delimiter does
not count. A supervisor may lower or raise them, up to 64 MiB, and then confirm
the effective values in the handshake:

```bash
fylo exec --loop --root /mnt/fylo \
  --max-request-bytes 1048576 \
  --max-response-bytes 8388608
```

FYLO uses a fixed-capacity input buffer. It rejects invalid UTF-8
(`EFRAME_UTF8`), malformed JSON (`EFRAME_JSON`), duplicate object keys
(`EFRAME_DUPLICATE_KEY`), and oversized requests
(`EFRAME_REQUEST_TOO_LARGE`). When an LF boundary is known, it emits one error
response and safely resumes with the next frame. An incomplete final frame
returns `EFRAME_TRUNCATED` at EOF and the loop ends; retry it only after
starting a new child.

Responses never silently cross the advertised maximum. `findDocs` and
`findDeletedDocs` support bounded continuation on a persistent loop:

```json
{"op":"findDocs","collection":"posts","query":{"$ops":[]},"page":{"limit":256}}
{"op":"findDocs","collection":"posts","query":{"$ops":[]},"page":{"limit":256,"cursor":"<opaque>"}}
```

The result is `{ items, nextCursor, page: { count, limit } }`. The first request
materializes an immutable, process-scoped snapshot ordered by TTID binary text, so
concurrent mutations cannot duplicate or skip entries. Cursors are scoped to
the operation, collection and query; they expire after 15
minutes and become invalid when the loop exits. On `EINVALIDCURSOR` or child
restart, discard partial state and restart from page one. Snapshot state is
private to the native process, cleaned on completion/expiry/shutdown, and capped
at 1 GiB.

#### Change feed

`changes` returns one page of a collection's change feed: every document whose
newest change comes after the cursor, once, oldest change first
([ADR 0024](docs/adr/0024-collection-change-feed.md)).

```json
{"op":"changes","collection":"posts","after":null,"limit":256}
{"op":"changes","collection":"posts","after":"<cursor>","limit":256}
```

The result is `{ changes: [{ id, kind }], cursor, more }`. `kind` is `put`
(created, changed or restored) or `delete`. Read the document with `getDoc` to
see its new content. A document changed twice since the cursor appears once,
at its newest change. The cursor is durable: keep it, and continue from it
after a restart or in another process. `limit` is 1 to 1000 (default 256).

- The feed reads collections in the segments layout and in `.vdbx` databases.
  A collection in the files layout is refused with `ELAYOUTUNSUPPORTED`; move
  it with `fylo layout <collection> --to segments`.
- Changes are kept as long as the collection's documents are: `fylo compact`
  keeps every document's newest change, so a cursor never goes stale with
  time or compaction.
- A cursor expires only when the collection's change history starts again: a
  drop and re-create, `fylo pack`, `fylo rekey`, a layout move, or an older copy
  of the files put back. Then the answer is `ECURSOR_EXPIRED`: read the
  collection again, and continue from `"after": null`.
- The JavaScript browser engine (`fylo-web.mjs`) reads the files layout and
  keeps its own feed in `events/<collection>.ndjson`. When the native engine
  writes the same root, the browser engine sees the native commit in the
  collection's generation (`.fylo-transactions/.collections/<collection>/state.json`)
  and adds a `put` or `delete` for every document that changed before it
  answers. One limit: a native change to a document's metadata only
  (`setDocMeta`, kept in extended file attributes) does not show in the
  browser engine's feed. A native writer in the middle of a commit makes the
  call fail with `ENATIVE_CONCURRENT_WRITE`; call `changes` again.

Unpaged operations that exceed the frame still return
`EFRAME_RESPONSE_TOO_LARGE` while preserving stream synchronization. A single
query item that cannot fit returns `EQUERYITEMTOOLARGE`; an oversized snapshot
returns `EQUERYSNAPSHOTTOOLARGE`. If a client observes an oversized or malformed
response despite the negotiated contract, it must kill and restart the child
because its framing can no longer be trusted.

#### Stable machine error codes

Every `ok: false` machine response carries a non-empty `error.code`. The set
is additive: new codes may appear in later releases, and an existing code
keeps its meaning. Clients should branch on `error.code`, never on message
text.

| Code                                                                   | Meaning                                                                             | Retry guidance                                                            |
| ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `EBADREQUEST`                                                          | The request shape, field types, access object, or page options are invalid          | Do not retry; fix the request                                             |
| `EUNSUPPORTEDOP`                                                       | The operation is unknown to this runtime                                            | Do not retry; check the handshake capabilities                            |
| `EINVALIDDOCID`                                                        | The supplied document ID is not a valid TTID                                        | Do not retry; fix the ID                                                  |
| `EARRAYOFOBJECTS`                                                      | The document contains an array of objects, which the data model rejects             | Do not retry; restructure per the document-model rule below               |
| `EACCES`                                                               | The identity has no grant for the operation on a protected database                 | Do not retry with the same identity                                       |
| `EQUEUE_INVALID`                                                       | A queue name, identity, option, or idempotency reuse is invalid                     | Do not retry without correcting the request                               |
| `EQUEUE_RECEIPT`                                                       | A queue receipt is incorrect, expired, or already superseded                        | Do not acknowledge as that worker                                         |
| `EQUEUE_LIMIT`                                                         | A queue message, claim, delay, attempt, state, or read limit was exceeded           | Reduce the requested resource                                             |
| `EINDEX_FULL`                                                          | The write could grow the collection's index past its 240 MiB write limit            | Do not retry; delete documents or use another collection                  |
| `EDECRYPTFAILED`                                                       | An `$encrypted` field could not be decrypted with the configured key                | Do not retry; fix the key configuration                                   |
| `ECURSOR_EXPIRED`                                                      | The change cursor is from before the collection's change history started again      | Read the collection again; continue from `after: null`                    |
| `EINVALIDCURSOR`                                                       | The pagination cursor is invalid, expired, or from another process                  | Restart the traversal from page one                                       |
| `ENATIVE_NOT_FOUND`                                                    | The named collection does not exist                                                 | Do not retry; create the collection first                                 |
| `EBROWSER_UNSUPPORTED`                                                 | The browser or mobile engine does not implement the requested feature               | Do not retry; use the native engine                                       |
| `EROOTLOCKED` / `EROOTLEASELOST`                                       | Another live process owns the root (see Root owner), or this session lost ownership | Retry after the owning process exits; run one `fylo exec --loop` per root |
| `EFRAME_*`                                                             | Frame-contract violations, as documented above                                      | Per the framing rules above                                               |
| `EQUERYLOOPREQUIRED` / `EQUERYITEMTOOLARGE` / `EQUERYSNAPSHOTTOOLARGE` | Pagination contract violations, as documented above                                 | Per the pagination rules above                                            |
| `ESCHEMA`                                                              | A document does not match the head schema, or a schema is invalid                   | Do not retry; fix the document or schema                                  |
| `ESCHEMACONFLICT`                                                      | A pushed schema label is already committed with different content                   | Do not retry; give the changed schema a new version label                 |
| `ESCHEMAUNAVAILABLE`                                                   | This build cannot validate the collection's head schema version                     | Do not retry; push the schema natively, or write natively                 |
| `EVDBX_NOT_VDBX` / `EVDBX_VERSION`                                     | The file is not a `.vdbx` database, or needs a newer FYLO                           | Do not retry; open it with a build that supports it                       |
| `EVDBX_CORRUPT`                                                        | A `.vdbx` structure failed its checks                                               | Do not retry; run `fylo verify <db.vdbx>`                                 |
| `ENATIVE_IO`                                                           | Native filesystem I/O failed, including disk pressure or xattr operations           | Treat a mutation as ambiguous; inspect the cause before retrying          |
| `EUNKNOWN`                                                             | An engine failure without a more specific classification                            | Treat conservatively; inspect `error.message` diagnostically              |

Storage-level failures may carry additional stable codes (for example
`FYLO_COLLECTION_NOT_FOUND`); those retain their meaning across releases under
the same additive policy.

#### Document model: no arrays of objects

A stored document may contain scalars, nested objects, and arrays of scalars.
It may **not** contain an array of objects at any depth. FYLO treats an array
of objects as a sign the data wants to be its own collection, referenced by
key, so every field is independently indexable.

```json
{ "tags": ["draft", "review"], "author": { "name": "Ada" } }
{ "items": [{ "sku": "a" }] }
```

The first document is accepted; the second is rejected before any disk work
with `EARRAYOFOBJECTS` and a message naming the offending field path. The
rejection is deterministic and leaves no partial state—the write never starts.
This applies equally to `putData`, `batchPutData`, patches, and SQL.

To model a collection of records, store them in their own collection and
reference them by key or public ID. To keep an opaque payload that is never
field-queried, serialize it to a single string field.

#### Root owner

One process owns a root
([ADR 0003](docs/adr/0003-native-and-browser-storage-boundaries.md)). Every
`fylo exec` session, a single `--request` or a `--loop`, takes the root's
non-blocking kernel file lock when its first request opens the root, and holds
it until the process exits. It does not release the lock between requests,
even while it is idle.

A second `fylo exec` on the same root gets `EROOTLOCKED` for every request
until the first process exits. It does not wait, and it executes no read or
write. A second `--loop` keeps running and answers each request with that
error; its first request after the owner exits succeeds, without a restart.
So run one `fylo exec --loop` per root and send every caller through it. Two
containers or serverless instances that mount the same root are two processes:
only one of them can use it at a time.

`--exclusive-root` takes the lock at startup instead, before the loop reads
stdin:

```bash
fylo exec --loop --root /mnt/fylo --exclusive-root
```

If another process owns the root, the session answers its first request with
`EROOTLOCKED` and ends. Normal shutdown releases the lock. After a crash or
`SIGKILL`, the operating system releases it; persistent metadata is only
diagnostic/fencing state and is not a PID-file claim, so PID reuse and stale
metadata cannot retain ownership. Every request verifies the unique owner generation, and a replaced former owner
fails closed with `EROOTLEASELOST`.

Because nothing else can write the root, an exclusive session also keeps a
hot set of parsed documents and indexes in memory, 64 MiB by default.
`--hot-set-bytes <n>` sets the budget and `0` turns it off; the handshake
reports it as `capabilities.hotSet` (see
[ADR 0014](docs/adr/0014-in-memory-hot-set.md)).

The lease contract is supported on native local filesystems on macOS, Linux,
and Windows. Containers on the same host can share it only when they share the
same canonical bind-mounted root and host kernel lock domain. Network,
clustered, object-backed, and independently synchronized filesystems are not a
distributed lock service and are unsupported for `--exclusive-root`; use an
external lease/consensus service there. Windows UNC/network shares have the
same restriction.

### Compiled Executable

The `fylo` binary (installed from a release) runs the same machine interface:

```bash
fylo exec --request @request.json
```

Callable from any language that can spawn a process and read JSON: write a
machine request to stdin or `--request`, then read the JSON response from stdout.

The compiled executable interop contract is tested in CI against Python, Ruby,
PHP, Dart, Java, C#, C++, Swift, Kotlin, and Rust. Each language invokes the
same `fylo exec --request <json>` machine protocol, so non-JS callers do not
depend on JS-only conveniences such as `new Fylo(...)`, `sql` template tags, or
`db.<collection>` facades.

---

## Recovery & Rebuild

Documents are truth. Indexes are derived. When they drift:

```ts
const result = await db.posts.rebuild()
// {
//   collection: 'posts',
//   docsScanned: 42,
//   indexedDocs: 42
// }
```

```bash
fylo rebuild posts --root /mnt/fylo --json
```

Use `db.<collection>.rebuild()` after operator-level recovery or when external
processes have modified data files directly.

Version-control restore and merge operations maintain a durable transaction
under `.fylo-vcs/staging/`; ordinary mutations use `.fylo-transactions/`. The
native engine recovers both before opening a collection. An active transaction
rolls back, a committed transaction rolls forward, and derived indexes are
rebuilt when recovery changes documents. Reopening is idempotent.

Treat a failed startup recovery as an operator incident: stop writers, preserve
the root, and restore a verified snapshot rather than hand-editing journal or
generation files.

The same crash contract is release-gated on local macOS/Linux filesystems and
native x64 Windows on NTFS. On Windows, kernel-owned `LockFileEx` claims are
released when a process dies, and recovery uses directory handles while
rejecting junction/reparse-point traversal before rename or deletion. This does
not make arbitrary network shares, sync folders, or filesystems without local
atomic link/rename semantics supported storage targets.

---

## EFS metadata candidate

FYLO's broader direction is provider-independent serverless runtime capability.
This storage work is a first backend slice inside FYLO, not an external AWS shim.
Invocation lifetime, fencing, retries and cold-start recovery need their own
explicit core contracts. PaaS retains AWS infrastructure configuration.

The isolated general invocation source candidate adds typed `InvocationOptions`
and `InvocationRuntime` inside FYLO. It handles one bounded core frame with
trusted host configuration and a monotonic deadline/cleanup reserve, then returns
buffered response bytes only after private handles are gone and the owning lease
has completed unique checked release. Legacy streaming APIs keep their behavior.
The host selects an existing root/repository. This initial invocation slice
refuses repository/version operations listed in the
[machine contract](api/machine/v1/README.md); its reused handshake retains the
legacy capability fields. Recovery is capped at 10,000 collection names per root.
The 22 invocation contract tests are authored but unexecuted; distributed fencing,
general idempotency and provider support are not established. See
[ADR 0011](docs/adr/0011-serverless-invocation-contract.md).

An isolated candidate adds explicit `--filesystem-profile efs-sidecar-v1` for
filesystems without native xattrs. Existing constructors, shims and CLI calls
default to `native`. Selection does not promote EFS or change the local-filesystem
support boundary described above. No native compiler/tests or actual EFS/Lambda
qualification have completed for this candidate.

The profile is Unix-only (macOS and Linux), because its sidecars need POSIX
file identity. On Windows, opening a root with it fails at the handshake with
`ENATIVE_UNSUPPORTED: EFS metadata profile requires supported POSIX file
identity`.

The profile uses a versioned root descriptor and mandatory private
`<payload>.fylo-attrs` JSON/base64 files, including an empty `{}` manifest.
Readers and writers require the same admitted root owner. Payload, metadata and
authorization share a stable-generation read; paired mutations use the existing
journal. Initialization is restricted to a new empty root. A native open refuses
a marked EFS root; old binaries must remain restricted to their original roots.
No live conversion is provided.

The initial identity policy preserves the actual root UID/GID and rejects
unsupported ownership changes. The initial sidecar backend requires owner-readable
payload modes; unsupported projections fail before record mutation. Advisory-lock
admission does not establish remote
fencing or NFS cache coherence. Record-identity binding for same-payload manifest
swaps is under review. EFS queue execution also remains blocked: the current
queue entry point opens the native profile, and its source path is outside the
initial adapter grant. Queue state must still be included in every full backup.

See [ADR 0010](docs/adr/0010-explicit-efs-metadata-profile.md), the
[storage contract](api/storage/v1/README.md) and the
[qualification plan](docs/operations/efs-profile-qualification.md). A successful
local fixture, mount or infrastructure plan is not a production support claim.

## Limitations

| Limitation                           | Detail                                                                                                                                                                                                                                                                             |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Filesystem-only engine**           | One engine writes to one supported local path. Snapshot, replication, and remote-copy tooling remain deployment concerns outside FYLO.                                                                                                                                             |
| **Local-filesystem locking**         | PID-aware lock files plus kernel-owned takeover claims; live owners are never evicted by TTL. Use local POSIX filesystems or NTFS, not network/sync filesystems without equivalent atomic semantics.                                                                               |
| **Indexes are derived**              | External writes to data files won't update indexes. Use `db.<collection>.rebuild()`.                                                                                                                                                                                               |
| **Frequency leaks on encryption**    | HMAC blind indexes for `$eq` reveal value repetition even without decryption.                                                                                                                                                                                                      |
| **Process-global cipher**            | One key per process for all `$encrypted` fields. No per-collection key rotation built in.                                                                                                                                                                                          |
| **No cross-collection transactions** | SQL mutations and ordinary writes are atomic within one collection; there is no atomic multi-collection commit.                                                                                                                                                                    |
| **Timestamp metadata**               | `createdAt` comes from TTID; `updatedAt` comes from file modification metadata. Every timestamp is whole epoch milliseconds.                                                                                                                                                       |
| **Canonical identifiers**            | TTID matches case-insensitively but FYLO names a file after the identifier, so a write with a non-canonical spelling is refused. Uppercasing the identifier repairs it.                                                                                                            |
| **Shard width is per collection**    | Recorded in the catalog descriptor and 1–4. A write configured for a different width fails with `ESHARDWIDTH`; `fylo reshard` moves the collection.                                                                                                                                |
| **Bulk import for trusted sources**  | SSRF guard blocks private addresses and caps at 50 MiB. Not for user-provided URLs.                                                                                                                                                                                                |
| **Word search**                      | `$match` matches word prefixes after lower-casing and removing Latin accents. No ranking, stemming, phrases or segmentation of text written without spaces. A release before ADR 0023 that writes to a word field leaves its words out until the next `$match` rebuilds the index. |
| **Sorted queries**                   | `$sort` orders by one field's number only; documents without a number in that field are not returned. Sorting by text is not supported yet.                                                                                                                                        |
| **Index size per collection**        | A file-layout collection's index is read whole and capped at 256 MiB: roughly 150k-200k small or 35k-48k medium documents. Writes stop at 240 MiB with `EINDEX_FULL`; deletes still work.                                                                                          |

---

## License

MIT © D31MA
