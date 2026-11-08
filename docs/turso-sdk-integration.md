# Using Turso SDKs with FYLO

> **History, kept as written.** This is the design study behind
> [ADR 0019](adr/0019-fylo-through-tursos-engine.md) and
> [ADR 0020](adr/0020-turso-engine-and-binding-builds.md); where they differ,
> the ADRs are authoritative. It was written on 2026-11-01 and is kept as the
> record of the study (revisions 1-6). Paths and machine names in it are from
> that day: `/Volumes/HAVEN/Developer` is now `/Volumes/HAVEN/Repositories`,
> worktrees moved from `~/Developer/Worktrees` to `/Volumes/ANVIL/Worktrees`,
> WIN11-VM is now HANGAR and UBUNTU-VM is now ANVIL.

- Status: **design note**, not a decision.
- Written 2026-11-01 against FYLO `origin/develop` `24f43a6` and TACHYON
  `origin/develop` `9ea846513`. This is revision 6, from the same day. It
  adds FYLO WebAssembly for TACHYON's Tac client (§7.4, revision 5) and
  language coverage for the Tac, Yon and Turso SDK languages (§7.5).
- The question: how a **new FYLO release** lets developers use Turso's official
  SDKs instead of FYLO's own language clients, and how that fits TACHYON,
  FYLO's main embedding framework.
- Scope: research and recommendation only. Nothing is implemented. The
  experiments in §12 ran in a scratch folder on this Mac.

## 0. Rules the user has set

1. **No server or listener of any kind.**
   - That rules out TCP, Unix sockets and named pipes.
   - Allowed: a spawned child on anonymous pipes, and files.
2. **Writes are synchronous.** An app's write either commits in FYLO's documents
   or fails with FYLO's error before the call returns.
3. **When a rule blocks, look for a workaround or hack, and label it.** Each
   one is marked **Supported**, **Semi-supported** or **Hack**, with its
   fragility and upkeep.
4. **A new release with no backward compatibility.** Earlier releases' clients
   and protocols don't constrain the design.
5. **Language scope** (widened 2026-11-01). FYLO is supported for three sets of
   languages:
   - **Tac framework languages:** JavaScript, TypeScript, Dart, Rust, C#, F#,
     Go, Swift, Kotlin, Java, Ruby, Lua, Python and C++
   - **Yon framework languages:** JavaScript, TypeScript, Python, Java, Groovy,
     C#, F#, Kotlin, PHP, Rust, Dart, Go, Ruby, Lua and C++
   - **Turso SDK languages:** JavaScript/TypeScript, Python, Go, Rust, Java,
     .NET, and C through Turso's C binding

   This replaces revision 4's "Turso languages only", and reopens the decision
   to drop Ruby, PHP and Dart. §7.5 maps every language to its route.
6. **Approved** (2026-11-01, "yes … with your recommendations"):
   - the exact Turso version pin
   - FYLO inside TACHYON's Yon process through the crate
   - no public `fylo exec --loop` or WASI contract
   - native FYLO on Tac's app targets
   - parking for Tac's synchronous Wasm languages
   - starting phase 1 (a separate session is running it)
7. **Check TACHYON before any implementation starts.** §7 covers it.

## 1. Summary

**Recommendation, adjusted for TACHYON:** FYLO's engine plus a SQL layer built
on Turso's engine (`turso_core`), shipped as **one Rust crate**.

**Layer 1: the `fylo` crate** (supported API; TACHYON and every Rust app).

- It opens a FYLO root in the host's own process.
- It offers two APIs on that root:
  - **Typed calls** for documents, queue and admin.
  - **A Turso SQL connection** on which collections are tables.
- No binary, no child process, no listener.
- Cargo pins the Turso version.
- TACHYON's Yon server switches its FYLO datasource from the `fylo exec --loop`
  subprocess to this crate. It is the only consumer whose deployment had a
  real choice, and it isn't a Turso SDK user (§7).

**Layer 2: `fylo link`** (FYLO's engine inside Turso's SDKs).

- It covers apps written in the other Turso languages (JS/TS, Python, Go, Java,
  .NET).
- FYLO ships builds of each Turso binding's native library, compiled with
  layer 1 inside.
- `fylo link` points the app's installed Turso SDK at that build.
- The app then opens its FYLO root as its database:

  ```js
  connect('./data')
  ```

- Writes are synchronous and in-process.

Both layers share one engine and one SQL mapping (§4). The same `turso_core`
patch registers FYLO in both.

**Main tradeoffs:**

1. **Exact Turso pin.** Each FYLO release pins one Turso version:
   - layer 1: through Cargo
   - layer 2: through about 18 binding builds per release, and apps install the
     exact SDK version FYLO names
2. **The JS and Python placements of `fylo link` are hacks.** A symlink inside
   `node_modules` restored by `postinstall`, and a `.pth` import hook. Go and
   Java are semi-supported; .NET and Rust are supported.
3. **TACHYON gives up process isolation.** FYLO runs inside Yon instead of in a
   killable subprocess, so a cancelled call can't be killed, only abandoned.
   Its build also gains `turso_core`.
4. **Browser and mobile have no Turso route.** Turso's Wasm and React Native
   packages can't load FYLO. FYLO's own browser engine stays as the one
   browser client (JS is a Turso language), and Tac uses it (below). The Swift
   and Kotlin WebView hosts are recommended for removal (§8, §10).

**Tac's client side (§7.4) gets FYLO the way sqlite-wasm works.** A built-in
`"fylo"` Store adapter, beside Tac's `"indexeddb"` one, takes the same commands
as Yon's FYLO datasource.

- **On the web** it runs FYLO's existing `fylo-browser.wasm` in a dedicated
  Worker, over one `.vdbx` file in OPFS. That is FYLO's equivalent of
  sqlite-wasm's `opfs-sahpool` VFS: one synchronous access handle, a Web Lock
  for a single owner, and no cross-origin isolation.
- **In TACHYON's native apps** it runs native FYLO through the `fylo` crate,
  linked into the app host. macOS and iOS serve pages from `tachyon-app://`,
  which is not a secure context, so OPFS isn't available there.
- **No SQL engine is needed in the browser** for Tac, because Tac stores send
  structured commands, never SQL. FYLO plus `turso_core` in the browser stays an
  optional later phase.

**Language coverage (§7.5).**

- **Inside TACHYON apps,** every Tac and Yon language reaches FYLO through
  TACHYON:
  - Yon: the resource channel
  - Tac: the `"fylo"` Store adapter

  The gaps are TACHYON's own, and are listed in §7.5:
  - Yon's Lua has no resource channel.
  - Tac's built-in Store isn't wired for Java, F#, Python, Lua or C++, or for
    natively run services.
- **Standalone, with no TACHYON,** a language uses one of three routes:
  - **Turso SDK plus `fylo link`:** JS/TS, Python, Go, Java, .NET.
  - **The `fylo` crate:** Rust.
  - **FYLO's build of Turso's C library** (the same `turso_sdk_kit` file that
    `fylo link` ships for Go), which C, C++ and Swift import from its header,
    with no binding code.
- **Free riders:** the other JVM languages (Kotlin, Groovy) ride Java's SDK,
  and F# rides .NET's.
- **What's left:** Ruby, PHP, Dart and Lua have no free route. Standalone use
  for them would need thin FFI bindings over that C library: generated for
  Dart, small and hand-written for Ruby, PHP and Lua. The recommendation is
  to leave them TACHYON-only until someone asks (§10).

**What the new release removes:**

- all nine thin NDJSON shims. Nothing replaces them one for one: TACHYON,
  Turso SDKs, the crate and the C library cover their languages (§7.5).
- the shim and mobile-bridge interop tests
- the Flutter (Dart) client
- the `fylo-clients.tar.gz` release asset
- FYLO's own SQL dialect, replaced by Turso's
- the machine protocol's public, versioned client contract (§9)

## 2. FYLO today

What `origin/develop` ships:

- **Process model.** An "embedded library, like SQLite" with no network
  protocol.
  - Native languages spawn `fylo exec --loop` and speak NDJSON machine
    protocol v1: about 50 ops, stable `error.code`s and a `handshake`.
  - [ADR 0007](adr/0007-filesystem-only-native-storage.md): FYLO uses the
    filesystem only.
  - [ADR 0012](adr/0012-opt-in-network-server.md), a network server, was
    withdrawn on 2026-10-29.
- **Artifacts** ([ADR 0008](adr/0008-fylo-wasm-browser-and-mobile-engine.md)):
  `fylo`, `fylo.wasm` (WASI), and `fylo-browser.wasm` over a `HostVfs` table.
- **Crates.** Every crate is `publish = false`. `fylo-engine`,
  `fylo-storage-native` and `fylo-query` already form an in-process engine
  that the CLI and machine layer wrap.
