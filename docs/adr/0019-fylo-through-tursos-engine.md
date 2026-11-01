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
  `fylo` feature that registers FYLO's tables when the opened path is a FYLO
  root, then builds each Turso binding's native library with it (about 18
  artifacts per release), signed and with provenance, keeping Turso's MIT
  notice.
- `fylo link [--check | --undo]` detects the installed Turso SDKs, refuses a
  version other than the pinned one (printing the exact install command), and
  places FYLO's build without editing a package file. Each placement carries a
  label:

  | SDK | Placement | Label |
  |---|---|---|
  | .NET | `EnableExtensions()` plus `LoadExtension(path)` | Supported |
  | Rust | the `fylo` crate | Supported |
  | Go | `InitLibrary(... "system")`, plus a library on the search path | Semi-supported |
  | Java / Kotlin | a library on `java.library.path` | Semi-supported |
  | JS / TS | a symlinked `.node` next to the loader, restored by `postinstall` | Hack |
  | Python | a `.pth` import hook | Hack |

- On Windows, where symlinks need Developer Mode, `fylo link` falls back to a
  hard link or a copy, and `--check` compares hashes.
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
  third-party binding builds. At 0.8.3 the `sql` feature adds or bumps 149
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
