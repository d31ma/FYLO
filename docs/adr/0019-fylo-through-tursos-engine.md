# ADR 0019: FYLO through Turso's engine

- Status: **Proposed** (draft, 2026-11-01)
- Date: **2026-11-01**
- Owners: **FYLO maintainers**
- Amends: ADR 0004 (dependency policy: `turso_core`, and third-party binding
  builds). ADR 0007 stands; ADR 0012 stays withdrawn.
- Source: the design note "Using Turso SDKs with FYLO" (revision 5,
  2026-11-01), its section 11, and the answers the user approved in its
  section 10.

## Context

FYLO ships nine thin language shims that spawn `fylo exec --loop` and speak
the NDJSON machine protocol, plus its own small SQL dialect. The next release
breaks compatibility with earlier ones, and the user set four rules for it:

1. No server or listener of any kind: no TCP, no Unix socket, no named pipe.
   A spawned child on anonymous pipes, and files, are allowed.
2. Writes are synchronous: a write commits in FYLO's documents, or fails with
   FYLO's error, before the call returns.
3. No backward compatibility with earlier releases' clients or protocols.
4. Where Turso ships an official SDK, apps use it instead of a FYLO client.

Turso's SDKs (JS/TS, Python, Go, Rust, Java and .NET at 0.8.3) all run Turso's
engine, `turso_core`, in-process. None loads a SQLite extension by default,
but `turso_core` already registers internal virtual tables and static
extensions through the same path its own built-ins use. TACHYON, FYLO's main
embedding framework, runs its Yon server in Rust and reaches FYLO today
through one long-lived `fylo exec --loop` subprocess (TACHYON ADR 0032).

## Decision

### 1. The `fylo` crate is FYLO's native API

- A new crate, `fylo`, runs FYLO in the host's own process. It wraps
  `fylo-engine`, `fylo-storage-native` and `fylo-query` behind a small facade:
  `Root` (open, lease, close), documents (put, patch, get, latest, delete,
  find), collections (create, drop, inspect) and the queue (publish, claim,
  ack, nack). Every call is synchronous and durable before it returns.
- A `Root` holds the root's exclusive lease for its lifetime, so one process
  owns a root, as one `fylo exec --loop` session does today. It is `Send` and
  `Sync`; calls may come from any thread.
- Behind its `sql` feature the crate adds a Turso SQL connection on the same
  root (section 3). Without the feature it has no Turso dependency.
- It is published to crates.io, or consumed as a git tag of `d31ma/Fylo`. The
  workspace crates it depends on stop being `publish = false`.
- Rust apps, TACHYON included, depend on the crate. It replaces the Rust shim.

### 2. `fylo link` puts FYLO's engine inside Turso's SDKs

- FYLO's release pipeline patches `turso_core` at the pinned tag with a
  `fylo` feature, then builds each Turso binding's native library with it
  (about 18 artifacts per release), signed and with provenance, keeping
  Turso's MIT notice.
- The patch's open hook (`turso_core::fylo_hook`) is one function pointer,
  checked first in `Database::open`. A binding built with FYLO installs
  `fylo::install_turso_open_hook()` before it opens a database. The hook
  claims only an existing directory with FYLO collections (`.collections`),
  opens that root, takes its lease until the database closes, and returns an
  in-memory database with every collection as a table; any other path, and
  every path in a build without the hook, opens exactly as stock Turso.
  `.vdbx` files are not claimed yet, because the `fylo` crate does not open
  them.
- Bindings are built in a separate workspace, Turso's repository at the
  pinned tag, by `bindings/turso/build.sh`, with
  `bindings/turso/turso-<version>-bindings.patch`: the workspace reduced to
  the binding crates, its `turso_core` replaced by FYLO's vendored one, and
  two safe calls (the JavaScript binding before
  `Database::open_file_with_flags`; Turso's SDK kit before it opens the file
  itself). ADR 0020 sets the `unsafe` and dependency policy for that
  workspace.