- **Clients** (`clients/`, 6,077 lines):
  - nine thin shims (Python, Ruby, Node, PHP, Go, Rust, C#, Java, Dart)
  - the browser loader (`browser/fylo.js`, backed by `src/browser/`)
  - Swift and Kotlin WebView hosts
  - Flutter
  - the mobile bridge
- **SQL.** FYLO's own small dialect (`executeSQL`, `fylo sql`). No bound
  parameters, no aggregates.
- **Data model.**
  - JSON documents may not contain arrays of objects.
  - Every field can be indexed with zero-payload prefix keys.
  - Documents are the truth; indexes can be rebuilt.
- **IDs.** TTIDs, sorting as text. `putData` takes a caller's `id`, and
  `allocate_identifier()` exists.
- **Transactions.** Atomic per collection
  ([ADR 0013](adr/0013-collection-write-ahead-log.md)). Cross-collection
  transactions come only with `.vdbx` ([ADR 0016](adr/0016-single-file-vdbx-database.md) §6).
- **Root paths.** The root path must be canonical, and **FYLO refuses a root
  reached through a symlink**, as documented in TACHYON's `MANAGED_STORAGE.md`.

## 3. What the Turso SDKs offer (checked 2026-11-01 in published packages)

| Language | SDK (Turso engine) | How its native code loads | Way to put FYLO in |
|---|---|---|---|
| JS/TS | `@tursodatabase/database` 0.8.3 (also `/sync`) | napi-rs loader: `./turso.<triple>.node` next to `index.js` first, then `@tursodatabase/database-<triple>`. `NAPI_RS_NATIVE_LIBRARY_PATH` is broken in 0.8.3 (napi-rs PR #2907 fixes it upstream). | **Hack:** symlink next to `index.js` |
| Python | `pyturso` 0.8.3 | `from ._turso import …` (abi3 `.so`) | **Hack:** a `.pth` import hook |
| Go | `tursogo` 0.8.3 | purego `dlopen` of an embedded, sha256-checked `libturso_sync_sdk_kit`. `InitLibrary({LoadStrategy:"system"})` searches the library path and the working directory. | **Semi-supported:** one `InitLibrary` line plus a symlink on the library path |
| Java / Kotlin (JVM) | `tech.turso:turso` 0.8.3 (JDBC) | `System.loadLibrary("_turso_java")` first | **Semi-supported:** a symlink in `~/Library/Java/Extensions` (macOS default) or on `-Djava.library.path` |
| .NET / C# | `Turso.Data.Sqlite.Provider` 0.8.3 | `[DllImport("turso_sdk_kit")]`, plus `EnableExtensions()` and `LoadExtension()` | **Supported:** `LoadExtension` |
| Rust | `turso` 0.8.3 / `turso_core` | static crate | **Supported:** depend on the `fylo` crate (§6.1) |
| Browser | `@tursodatabase/database-wasm` | static Wasm; extension loading compiled out | none; FYLO's own browser engine stays |
| React Native | `@tursodatabase/sync-react-native` | native module | not studied; only as a hack |

Facts that matter:

- **Version.** Turso is pre-1.0 with no semver policy, and everything is MIT.
- **No extension loading.** The Turso engine loads only `turso_ext` modules,
  and loading is off by default (only .NET exposes the switch).
- **Hooks already in `turso_core`:**
  - `Database::register_internal_vtab`
  - `Connection::register_static_extension`
  - `register_builtin_catalog`, at `core/dialect/sqlite.rs:145`, which its own
    comment calls "the same path external callers use"

  So FYLO's patch to `turso_core` can stay small.
- **libSQL.** The libSQL SDKs (`@libsql/client` and others) are Turso's older
  family. Every language above already has a Turso-engine SDK, so the new
  scope needs no libSQL route.

## 4. The shared core: FYLO collections as SQL tables

Both layers use it. It lives in the `fylo` crate, behind its `sql` feature, as a
`turso_core` virtual-table module.

### Opening a root

- The database path is a FYLO root (a directory or a `.vdbx` file).
- FYLO's build of the engine opens an **in-memory** SQL catalog and registers
  one table per collection. No SQL engine file is ever written.
- The stock engine fails closed on a directory: SQLite does (tested, §12);
  Turso is unverified. It also fails on a `.vdbx` file, whose magic number
  isn't SQLite's.

### Columns

- `id` (the TTID)
- `doc` (JSON)
- one column per head-schema field
  ([ADR 0017](adr/0017-schemas-in-the-database.md))
- hidden `meta`, `created_at` and `updated_at`
- the rowid is the TTID's creation segment in base 36. Uniqueness must be
  verified.

### Pushdown

| Query | FYLO index used |
|---|---|
| `=` and `IN` | `eq` |
| ranges | `n` / `nr` |
| `LIKE 'x%'` | `f` |
| `LIKE '%x'` | `r` |
| `LIKE '%x%'` | `g3` |
| `doc MATCH '<FYLO query JSON>'` | `findDocs` |
| `ORDER BY id` | consumed by the index |

### Writes and errors

- `INSERT` maps to `putData`, `UPDATE` to `patchDoc` and `DELETE` to `delDoc`.
- Each is synchronous, with CHEX, `EARRAYOFOBJECTS`, `$encrypted` and POSTIX
  checks.
- **Bound parameters are real.** Turso binds `?`, `?NNN`, `:name` and `$name`.
  That removes TACHYON's SEC-3 literal-escaping workaround (§7).
- **Errors** are the SDK's own SQL error, with FYLO's code as the message
  prefix (`EARRAYOFOBJECTS: …`).

### Transactions

- Writes in `BEGIN … COMMIT` are buffered per collection, and cursors read the
  transaction's own writes.
- `COMMIT` is one ADR 0013 group commit.
- A second collection in the same transaction fails with `EFYLO_TXN_SCOPE`
  until `.vdbx` §6 lands.

### Non-CRUD

- In SQL:
  - `fylo_version()`
  - `fylo_queue_publish` / `_claim` (table-valued) / `_ack` / `_nack` /
    `_extend`
  - `fylo_deleted`, `fylo_restore`
- In the crate's typed API: the same operations, plus collection admin.
- On the CLI: version control and schema administration.

### Concurrency

- The engine runs in-process.
- Several processes on one root use FYLO's root locks.
- An exclusive owner gets the hot set
  ([ADR 0014](adr/0014-in-memory-hot-set.md)).

## 5. Routes considered (revisions 1–3, in brief)

| Route | Verdict | Why |
|---|---|---|
| Hrana over loopback TCP | **rejected** | it is a listener |
| Unix socket / named pipe | **rejected** | it is a listener |
| Two-way file sync (tentative writes) | **rejected** | writes must be synchronous |
| Read-only SQLite mirror | optional | reads only; useful for reporting |
| Stdio Hrana (child process plus an SDK transport hook) | **dropped by the new scope** | It reached `@libsql/client`, .NET serverless and Rust `libsql`. Every Turso language now has an in-process route, so an extra out-of-process protocol isn't needed. |
| SQLite C extension for libSQL / `better-sqlite3` | dropped | libSQL isn't needed (§3) |
| Custom VFS / FUSE / FSKit / preload interposition | **rejected** | Pages become the truth, or it's too fragile. A plain file write never passes through FYLO. |
| Symlinked or FIFO database, WAL or journal | **rejected** | SQLite refuses `-wal`/`-journal` symlinks and FIFOs (tested). FYLO also refuses symlinked roots. |
| Mailbox RPC through a shared database file | **rejected** | needs app-side polling and doesn't use the SDK's API |
| Generated shims | **dropped by the new scope** | Turso SDKs replace FYLO's own clients |

## 6. The recommendation in detail

### 6.1 Layer 1: the `fylo` Rust crate (supported API)

**The crate.** One published crate, `fylo` (working name), re-exporting
`fylo-engine`, `fylo-storage-native`, `fylo-query` and `fylo-format` behind a
small facade:

```rust
let root = fylo::Root::open("/srv/catalog", fylo::OpenOptions::default())?;   // canonical path, lease, recovery
let id   = root.collection("products")?.put(json!({ "name": "Ada" }))?;      // typed API, synchronous
let docs = root.collection("products")?.find(&query)?;
let tx   = root.queue("orders").claim("billing", 10, lease)?;

#[cfg(feature = "sql")]
let conn = root.sql()?;                               // turso_core connection with FYLO tables
conn.execute("UPDATE products SET doc = json_set(doc,'$.price',?) WHERE id = ?", (9.5, id))?;
```

- **Release.** Published to crates.io, or consumed as a git tag of
  `d31ma/Fylo`. The workspace crates stop being `publish = false`.
- **Version pin.** The `sql` feature depends on `turso_core = "=0.8.3"`, and
  Cargo enforces the pin.
- **Platforms.**
  - Desktop: macOS, Linux and Windows.
  - Mobile: iOS and Android work natively through `fylo-vfs`'s `std::fs`
    path.
  - Browser: `wasm32-unknown-unknown` works through `HostVfs`, but that is
    FYLO's own browser engine, not a Turso SDK.
- **Labels.** Supported. No hack.
- **Rules.**
  - No listener and no child process.
  - Documents stay the truth.
  - Writes are synchronous.

### 6.2 Layer 2: `fylo link` for the other Turso languages

**FYLO's release pipeline:**

1. Patch `turso_core` at the pinned tag: a `fylo` feature that calls the `fylo`
   crate's registration when the opened path is a FYLO root.
2. Build each binding's native library with that feature (about 18 artifacts):

   | Binding | Builds |
   |---|---|
   | JS | 4 |
   | Python | 4–5 |
   | Go | 5 |
   | Java | 4 |
   | .NET | the SDK kit, or a `turso_ext` cdylib |

3. Sign them and record their provenance, keeping Turso's MIT notice.
4. `fylo version --output json` reports `tursoLink`.

**`fylo link [--check | --undo]`:**

- detects the SDKs installed in the project or environment
- refuses a version mismatch, and prints the exact install command for the
  pinned version
- places the build without editing any package file:

| SDK | Placement | Label | What undoes it | Upkeep |
|---|---|---|---|---|
| JS | symlink `node_modules/@tursodatabase/database/turso.<triple>.node`. The loader tries it before the platform package; Node and Bun load through it (tested). | **Hack** | `npm ci` and `bun install` remove it (tested). The app's `"postinstall": "fylo link"` restores it (tested). `--ignore-scripts` and Yarn PnP defeat it. | Re-check the loader on each Turso bump. Switch to `NAPI_RS_NATIVE_LIBRARY_PATH` once Turso ships the fixed loader. |
| Python | a `fylo_turso.pth` import hook mapping `turso._turso` to FYLO's build | **Hack** | survives a pyturso reinstall; removing the `.pth` undoes it | low |
| Go | the app calls `tursogo.InitLibrary(... "system")` once; `fylo link` symlinks `libturso_sync_sdk_kit` onto the library path or into the working directory | **Semi-supported** | – | low |
| Java / Kotlin (JVM) | symlink `lib_turso_java` in `~/Library/Java/Extensions` (on macOS's default `java.library.path`; tested). On Linux use `-Djava.library.path=` or `LD_LIBRARY_PATH`; on Windows, `PATH`. | **Semi-supported** | – | Newer JDKs warn about native access. |
| .NET / C# | `conn.EnableExtensions(); conn.LoadExtension(path)`, with the path printed by `fylo link` | **Supported** | – | low; the `turso_ext` interface follows the engine version |

**Windows:** symlinks need Developer Mode or admin, so `fylo link` falls back
to a hard link or a copy, and `--check` compares hashes.

**What symlinks fix:** placement. No package file is overwritten, no lockfile
or override changes, one command checks or undoes the links, and an upgrade
re-points everything.

**What they don't fix:**

- the version pin
- reinstalls removing the links
- the JS and Python hack status

## 7. TACHYON: how it integrates databases, and whether this fits

All of this was read from TACHYON `origin/develop` `9ea846513`. The sources are
`docs/MANAGED_STORAGE.md`, `docs/adr/0032-fylo-datasource.md`,
`crates/tachyon-core/src/storage{,.rs,/fylo.rs}`, `crates/tachyon-core/Cargo.toml`,
the handler preludes, and `docs/SUPPORT_TIERS.md`.

### 7.1 How TACHYON integrates databases today

**The server owns storage.**

- TACHYON's server, Yon (Rust, `tachyon-core`), owns **one datasource and one
  Redis**, configured in `server/application.json` (`yon.datasource.driver`
  plus `url`).
- `ManagedStorage` (`storage.rs`) holds one of:
  - **`SqlStore`**: `sqlx` 0.9 for `postgresql`, `mysql`/`mariadb` and
    `sqlite` (bundled `libsqlite3-sys`), plus `tiberius` for `sqlserver`.
  - **`FyloStore`**.
- Bounds: 16 admitted operations, 5 s operations, 10 s startup, 30 s scroll,
  4 SQL transactions.

**FYLO is a first-class driver** (`fylo:///absolute/root`, ADR 0032).

- **Process.** `FyloStore` finds `fylo` on `PATH` (`ty doctor` checks it, as
  "fylo datasource"). It holds **one long-lived `fylo exec --loop`
  subprocess** and speaks NDJSON machine protocol v1.
- **Operations used.** `putData`, `patchDoc`, `getDoc`, `getLatest`, `delDoc`,
  `findDocs`, `createCollection`, `dropCollection`, `inspectCollection`,
  `queuePublish`, `queueClaim`, `queueAck`, `queueNack` and `executeSQL`.
- **Workarounds TACHYON carries:**
  - ordering and paging inside the server (10,000-match bound)
  - its own scroll snapshots, because FYLO 26.33 reruns queries per page
  - a patch queue, so a patch and the `getDoc` of its result don't interleave
  - SEC-3 literal escaping of `$1` parameters, because protocol v1 has no
    bound parameters
- **Restrictions.**
  - **No transactions** on FYLO ("The FYLO datasource has no transactions").
  - A cancelled call **kills and restarts the subprocess**, so a late answer is
    never misread.
  - The root path is canonicalised, and symlinked roots are refused.

**Languages never touch a database SDK.**

- Yon's handlers run in many languages: JavaScript/TypeScript, Python, Ruby,
  Go, C#, Java, Kotlin, Dart, PHP, Rust, F#, C++, and Lua (no resources).
- Each reaches storage through **Yon's private resource channel**
  (`YON_RESOURCE_ENDPOINT` plus `YON_RESOURCE_TOKEN`, per invocation).
- Each language has the same `DataSource` and `Store` API in TACHYON's own
  preludes.
- **TACHYON loads no Turso or libSQL SDK anywhere.** The lockfile's only
  "libsql" match is `libsqlite3-sys` under `sqlx-sqlite`.

**The client side (Tac) doesn't use FYLO.**

- Tac runs in the browser, WKWebView, WebView2, WebKitGTK, and iOS and Android
  WebViews, including Wasm service languages.
- Its stores use the built-in `"indexeddb"` adapter or a Setup-provided one.
  FYLO is server-only.

### 7.2 Does the recommendation fit?

| Question | Answer |
|---|---|
| Does TACHYON load Turso or libSQL SDKs in-process? | **No.** It's a Rust server with `sqlx`, `tiberius` and a FYLO subprocess, so `fylo link` (layer 2) has nothing to link. |
| Can it run `fylo link`, or does it need a Rust crate API? | **A crate API: layer 1.** `fylo link` is for apps using Turso's language SDKs. TACHYON should depend on the `fylo` crate and drop the subprocess. That is why the recommendation now leads with the crate. |
| Can it live with the exact pin? | **Yes.** TACHYON already pins FYLO, and its CI lanes install a pinned FYLO. Through Cargo, the Turso pin comes with the `fylo` crate version, and `Cargo.lock` enforces it. The cost is that `turso_core` and its dependencies join TACHYON's graph and `cargo deny` review. A `storage-fylo` feature can keep them out of builds that don't use FYLO. |
| Browser, Wasm and mobile targets? | **Unaffected by the server change.** Tac's browser, WebView and mobile targets don't use FYLO today; their stores are IndexedDB or custom adapters, and Yon runs on the host. Adding FYLO to Tac (FYLO-Wasm on the web, the native crate in app hosts) is designed in §7.4. |
| Do TACHYON's Ruby, PHP, Dart, F#, C++ handlers lose FYLO when FYLO drops those languages? | **No.** They reach FYLO through Yon's resource channel, and Yon is Rust. FYLO's language scope and TACHYON's language scope are independent. |
| Does the new scope break TACHYON? | **Yes, if FYLO drops NDJSON protocol v1 before TACHYON moves to the crate.** Both repos must move together (§7.3). |

### 7.3 What TACHYON would need to change

**Driver:**

1. **`FyloStore`.** Replace the subprocess and NDJSON client (`fylo.rs` and its
   ten submodules) with calls on `fylo::Root`. The typed API covers
   documents, collections and the queue; `root.sql()` covers raw SQL.
   - Blocking engine calls run in `spawn_blocking`.
   - The resource-channel commands, and every language's `DataSource` API,
     stay as they are.
2. **Cancellation.** "A cancelled FYLO call restarts the subprocess" no longer
   works, because an in-process call can't be killed.
   - A timed-out call is abandoned: its result is dropped when it finishes, and
     the admission permit is held until then.
   - "A cancellation does not prove that a mutation did not commit" still
     holds.
   - ADR 0032 needs an amendment for this.
3. **Raw SQL.** Move to Turso's dialect with bound parameters. That retires
   `SqlStatement`'s literal escaping (the SEC-3 amendment) and allows
   aggregates, `ORDER BY` and `LIMIT`. FYLO raw SQL could then sit closer to
   the other SQL drivers, though TACHYON's dialect layer stays per driver.

**Behaviour:**

4. **Ordering, paging and scroll.** `ORDER BY id`, and indexed ordering pushed
   into FYLO, can replace the 10,000-match server-side ordering. ADR 0032
   already says the scroll snapshot "can move back into FYLO without a change
   to the API".
5. **Transactions.** Single-collection transactions become possible. Today
   TACHYON refuses them. It's optional, under `EFYLO_TXN_SCOPE` rules.
6. **Patch.** The patch queue can go if FYLO's patch returns the stored
   document atomically.

**Tooling and dependencies:**

7. **Doctor and toolchain.** The "fylo datasource" requirement no longer needs
   the `fylo` binary on `PATH`. Keep it only if TACHYON still calls the CLI for
   admin.
8. **Dependency policy.**
   - `turso_core` (pre-1.0, MIT) and FYLO's crates enter `Cargo.lock`, behind
     a `storage-fylo` feature.
   - The Linux and Windows CI lanes stop installing a pinned `fylo` binary for
     the storage tests and compile the crate instead.
   - Expect a longer build.
9. **Process.** FYLO's root lease, and optionally its hot set, are held by the
   Yon process. That is one owner per root, exactly as today's single
   subprocess.

### 7.4 Tac's client side: FYLO WebAssembly, the way sqlite-wasm works

TACHYON was read only, at `origin/develop` `9ea846513`. The sources are:

- `crates/tachyon-core/src/template/tac-storage.js`
- `crates/tachyon-core/src/behavior/runtime.js`
- `docs/BEHAVIOR_ANNOTATIONS.md` ("Adapter Contracts", "Structured stores")
- `docs/APPLICATION_CONFIGURATION.md` (`server.origins`)
- ADR 0065 and ADR 0089

The web research was checked on 2026-11-01; the sources are listed in §13.

#### The model: sqlite-wasm, and Turso's Wasm package

| | sqlite-wasm `opfs` | sqlite-wasm `opfs-sahpool` | Turso `@tursodatabase/database-wasm` 0.8.3 | FYLO today |
|---|---|---|---|---|
| Target | Emscripten | Emscripten | `wasm32-wasip1-threads` (napi-rs and emnapi) | `fylo-browser.wasm`, `wasm32-unknown-unknown`, 17 imports, no glue code |
| Storage | async OPFS made synchronous by a proxy Worker using `SharedArrayBuffer` and `Atomics` | a pool of synchronous access handles opened up front, in one Worker | a Rust `IO` impl (`bindings/javascript/src/browser.rs`); a Worker holds two synchronous access handles (the database file and its `-wal`) and writes into shared memory | **Folder root:** two Workers and a `SharedArrayBuffer` (sqlite's `opfs` design). **One `.vdbx` file:** one synchronous access handle in one Worker (`fylo-single-file-worker.mjs`, ADR 0016 §10) |
| Needs cross-origin isolation (COOP/COEP) | yes | **no** | **yes**, and it reserves about 250 MiB of shared memory | **folder root: yes**; `.vdbx`: **no** |
| Several tabs | exclusive per origin, retries on busy; `opfs-wl` (3.53) uses Web Locks | one connection; `pauseVfs` / `unpauseVfs` (3.50) hand the pool over | no Web Locks; a second tab should fail to open (not tested) | `.vdbx`: a Web Lock, and a second tab is refused |
| `.wasm` size | 879 KB | same module | **11.6 MB** | 2.23 MiB; **735 KiB gzipped** (budget 2.38 MiB / 800 KiB) |

**What carries over:**

- The **sahpool shape**: synchronous handles, one Worker, no
  `SharedArrayBuffer`. FYLO's `.vdbx` worker already is that shape.
- The **pause and unpause handover** between tabs.
- **Web-Lock-based ownership.**

**What doesn't carry over:**

- Turso's package needs cross-origin isolation and is 11.6 MB.
- It can't load FYLO either: dynamic loading is compiled out on Wasm.

**Safari hardening already on FYLO `develop`:**

- `e4a6aba`: wake the engine Worker before a session ends, so Safari frees the
  root.
- `87abb10`: bound the OPFS bridge's wait, because Safari can lose the wakeup.
- `266b3ed`: one stack frame per document level, for Safari Workers.
- `5c64027`: a closed session gives back its handles and its root.
- WebKit still can't start a module Worker over OPFS in FYLO's test harness,
  so it is skipped there.

#### Can layer 1 (FYLO plus the `turso_core` SQL layer) compile for the browser?

**Probably, with work. Tac doesn't need it.**

- **Turso's CI** type-checks `turso` for `wasm32-unknown-unknown`
  (`.github/workflows/rust.yml:212-225`). Nothing is linked or run there.
- **Storage isn't the problem.** §4's design gives `turso_core` an empty
  in-memory catalog and keeps all data in FYLO's tables. So `turso_core` needs
  no OPFS IO: FYLO supplies a memory-backed `Arc<dyn IO>`
  (`Database::open(io, …)`, `core/database.rs:1216`) whose `Clock` reads FYLO's
  own clock import. That also covers `IO::sleep`.
- **Blockers on `wasm32-unknown-unknown`, with the workaround for each:**

  | Blocker | Workaround |
  |---|---|
  | `std::time::Instant` and `SystemTime::now` panic. They are called inside `MemoryIO` and `DefaultClock` (`io/clock.rs`), and directly by `date('now')` (`functions/datetime.rs:199`), `Utc::now` (`time/internal.rs:115`) and `PRAGMA` (`translate/pragma.rs:386`). | FYLO's own `IO`/`Clock`, plus three small edits in FYLO's existing `turso_core` patch that route those calls through the IO clock |
  | getrandom's `wasm_js` backend, and uuid's `js` feature, need wasm-bindgen glue. FYLO's module allows none. | getrandom 0.3's **custom backend**, fed by FYLO's existing entropy import |
  | **Writable** virtual tables need `register_static_extension` with `turso_ext`. `register_internal_vtab` is read-only (`core/vtab.rs:597-623`). | the static `turso_ext` route; its function-pointer interface works inside one module |
  | **Size.** `turso_core` would push the module far past FYLO's 800 KiB budget. Turso's whole browser module is 11.6 MB. | a **separate, lazily loaded `fylo-sql-browser.wasm`**, used only by apps that want SQL in the browser |

- **If `turso_core` won't link or run on `wasm32-unknown-unknown`:**
  - Build the SQL module for `wasm32-wasip1`, with no threads, unlike Turso's
    package.
  - Run it under a small WASI preview 1 shim in the same Worker, mapping file
    calls onto the same handle.
  - It still needs no `SharedArrayBuffer`.
  - Either way, Tac works without it.

#### How Tac's stores work today

**The adapter contract.**

- Every entity adapter implements `execute(command, execution)`
  (`BEHAVIOR_ANNOTATIONS.md` "Repositories"). The operations are:
  - `save`
  - `delete`
  - `find` (with a page)
  - `query`, of kind `find`, `count`, `exists` or `delete`, with `where`,
    `orderBy` and a page
  - `transaction` (begin, commit or rollback)
- Stores and Repositories are contracts that Tachyon writes. The Tac docs say
  queries are "Never SQL text".
- `DataSource` (raw SQL) exists only in Yon ("Tac never initializes these
  resources").

**The built-in `"indexeddb"` adapter** (`template/tac-storage.js`, 597 lines):

- The runtime publishes `globalThis[Symbol.for('tachyon.tac.storage')]`, with
  `entities()`, `declare(table, id, fields)`, `cache(name)`, `host(text)` and
  `settled()`.
- `runtime.js` `builtinAdapter('entities', 'indexeddb')` returns it.
- Other adapters come from a Setup (`client/setups/…`).

**Synchronous Wasm service languages.** Tac compiles service languages to Wasm:
Java (TeaVM), Kotlin, Swift, C#, Ruby, Rust, Dart, C++ and others.

- They call storage synchronously through `host(text)`.
- That path is answered by `TacEntityMirror`: an **in-memory copy** of
  IndexedDB whose writes **persist asynchronously**.
- Tables beyond 10,000 rows are left out of the mirror.

**Targets and origins** (ADR 0103 via `APPLICATION_CONFIGURATION.md`):

| Target | Origin |
|---|---|
| web | the site's own origin |
| macOS and iOS (WKWebView) | `tachyon-app://bundle` |
| Android | `https://appassets.tachyon.local` |
| Windows (WebView2) | `https://tachyon.local` |
| Linux (WebKitGTK) | `tachyon://app` |

**No cross-origin isolation.** TACHYON sends no COOP or COEP headers anywhere
(searched on `origin/develop`).

#### Where FYLO would run, per Tac target

| Target | Secure context? | OPFS synchronous handles | FYLO for Tac |
|---|---|---|---|
| Web, over HTTPS | yes | Chrome 102 (all synchronous from 108), Firefox 111, Safari 16.4. Off in Lockdown Mode. | **`fylo-browser.wasm` over one `.vdbx` file in OPFS**, in a dedicated Worker |
| macOS and iOS WKWebView, `tachyon-app://bundle` | **no**, per TACHYON ADR 0065. WKWebView has no public API to mark a custom scheme secure. | **unavailable**: WebKit's OPFS interfaces are `[SecureContext]` | **Native FYLO in the app host**: link the `fylo` crate into the host's Rust companion, which TACHYON already does for Rust services. Reach it through the native bridge (ADR 0089). Store the root in the app's container. |
| Android WebView, `https://appassets.tachyon.local` | yes (intercepted HTTPS) | WebView 109+ | Wasm works, but **native is recommended**: one implementation for every app target, durable app storage, outside WebView eviction |
| Windows WebView2, `https://tachyon.local` | yes | follows Edge | Wasm works; **native recommended** for the same reasons |
| Linux WebKitGTK, `tachyon://app` | only if registered as secure (`webkit_security_manager_register_uri_scheme_as_secure`) | **off by default until WebKitGTK 2.56**, not yet released | **native** |

The rule that comes out of this:

- **Web pages** use FYLO-Wasm over `.vdbx`.
- **TACHYON's native apps** use the native crate. That is the same engine and
  the same command mapping as Yon, which keeps it to one code path per kind of
  target.

#### Fitting the Store/DataSource API

**A built-in `"fylo"` entity adapter** (`@Entity({"adapter":"fylo"})`):

- It implements the same command set as Yon's `FyloStore`, so a Store moves
  between Yon and Tac, or from `"indexeddb"` to `"fylo"`, unchanged:

  | Command | FYLO operation |
  |---|---|
  | `save` without an id | `putData` |
  | `save` with an id | `patchDoc` |
  | `delete` | `delDoc` |
  | `find` and `query` | `findDocs` with `$ops` |
  | `count` and `exists` | counting matches |
  | ordering on fields other than `id` | done inside the adapter, bounded as in Yon |

- **Ids.** Entities on `"fylo"` use **FYLO-assigned TTIDs**, as in Yon. Tac's
  IndexedDB adapter accepts application-chosen strings and integers, so moving
  a Store from `"indexeddb"` to `"fylo"` changes that rule.
- **Transactions** follow Yon: refused until FYLO's single-collection
  transactions (§4) exist in the browser build.
- **Synchronous Wasm languages.** Route `"fylo"` calls through ADR 0090's
  parking (the fiber, JSPI or task waits while the Worker or native bridge
  answers) rather than through `TacEntityMirror`. Otherwise their writes would
  persist after the call returns, which breaks the synchronous-writes rule.

**Several tabs.**

- The `.vdbx` Worker holds a Web Lock, so a second tab is refused.
- Tac needs a **leader**: the tab that wins `navigator.locks.request` owns the
  Worker, and other tabs send commands over `BroadcastChannel`.
- A `SharedWorker` can't hold synchronous handles.
- The alternative is sahpool-style pause and unpause handover.
- `readwrite-unsafe` handles are Chromium-only, so they aren't an option.

**Persistence and eviction on the web.**

- Call `navigator.storage.persist()` when the database opens.
- Chromium grants it by heuristics, Firefox asks the user, and Safari uses
  heuristics (a Home Screen web app helps).
- Safari deletes script-written storage after **7 days without interaction**,
  unless it is a Home Screen web app.
- Quota is up to 60% of the disk in browsers, and 15% for apps that embed
  WebKit.
- Native targets avoid all of this, which is another reason to run native FYLO
  there.

**Bundle size.**

- About 735 KiB gzipped for `fylo-browser.wasm`, loaded lazily only when a Store
  uses `"fylo"`. For comparison, sqlite3.wasm is 879 KB uncompressed, and
  IndexedDB adds nothing.
- The optional SQL module would come on top, size to be measured.

**Cross-origin isolation.** None is needed for `.vdbx` mode. FYLO's folder-root
browser mode, and Turso's Wasm package, both need COOP/COEP that Tac doesn't
send, so neither fits Tac as it stands.

**What `.vdbx` version 1 covers.**

- Documents only.
- No buckets, version control, queue or cross-collection transactions
  (ADR 0016 §1).
- That's enough for Tac entity stores.

**Syncing with Yon's server-side FYLO later.** This is noted, not designed.

- Today a browser root reaches a server only by export: a `.vdbx` snapshot
  `Blob` ("Moving a browser root to a server needs an export", artifact
  matrix).
- ADR 0007 keeps replication outside the engine.
- A document-level sync between a Tac `.vdbx` and Yon's root would be its own
  ADR. TTID order helps with ordering and conflicts, and Turso's push/pull sync
  is the model to compare against.

#### What Tac would need to change

1. Add `"fylo"` as a built-in entity adapter: in `runtime.js`
   `builtinAdapter`, and in a new `tac-fylo-storage.js` beside
   `tac-storage.js`.
2. **Web:** bundle FYLO's browser assets (`fylo-browser.wasm`, the `.vdbx`
   Worker, `host-vfs.mjs`) into `dist/web`, pinned to the same FYLO version as
   the server crate and loaded lazily.
3. **Native hosts** (macOS, iOS, Android, Windows, Linux):
   - link the `fylo` crate into the Rust companion
   - add a storage bridge operation that answers entity commands natively
   - keep the root in the app's data folder
4. **Synchronous Wasm languages:** use ADR 0090 parking for `"fylo"`, not
   `TacEntityMirror`.
5. **Web, multiple tabs:** leader election.
6. **Web:** the `persist()` request, and eviction docs.
7. **Tests:**
   - browser gates in Chromium, Firefox and WebKit, including a second-tab
     case and a Safari Worker-termination case
   - native host gates on macOS, iOS, Android, Windows and Linux

### 7.5 Language coverage: Tac, Yon and Turso SDK languages

**Sources** (TACHYON `origin/develop` `9ea846513`, read only):

- `README.md`, for the Tac and Yon language lists, and which languages run on
  Wasm or natively.
- `crates/tachyon-core/src/behavior/polyglot/native_storage.rs`. Its message
  says: "The built-in browser cache and Store work for JavaScript, TypeScript,
  Dart and Ruby services and for Go, Rust, C#, Kotlin and Swift WebAssembly
  services". Natively run services are refused the built-ins.
- `rust_storage.rs`, `runtime.js` and `tac-storage.js`, for the built-in
  adapter hook.
- ADR 0032 and ADR 0085 ("Lua has no resources").

**Turso's bindings** are from §3, plus Turso's own `bindings/c` and
`turso_sdk_kit` C API. Turso also has a Tcl binding, which this note doesn't
cover.

**What each route means:**

| Route | What it is | Kind |
|---|---|---|
| **Yon** | The language's TACHYON `DataSource` / Store reaches Yon's FYLO datasource through the resource channel. Yon runs FYLO through the crate, in-process (§7.3). | supported (TACHYON API) |
| **Tac-W** | Tac's built-in `"fylo"` adapter, on the web: `.vdbx` in OPFS, in a Worker (§7.4) | supported once built |
| **Tac-N** | Tac's built-in `"fylo"` adapter, in native apps: the crate in the app host, reached over the native bridge (§7.4) | supported once built |
| **SDK** | the Turso SDK plus `fylo link` (§6.2) | see §6.2 for each placement's label |
| **Crate** | the `fylo` Rust crate (§6.1) | supported |
| **C** | FYLO's build of Turso's C library (`turso_sdk_kit` with FYLO compiled in, the same file `fylo link` ships for Go). Its header is Turso's C API, and SQL over §4's tables reaches documents and the queue. | supported (C ABI); version pinned |
| **FFI** | a thin binding over **C**, in the language's own FFI | FYLO-maintained, generated where a generator exists |

**Coverage per language:**

| Language | Turso SDK | Yon | Tac | Inside TACHYON | Standalone (no TACHYON) |
|---|---|---|---|---|---|
| JavaScript | yes | yes | yes | Yon; Tac-W / Tac-N | **SDK** (`@tursodatabase/database`; JS placement is a hack) |
| TypeScript | yes (JS) | yes | yes | Yon; Tac-W / Tac-N | **SDK** |
| Python | yes | yes | yes (Pyodide, limits) | Yon. **Tac gap:** built-in Store not offered to Python services | **SDK** (`pyturso`; `.pth` hack) |
| Go | yes | yes | yes (Wasm) | Yon; Tac-W / Tac-N | **SDK** (`tursogo`, `InitLibrary("system")`) |
| Rust | yes | yes | yes (Wasm, and native on every target) | Yon; Tac-W; native Rust services use the **crate** directly (today they're refused the built-ins) | **Crate** |
| Java | yes | yes | yes (WasmGC) | Yon. **Tac gap:** built-in Store not offered to Java services | **SDK** (JDBC, `java.library.path`) |
| C# | yes (.NET) | yes | yes (Wasm; native on Windows) | Yon; Tac-W / Tac-N | **SDK** (.NET `LoadExtension`, supported) |
| F# | rides .NET | yes | yes (Wasm) | Yon. **Tac gap:** built-in Store not offered to F# services | **SDK via .NET** (same provider; no new code) |
| Kotlin | rides Java | yes | yes (WasmGC; native on Android) | Yon; Tac-W / Tac-N | **SDK via Java** on the JVM. **Android** needs a FYLO-built JNI library packaged as an AAR, because Turso's jar ships no Android libraries: a packaging cost, no new API. |
| Groovy | rides Java | yes | no | Yon | **SDK via Java** |
| Swift | no Turso-engine SDK (`libsql-swift` is libSQL, stale) | no | yes (Wasm; native on macOS and iOS) | Tac-W / Tac-N (the macOS and iOS hosts are Swift) | **C**: import the header through a module map, ship an `.xcframework`; no binding code |
| C++ | C (Turso's C binding) | yes | yes (Emscripten, limits) | Yon (Stores through a Setup). **Tac gap:** built-in Store not offered to C++ services | **C**: include the header |
| C | yes (`bindings/c`) | no | no | – | **C** |
| Dart | community `libsql_dart` only (libSQL, its own engine) | yes | yes (WasmGC) | Yon; Tac-W / Tac-N | **FFI**: `dart:ffi` bindings **generated** by `ffigen` from the C header, plus a thin idiomatic layer. Optional (§10). |
| Ruby | none | yes | yes (mruby) | Yon; Tac-W / Tac-N | **FFI**: the `ffi` gem over **C**, a few hundred lines by hand. Optional (§10). |
| PHP | community `turso-client-php` (libSQL) | yes | no | Yon | **FFI**: PHP's `FFI` extension with `FFI::cdef` from the header. Web servers need `ffi.enable=preload` or a preload script. Optional (§10). |
| Lua | none | yes, **but no resources** (ADR 0085) | yes (Lua 5.4 on Wasm, limits) | **Yon gap:** Lua handlers can't reach a DataSource. **Tac gap:** built-in Store not offered. | Lua 5.4 has no FFI, so it needs a small C module per platform (LuaJIT `ffi.cdef` works without one). Not recommended (§10). |

#### Gaps and their routes

**TACHYON-side gaps (supported inside TACHYON apps).** These are TACHYON
changes; FYLO has nothing to add.

1. **Tac's built-in Store doesn't reach Java, F#, Python, Lua or C++
   services**, or natively run services.
   - Today those use a Setup-provided entity adapter. A Setup adapter for
     `"fylo"` works the same way.
   - TACHYON can extend `behavior_builtin_entities` to them: each runtime asks
     the host through `host(text)` / `tac_host_storage`, so the built-in is one
     host operation, not per-language storage code.
   - Native Rust services link the crate. Native Swift, Kotlin and C# services
     ask the host, as Tac-N does.
2. **Yon's Lua handlers have no resources** (ADR 0085).
   - Workaround inside TACHYON today: a Lua handler reaches FYLO through a
     `@Gateway` or `relay(...)` to another Yon language.
   - Proper fix: give Lua the resource channel, which is TACHYON's call.

**Standalone gaps.** These are the languages with no Turso SDK and no free
ride:

| Language | Cheapest route that keeps the rules | Cost | Fragility |
|---|---|---|---|
| Swift | **C**: module map plus `.xcframework` | packaging only | low; tracks the C API at the pinned Turso version |
| C / C++ | **C**: header plus library | none | low |
| Kotlin (Android) | **SDK via Java**, with a FYLO-built Android JNI library in an AAR | packaging only; the Android build is added to the matrix | low |
| Dart | **FFI**, generated by `ffigen` | a generator config plus a thin layer; regenerated per Turso pin | low to medium |
| Ruby | **FFI** (`ffi` gem) | a small hand-written binding | medium, since it's hand-written and tracks the C API |
| PHP | **FFI** (`FFI::cdef`) | a small hand-written binding; needs PHP's FFI turned on | medium |
| Lua | a C module (or LuaJIT FFI) | a native build per platform | medium to high |

Every route here keeps the rules:

- **In-process,** so no listener.
- **Writes are synchronous,** since each statement is a FYLO commit.
- **Documents stay the truth.**

No route brings back an NDJSON shim. The FFI bindings bind Turso's generic SQL
C API, not FYLO's roughly 50 operations: SQL reaches documents and the queue
through §4's tables and functions.

**A workaround if Turso's C API churns before 1.0:**

- Expose a **FYLO-owned `libfylo` C ABI** from the crate:
  - a small, stable surface: open root, execute SQL, plus typed document and
    queue calls
  - versioned by FYLO
- Point C, C++, Swift and the FFI bindings at it instead.
- That moves one contract into FYLO's hands. Start with Turso's C API, which
  adds no FYLO contract, and switch only if pin bumps keep breaking the
  bindings.

#### Which "supported" to aim for

- **Aim (a): supported inside TACHYON apps, for every Tac and Yon language.**
  - It is nearly free on FYLO's side: one Yon driver (phase 1, running now),
    one Tac adapter (phase 3b), and TACHYON's own per-language APIs, which
    already exist.
  - The remaining work is TACHYON's two gaps above.
- **Aim (b): standalone only where it's free or packaging-only.**
  - Turso SDK languages
  - the JVM and .NET riders (Kotlin, Groovy, F#)
  - C, C++ and Swift over the C library
- **Don't commit to standalone Ruby, PHP, Dart or Lua in this release.** Their
  FFI bindings are small, but they are per-language code FYLO would own, which
  is what the user set out to stop. Generated Dart is the cheapest to add later
  if asked.

## 8. What the new release removes and changes in FYLO

**Deleted:**

| Item | Why |
|---|---|
| `clients/python`, `node`, `go`, `rust`, `csharp`, `java` (≈3,300 lines) | Turso SDKs plus `fylo link`, or the crate for Rust |
| `clients/ruby`, `php`, `dart` (≈1,100 lines) | These languages stay **supported inside TACHYON** (Yon and Tac, §7.5), which doesn't use these shims. Standalone use, if wanted later, comes back as thin FFI bindings over the C library, not as NDJSON shims. |
| `clients/flutter` (a WebView host of the JS engine) | Dart stays in scope through TACHYON. A standalone Flutter route would be FFI over the C library (§7.5), not a WebView. |
| `tests/interop/shim-interop.test.js` | no shims; replaced by a Turso-SDK conformance suite |
| the `fylo-clients.tar.gz` release asset and its install docs | no clients |
| FYLO's own SQL dialect (`fylo-query/src/sql.rs` planner and the `executeSQL` subset) | Replaced by Turso SQL over the §4 tables; `fylo sql` runs on `turso_core`. **Deferred to the release after next:** TACHYON 26.36.01's raw SQL calls `executeSQL` (see "Release order" below). |
| the **public** NDJSON client contract (`api/machine/v1` as a versioned interface for third-party clients) | No external client but TACHYON speaks it. **Frozen in the next release and removed in the one after** (see "Release order"). |

**Kept, possibly reshaped:**

| Item | Status |
|---|---|
| `fylo` CLI (admin, version control, schema, pack, verify) | kept |
| `fylo exec --loop` (the CLI's public NDJSON loop) and the WASI artifact's interchange contract | **Kept, frozen and deprecated, for one release; then removed.** TACHYON 26.36.01 and its current release candidate run `fylo exec --loop` with machine protocol v1 (ADR 0032). Removal follows once a TACHYON release on the crate has shipped (see "Release order"). |
| The batch interface inside the module (`fylo_exec`) and the Worker's message protocol | **Kept, private:** shipped only together with FYLO's own browser loader, and checked by `fylo_abi_version`. Not a public contract. |
| `fylo-browser.wasm`, the `.vdbx` Worker (`fylo-single-file-worker.mjs`, `single-file-vfs.mjs`, `host-vfs.mjs`), and the `clients/browser` loader | **Kept and promoted:** a `.vdbx` file in OPFS becomes the default browser storage, and Tac's `"fylo"` adapter builds on it (§7.4). JS is a Turso language, and the browser has no Turso route. |
| The folder-root browser mode (two Workers and a `SharedArrayBuffer`) and the older JS engine (`FyloBrowser` over OPFS or File System Access) | **The user decides (§10).** Explorer's folder picking uses them, and they need cross-origin isolation. Neither is needed by Tac. |
| `clients/swift`, `clients/kotlin`, `clients/mobile` (WebView hosts of the JS engine) | **Replaced, not just removed** (reopened 2026-11-01). Swift and Kotlin stay supported: inside TACHYON through Tac-N (the native hosts link the crate), and standalone through the C library (Swift: module map plus `.xcframework`) and the Java route with an Android AAR (Kotlin) (§7.5). The WebView hosts and the mobile bridge go, because they run the weaker JS engine. |
| **New:** FYLO's build of Turso's C library, shipped for C, C++ and Swift, plus Swift and Android packaging | The same `turso_sdk_kit` file `fylo link` already builds for Go, published with its header, an `.xcframework`, and an Android AAR. |

**New:**

- the `fylo` crate, published
- `fylo link`
- the binding artifacts per release
- `api/sql/v1` (the table mapping, functions, errors and transaction scope)
- a conformance suite running each stock Turso SDK, at the pinned version,
  against FYLO's build

### Release order with TACHYON

This section follows constraints from TACHYON's Release Coordinator,
2026-11-01.

**The constraint.** FYLO's next release must not break TACHYON 26.36.01, or
TACHYON's current release candidate.

- Both start `fylo` from `PATH` and drive `fylo exec --loop` over machine
  protocol v1 (TACHYON ADR 0032, `crates/tachyon-core/src/storage/fylo.rs`).
- Their raw SQL goes through `executeSQL`, in FYLO's own dialect.
- A user who upgrades `fylo` on `PATH` would break an installed TACHYON if v1
  disappeared.

**Option A: compatibility window (recommended).**

- **FYLO's next release** ships all of the new interfaces: the crate,
  `fylo link`, the C library and the browser `.vdbx` default.
- **It also keeps, frozen and deprecated:**
  - `fylo exec --loop`
  - machine protocol v1, with the operations ADR 0032 uses
  - `executeSQL` with FYLO's dialect

  The handshake keeps reporting `protocolVersion: 1`. Nothing new is added to
  v1, and the deprecation is documented.
- **TACHYON's next release** moves Yon to the crate, adds Tac's `"fylo"`
  adapter, and closes its two gaps (§7.5).
- **The FYLO release after that** removes v1, `fylo exec --loop` and FYLO's
  SQL dialect. Only once TACHYON's crate-based release is out.
- **Why A:**
  - The two release trains don't wait for each other.
  - TACHYON 26.36.01 and the current release candidate keep working against
    any FYLO from now until the removal.
  - The cost is one more release of code that already exists and is tested
    (`fylo-machine`, the `exec` CLI, `fylo-query/src/sql.rs`), frozen.

**Option B: ship together.**

- FYLO's next release and TACHYON's crate-based release ship together, after
  TACHYON's current release candidate.
- v1 is removed at once.
- It is simpler in code, but it couples the two schedules. An older TACHYON
  still breaks if someone upgrades `fylo` alone, unless TACHYON's installers pin
  FYLO and doctor refuses a mismatch.

**This softens rule 4 ("no backward compatibility").** That is for TACHYON's
sake: one protocol and one SQL dialect survive one extra release. It is flagged
as a question in §10.

## 9. Phased plan

**Phase 0: decide.**

- The user answers §10.
- Draft FYLO ADR 0019, "FYLO through Turso's engine", as Proposed.
- Draft the TACHYON ADR 0032 amendment.

**Phase 1: layer 1 spike, in FYLO and TACHYON together.**

1. `fylo` crate facade (`Root`, documents, queue) without SQL.
2. In a TACHYON worktree, switch `FyloStore` to it behind a feature.
3. Run TACHYON's FYLO tests (`storage/tests/fylo_*`,
   `tachyon-cli/tests/it/fylo_datasource.rs`) unchanged.
4. Measure latency against the subprocess, and check cancellation behaviour.

**Phase 2: SQL core.**

1. Patch `turso_core` at 0.8.3 to add the `fylo` feature. Build read-only
   tables with pushdown.
2. Then add writes, transactions and the queue functions.
3. In TACHYON, move raw SQL to bound parameters and drop the SEC-3 escaping.
4. Add the crash matrix: kill mid-statement and mid-COMMIT, then recover.

**Phase 3: layer 2.**

1. Build the binding artifacts for JS and Go first.
2. Add `fylo link [--check|--undo]`.
3. Run the conformance suite for `@tursodatabase/database` and `tursogo`.
4. Then Python, Java and .NET.
5. Cover macOS, Linux and Windows, with the hard-link or copy fallback on
   Windows.
6. Run the security review: supply chain, link permissions and FFI.

**Phase 3b: the Tac `"fylo"` adapter** (TACHYON worktree; it can run alongside
phase 3).

1. Web: the adapter over the `.vdbx` Worker. Run browser gates in Chromium,
   Firefox and WebKit, with a second tab, a reload, and a killed Worker.
2. Leader election across tabs, and `persist()`.
3. Native: link the crate into the macOS and iOS hosts first, since they have
   no OPFS, through the host bridge. Then Android, Windows and Linux.
4. ADR 0090 parking for synchronous Wasm languages.
5. Optional, later: `fylo-sql-browser.wasm` (FYLO plus `turso_core`, loaded
   lazily), spiked for whether it links on `wasm32-unknown-unknown`, its size,
   and the clock patches.

**Phase 3c: language coverage** (after phase 3; §7.5).

1. Publish the C library (`turso_sdk_kit` with FYLO compiled in) with its
   header. Smoke-test it from C, C++ and Swift (module map and
   `.xcframework`).
2. Kotlin on Android: build the JNI library for Android targets, and package
   an AAR around Turso's jar.
3. Add F#, Kotlin/JVM and Groovy to the conformance suite, riding the .NET and
   Java SDK builds.
4. **TACHYON** (its own sessions):
   - extend Tac's built-in Store to Java, F#, Python, Lua, C++ and natively
     run services
   - give Yon's Lua a resource route, or document the `@Gateway` workaround
5. Only if §10 question 1 says so: generated Dart FFI, then Ruby and PHP FFI
   bindings over the C library.

**Phase 4: release.** This follows Option A in §8 "Release order".

1. **FYLO's next release:**
   - Delete the §8 items that TACHYON doesn't use: the nine NDJSON shims, the
     shim interop tests, the WebView hosts, and the `fylo-clients.tar.gz`
     asset.
   - Ship the crate, `fylo link`, the binding artifacts, the C library and the
     browser `.vdbx` default.
   - **Keep `fylo exec --loop`, machine protocol v1 and `executeSQL` frozen
     and deprecated.**
   - Rewrite the README Quick Start around Turso SDKs plus `fylo link`, and
     the crate.
   - Before release, run TACHYON 26.36.01's FYLO storage tests
     (`storage::tests::fylo_*`, `tachyon-cli/tests/it/fylo_datasource.rs`)
     against the new `fylo` binary. That proves nothing broke.
2. **TACHYON's next release** (all of these target it):
   - Yon's `FyloStore` on the in-process crate, with bound parameters replacing
     the SEC-3 literal rendering (§11)
   - Tac's `"fylo"` adapter (§7.4)
   - Tac's built-in Store extended to Java, F#, Python, Lua, C++ and natively
     run services
   - Lua datasource access (§7.5)
3. **The FYLO release after TACHYON's ships:**
   - remove machine protocol v1, `fylo exec --loop`, `executeSQL` and FYLO's
     SQL dialect (`fylo-query/src/sql.rs`)
   - retire `api/machine/v1`

Under Option B, steps 1 and 3 merge into one FYLO release that ships with step 2,
after TACHYON's current release candidate.

## 10. Open questions for the user

**Settled:**

- no listener
- synchronous writes
- no backward compatibility, except the one-release TACHYON window in
  question 3 below
- no NDJSON shims and no stdio Hrana
- **language scope:** Tac, Yon and Turso SDK languages (rule 5)
- **approved on 2026-11-01:**
  - the exact Turso pin
  - TACHYON running FYLO in-process through the crate
  - no public `fylo exec --loop` or WASI contract (its removal now waits one
    release; see question 3)
  - native FYLO on Tac's app targets
  - parking for synchronous Wasm languages

**Reopened by the wider scope, and answered here:**

- **Ruby, PHP and Dart** are supported inside TACHYON (Yon, and Tac where it
  runs them). Their NDJSON shims still go (§8).
- **Swift and Kotlin** are supported:
  - inside TACHYON through Tac-N
  - standalone through the C library (Swift) and Java plus an Android AAR
    (Kotlin)

  The WebView hosts still go (§8).

**Still open:**

1. **Standalone scope for languages with no Turso SDK and no free ride** (Ruby,
   PHP, Dart, Lua). Choose one:
   - **(a)** TACHYON-only in this release (recommended).
   - **(b)** Also ship FFI bindings over the C library: generated Dart first,
     then hand-written Ruby and PHP. Lua stays TACHYON-only either way.
2. **TACHYON's gaps** (§7.5) are TACHYON work, so they need the user's go-ahead
   for TACHYON:
   - extending Tac's built-in Store to Java, F#, Python, Lua, C++ and natively
     run services
   - giving Yon's Lua a resource route

   TACHYON's Release Coordinator has targeted them at TACHYON's next release.
   Confirm?
3. **Release order with TACHYON** (§8 "Release order"). This softens the "no
   backward compatibility" rule. Choose one:
   - **(A, recommended)** FYLO's next release keeps `fylo exec --loop`,
     machine protocol v1 and `executeSQL` frozen and deprecated, so TACHYON
     26.36.01 and its current release candidate keep working. Remove them in
     the FYLO release after TACHYON's crate-based release ships.
   - **(B)** Ship FYLO's next release together with TACHYON's crate-based
     release, after the current release candidate, and drop v1 at once.

Minor, with defaults the plan follows unless the user says otherwise:

- **JS and Python hack placements.** Ship them, documented as hacks, and switch
  JS to Turso's loader variable once that's fixed.
- **Explorer.** Keep the folder-root browser mode and the older JS engine for
  Explorer this release, because it needs folder picking.
- **SQL in the browser.** Leave it out of this release; Tac doesn't need it.

## 11. Contracts and ADRs that change

**FYLO:**
- **New ADR 0019:**
  - the crate as the native API
  - `fylo link`
  - the Turso pin
  - the no-listener rule restated, with ADR 0007 standing and ADR 0012 still
    withdrawn
  - the end of shims and the machine protocol's public contract
- **ADR 0004:** amended for `turso_core` and for shipping third-party binding
  builds.
- **New `api/sql/v1`.**
- **`api/errors/v1.json`:** gains `EFYLO_TXN_SCOPE`.
- **Retired:** `api/machine/v1` as a public contract. It is frozen and
  deprecated in the next release and removed in the release after TACHYON's
  crate-based release (§8 "Release order"). ADR 0019 records the window.
- **Docs:** README, `clients/README.md` and `CONTEXT.md`.

**TACHYON** (all of these target TACHYON's next release):

- **ADR 0032 already has a SEC-3 amendment (2026-10-31).** It came from
  TACHYON FU-04 (fix `c55481537`, tests up to `fc50ffb43`, both on
  `origin/develop`). It says:
  - FYLO raw SQL renders each parameter as **one self-contained literal**.
  - It refuses quoted placeholders such as `'$1'`, plus `;`, NUL and indexes
    out of range.
  - It refuses negative and exponent numbers, because FYLO drops a leading `-`
    and reads `1e2` as `1` (`TY2160`).
  - It records that bound parameters need a FYLO protocol change:
    `executeSQL` in machine protocol v1 takes only `sql` and `access`.
- **This plan is that FYLO change.** Through the crate's `root.sql()` on
  `turso_core`:
  - Values are bound as data (`?`, `?NNN`, `:name`, `$name`).
  - The literal rendering, and the refusal of negative or exponent numbers,
    can go.
  - Raw SQL gains aggregates, `ORDER BY` and `LIMIT`.
- **A new ADR 0032 amendment** then covers:
  - the crate driver
  - cancellation that abandons a call instead of killing a subprocess
  - bound parameters, replacing the SEC-3 rendering
  - optional single-collection transactions
  - which FYLO release is the minimum (the one with the crate)
- **`docs/MANAGED_STORAGE.md`:** update to match.
- **Doctor:** update the `known_toolchains` FYLO entry.
- **CI:** the storage lanes compile the crate instead of installing a binary.

## 12. Experiments run on this Mac (2026-11-01)

These ran in `/private/tmp/claude-501/-Volumes-HAVEN-Developer-DELMA-FYLO/e3e41c1a-613b-44c9-ae24-6671fc084723/scratchpad/symlink-tests`.
No Turso or libSQL package was installed or run. TACHYON was only read.

| # | Test | Result |
|---|---|---|
| T1 | `app.db` symlinked to a file in another folder, WAL write (sqlite3 3.50.6, Python SQLite 3.54.1) | works; `-wal` and `-shm` are created next to the **target** |
| T2 / T3 | pre-placed `-wal` / `-journal` symlink | SQLite **refuses** (error 14) |
| T4 | FIFO as the database | CLI **hangs**; Python gives "disk I/O error" |
| T5 | symlink to a directory | open **fails**: the stock engine fails closed |
| T6 | `System.loadLibrary("_turso_java")` through a symlink (OpenJDK 25) | **loads**; macOS's default `java.library.path` includes `~/Library/Java/Extensions` and `.` |
| T7 | Python extension import through a symlink | **imports** |
| T8 | napi-rs-style loader, with the adjacent `.node` symlinked to a stand-in addon built here | **loads** in Node and Bun |
| T9 | extra symlink in `node_modules/<pkg>/` across installs (local `file:` tarball) | `npm install` keeps it; `npm ci` removes it; `bun install` removes it every time |
| T10 | root `"postinstall"` re-linking | present after `npm install`, `npm ci` and `bun install`; missing after `npm ci --ignore-scripts` |

Read from source, not run:
- Turso builds its WAL path from the unresolved database path
  (`core/database.rs:952`) and opens it following symlinks
  (`core/io/unix.rs:50`).

## 13. Sources

**Turso**
- https://github.com/tursodatabase/turso (README, `COMPAT.md`, `docs/manual.md`,
  `extensions/core/src/vtabs.rs`, and at v0.8.3: `core/ext/mod.rs:309`,
  `core/dialect/sqlite.rs:145`, `core/database.rs:952`, `core/io/unix.rs:50`,
  `core/vdbe/execute.rs:10609`)
- https://docs.turso.tech/sdk/introduction
- https://docs.turso.tech/sql-reference/pragmas
- https://github.com/tursodatabase/turso/releases
- https://github.com/tursodatabase/libsql

**Native loading** (published 0.8.3 packages)
- `@tursodatabase/database` `index.js` L67-74, 194-209, 478-508
- napi-rs PR https://github.com/napi-rs/napi-rs/pull/2907
- `pyturso` wheel
- `turso-go-platform-libs` `load_turso.go`
- `tech.turso:turso` `TursoDB.java` L108-155
- `Turso.Data.Native` and `SqliteConnection.cs` L474-495
- `bindings/rust/src/connection.rs` L52-58

**Licenses**
- https://github.com/tursodatabase/turso/blob/main/LICENSE.md

**FYLO** (`24f43a6`)
- `README.md`, `clients/README.md`
- ADRs 0004, 0007, 0008, 0012, 0013, 0014, 0016, 0017
- `crates/fylo-query/src/sql.rs`
- `crates/fylo-machine/src/lib.rs`
- `crates/fylo-vfs/src/lib.rs`
- `crates/fylo-format/src/ttid_timestamps.rs`
- `api/machine/v1/`
- `tests/interop/`

**TACHYON** (`9ea846513`)
- `docs/MANAGED_STORAGE.md`
- `docs/adr/0032-fylo-datasource.md`
- `docs/BEHAVIOR_ANNOTATIONS.md`, `docs/SUPPORT_TIERS.md`
- `crates/tachyon-core/Cargo.toml`
- `crates/tachyon-core/src/storage.rs` and `src/storage/fylo.rs`
- `src/handler/preludes/*`, `src/handler/resources.rs`
- `src/doctor/known_toolchains.rs`
- `Cargo.lock`

**Browser and Wasm** (§7.4)

- Turso at v0.8.3:
  - `bindings/javascript/packages/wasm/package.json:41-51`
  - `packages/wasm-common/index.ts:45-105, 181-191, 385-430`
  - `packages/wasm/promise-default.ts:28-37`
  - `packages/wasm/vitest.config.ts:10-11`
  - `bindings/javascript/src/browser.rs:27-295`
  - `core/io/mod.rs:48-60, 316-327, 424, 506`
  - `core/io/clock.rs`
  - `core/database.rs:1216, 3355`
  - `core/vtab.rs:597-623`
  - `core/Cargo.toml:20-30, 110, 153-161`
  - `.github/workflows/rust.yml:212-225`
  - published `@tursodatabase/database-wasm@0.8.3`:
    https://unpkg.com/@tursodatabase/database-wasm@0.8.3/?meta
- sqlite-wasm:
  - https://sqlite.org/wasm/doc/trunk/persistence.md
  - `ext/wasm/api/sqlite3-opfs-async-proxy.c-pp.js`
  - `sqlite3-vfs-opfs-sahpool.c-pp.js` (sqlite `master`)
  - https://unpkg.com/@sqlite.org/sqlite-wasm/?meta
- OPFS support and storage policy:
  - MDN browser-compat-data `api/FileSystemSyncAccessHandle.json`
  - https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle
  - https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria
  - https://webkit.org/blog/14403/updates-to-storage-policy/
  - https://webkit.org/blog/10218/full-third-party-cookie-blocking-and-more/
  - https://webkit.org/blog/10882/app-bound-domains/
  - WebKit `FileSystemFileHandle.idl`, `StorageManager.idl`,
    `UnifiedWebPreferences.yaml` (webkitgtk-2.54.2 and `main`)
  - https://commits.webkit.org/318405@main
  - https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2customschemeregistration
  - https://alastair.is/getting-wkwebview-to-treat-a-wkurlschemehandler-as-secure/
- FYLO:
  - `docs/releases/artifact-matrix.md`
  - `src/browser/fylo-single-file-worker.mjs`, `vdbx-client.mjs`
  - commits `e4a6aba`, `87abb10`, `266b3ed`, `5c64027`
- TACHYON:
  - `crates/tachyon-core/src/template/tac-storage.js`
  - `crates/tachyon-core/src/behavior/runtime.js`
  - `docs/APPLICATION_CONFIGURATION.md`
  - `docs/adr/0065-*`, `docs/adr/0089-*`

**Not verified (flagged where used)**

- whether `turso_core` links and runs on `wasm32-unknown-unknown`: CI only
  type-checks it, and `parking_lot`, uuid `now_v7` and the aegis prebuilt
  libraries are open
- the size of a FYLO plus `turso_core` Wasm module
- that a second tab fails with Turso's Wasm package
- that no public WKWebView API marks a custom scheme secure, and how the 7-day
  eviction applies to WKWebView apps
- whether WebKitGTK before 2.56 can turn OPFS on at runtime
- Safari releasing OPFS handles when a Worker is terminated
- OPFS on Android's intercepted HTTPS origin, which comes from general knowledge
- whether the stock Turso engine fails closed on a directory
- `RETURNING` on Turso virtual tables
- rowid uniqueness of the creation segment
- how FYLO errors appear in each SDK's error type
- pnpm, and Bun with registry dependencies, keeping the link
- the Python `.pth` hook end to end
- .NET `LoadExtension` end to end
- the Windows hard-link fallback
- `turso_core` building for iOS and Android targets
- `$1` parameters binding as named parameters in Turso
- the build-time and size impact on TACHYON
