<p align="center">
  <strong style="font-size: 2em;">FYLO</strong><br/>
  <em>A Bun-native document store with zero-payload prefix indexes.</em>
</p>

<p align="center">
  <a href="https://github.com/d31ma/Fylo/releases/latest"><img src="https://img.shields.io/github/v/release/d31ma/Fylo?label=latest&color=blue" alt="Latest Release"></a>
  <a href="https://github.com/d31ma/Fylo/actions"><img src="https://img.shields.io/github/actions/workflow/status/d31ma/Fylo/publish.yml?label=build" alt="Build Status"></a>
  <a href="https://www.npmjs.com/package/@d31ma/fylo"><img src="https://img.shields.io/npm/v/@d31ma/fylo?label=npm" alt="npm"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/license-MIT-green" alt="License: MIT"></a>
  <a href="https://github.com/d31ma/Fylo/stargazers"><img src="https://img.shields.io/github/stars/d31ma/Fylo?style=flat" alt="GitHub Stars"></a>
</p>

<p align="center">
  <strong>One canonical file per document. Key-only indexes. No monolithic caches.</strong><br/>
  Just&nbsp;<code>bun add @d31ma/fylo</code>.
</p>

---

## Table of Contents

- [Why FYLO?](#why-fylo)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [Configuration](#configuration)
- [CRUD Operations](#crud-operations)
- [Querying](#querying)
- [Schema Versioning](#schema-versioning)
- [Encryption](#encryption)
- [Auth & Row-Level Security](#auth--row-level-security)
- [WORM Mode](#worm-mode)
- [Syncing & Replication](#syncing--replication)
- [Local Queue](#local-queue)
- [CLI & Machine Interface](#cli--machine-interface)
- [Recovery & Rebuild](#recovery--rebuild)
- [Limitations](#limitations)
- [License](#license)

---

## Why FYLO?

FYLO trades complexity for clarity. Documents are plain JSON files on disk. Indexes are zero-byte key entries that accelerate queries without duplicating data. If the index ever drifts, FYLO rebuilds it from the documents — the files are always the source of truth.

| Principle                            | Implementation                                                |
| ------------------------------------ | ------------------------------------------------------------- |
| **Documents are truth**              | One `.json` file per document, sharded by TTID prefix         |
| **Indexes are accelerators**         | Zero-payload prefix keys in a sorted catalog file             |
| **Rebuildable, not sacred**          | `rebuildCollection()` reconstructs indexes from documents     |
| **Bun-native, zero-dependency core** | `bun:sqlite`, `Bun.mmap()`, `Bun.S3Client` — no native addons |
| **Filesystem-first**                 | One engine. Sync to S3/GCS is your deployment choice          |

---

## Quick Start

```bash
bun add @d31ma/fylo
```

```ts
import Fylo from '@d31ma/fylo'

const db = new Fylo({ root: '/mnt/fylo' })

await db.createCollection('users')

const id = await db.putData('users', {
    name: 'Ada',
    role: 'admin',
    tags: ['engineering', 'platform']
})

const doc = await db.getDoc('users', id).once()
console.log(doc[id]) // { name: 'Ada', role: 'admin', ... }
```

---

## Architecture

Each collection lives under `.collections` in the configured root:

```text
<root>/.collections/<collection>/
  docs/                    ← one .json file per document (TTID-named)
    4U/
      4UUB32VGUDW.json
  index/                   ← local filesystem prefix index catalog
    manifest.json          ← format version marker
    keys.snapshot          ← sorted index keys, mmap'd for O(log n) lookup
    keys.wal               ← append-only mutation log (compacted at 1 MiB)
  events/
    <collection>.ndjson    ← append-only event journal
  locks/                   ← advisory file locks
  heads/                   ← WORM lineage heads (WORM mode only)
  versions/                ← version metadata (WORM mode only)
```

See [examples/production-folder-structure.md](examples/production-folder-structure.md) for a
production-style tree with multiple collections and queue storage.

**Index keys** look like S3 object keys — field path, kind, value, doc ID:

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

### Index Backends

| Backend              | Description                                            | Best for                  |
| -------------------- | ------------------------------------------------------ | ------------------------- |
| `local-fs` (default) | mmap'd sorted file + WAL. Binary search, zero JS heap. | Embedded, single-process  |
| `s3-client`          | Bun.S3Client. Each key is a zero-byte S3 object.       | Distributed, multi-writer |

```ts
// S3-backed indexes (documents stay on local filesystem)
const db = new Fylo({
    root: '/mnt/fylo',
    index: {
        backend: 's3-client',
        s3: { region: 'us-east-1' }
    }
})
```

Collection names map directly to S3 bucket names. Credentials resolve from `AWS_*` env vars, `FYLO_S3_*` aliases, or explicit `index.s3` options.

---

## Configuration

| Variable              | Purpose                                                            | Default        |
| --------------------- | ------------------------------------------------------------------ | -------------- |
| `FYLO_ROOT`           | Filesystem root for collections                                    | `./.fylo-data` |
| `FYLO_SCHEMA_DIR`     | Directory containing JSON validation schemas                       | —              |
| `CHEX_SCHEMA_DIR`     | Schema directory for `@d31ma/chex` (synced from `FYLO_SCHEMA_DIR`) | —              |
| `FYLO_STRICT`         | Validate documents with chex before writes                         | —              |
| `FYLO_ENCRYPTION_KEY` | AES-GCM key for `$encrypted` fields (≥32 chars)                    | —              |
| `FYLO_CIPHER_SALT`    | Salt for blind index derivation                                    | —              |
| `FYLO_LOGGING`        | Enable logging (`"1"`)                                             | —              |

S3 index credentials (resolved in order: explicit options → `AWS_*` → `FYLO_S3_*`):

| Variable                    | AWS equivalent                             |
| --------------------------- | ------------------------------------------ |
| `FYLO_S3_ACCESS_KEY_ID`     | `AWS_ACCESS_KEY_ID`                        |
| `FYLO_S3_SECRET_ACCESS_KEY` | `AWS_SECRET_ACCESS_KEY`                    |
| `FYLO_S3_SESSION_TOKEN`     | `AWS_SESSION_TOKEN`                        |
| `FYLO_S3_ENDPOINT`          | `AWS_ENDPOINT_URL_S3` / `AWS_ENDPOINT_URL` |
| `FYLO_S3_REGION`            | `AWS_REGION` / `AWS_DEFAULT_REGION`        |

Copy `.env.example` to `.env` and fill in your values.

---

## CRUD Operations

### Create

```ts
const id = await db.putData('users', {
    name: 'Jane Doe',
    age: 29,
    team: 'platform'
})
```

### Read

```ts
const doc = await db.getDoc('users', id).once()
```

### Update (creates a new immutable version)

```ts
const nextId = await db.patchDoc('users', {
    [id]: { team: 'core-platform' }
})
```

### Delete

```ts
await db.delDoc('users', nextId)
```

---

## Querying

FYLO queries use prefix indexes first, then hydrate only matching documents.

```ts
// Exact match
const results = {}
for await (const doc of db
    .findDocs('users', {
        $ops: [{ name: { $eq: 'Alice' } }]
    })
    .collect()) {
    Object.assign(results, doc)
}

// Range query (numeric fields)
for await (const doc of db
    .findDocs('users', {
        $ops: [{ age: { $gte: 18 } }]
    })
    .collect()) {
    Object.assign(results, doc)
}

// Contains (array membership)
for await (const doc of db
    .findDocs('users', {
        $ops: [{ tags: { $contains: 'engineering' } }]
    })
    .collect()) {
    Object.assign(results, doc)
}

// OR across conditions
for await (const doc of db
    .findDocs('users', {
        $ops: [{ role: { $eq: 'admin' } }, { role: { $eq: 'owner' } }]
    })
    .collect()) {
    Object.assign(results, doc)
}
```

### SQL Support

```ts
await db.executeSQL(`CREATE TABLE posts`)
await db.executeSQL(`INSERT INTO posts VALUES { "title": "Hello", "published": true }`)
const posts = await db.executeSQL(`SELECT * FROM posts WHERE published = true`)
```

### Query Strategy

| Operator                     | Index used                        | Fallback                 |
| ---------------------------- | --------------------------------- | ------------------------ |
| `$eq`                        | Exact match key (`eq`)            | —                        |
| `$gt`, `$gte`, `$lt`, `$lte` | Sortable numeric key (`n`/`nr`)   | Full scan if non-numeric |
| `$contains`                  | Exact match on array members      | —                        |
| `$like "ali%"`               | Forward prefix (`f`)              | Full scan                |
| `$like "%ice"`               | Reversed prefix (`r`)             | Full scan                |
| `$like "%lic%"`              | Trigram (`g3`) → hydrate → verify | Full scan                |

---

## Schema Versioning

Schemas live under `FYLO_SCHEMA_DIR` in a per-collection layout:

```text
<FYLO_SCHEMA_DIR>/
  <collection>/
    manifest.json          ← { current, versions: [{v, sha256?, addedAt?}] }
    history/
      v1.json              ← chex regex schema
      v2.json              ← head is whichever manifest.current points at
    upgraders/
      v1-to-v2.js          ← export default async (doc) => upgradedDoc
    rules.json             ← optional RLS rules
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

Chex regex schemas (`history/v2.json`):

```json
{
    "id": "^[0-9]+$",
    "title": "^.+$",
    "body": "^.+$",
    "slug": "^[a-z0-9-]+$"
}
```

Upgraders are pure functions:

```js
export default function upgrade(doc) {
    return {
        ...doc,
        slug:
            String(doc.title ?? '')
                .toLowerCase()
                .replace(/[^a-z0-9]+/g, '-')
                .replace(/^-+|-+$/g, '') || 'untitled'
    }
}
```

Behavior:

- Documents carry `_v` (version label)
- Reads materialize old docs to head shape in memory
- Strict writes validate against head schema via `@d31ma/chex`
- Documents missing `_v` are treated as oldest version (legacy upgrade on read)

---

## Encryption

Fields declared in `$encrypted` arrays are stored with AES-GCM. Equality queries use HMAC blind indexes — lookups work without decrypting, but an attacker with index access can count repetitions.

```json
{
    "$encrypted": ["ssn", "email"],
    "id": "^[0-9]+$",
    "name": "^.+$",
    "email": "^.+$",
    "ssn?": "^[0-9-]+$"
}
```

Requirements:

- `FYLO_ENCRYPTION_KEY` must be ≥32 characters
- `FYLO_CIPHER_SALT` is recommended
- Process-global: one key for all collections

---

## Auth & Row-Level Security

FYLO does not authenticate. Your app verifies identity; FYLO enforces policy.

```ts
const db = new Fylo({
    root: '/mnt/fylo',
    auth: {
        authorize({ auth, action, collection, data }) {
            if (auth.roles?.includes('admin')) return true
            if (action === 'doc:create') {
                return (data as { tenantId?: string }).tenantId === auth.tenantId
            }
            return action === 'doc:read' || action === 'doc:find'
        }
    }
})

const user = await verifyRequest(request)
const scoped = db.as({
    subjectId: user.id,
    tenantId: user.tenantId,
    roles: user.roles
})

const posts = scoped.findDocs('posts', {
    $ops: [{ tenantId: { $eq: user.tenantId } }]
})
```

Actions: `doc:read`, `doc:create`, `doc:update`, `doc:delete`, `bulk:import`, `bulk:export`, `join:execute`, `sql:execute`, `collection:rebuild`.

---

## WORM Mode

Append-only at the logical document level:

```ts
const db = new Fylo({
    root: '/mnt/fylo',
    worm: {
        mode: 'append-only',
        deletePolicy: 'tombstone' // or 'reject'
    }
})

const v1 = await db.putData('posts', { title: 'v1' })
const v2 = await db.patchDoc('posts', { [v1]: { title: 'v2' } })

const head = await db.getLatest('posts', v1) // current head
const history = await db.getHistory('posts', v2) // full chain
```

- Each update writes a new immutable version
- One logical head per lineage for normal queries
- `getDoc(versionId)` reads any retained version directly
- Timestamps come from TTIDs, not filesystem `mtime`

---

## Syncing & Replication

FYLO owns document storage and querying. **You** own how the root directory reaches remote storage.

Sync hooks let FYLO notify your storage client:

```ts
const db = new Fylo({
    root: '/mnt/fylo',
    syncMode: 'await-sync', // or 'fire-and-forget'
    sync: {
        async onWrite(event) {
            await s3.putObject({
                key: `${event.collection}/${event.docId}.json`,
                body: await Bun.file(event.path).arrayBuffer()
            })
        },
        async onDelete(event) {
            await s3.deleteObject({
                key: `${event.collection}/${event.docId}.json`
            })
        }
    }
})
```

| Mode              | Behavior                                       |
| ----------------- | ---------------------------------------------- |
| `await-sync`      | Waits for hook, throws if sync fails           |
| `fire-and-forget` | Commits locally first, runs hook in background |

WORM mode sync carries lineage metadata under `event.worm` (head operation type, lineage ID, delete mode).

---

## Local Queue

Opt-in durable local queue for event-driven workflows:

```ts
const db = new Fylo({ root: '/mnt/fylo', queue: true })

import { consume, publish } from '@d31ma/fylo'

class UserConsumer {
    @consume('users.insert', { group: 'email-service', autoAck: false })
    async welcome(message, context) {
        await sendWelcomeEmail(message.payload.doc.email)
        context.ack()
    }
}

class UserService {
    @publish('users.created')
    async createUser(input) {
        return { id: 'u1', email: input.email }
    }
}

await db.queue.drainRegistered(new UserConsumer())
```

Queue files:

```text
<root>/.queue/topics/<topic>.ndjson
<root>/.queue/consumers/<group>/<topic>.json
<root>/.queue/dlq/<topic>.ndjson
```

At-least-once delivery, consumer-group checkpoints, advisory leases, retry tracking, dead-letter files. Handlers should be idempotent.

---

## CLI & Machine Interface

### CLI

```bash
# Query
fylo.query "SELECT * FROM posts WHERE published = true"
fylo.query sql "SELECT * FROM posts" --page-size 25

# Admin
fylo.admin inspect posts --root /mnt/fylo --json
fylo.admin rebuild posts --root /mnt/fylo
fylo.admin get posts 4UUB32VGUDW --root /mnt/fylo --json

# Schema
fylo.admin schema inspect article --schema-dir ./schemas --json
fylo.admin schema doctor article --schema-dir ./schemas
fylo.admin schema validate article @article.json --schema-dir ./schemas --json
```

### Machine Interface (cross-language)

```bash
echo '{"op":"inspectCollection","root":"/mnt/fylo","collection":"posts"}' | fylo.exec exec --request -
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

Supported operations: `executeSQL`, `createCollection`, `dropCollection`, `inspectCollection`, `rebuildCollection`, `getDoc`, `getLatest`, `getHistory`, `findDocs`, `joinDocs`, `putData`, `batchPutData`, `patchDoc`, `patchDocs`, `delDoc`, `delDocs`, `importBulkData`, `schemaInspect`, `schemaCurrent`, `schemaHistory`, `schemaDoctor`, `schemaValidate`, `schemaMaterialize`.

### Compiled Executable

```bash
bun run build:exe
./dist-bin/fylo exec --request @request.json
```

Callable from Python, Go, Rust, Java — write JSON to stdin, read JSON from stdout.

---

## Recovery & Rebuild

Documents are truth. Indexes are derived. When they drift:

```ts
const result = await db.rebuildCollection('posts')
// {
//   collection: 'posts',
//   worm: true,
//   docsScanned: 42,
//   indexedDocs: 30,
//   headsRebuilt: 30,
//   versionMetasRebuilt: 42,
//   staleHeadsRemoved: 1,
//   staleVersionMetasRemoved: 0
// }
```

```bash
fylo.admin rebuild posts --root /mnt/fylo --json
```

Use `rebuildCollection()` after operator-level recovery or when external processes have modified document files directly.

---

## Limitations

| Limitation                           | Detail                                                                                         |
| ------------------------------------ | ---------------------------------------------------------------------------------------------- |
| **Filesystem-only engine**           | One engine writes to a local path. Remote replication is your responsibility.                  |
| **Advisory locking**                 | Lock-files with TTL. Networked filesystems without atomic `link()` are not supported.          |
| **Indexes are derived**              | External writes to document files won't update indexes. Use `rebuildCollection()`.             |
| **Logical WORM**                     | Enforced by FYLO write paths. Pair with OS-level immutability for true WORM.                   |
| **Frequency leaks on encryption**    | HMAC blind indexes for `$eq` reveal value repetition even without decryption.                  |
| **Process-global cipher**            | One key per process for all `$encrypted` fields. No per-collection key rotation built in.      |
| **No cross-collection transactions** | Writes are serialized per collection. No atomic multi-collection commits.                      |
| **TTID-derived timestamps**          | `createdAt`/`updatedAt` from TTIDs, not filesystem `mtime`. Stable but tooling can't override. |
| **Bulk import for trusted sources**  | SSRF guard blocks private addresses and caps at 50 MiB. Not for user-provided URLs.            |

---

## License

MIT © D31MA
