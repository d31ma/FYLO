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

| Platform         | File                     | How                                     |
| ---------------- | ------------------------ | --------------------------------------- |
| Browser (JS)     | `browser/fylo.js` loader | OPFS store                              |
| iOS (Swift)      | `swift/Fylo.swift`       | WKWebView hosting `fylo.mjs`            |
| Android (Kotlin) | `kotlin/Fylo.kt`         | android.webkit.WebView                  |
| Flutter (Dart)   | `flutter/fylo.dart`      | flutter_inappwebview hosting `fylo.mjs` |

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

| Op         | Python / Ruby / Rust | Node / PHP / Java / Swift / Kotlin / Dart | Go / C#      |
| ---------- | -------------------- | ----------------------------------------- | ------------ |
| putData    | `put_data`           | `putData`                                 | `PutData`    |
| getMeta    | `get_meta`           | `getMeta`                                 | `GetMeta`    |
| setMeta    | `set_meta`           | `setMeta`                                 | `SetMeta`    |
| findDocs   | `find_docs`          | `findDocs`                                | `FindDocs`   |
| executeSQL | `execute_sql`        | `executeSQL`                              | `ExecuteSQL` |

The larger dynamic shims cover the full common machine-operation set, including
batch, bulk, deleted-document, and join helpers. Compact compiled-language
shims intentionally expose a smaller dedicated-method set. Use the raw
`request(op)` escape hatch for an operation that has no dedicated method in your
language (including branching and schema administration); the authoritative
operation list is in `fylo --help` and `src/cli/machine.js`.

Most shims unwrap and return the operation's `result`. Rust and Java return the
validated raw response JSON string from their dedicated methods, so callers
decode the envelope and read its `result` field. C# returns an unwrapped
`JsonElement`. Every shim raises, throws, or returns an error for a failed
machine response.

