# Fylo language clients

Drop-in clients that let an app use Fylo like a library — no npm, no native
addon. There are two kinds, sharing the same method names:

- **Thin shims** spawn the compiled `fylo` binary and speak the machine protocol
  over stdin/stdout. Install one binary, drop in one file for your language.
- **Local-first clients** embed Fylo's engine on-device (a phone or a browser
  can't spawn the binary) and sync to a backend `fylo serve` over REST — reads
  and writes work fully offline.

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

### Local-first clients (embed the engine, sync over REST)

| Platform         | File                     | How                                     |
| ---------------- | ------------------------ | --------------------------------------- |
| Browser (JS)     | `fylo-web.mjs` (release) | OPFS store + sync engine                |
| iOS (Swift)      | `swift/Fylo.swift`       | WKWebView hosting `fylo.mjs` + sync     |
| Android (Kotlin) | `kotlin/Fylo.kt`         | android.webkit.WebView + sync           |
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
| findDocs   | `find_docs`          | `findDocs`                                | `FindDocs`   |
| executeSQL | `execute_sql`        | `executeSQL`                              | `ExecuteSQL` |

Covered: `createCollection`, `dropCollection`, `inspectCollection`,
`rebuildCollection`, `putData`, `batchPutData`, `getDoc`, `getLatest`,
`patchDoc`, `patchDocs`, `delDoc`, `delDocs`, `restoreDoc`, `findDocs`,
`findDeletedDocs`, `joinDocs`, `executeSQL`, `importBulkData`. Each returns the
operation's **result** and raises/returns an error on failure. For anything
without a dedicated method (branching, schema ops), use the raw `request(op)`
escape hatch — see `fylo --help` and `src/cli/machine.js`.

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
against the local on-device store and its writes are **not** synced — use the
document methods to sync writes.

## How the shims work

Each thin shim spawns **one** long-lived process — `fylo exec --loop --root <db>` —
and talks to it over stdin/stdout as newline-delimited JSON: one request object
per line, one response object per line, in order. The process keeps the engine
(indexes, catalog) warm across every call, so you pay startup once, not per
operation. No port, no network, no auth surface; the child dies with your app.
Pass `--root` once on spawn (the shims do this); per-request `root` is optional.

## Browser (`fylo-web.mjs`)

Browsers can't spawn the binary, so the web client is different: it's a **bundled
local-first engine** (built from `src/browser`, released as `fylo-web.mjs`). It
reads and writes an OPFS/memory store directly — fully offline — and a background
sync engine reconciles with a backend `fylo serve` over REST (`POST /v1/exec` push,
`GET /v1/:collection/events` SSE pull, document-level three-way merge). With no
`serverUrl`, or when the backend can't be pinged, the local store is the store and
writes queue for the next reconnect.

```js
import { createSyncedClient } from './fylo-web.mjs'

const db = createSyncedClient({ serverUrl: 'https://api.example.com', token: FYLO_TOKEN })
await db.ready()
await db.sync.start() // begin connectivity + sync; omit serverUrl for offline-only

const id = await db.users.put({ name: 'Ada', role: 'admin' }) // local write, synced in background
const doc = await db.users.latest(id)
```

Grab `fylo-web.mjs` from a release. Run its backend with
`fylo serve --root <db> --token <token>`.

## Mobile (iOS / Android)

Phones can't spawn the binary either, so the mobile clients are **local-first**
like the browser: they host the same web engine (`fylo.mjs`) in a headless
WebView (WKWebView on iOS, `android.webkit.WebView` on Android) and expose a
native async API. Reads and writes hit an on-device OPFS store (fully offline);
a background sync engine reconciles with a backend `fylo serve` over REST/SSE.

Bundle three files as app assets: `fylo.mjs` (from a Fylo release) plus
`host.html` and `bridge.js` from [`mobile/`](mobile/). They must be served from a
**secure origin** — a custom `fylo-app://` scheme on iOS, an `https://` origin via
request interception on Android — or the browser engine can't use OPFS.

```swift
// iOS — clients/swift/Fylo.swift (Foundation + WebKit)
let db = try await Fylo(serverUrl: "https://api.example.com", token: token)
try await db.putData("users", ["name": "Ada", "role": "admin"])
let admins = try await db.findDocs("users", ["$ops": [["role": ["$eq": "admin"]]]])
try await db.syncStart()   // background sync; omit serverUrl to stay offline
```

```kotlin
// Android — clients/kotlin/Fylo.kt (android.webkit + org.json + coroutines)
val db = Fylo.open(context, serverUrl = "https://api.example.com", token = token)
db.putData("users", mapOf("name" to "Ada", "role" to "admin"))
val admins = db.findDocs("users", mapOf("\$ops" to listOf(mapOf("role" to mapOf("\$eq" to "admin")))))
db.syncStart()   // background sync; omit serverUrl to stay offline
```

Same method names as the shims (`createCollection`, `putData`, `getLatest`,
`patchDoc`, `delDoc`, `restoreDoc`, `findDocs`, `sql`), plus `syncStart`/
`syncStop`/`isOnline`. Collections auto-create on first write. Note that mobile
OSes suspend the WebView in the background, so sync runs while the app is active.

## Flutter

Flutter apps are Dart, so where the app runs decides which client to use:

- **Flutter iOS / Android / desktop** → `flutter/fylo.dart` — the same local-first
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

final db = await Fylo.open(serverUrl: 'https://api.example.com', token: token);
await db.collection('users').put({'name': 'Ada', 'role': 'admin'});
final admins = await db.collection('users').find({r'$ops': [{'role': {r'$eq': 'admin'}}]});
await db.syncStart(); // background sync; omit serverUrl to stay offline
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