- `fylo link [--check | --undo]` detects the installed Turso SDKs, refuses a
  version other than the pinned one (printing the exact install command), and
  places FYLO's build without editing a package file. Each placement carries a
  label:

  | SDK | Placement | Label |
  |---|---|---|
  | .NET | the SDK kit as `turso_sdk_kit`, loaded by one `NativeLibrary.SetDllImportResolver` line in the app | Semi-supported |
  | Rust | the `fylo` crate | Supported |
  | Go | `InitLibrary(... "system")`, plus a library on the search path | Semi-supported |
  | Java / Kotlin | a library on `java.library.path` | Semi-supported |
  | JS / TS | a symlinked `.node` next to the loader, restored by `postinstall` | Hack |
  | Python | a `.pth` import hook | Hack |

- `fylo link` finds the FYLO builds through `--artifacts`,
  `$FYLO_LINK_ARTIFACTS`, or `../lib/fylo-link` beside the `fylo`
  executable; `fylo-link.json` there names the Turso version and platform.
  It places a symlink where it can: `turso.<triple>.node` beside the JS SDK's
  `index.js`, and `libturso_sync_sdk_kit.<ext>` in a Go project's directory,
  which `LoadStrategy: "system"` searches after `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` / `PATH`. It never writes a package file or a lockfile,
  refuses a file or symlink it did not place, and `--undo` removes only its
  own links.