Object arguments are always the language's **native container** — a `dict`
(Python), `Hash` (Ruby), object (Node), associative array (PHP), `map[string]any`
(Go), `Map`/`List` (Java/Kotlin/Dart), `Dictionary`/array (C#/Swift), or the small
built-in `Json` builder (Rust). No hand-written JSON strings; each shim serializes
for you with a dependency-free encoder (or the platform's built-in one).

### Collection facade

Every client also has a **collection-scoped** view with short method names, so you
don't repeat the collection on each call — this reads like the browser client:

```
db.collection("users").put(data)     // create/drop/inspect/rebuild
db.collection("users").latest(id)    // put/get/latest/patch/delete/restore
db.collection("users").find(query)   // find
```

`db.collection(name)` (PascalCase `Collection` in Go/C#) works in every language.
In the **dynamic languages** you can drop `collection(...)` entirely and index by
name — `db.users.put(data)` (Node), `db.users.put(data)` (Python), `db.users.put(data)`
(Ruby), `$db->users->put($data)` (PHP). The method-per-op API (`db.putData("users",
data)`) stays available everywhere; the facade is additive sugar over it.

Developer metadata uses `getMeta(collection, id)` and `setMeta(collection, id,
meta)` (snake_case in Python/Ruby/Rust; PascalCase in Go/C#). Collection facades
use `getMeta(id)` / `setMeta(id, meta)`, except Python and Ruby, which use
`get_metadata(id)` / `set_metadata(id, meta)`, and PHP, which uses
`getMetadata(id)` / `setMetadata(id, meta)`.

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

## SQL

Every client has `executeSQL(sql)` for a raw string. Each also exposes a `sql`
helper you interpolate with your language's **native** syntax:

| Language         | Interpolation                            | Escapes values? |
| ---------------- | ---------------------------------------- | --------------- |
| Node/TS          | `` db.sql`… ${x}` `` (tagged template)   | ✅ safe         |
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

Node and C# hand the interpolated values to the client, which escapes them —
those are injection-safe. The rest interpolate **before** the client sees the
string, so values are inlined verbatim: **escape or validate untrusted input
yourself**, or keep to app-generated SQL. On the mobile clients, `sql` runs
against the local on-device store.

### SQL UID and mode

Thin shims can send an authenticated POSIX UID with SQL execution. `mode` is
accepted only for `INSERT`; omit it from `SELECT`, `UPDATE`, and `DELETE`.
UID-only inserts default to `0o600`.

| Language | Protected SQL call                                                                   |
| -------- | ------------------------------------------------------------------------------------ |
| Node/TS  | ``await db.sql`INSERT INTO users (name) VALUES (${name})`.as({ uid, mode: 0o600 })`` |
| Python   | `db.sql(query, {"uid": uid, "mode": 0o600})`                                         |
| Ruby     | `db.sql(query, { "uid" => uid, "mode" => 0o600 })`                                   |
| PHP      | `$db->sql($query, ['uid' => $uid, 'mode' => 0600])`                                  |
| Go       | `db.Sql(query, map[string]any{"uid": uid, "mode": 0o600})`                           |
| Rust     | `db.sql_as(query, uid, Some(0o600))`                                                 |
| Java     | `db.sql(query, Map.of("uid", uid, "mode", 384))`                                     |
| C#       | `db.Sql($"INSERT ...", new { uid, mode = 384 })`                                     |
| Dart     | `db.sql(query, uid: uid, mode: 384)`                                                 |

The machine payload is the same for every shim:

```json
{
    "op": "executeSQL",
    "sql": "INSERT INTO users (name) VALUES ('Ada')",
    "access": { "uid": 1001, "mode": 384 }
}
```

`SELECT` returns only rows readable by that UID. `UPDATE` and `DELETE` use the
same UID for both candidate visibility and write authorization. This is a
native-POSIX feature: browser and mobile local-only clients cannot enforce
`chown`/`chmod` and therefore do not accept this access context.

## How the shims work

Each thin shim spawns **one** long-lived process — `fylo exec --loop --root <db>` —
and talks to it over stdin/stdout as newline-delimited JSON: one request object
per line, one response object per line, in order. The process keeps the engine
(indexes, catalog) warm across every call, so you pay startup once, not per
operation. No port, no network, no auth surface; the child dies with your app.
Pass `--root` once on spawn (the shims do this); per-request `root` is optional.

## Browser (`fylo-web.mjs`)

Browsers can't spawn the binary, so the web client is different: it's a **bundled
local-only engine** (built from `src/browser`, released as `fylo-web.mjs`). It
reads and writes OPFS, memory, or a user-selected File System Access directory
directly — fully offline, no backend, no network.

For a regular website, add a version-pinned loader to the document head:

```html
<script src="https://d31ma.github.io/FYLO/version/26.29.04/fylo.js"></script>
```

Then open the browser-local database from your application code:

```js
const db = await Fylo.open()

const id = await db.users.put({ name: 'Ada', role: 'admin' })
await db.users.put(id).metadata({ source: 'browser' })
const metadata = await db.users.get(id).metadata()
const doc = await db.users.latest(id)
```

Use `https://d31ma.github.io/FYLO/version/latest/fylo.js` when you intentionally
want the newest release. For direct ESM imports, the engine is published beside
the loader:

```js
import { createBrowserClient } from 'https://d31ma.github.io/FYLO/version/26.29.04/fylo-web.mjs'

const db = createBrowserClient()
await db.ready()
```

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

## Mobile (iOS / Android)

Phones can't spawn the binary either, so the mobile clients are **local-only**
like the browser: they host the same web engine (`fylo.mjs`) in a headless
WebView (WKWebView on iOS, `android.webkit.WebView` on Android) and expose a
native async API. All reads and writes hit an on-device OPFS store — fully
offline, no backend.

Bundle three files as app assets: `fylo.mjs` (from a Fylo release) plus
`host.html` and `bridge.js` from [`mobile/`](mobile/). They must be served from a
**secure origin** — a custom `fylo-app://` scheme on iOS, an `https://` origin via
request interception on Android — or the browser engine can't use OPFS.

```swift
// iOS — clients/swift/Fylo.swift (Foundation + WebKit)
let db = try await Fylo()
try await db.putData("users", ["name": "Ada", "role": "admin"])
let admins = try await db.findDocs("users", ["$ops": [["role": ["$eq": "admin"]]]])
```

```kotlin
// Android — clients/kotlin/Fylo.kt (android.webkit + org.json + coroutines)
val db = Fylo.open(context)
db.putData("users", mapOf("name" to "Ada", "role" to "admin"))
val admins = db.findDocs("users", mapOf("\$ops" to listOf(mapOf("role" to mapOf("\$eq" to "admin")))))
```

Same method names as the shims (`createCollection`, `putData`, `getLatest`,
`patchDoc`, `delDoc`, `restoreDoc`, `findDocs`, `sql`).

## Flutter

Flutter apps are Dart, so where the app runs decides which client to use:

- **Flutter iOS / Android / desktop** → `flutter/fylo.dart` — the same local-only
  model as the native mobile clients, hosting `fylo.mjs` in a headless
  [`flutter_inappwebview`](https://pub.dev/packages/flutter_inappwebview) (`^6.0.0`).
  It exposes the same async API + `collection()` facade. Add the three engine
  assets under `assets/fylo/` (`fylo.mjs` from a release, plus `host.html` and
  `bridge.js` from `mobile/`) and declare them in `pubspec.yaml`.
- **Flutter web** → use `fylo-web.mjs` directly via `dart:js_interop`. It already
  runs in a browser with OPFS, so no WebView is needed.
- **Flutter desktop, native binary** → the `dart/fylo.dart` shim also works, since
  desktop can spawn the `fylo` binary.

```dart
import 'fylo.dart';

final db = await Fylo.open();
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

The shims send one request at a time and read one response (guarded by a lock
where the language needs it). The protocol carries a `requestId` echoed back in
each response, so if you need pipelining you can send many requests and match
replies by id — but start simple; one-in-flight is enough for most apps.

## Example (Python)

```python
from fylo import Fylo

with Fylo("/path/to/db") as db:
    db.createCollection("users")
    doc_id = db.putData("users", {"name": "Ada", "score": 90})
    doc = db.getLatest("users", doc_id)
    print(doc)   # {"<id>": {"name": "Ada", "score": 90}}
    winners = db.findDocs("users", {"$ops": [{"score": {"$gte": 50}}]})
```

Construct with a db root, call the operation methods, close when done (or use a
`with`/`using`/`try`-with-resources block). Each file's header comment has a
runnable example in that language.
