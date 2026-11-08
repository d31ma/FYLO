# Fylo language clients

Drop-in clients that let an app use Fylo like a library — no npm, no native
addon. There are two kinds, sharing the same method names:

- **Thin shims** spawn the compiled `fylo` binary and speak the machine protocol
  over stdin/stdout. Install one binary, drop in one file for your language.
- **Local-only clients** embed Fylo's engine on-device (a phone or a browser
  can't spawn the binary). Reads and writes hit the device's own OPFS store —
  fully offline, no backend, no network.

### Thin shims (spawn the binary)

| Language | File             | Runtime deps    |
| -------- | ---------------- | --------------- |
| Python   | `python/fylo.py` | none (stdlib)   |
| Ruby     | `ruby/fylo.rb`   | none (stdlib)   |
| Node/TS  | `node/fylo.mjs`  | none (stdlib)   |
| PHP      | `php/fylo.php`   | none (ext-json) |
| Go       | `go/fylo.go`     | none (stdlib)   |
| Rust     | `rust/fylo.rs`   | none (std)      |
| C#       | `csharp/Fylo.cs` | none (BCL)      |
| Java     | `java/Fylo.java` | none (JDK)      |
| Dart     | `dart/fylo.dart` | none (SDK)      |

### Local-only clients (embed the engine on-device)

| Platform         | File                     | How                                       |
| ---------------- | ------------------------ | ----------------------------------------- |
| Browser (JS)     | `browser/fylo.js` loader | OPFS store                                |
| iOS (Swift)      | `swift/Fylo.swift`       | WKWebView hosting `fylo.mjs`              |
| Android (Kotlin) | `kotlin/Fylo.kt`         | android.webkit.WebView hosting `fylo.mjs` |
| Flutter (Dart)   | `flutter/fylo.dart`      | flutter_inappwebview hosting `fylo.mjs`   |