- Python (pyturso in the project's `.venv` or `venv`, else `$VIRTUAL_ENV`):
  `fylo link` writes `fylo_turso.pth` in that `site-packages`, an import
  hook that loads FYLO's `_turso.abi3.so` for `turso._turso`; the artifact
  path is JSON-quoted into it. A pyturso reinstall keeps the hook; a `.pth`
  without FYLO's marker line is never touched. pyturso 0.8.3 ships no
  Windows wheel, so there is no Windows build.
- Java (`tech.turso:turso` in `build.gradle(.kts)` or `pom.xml`): the JNI
  library (`lib_turso_java.<ext>`, `_turso_java.dll`) in the project
  directory. The driver tries `System.loadLibrary("_turso_java")` before its
  own copy, so the app runs with `-Djava.library.path=<project>`; without it
  the driver silently falls back to stock Turso.
- .NET (`Turso.Data.Sqlite.Provider`, `Turso.Data.Common` or
  `Turso.EntityFrameworkCore.Sqlite` in a `.csproj`): Turso's own .NET
  packages ship the SDK kit renamed `turso_sdk_kit`, so `fylo link` places
  FYLO's SDK-kit build under that name in the project directory and prints
  the one line the app adds before its first Turso call:
  `NativeLibrary.SetDllImportResolver(Assembly.Load("Turso.Raw"), …)`.
  `LoadExtension` was not used: it needs a `turso_ext` entry point, and the
  build output's native folder is rewritten by every `dotnet build`.
- Where symlinks are not allowed (Windows outside Developer Mode),
  `--method auto` falls back to a hard link on the same volume, then to a copy
  checked by SHA-256; `--method symlink|hardlink|copy` forces one. A hard
  link or a copy gets a `<file>.fylo-link` record (artifact path, SHA-256,
  method): `--check` verifies the hash against the artifact, `--undo` removes
  only a file whose hash is unchanged, and a file without a record is never
  touched. Windows cannot replace or remove a library a running process has
  loaded; `fylo link` says so when that is why it failed.
- On Linux, tursogo's `"system"` search needs the project directory as an
  absolute entry first on `LD_LIBRARY_PATH`. tursogo joins each entry with
  the file name and passes the result to `dlopen`; with the variable unset
  (or `.`) it finds the library in the working directory but passes a bare
  file name, which glibc's `dlopen` looks up only on the system paths.
  `fylo link` recommends `os.Setenv("LD_LIBRARY_PATH", dir)` before
  `InitLibrary` (tursogo reads the variable itself at that moment), and
  prints the `export LD_LIBRARY_PATH=…` line as the alternative. The in-app
  form is preferred: the dynamic loader reads an exported `LD_LIBRARY_PATH`
  for every library the process loads, so a file in the project directory
  could stand in for a system library. Rejected: an rpath in the app (tursogo's
  bare-name `dlopen` comes through purego, so the app's `RUNPATH` is not the
  caller's, and it would need custom linker flags in every app), and a copy
  on the default search path (`/usr/local/lib` plus `ldconfig`: root, and one
  machine-wide library for every project).
- `fylo version --output json` reports `tursoLink`: the pinned Turso version
  and the artifacts built for it.

### 3. One SQL mapping for both layers (`api/sql/v1`)

- A FYLO root opened through Turso gets an in-memory SQL catalog with one
  table per document collection: `id` (the TTID), `doc` (JSON text), one
  column per head-schema field, and hidden `meta`, `created_at` and
  `updated_at`. No SQL engine file is ever written; documents stay the truth.
  The catalog runs on FYLO's own `IO` (memory only, FYLO's clock).
- The tables are Turso **internal virtual tables**, a safe Rust interface,
  not the `turso_ext` C extension interface, which FYLO's `unsafe_code =
  "forbid"` keeps out. Turso 0.8.3 makes internal tables read-only, so FYLO's
  patch makes them writable (section 4).
- `=` on `id` and on field columns, and numeric ranges on field columns, push
  down to FYLO; the engine still rechecks every pushed condition, so pushdown
  can only narrow what FYLO reads. Turso 0.8.3 passes no `LIKE`, `IN` or
  `MATCH` constraint to a virtual table, so those wait for more planner
  patching, and FYLO's native engine plans only equality from its indexes, so
  a pushed range still scans.
- `INSERT`, `UPDATE` and `DELETE` map to puts, replaces and deletes, with
  every check a typed call runs. Errors are the SDK's own SQL error carrying
  FYLO's code (`… EARRAYOFOBJECTS: …`); the Rust API returns that code.
- Turso's bound parameters (`?`, `?NNN`, `:name`, `$name`) replace FYLO's
  literal-only dialect.
- Every statement's writes, and every `BEGIN … COMMIT`'s, are buffered and
  committed as one ADR 0013 log record (the new
  `NativeWriteRoot::commit_document_writes_with`), so a statement or a
  transaction lands whole or not at all. Scans in a transaction read its own
  writes. A second collection in one transaction fails with the new code
  `EFYLO_TXN_SCOPE` until ADR 0016 section 6 lands. A failing statement inside
  a transaction discards the transaction's buffered writes. Batches need the
  files layout; the segments layout refuses them for now.
- Turso 0.8.3 registers scalar functions only through the C interface, so the
  version and the queue are tables: `fylo_info`, `fylo_queue_publish`,
  `fylo_queue_claim` (a `SELECT` with `topic` and `group_name`),
  `fylo_queue_ack` and `fylo_queue_nack`. Queue writes take effect when their
  statement runs, not at `COMMIT`.

### 4. The Turso pin

- Each FYLO release pins one exact Turso version. The crate's `sql` feature
  depends on `turso_core = "=<version>"`; Cargo enforces it for Rust, and
  `fylo link` enforces it for every other SDK.
- Moving the pin rebases FYLO's small `turso_core` patch and rebuilds every
  binding artifact.
- The patch lives in the repository as `vendor/turso_core-<version>-fylo.patch`
  next to the patched source in `vendor/turso_core` (Cargo builds a patched
  crate only from source). `scripts/vendor-turso-core.sh [--check]` rebuilds
  or checks the tree from the crates.io release, whose SHA-256 it verifies.
  The workspace excludes the vendored crate, so FYLO's lints and tests do not
  run on it.
- At 0.8.3 the patch is 6 files, +147/−32 lines, all behind a `fylo`
  feature except the manifest's dependency lines:
  - internal virtual tables become writable, with begin, commit and rollback,
    and join the connection's virtual-table transactions;
  - `COMMIT` and `ROLLBACK` commit and roll back virtual tables (upstream
    never calls them from an explicit transaction);
  - a failed statement, or one reset while running, rolls its virtual-table
    writes back (upstream leaves them buffered for the next commit);
  - a multi-row `INSERT … VALUES` into a virtual table writes every row
    (upstream writes only the last);
  - `regex` and `regex-syntax` use CHEX's feature set, because Cargo unifies
    features: Turso's default `regex` features changed CHEX's validation
    dialect for the whole build.
- `turso_core` 0.8.3 needs its `fs` and `uuid` features to build; FYLO leaves
  encryption, SIMD and full-text search off.

### 5. The no-listener rule, restated

- FYLO opens no server, socket, pipe or port, in-process or out. ADR 0007
  stands as written, and ADR 0012 stays withdrawn.
- The `fylo` crate and every `fylo link` placement run FYLO inside the app's
  own process.

### 6. What ends

- The nine thin shims (Python, Ruby, Node, PHP, Go, Rust, C#, Java, Dart), the
  Flutter client, the shim interop tests and the `fylo-clients.tar.gz` asset.
- Ruby, PHP and Dart, which have no Turso SDK.
- FYLO's own SQL dialect: `fylo sql` and `executeSQL` run on `turso_core`.
- `api/machine/v1` as a public, versioned client contract, and `fylo exec
  --loop` as a public interface. The batch interface inside the Wasm module
  and the browser Worker's message protocol stay, private to FYLO's own
  browser loader and checked by `fylo_abi_version`.
- Until TACHYON's driver moves to the crate, `fylo exec --loop` and machine
  protocol v1 stay exactly as they are: TACHYON 26.36.01 and its current
  release candidate depend on them. They are removed only in the release that
  ships the crate and TACHYON's in-process driver together.
- The Swift and Kotlin WebView hosts and the mobile bridge. Mobile apps get
  FYLO through TACHYON's native hosts linking the crate.
- FYLO's browser engine (`fylo-browser.wasm` over a `.vdbx` file in OPFS)
  stays as the one browser client: Turso's Wasm package can't load FYLO.

## Consequences

- TACHYON drops its FYLO subprocess and gains process-local FYLO. A cancelled
  call can no longer be killed, only abandoned (TACHYON ADR 0032 amendment),
  and `turso_core` joins its build behind a feature.
- Every release carries about 18 binding builds and a rebased `turso_core`
  patch. Apps install the exact Turso SDK version FYLO names.
- The JS and Python placements are hacks: a reinstall without scripts, or
  Yarn PnP, removes them silently, and a Turso loader change can break them.
- `api/sql/v1` replaces FYLO's SQL contract, and `api/errors/v1.json` gains
  `EFYLO_TXN_SCOPE`.
- ADR 0004 is amended for `turso_core` (pre-1.0, MIT) and for shipping
  third-party binding builds: ADR 0020 (proposed) keeps FYLO's crates
  `unsafe`-free and confines Turso's upstream `unsafe` to the vendored engine
  and the separate binding workspace. At 0.8.3 the `sql` feature adds or bumps 149
  packages, all under permissive licenses: MIT or Apache-2.0 for most,
  Unicode-3.0 (the `icu` family), BSD-2/3-Clause, ISC, Zlib, BSL-1.0 and
  Unlicense alternatives; `cfg_block` declares no SPDX expression, only a
  license file. `turso_core`, `turso_ext`, `turso_parser` and `turso_macros`
  are MIT. It adds no advisory to `cargo deny`.
- README, `clients/README.md` and `CONTEXT.md` are rewritten around Turso
  SDKs, `fylo link` and the crate.

## Evidence required before acceptance

- Phase 1 (2026-11-01 spike): the crate facade without SQL, and TACHYON's
  FYLO datasource running on it unchanged, with latency against the
  subprocess and the cancellation behaviour measured.
  - Measured 2026-11-01 on the M4 Mac, one engine build (24f43a6, release),
    200 iterations, a 1,000-document collection, roots on the internal disk:
    the in-process path (crate behind `spawn_blocking`) beats one long-lived
    `fylo exec --loop` by 1.1–1.4× at p50 for document and queue calls
    (getDoc 453 → 346 µs, putData 1,276 → 1,029 µs, patchDoc 1,008 → 917 µs,
    findDocs by equality 2,965 → 2,794 µs, queueClaim 815 → 743 µs). The
    pipe and NDJSON cost is about 70–250 µs a call; the engine's own durable
    write and per-call open dominate. `inspectCollection` is 21× faster only
    because the machine operation also verifies the index.
- Phase 2: `turso_core` patched at the pin with read-only tables and pushdown,
  then writes, transactions and the queue functions, plus a crash matrix
  (kill mid-statement and mid-`COMMIT`, then recover).
  - Done 2026-11-01 on branch `feat/fylo-sql-core` (crate feature `sql`).
    Pushdown returns exactly what a full scan returns on 14 query shapes,
    joins included. In a release build over 5,000 documents, `id =` takes
    0.28 ms against a 261 ms full scan, `name =` 0.53 ms, `price =` (100
    rows) 5.5 ms, and `group = AND price >` 36.5 ms; a numeric range is not
    faster (263 ms), for the reason in section 3.
  - The crash matrix interrupts an autocommit `UPDATE` of many rows and a
    `BEGIN`/`INSERT`/`UPDATE`/`DELETE`/`COMMIT` at every write-ahead-log
    transition (before and after the append, before and after the sync, and
    during apply), by losing the process (`abort`) and by an in-process
    `ENOSPC`. In all 24 cases the reopened collection holds exactly the state
    before or after, never a mix; every commit whose record was appended
    survives; every `ENOSPC` leaves the state before; and the index equals an
    independent rebuild from the documents.
- Phase 3: binding artifacts and `fylo link` for JS and Go first, with a
  conformance suite running each stock Turso SDK at the pinned version against
  FYLO's build; then Python, Java and .NET; macOS, Linux and Windows; and a
  security review of the supply chain, link permissions and FFI.
  - Done 2026-11-01 for macOS arm64, JS and Go, on branch `feat/fylo-link`.
    The patch grew by the open hook and two SQLite behaviours a
    virtual-table write was missing (`changes()` and `last_insert_rowid()`):
    9 files, +184/−33 lines. The binding patch is 5 files, +27/−86.
  - Artifacts, built cold on the ANVIL SSD with sccache warm: the JS addon
    `turso.darwin-arm64.node` 20 MB in 159 s (stock 16.7 MB), and tursogo's
    `libturso_sync_sdk_kit.dylib` 18 MB in 95 s (stock 15.8 MB).
  - Conformance (`bindings/turso/conformance/run.sh`), with
    `@tursodatabase/database` 0.8.3 from npm and `tursogo` v0.8.3 from the Go
    proxy, unmodified: before `fylo link` the stock SDK cannot open a FYLO
    root ("is a directory"); after it, both SDKs read collections as tables,
    get pushdown results equal to full scans, insert, update and delete
    through FYLO with correct change counts, commit and roll back a
    transaction, hit `EFYLO_TXN_SCOPE`, see FYLO's error codes, use the queue
    tables, and still open a plain database path as stock Turso. `npm ci`
    removes the JS link; a `postinstall` of `fylo link` restores it, and the
    suite passes again. A version mismatch is refused with the install
    command; `--undo` leaves nothing behind.
  - Linux and Windows: see "Platforms" below.
  - Python, Java and .NET: see "More SDKs" below. Still to do: musl builds
    for tursogo, artifact signing, and x64 runs of Python, Java and .NET.

### Platforms (2026-11-01, branch `feat/fylo-link-platforms`)

Built by `bindings/turso/ci-linux.sh` in a container on Colima's Linux VM
(Ubuntu 24.04, glibc, aarch64): aarch64 natively, x86_64 with Ubuntu's
`x86_64-linux-gnu` cross toolchain. Sizes are stripped of debug info; build
times are warm (a cold build took 105-110 s for the addon and 87-88 s for the
SDK kit per target).

| Platform | Artifact | Size | Build | Ran | Conformance |
|---|---|---|---|---|---|
| linux-arm64-gnu | `turso.linux-arm64-gnu.node` | 22.3 MiB | 54 s | native | pass (auto, copy) |
| linux-arm64-gnu | `libturso_sync_sdk_kit.so` | 20.0 MiB | 61 s | native | pass (auto, copy) |
| linux-x64-gnu | `turso.linux-x64-gnu.node` | 24.1 MiB | 64 s | amd64 container, emulated | pass (auto, copy) |
| linux-x64-gnu | `libturso_sync_sdk_kit.so` | 22.1 MiB | 76 s | amd64 container, emulated | pass (auto, copy) |

- The x64 artifacts ran: an `ubuntu:24.04` amd64 container on the arm64 VM,
  with an x64 Node (`npm ci` and the `postinstall` link included) and an x64
  Go app. The Go toolchain itself crashes under that emulation ("found bad
  pointer in Go heap"), so the app is cross-compiled on arm64
  (`CGO_ENABLED=0 GOARCH=amd64`; tursogo loads through purego, without cgo)
  and only run emulated.
- Go on Linux needs the absolute `LD_LIBRARY_PATH` described in section 2;
  the suite also checks that without it tursogo cannot load the link.
- The `fylo link` unit tests pass on Linux arm64.

Built by `bindings/turso/ci-windows.ps1` in `windows-vm` (Windows 11 Pro on
ARM64, MSVC 14.44, Rust 1.97.1), in a clone inside the VM. aarch64 uses the
ARM64-hosted MSVC tools. The VM has no ARM64-hosted x64 tools, so x86_64
uses vcvarsall's `x64` environment, whose x64-hosted compiler and linker
run under Windows on ARM's x64 emulation; nothing was installed for it.
Build times are cold for the target, with sccache.

| Platform | Artifact | Size | Build | Ran | Conformance |
|---|---|---|---|---|---|
| win32-arm64-msvc | `turso.win32-arm64-msvc.node` | 14.8 MiB | 198 s | native | pass (auto, hardlink, copy) |
| win32-arm64-msvc | `turso_sync_sdk_kit.dll` | 13.5 MiB | 152 s | native | pass (auto, hardlink, copy) |
| win32-x64-msvc | `turso.win32-x64-msvc.node` | 17.5 MiB | 214 s | x64 emulation | pass (auto, hardlink, copy) |
| win32-x64-msvc | `turso_sync_sdk_kit.dll` | 16.1 MiB | 155 s | x64 emulation | pass (auto, hardlink, copy) |

- The x64 artifacts ran: an x64 Node 24.22.2 (with `npm ci` and the
  `postinstall` link) and a `GOARCH=amd64` Go app, both under emulation.
- tursogo's Windows search is `PATH`, then the working directory, joined
  into an absolute path, so the library in the project directory loads
  with no environment change.
- Git Bash puts its own `/usr/bin/link` ahead of MSVC's `link.exe`, so the
  script names MSVC's linker per target (`CARGO_TARGET_<T>_LINKER`).
- A real bug found here: `fylo link` writes a symlink's target in its
  canonical `\\?\C:\…` form, and Windows can hand it back in another
  form, so `fylo link` and `--check` did not recognise their own link. They
  now compare the resolved paths (unit test
  `a_symlink_to_the_artifact_written_another_way_is_recognized`).
- The `fylo link` unit tests pass on Windows, including the fallback paths
  (no symlink right, then a hard link; across volumes, a copy; a stale copy
  reported by `--check`; a hand-edited copy left by `--undo`).
- The VM's SSH session is elevated, so `--method auto` made real symlinks
  there; `--method hardlink` and `--method copy` ran for real on NTFS.
- Without the symlink right, for real: `fylo link` run under a basic-user
  token (`runas /trustlevel:0x20000`, which does not hold
  `SeCreateSymbolicLinkPrivilege`; Developer Mode off) fell back to a hard
  link after "A required privilege is not held by the client. (os error
  1314)", `--check` verified it by SHA-256, and `--undo` removed it, for
  both the JS addon and the Go library.

### More SDKs (2026-11-02, branch `feat/fylo-link-platforms`)

Python, Java and .NET join JS and Go. The binding patch adds `bindings/java`
(the hook installed before both JNI opens) and `bindings/python` (through
the already patched SDK kit) to the workspace; `build.sh` builds the JNI
library with cargo and the Python extension with maturin 1.16.0. .NET needs
no build of its own. Each SDK's conformance program
(`conformance/{python,java,dotnet}`) runs the checks the Go one does, with
the stock package from PyPI, Maven Central (jar checked against Maven's
SHA-1) or NuGet.

| Platform | Python | Java | .NET |
|---|---|---|---|
| macOS arm64 | pass | pass | pass |
@@SDK-ROWS@@

- macOS arm64 artifacts: `_turso.abi3.so` 23.5 MiB (67 s warm through
  maturin), `lib_turso_java.dylib` 16.1 MiB (52 s warm).
- A failing statement inside a transaction already ends it, so a later
  `ROLLBACK` reports "no transaction is active"; the Python, Java and .NET
  checks accept that and verify the transaction's write is gone (Go's
  `Rollback` error was always ignored).
- Found while building: the macOS dylibs stripped of debug info by cargo
  (`strip = "debuginfo"`) would not load ("mis-aligned LINKEDIT string
  pool"), and Node's loader then fell back to the stock platform package
  without a word. `build.sh` no longer strips on macOS (19.4 vs 20 MB).
- The `.pth` line runs inside `site`'s function scope, so the hook imports
  `importlib.util` inside its lambda rather than relying on the line's own
  import.

### Security review (2026-11-02)

Scope: supply chain of the artifacts, what `fylo link` writes and trusts,
and the FFI surface.

- **Code a linked app loads is whatever sits in the artifacts directory.**
  Every placement is a symlink to it, or a copy or hard link of it, or (for
  Python) a `.pth` naming it. Fixed: `build.sh` pins each artifact's SHA-256
  in `fylo-link.json`, and `fylo link` and `--check` refuse an artifact that
  no longer matches, so a library swapped after the build is caught for
  symlinks too, not only for copies. On Unix `fylo link` refuses an
  artifacts directory, manifest or artifact that other users can write
  (group-writable is allowed: Ubuntu's default umask makes it common).
  Windows ACLs are not checked. The pin sits next to the artifacts, so it
  catches accidents and other users, not an attacker who can rewrite both;
  that needs signed releases (open).
- **`LD_LIBRARY_PATH` widens the loader's search.** Fixed by recommending
  the in-app `os.Setenv` form (see section 2).
- **The `.pth` hook runs at every interpreter start in that environment.**
  It imports nothing at start-up beyond `sys`, matches only
  `turso._turso`, and its artifact path is JSON-quoted, so a path cannot
  inject code. A `.pth` without FYLO's marker is never touched.
- **Ownership records** (`<file>.fylo-link`, `fylo_turso.pth`) are plain
  files; forging one only lets `--undo` remove a file whose hash equals the
  recorded one, and never a file it did not match.
- **Supply chain.** The Turso checkout is pinned by commit, the binding
  build now runs `--locked` against FYLO's `bindings/turso/Cargo.lock`, and
  maturin is pinned to 1.16.0. `cargo deny` over the binding workspace with
  Java and Python added found one more advisory, RUSTSEC-2026-0190
  (`anyhow` 1.0.98, pulled in by pyturso's binding; also in stock Turso's
  lock); FYLO's binding lock moves it to 1.0.104. What remains are the same
  8 advisories as phase 3, all in stock Turso 0.8.3's lock; bans, licenses
  and sources pass.
  The conformance suites install pyturso and the .NET packages by version
  without hash pinning; they are tests, not shipped.
- **FFI.** FYLO's crates stay `unsafe`-free; the `unsafe` code is Turso's own
  binding layers (napi, JNI, pyo3, the SDK kit's C API) at the pinned tag,
  plus FYLO's two-line hook call in each. No FYLO code crosses the FFI
  boundary except through `turso_core`'s safe virtual-table interface.
- Java 25 warns that `System.loadLibrary` is a restricted method; a future
  JDK needs `--enable-native-access=ALL-UNNAMED` for the stock driver too.

## Alternatives rejected

| Route | Why |
|---|---|
| Hrana over loopback TCP, a Unix socket or a named pipe | a listener |
| Two-way file sync with tentative writes | writes must be synchronous |
| Custom VFS, FUSE, FSKit or preload interposition | pages become the truth, or too fragile |
| Symlinked or FIFO database, WAL or journal files | SQLite refuses them, and FYLO refuses symlinked roots |
| A mailbox RPC through a shared database file | needs polling, and isn't the SDK's API |
| Generated shims, or stdio Hrana | the new scope replaces FYLO's clients with Turso's SDKs |
| A private subprocess protocol kept only for TACHYON | the crate gives TACHYON the same engine without a second contract |