The mobile hosts load the engine as `fylo.mjs`: save the release asset
`fylo-web.mjs` under that name (see [Mobile](#mobile-ios--android)). The local-only
clients do not support raw-file buckets, schemas, or field encryption in this
release; see [Local-only limits](#local-only-limits).

See [Browser](#browser-fylo-webmjs), [Mobile](#mobile-ios--android), and
[Flutter](#flutter) below.

## Install the binary

```sh
# macOS / Linux
curl -fsSL https://fylo.del.ma/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://fylo.del.ma/install.ps1 | iex
```

The installer grabs the right binary for your OS/arch from the latest
[GitHub release](https://github.com/d31ma/Fylo/releases), verifies its checksum,
and puts `fylo` on your PATH. Or pick a build from the
[download page](https://fylo.del.ma/download). Then verify: `fylo --help`.

## Get the client for your language

These files live in this directory, and each release also ships them as a
**version-matched** `fylo-clients.tar.gz` (paired with that release's binary /
machine protocol). Grab the one file for your language:

```sh
curl -fsSL https://github.com/d31ma/Fylo/releases/latest/download/fylo-clients.tar.gz | tar -xz
# then copy e.g. clients/python/fylo.py into your project
```

## The API

Each shim exposes a named method per operation, so you call
`db.findDocs("users", query)` rather than assembling a raw request. Method names
follow **each language's own paradigm** — `snake_case` in Python/Ruby/Rust,
`camelCase` in Node/PHP/Java/Swift/Kotlin/Dart, `PascalCase` in Go/C# (their
exported/public methods must be capitalized):

| Op           | Python / Ruby / Rust | Node / PHP / Java / Dart | Go / C#        |
| ------------ | -------------------- | ------------------------ | -------------- |
| putData      | `put_data`           | `putData`                | `PutData`      |
| getMeta      | `get_meta`           | `getMeta`                | `GetMeta`      |
| setMeta      | `set_meta`           | `setMeta`                | `SetMeta`      |
| findDocs     | `find_docs`          | `findDocs`               | `FindDocs`     |
| findDocsPage | `find_docs_page`     | `findDocsPage`           | `FindDocsPage` |
| executeSQL   | `execute_sql`        | `executeSQL`             | `ExecuteSQL`   |
| queuePublish | `queue_publish`      | `queuePublish`           | `QueuePublish` |
| queueClaim   | `queue_claim`        | `queueClaim`             | `QueueClaim`   |
| queueAck     | `queue_ack`          | `queueAck`               | `QueueAck`     |

The larger dynamic shims cover the full common machine-operation set, including
batch, bulk, deleted-document, and join helpers. Compact compiled-language
shims intentionally expose a smaller dedicated-method set. Use the raw
`request(op)` escape hatch for an operation that has no dedicated method in your
language (including branching and schema administration); the authoritative
operation list is in `fylo --help` and `api/machine/v1/operations.json`.

Most shims unwrap and return the operation's `result`. Rust and Java return the
validated raw response JSON string from their dedicated methods, so callers
decode the envelope and read its `result` field. C# returns an unwrapped
`JsonElement`. Every shim raises, throws, or returns an error for a failed
machine response.

### Durable serverless queue

Every thin binary shim exposes publish, claim, acknowledge, negative
acknowledge, lease extension, statistics, and dead-letter reads. The queue is
embedded in the Rust engine and persists under `.fylo-queue/v1`; local-only
clients continue to use their browser engine until the full Rust/Wasm engine
becomes their default storage path.

```js
const sent = await db.queue.publish(
    'jobs.resize',
    { assetId },
    {
        idempotencyKey: `resize:${assetId}`
    }
)
const [delivery] = await db.queue.claim('jobs.resize', 'image-workers', {
    visibilityTimeoutMs: 30_000,
    maxAttempts: 5
})
await db.queue.ack('jobs.resize', 'image-workers', delivery)
```

Other languages use the naming convention in the table. Delivery is at least
once, so handlers must be idempotent. `queueClaim` returns opaque `receipt`
values that fence stale workers. FYLO remains one-owner-per-root: concurrent
tasks share one engine process; this is brokerless local execution, not a
distributed cloud queue.

Where a shim accepts an option map or array, it copies only the documented
operation options. Unknown keys and protected request fields such as `op`,
`topic`, `group`, `payload`, `id`, `receipt`, and `root` are ignored.

#### Queue consumer decorators

Every thin shim includes a one-batch consumer adapter. One invocation claims a
bounded batch, calls the handler once per delivery, acknowledges successful
handlers, and negative-acknowledges thrown or returned failures. The result
reports `claimed`, `acknowledged`, `retried`, and `deadLettered` counts. Queue
settlement errors are propagated and are never mistaken for handler failures.
To avoid persisting credentials or payload data carried by exception messages,
automatic consumers always use the generic reason `queue handler failed`.
Trusted callers may provide an explicit diagnostic `reason` through the direct
negative-acknowledgement method.

```js
const sendEmail = db.queue.consumer('orders', 'email-service', {
    maxMessages: 10,
    maxAttempts: 5,
    retryDelayMs: 1_000
})(async (delivery) => {
    await email(delivery.payload)
})

// Invoke once from a serverless function, scheduled job, or worker loop.
const outcome = await sendEmail()
```

JavaScript/TypeScript's factory is compatible with standard method-decorator
semantics, and Python exposes a native decorator:

```python
@db.queue_consumer("orders", "email-service", max_messages=10, max_attempts=5)
def send_email(delivery):
    email(delivery["payload"])

outcome = send_email()
```

| Language        | Consumer surface                                                              |
| --------------- | ----------------------------------------------------------------------------- |
| Node/TypeScript | `db.queue.consumer(...)` / `db.queueProcess(...)`                             |
| Python          | `@db.queue_consumer(...)` / `db.queue_process(...)`; async handlers work      |
| Ruby            | `db.queue_consumer(...) { ... }` returns a `Proc`                             |
| PHP             | `#[FyloQueueConsumer(...)]` with `runQueueConsumer`, or `queueConsumer(...)`  |
| Go              | `QueueConsumer(...)` returns a function using `QueueConsumerOptions`          |
| Rust            | `queue_consumer(...)` returns a closure using `QueueConsumerOptions`          |
| Java            | `@Fylo.QueueConsumer(...)` with `runQueueConsumer`, or `queueConsumer(...)`   |
| C#              | `[FyloQueueConsumer(...)]` with `RunQueueConsumer`, or `QueueConsumer(...)`   |
| Dart            | `@FyloQueueConsumer(...)` metadata explicitly bound with `queueConsumer(...)` |

Go handlers receive `map[string]any`; C# and Dart receive native JSON objects.
The dependency-free Rust and Java shims pass one validated raw delivery JSON
object because those two single-file clients do not bundle a JSON decoder.
Python's `queue_process` also runs `async` handlers; inside an event loop that
is already running, prefer `queue_process_async`. Decorators process one batch
rather than polling forever, keeping execution and scaling under the host
platform's control. Long handlers can call the existing lease-extension method
before the visibility deadline.

Object arguments are always the language's **native container** — a `dict`
(Python), `Hash` (Ruby), object (Node), associative array (PHP), `map[string]any`
(Go), `Map`/`List` (Java/Kotlin/Dart), `Dictionary`/array (C#/Swift), or the small
built-in `Json` builder (Rust). No hand-written JSON strings; each shim serializes
for you with a dependency-free encoder (or the platform's built-in one).

### Collection facade

Every client also has a **collection-scoped** view with short method names, so you
don't repeat the collection on each call — this reads like the browser client:

```
db.collection("users").create()      // also drop/inspect/rebuild
db.collection("users").put(data)     // also get/latest/patch/delete/restore
db.collection("users").find(query)   // find
```

Go and Rust take the kind explicitly: `Create("document")` / `create("document")`.

`db.collection(name)` (PascalCase `Collection` in Go/C#) works in every language.
In the **dynamic languages** you can drop `collection(...)` entirely and index by
name — `db.users.put(data)` (Node), `db.users.put(data)` (Python), `db.users.put(data)`
(Ruby), `$db->users->put($data)` (PHP). The method-per-op API (`db.putData("users",
data)`) stays available everywhere; the facade is additive sugar over it.

The browser facade offers the fluent signatures
`put(id, documentOrFile).metadata(meta)`, `put(id).metadata(meta)`, and
`get(id).metadata()`. Thin binary shims map those semantics to the machine
protocol's explicit metadata calls: `getMeta(collection, id)` and
`setMeta(collection, id, meta)` (snake_case in Python/Ruby/Rust; PascalCase in
Go/C#). Collection facades use `getMeta(id)` / `setMeta(id, meta)`, except
Python and Ruby, which use `get_metadata(id)` / `set_metadata(id, meta)`, and
PHP, which uses `getMetadata(id)` / `setMetadata(id, meta)`. Creating data and
metadata in one machine request uses `putData` with its top-level `meta` field;
shim convenience methods do not change the merge or deletion rules.

In result-unwrapping shims, `getMeta` returns the complete canonical metadata
record plus developer metadata. Every record includes `id`, `mtime`,
`updatedAt`, and `createdAt`; raw files also include their stored file
descriptor. Canonical fields take precedence over custom keys with the same
name. Rust and Java return their raw response envelope as described above.
`setMeta` bulk-edits the record: supplied keys are set, `null` values remove
keys, and omitted keys remain unchanged. The metadata argument must be a plain native map/object with
names of 1-64 characters, starting with a letter or digit and containing only
letters, digits, `.`, `_`, or `-`. Each value must be JSON-serializable and no
larger than 60 KiB after UTF-8 JSON encoding. The local executable stores
metadata as filesystem xattrs; the browser client uses an internal OPFS sidecar.

The mobile clients are local-only WebView hosts. They allowlist the three FYLO
assets, deny other navigation/network resources, cap requests and responses,
bound pending calls, and time out or drain requests on cancellation, load or
renderer failure, and close. Configure a shorter RPC timeout when the default
30 seconds is inappropriate for your application; values are capped at five
minutes.

```js
// Node shim; use the naming convention from the table in other languages.
await db.users.setMeta(id, { source: 'import', reviewed: false })
await db.users.setMeta(id, { source: null }) // remove one key
const metadata = await db.users.getMeta(id)
// { id, mtime, updatedAt, createdAt, reviewed: false }
```

## Errors

A failed operation raises, throws, or returns an error, depending on the
language. The error carries the machine error `code` from
[`api/errors/v1.json`](../api/errors/v1.json) (for example `EBADREQUEST` or
`EROOTLOCKED`) alongside a human-readable message. Branch on the code, not on
the message: messages may change between releases, codes are part of the
versioned contract.

| Language        | Where the code is                                                                            |
| --------------- | -------------------------------------------------------------------------------------------- |
| Node/TypeScript | `err.code`                                                                                   |
| Python          | `FyloError.code`                                                                             |
| Ruby            | `FyloError#code`                                                                             |
| PHP             | `FyloError::getFyloCode()` (`getCode()` stays `0`)                                           |
| Go              | `var e *fylo.Error; if errors.As(err, &e) { e.Code }`                                        |
| Rust            | `FyloError::code_of(&io_error) -> Option<&str>`; methods still return `io::Result`           |
| Java            | `Fylo.FyloException.getCode()` (extends `IOException`; the message is the raw response line) |
| C#              | `FyloException.Code`                                                                         |
| Dart            | `FyloException.code`                                                                         |

The Swift, Kotlin, and Flutter clients also report the machine code on their
error type; see the header comment of each client file for the accessor.

The raw `request` escape hatch takes one JSON object per line, so the Rust,
Java, and C# shims reject a raw request string that contains a carriage return
or line feed before writing anything to the child: Rust returns an
`io::ErrorKind::InvalidInput` error, Java throws `IllegalArgumentException`,
and C# throws `ArgumentException`.

## SQL

Every client has `executeSQL(sql)` for a raw string (`execute_sql` in
Python/Ruby/Rust, `ExecuteSQL` in Go/C#). Each also exposes a `sql` helper you
interpolate with your language's **native** syntax:

| Language         | Interpolation                            | Escapes values? |
| ---------------- | ---------------------------------------- | --------------- |
| Node/TS          | `` db.sql`… ${x}` `` (tagged template)   | ✅ safe         |
| Browser (JS)     | `` db.sql`… ${x}` `` (tagged template)   | ✅ safe         |
| C#               | `db.Sql($"… {x}")` (`FormattableString`) | ✅ safe         |
| Python           | `db.sql(f"… {x}")`                       | ❌ verbatim     |
| Ruby             | `db.sql("… #{x}")`                       | ❌ verbatim     |
| PHP              | `$db->sql("… $x")`                       | ❌ verbatim     |
| Go               | `db.Sql(fmt.Sprintf("… %v", x))`         | ❌ verbatim     |
| Java             | `db.sql("… " + x)`                       | ❌ verbatim     |
| Rust             | `db.sql(&format!("… {x}"))`              | ❌ verbatim     |
| Swift (iOS)      | `try await db.sql("… \(x)")`             | ❌ verbatim     |
| Kotlin (Android) | `db.sql("… $x")`                         | ❌ verbatim     |
| Dart             | `db.sql("… $x")`                         | ❌ verbatim     |
| Flutter          | `db.sql("… $x")`                         | ❌ verbatim     |

Node, the browser client, and C# hand the interpolated values to the client,
which escapes them — those are injection-safe. The rest interpolate **before** the client sees the
string, so values are inlined verbatim: **escape or validate untrusted input
yourself**, or keep to app-generated SQL. In the browser, prefer the `sql` tagged
template over `executeSQL` whenever a statement includes a value. On the mobile
and Flutter clients, `sql` runs against the local on-device store.

### Access control

Per-record access was removed. No shim sends a `uid`, `gid`, `groups`, or
`mode` with a request, and the binary refuses a request that carries an
`access` field with `EBADREQUEST`.

Access is now granted per application identity on a protected `.vdbx`
database. The shim's `fylo` process opens the database as one identity. Set the
identity with `fylo.identity` in `application.json` or with `FYLO_IDENTITY`,
and pin the expected signer with `fylo.trustAnchor` or `FYLO_TRUST_ANCHOR`. An
administrator grants each identity `read`, `write`, or `admin` with
`fylo access grant`. To move an existing root, see
[Migrating to protected databases](../docs/migrating-to-protected-databases.md).

## How the shims work

Each thin shim spawns **one** long-lived process — `fylo exec --loop --root <db>` —
and talks to it over stdin/stdout as newline-delimited JSON: one request object
per line, one response object per line, in order. The process keeps the engine
(indexes, catalog) warm across every call, so you pay startup once, not per
operation. No port, no network, no auth surface; the child dies with your app.
Pass `--root` once on spawn (the shims do this); per-request `root` is optional.

The machine protocol defaults to a 1 MiB request frame and an 8 MiB response
frame, excluding the LF delimiter. The Node shim enforces those bounds on both
sides and performs a side-effect-free handshake when it starts:

```js
const db = new Fylo('/mnt/fylo', {
    binary: '/opt/fylo/bin/fylo',
    exclusiveRoot: true,
    maxRequestBytes: 1024 * 1024,
    maxResponseBytes: 8 * 1024 * 1024
})

const identity = await db.handshake()
console.log(identity.runtimeVersion, identity.commit, identity.machine)
```

Every binary-backed shim accepts an optional `application.json` configuration
path. Settings live under `fylo`, with nested keys such as `fylo.shard.width`.
The former `env` file/map option is replaced by `config`:

| SDK             | Configuration                                                                                                    |
| --------------- | ---------------------------------------------------------------------------------------------------------------- |
| Node/TypeScript | `new Fylo(root, { config: './application.json' })`                                                               |
| Python          | `Fylo(root, config='./application.json')`                                                                        |
| Ruby            | `Fylo.open(root, config: './application.json')`                                                                  |
| PHP             | `new Fylo($root, 'fylo', './application.json')`                                                                  |
| Go              | `fylo.OpenWithOptions(root, fylo.Options{Config: "./application.json"})`                                         |
| Rust            | `Fylo::open_with_options(root, FyloOptions { config: Some("./application.json".into()), ..Default::default() })` |
| Java            | `new Fylo(root, options)` with `options.config = "./application.json"`                                           |
| C#              | `new Fylo.Fylo(root, new Fylo.Fylo.Options { Config = "./application.json" })`                                   |
| Dart            | `Fylo.open(root, config: './application.json')`                                                                  |

Each component can choose its own configuration file:

```js
const mail = new Fylo('/var/lib/caduceus/mail', {
    binary: '/opt/fylo/bin/fylo',
    config: './config/mail/application.json'
})

const identity = new Fylo('/var/lib/caduceus/identity', {
    binary: '/opt/fylo/bin/fylo',
    config: './config/identity/application.json'
})
```

The native CLI loads the selected file, or discovers `application.json` in its
working directory when `config` is omitted. Explicit constructor roots and
engine flags take precedence over JSON. Paths inside JSON resolve from the
configuration directory. An absent default file uses engine defaults, but an
explicitly selected missing file, invalid setting, or missing secret reference
fails initialization.

Keep `.env` beside that configuration file and use it only for secrets. Reference
them explicitly with `"encryption": { "key": "${FYLO_ENCRYPTION_KEY}" }` and
`"cipher": { "salt": "${FYLO_CIPHER_SALT}" }`. Configure both references together
or omit both; literal credentials in JSON are rejected.
Inherited environment secrets take precedence over the adjacent `.env`; all
unreferenced variables are ignored. Loading configuration never mutates the
process environment or injects `.env` keys into the child environment.

Treat configuration and binary paths as trusted bootstrap input, not request or
tenant data. Keep secret files out of source control and owner-readable only
(for example, `chmod 600 .env`); prefer a secret manager in production. See the
[configuration guide](../docs/configuration.md) for the full migration and secret
reference contract.

Browser and mobile clients do not implicitly read these files because they do
not spawn the native FYLO binary. Configure their storage adapters directly,
and never publish native secrets in website JSON or bundles.

`exclusiveRoot` makes a competing loop reject its startup handshake with
`EROOTLOCKED`. The configured frame limits are passed to the child and must
match the effective values returned by its handshake. The Node shim rejects an
oversized outbound request without killing a healthy child; an oversized or
malformed inbound frame kills the child and rejects every pending request
because response synchronization is no longer trustworthy.

Every shipped binary-backed shim uses the same 1 MiB/8 MiB contract, rejects an
oversized outbound request, and caps input before LF. Supervisors that
implement their own protocol driver should issue `{"op":"handshake"}` first
and restart the child after any ambiguous response framing. See the root
[machine-interface contract](../README.md#machine-interface-cross-language)
for exact error, retry, filesystem, and crash-recovery semantics.

Large active and deleted-document queries use `findDocsPage` and
`findDeletedDocsPage` (idiomatic casing per the table above). Start with a page
such as `{ limit: 256 }`, consume `result.items`, and pass
`{ limit: 256, cursor: result.nextCursor }` until `nextCursor` is null. The
collection facade exposes `findPage`. Keep every continuation on the same shim
instance: the opaque cursor belongs to that persistent binary process and to
the exact query/access scope. If the child restarts, the cursor expires, or the
binary returns `EINVALIDCURSOR`, discard the partial traversal and restart page
one. The snapshot is immutable, so writes during traversal appear only in a new
traversal.

Pagination is a binary machine-protocol feature. Browser, mobile, and Flutter
clients query their embedded local engine directly and do not accept these
process-bound cursors.

## Browser (`fylo-web.mjs`)

Browsers can't spawn the binary, so the web client is different: it's a **bundled
local-only engine** (built from `src/browser`, released as `fylo-web.mjs`). It
reads and writes OPFS, memory, or a user-selected File System Access directory
directly — fully offline, no backend, no network.

For a regular website, add a version-pinned loader to the document head:

```html
<script src="https://d31ma.github.io/FYLO/version/26.33.02/fylo.js"></script>
```

Then open the browser-local database from your application code:

```js
const db = await Fylo.open()

await db.users.create()
const id = await db.users.put({ name: 'Ada', role: 'admin' })
await db.users.put(id).metadata({ source: 'browser' })
const metadata = await db.users.get(id).metadata()
const doc = await db.users.latest(id)
```

Use `https://d31ma.github.io/FYLO/version/latest/fylo.js` when you intentionally
want the newest release. For direct ESM imports, the engine is published beside
the loader:

```js
import { createBrowserClient } from 'https://d31ma.github.io/FYLO/version/26.33.02/fylo-web.mjs'

const db = createBrowserClient()
await db.ready()
```

The loader is executable code with the embedding origin's authority, including
access to that origin's FYLO data. Production applications should pin the
immutable version path; use the mutable `version/latest/` alias only for a
controlled upgrade policy. Higher-assurance deployments should download the
loader and all adjacent module, worker, and Wasm assets from the matching
release, verify them against `SHA256SUMS`, and self-host them. Constrain the
allowed script, module, worker, and asset origins with CSP. Subresource
integrity on `fylo.js` alone cannot authenticate the modules it imports
dynamically.

Enable the worker-hosted Wasm index scanner, or mount a user-selected FYLO root:

```js
const local = createBrowserClient({ storage: 'opfs', worker: true, wasm: true })

// Run from a user gesture. File System Access is currently Chromium-only.
const handle = await showDirectoryPicker({ mode: 'readwrite' })
const mounted = createBrowserClient({
    storage: { type: 'fsa', handle, access: 'readwrite' },
    worker: true,
    wasm: true
})
await mounted.ready()
```

Use `access: 'overlay'` for read-only inspection: document/index writes remain
in memory while reads fall through to the selected directory.

Every release remains available under `version/<version>/`; `version/latest/`
is updated only after a successful Release workflow.

### Local-only limits

The browser loader (`fylo.js` / `fylo-web.mjs`) and the iOS, Android, and Flutter
clients that host it do not support these features in this release:

- raw-file buckets (`createCollection(name, "file")`, `kind: 'file'`)
- schemas (collection schema validation and schema administration)
- field encryption (`$encrypted`)

Such requests fail with an error instead of being ignored. Use the native
`fylo` binary, through any binary-backed shim, when you need them. Create each
collection before its first write; writing to a collection that does not exist
fails with `Collection not found`.

## Mobile (iOS / Android)

Phones can't spawn the binary either, so the mobile clients are **local-only**
like the browser: they host the same web engine (`fylo.mjs`) in a headless
WebView (WKWebView on iOS, `android.webkit.WebView` on Android) and expose a
native async API. All reads and writes hit an on-device OPFS store — fully
offline, no backend.

Bundle three files as app assets: the release asset `fylo-web.mjs` saved as
`fylo.mjs` (the bridge imports `./fylo.mjs`), plus `host.html` and `bridge.js`
from [`mobile/`](mobile/). They must be served from a
**secure origin** — a custom `fylo-app://` scheme on iOS, an `https://` origin via
request interception on Android — or the browser engine can't use OPFS.

Raw-file buckets, schemas, and field encryption are not available on mobile in
this release (see [Local-only limits](#local-only-limits)).

On iOS, add `Fylo.swift` to your app target; it is compiled with your app, so
there is no module to import. On Android, add `Fylo.kt` to your app module
together with the `org.jetbrains.kotlinx:kotlinx-coroutines-android` dependency:
the client resumes on `Dispatchers.Main`, which that artifact provides at runtime.

```swift
// iOS — clients/swift/Fylo.swift (Foundation + WebKit)
let db = try await Fylo()
try await db.createCollection("users")
try await db.putData("users", ["name": "Ada", "role": "admin"])
let admins = try await db.findDocs("users", ["$ops": [["role": ["$eq": "admin"]]]])
```

```kotlin
// Android — clients/kotlin/Fylo.kt (android.webkit + org.json + kotlinx-coroutines-android)
// Inside a coroutine, e.g. lifecycleScope.launch { ... }
val db = Fylo.open(context)
db.createCollection("users")
db.putData("users", mapOf("name" to "Ada", "role" to "admin"))
val admins = db.findDocs("users", mapOf("\$ops" to listOf(mapOf("role" to mapOf("\$eq" to "admin")))))
```

Same method names as the shims (`createCollection`, `putData`, `getLatest`,
`patchDoc`, `delDoc`, `restoreDoc`, `findDocs`, `executeSQL`, `sql`).

## Flutter

Flutter apps are Dart, so where the app runs decides which client to use:

- **Flutter iOS / Android / desktop** → `flutter/fylo.dart` — the same local-only
  model as the native mobile clients, hosting `fylo.mjs` in a headless
  [`flutter_inappwebview`](https://pub.dev/packages/flutter_inappwebview) (`^6.0.0`).
  It exposes the same async API + `collection()` facade. Add the three engine
  assets under `assets/fylo/` (the release asset `fylo-web.mjs` saved as
  `fylo.mjs`, plus `host.html` and `bridge.js` from `mobile/`) and declare them
  in `pubspec.yaml`. Raw-file buckets, schemas, and field encryption are not
  available here in this release (see [Local-only limits](#local-only-limits)).
- **Flutter web** → use `fylo-web.mjs` directly via `dart:js_interop`. It already
  runs in a browser with OPFS, so no WebView is needed.
- **Flutter desktop, native binary** → the `dart/fylo.dart` shim also works, since
  desktop can spawn the `fylo` binary.

```dart
import 'fylo.dart';

final db = await Fylo.open();
await db.collection('users').create();
await db.collection('users').put({'name': 'Ada', 'role': 'admin'});
final admins = await db.collection('users').find({r'$ops': [{'role': {r'$eq': 'admin'}}]});
```

Like the browser, it needs the assets served from a **secure origin** — the client
does this for you by serving them over `http://localhost` (a secure context), so
OPFS is available. On **iOS**, add an App Transport Security exception so the
WebView can reach that localhost server — in `ios/Runner/Info.plist`:

```xml
<key>NSAppTransportSecurity</key>
<dict><key>NSAllowsLocalNetworking</key><true/></dict>
```

## Concurrency

The Node and Dart shims pipeline: each call writes its request to the child
immediately, and a first-in, first-out queue pairs each response with its
request, because the loop answers requests in the order it reads them. The
other shims send one request at a time and wait for its response (guarded by a
lock where the language needs it). Responses also echo an optional `requestId`
from the request, so a custom driver can match replies by id.

## Example (Python)

```python
from fylo import Fylo

with Fylo("/path/to/db") as db:
    db.create_collection("users")
    doc_id = db.put_data("users", {"name": "Ada", "score": 90})
    doc = db.get_latest("users", doc_id)
    print(doc)   # {"<id>": {"name": "Ada", "score": 90}}
    winners = db.find_docs("users", {"$ops": [{"score": {"$gte": 50}}]})
```

Construct with a db root, call the operation methods, close when done (or use a
`with`/`using`/`try`-with-resources block). Each file's header comment has a
runnable example in that language.
