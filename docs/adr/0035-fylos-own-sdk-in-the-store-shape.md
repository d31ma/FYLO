# ADR 0035: FYLO's own SDK in the shape of TACHYON's `Store`, without Turso

- Status: **Accepted, not yet built** (2026-11-21). The user made every
  decision below through the hub on 2026-11-21 (quoted in "Context"). Slices
  SDK-1 to SDK-10 build it. It ships in **the next FYLO release**, together
  with ADRs 0033 and 0034 ("there should be no later FYLO release").
- Date: **2026-11-21**
- Owners: **FYLO maintainers**
- Supersedes: **ADR 0019** sections 2, 3, 4 and 6 (`fylo link`, SQL through
  Turso's engine, the Turso pin, the end of FYLO's own clients) and **ADR
  0020** (Turso's engine and binding builds) as a whole. ADR 0019 sections 1
  and 5 (the `fylo` crate is FYLO's native API; no listener), which ADR 0026
  accepted, stay.
- Amends: **ADR 0004** (the dependency list loses `turso_core`, `turso_ext`
  and the binding workspace), **ADR 0013** (one log record may now hold a
  one-collection transaction opened by a client, section 5).
- Related: ADR 0009 (queue), ADR 0011 and 0028 (invocation mode), ADR 0016
  (`.vdbx`), ADR 0023 (word index and sorted queries), ADR 0026 (C ABI and
  mobile builds), ADR 0032 (system disk), ADR 0033 (layout, namespaces), ADR
  0034 (large objects).
- Frozen contracts: [`api/machine/v2`](../../api/machine/v2/README.md),
  [`api/store/v1`](../../api/store/v1/README.md),
  [`api/sql/v1`](../../api/sql/v1/README.md).
- Spec: FYLO-SDK1, `SDK1-spec.md` (outside the repository, in the FYLO logs
  folder), sections 9 and 10. Where the spec and this ADR differ, this ADR
  wins.

## Context

The user's direction, 2026-11-21, quoted exactly:

> "i'm thinking instead of following Turso SDK, we should have our own SDK --
> but using TACHYON's syntax (`Store` class) for communicating with a
> database"

Asked which part was meant:

> "Replace Turso inside too"

The user's answers to the spec's questions, quoted exactly:

| Question | Answer |
| -------- | ------ |
| Q1 `fylo link` | "Remove with Turso (Rec.)" |
| Q2 SQL | "Own thin SQL, v1 (Rec.)" |
| Q2b joins, `GROUP BY`, `OR` | "Keep them (Rec.)" |
| Q3 language clients | "lets have the same languages that TACHYON project support" |
| Q3 client table | "Approve the list (Rec.)" |
| Q3-T2 native mobile packages | "Keep both (Rec.)" |
| Q4 when | "Layout release (Rec.)" |
| Q5 old machine operations | "Remove with the SDK" |
| Q6 transactions | "One-collection only" |
| Q7 names | "TACHYON's names (Rec.)" |
| Q8 a `fylo store` command | "No (Rec.)" |

Then, on the release plan:

> "there should be no later FYLO release. all the features we're
> implementing now should go to the next release and should compactible
> withj TACHYON"

What the spec found (FYLO `develop` 7f0a190):

- Turso runs only one optional path: SQL through the `fylo` crate's `sql`
  feature (`Root::sql`), plus `fylo link` and `bindings/turso`. The CLI
  (`fylo sql`), the machine protocol (`executeSQL`), the browser engine and
  every client use FYLO's own SQL parser (`fylo-query/src/sql.rs`).
- No shipped artifact contains Turso. TACHYON does not depend on the `fylo`
  crate; it runs `fylo exec --loop`.
- Turso brings 96 packages into `Cargo.lock` that nothing else needs (among
  them `chrono`, whose advisory RUSTSEC-2026-0543 `deny.toml` ignores only
  for Turso, and `aegis`, the reason windows-vm keeps LLVM), plus 389,002
  lines of vendored Rust and a patch to rebase.
- TACHYON's `Store` is a contract (like a Spring Data repository): a
  Repository base (`findById`, `findAll`, `save`, `deleteById`) and derived or
  structured queries (`find`, `count`, `exists`, `delete` with AND-joined
  predicates `eq ne lt lte gt gte in isNull isNotNull`, an order and a page).
  Today TACHYON scans up to 10,000 FYLO documents and counts, sorts and pages
  them itself.

## Decision

### 1. One model: `Database`, `Collections`, `Store`, `Queue`

Every place FYLO ships offers the same model, with TACHYON's names
(snake_case in Python, Ruby and Rust):

- `Database`: opens a root (ADR 0033: `<drive>/<app>`), holds the namespace
  lease, closes. `namespace(name)` selects a namespace (default `public`).
- `Collections`: `exists(name)` (catalog only, no document read),
  `create(name)`, `drop(name)`, `list()`, `inspect(name)`.
- `Store` for one collection: `findById(id)`, `findAll(page)`,
  `save(entity)`, `deleteById(id)`, `patch(id, fields)`, `find(query, page)`,
  `count(query)`, `exists(query)`, `delete(query)`, `scroll(query, cursor,
  size)`, and transactions (section 5).
- `Queue(topic)`: unchanged (ADR 0009).

The engine answers count, exists, delete, sort and paging itself, with
indexes where they exist, and no 10,000-document bound. The query model,
result rules, limits and error codes are frozen in `api/store/v1`.

Everything FYLO has beyond this model keeps its current names: version
control, schemas, the change feed, files and buckets, word search, joins,
segments, identities, verify and rebuild.

### 2. Machine protocol v2

- `fylo exec --loop` and `fylo exec --invocation` speak **protocol v2**.
  Framing, limits, the handshake request and the error envelope are those of
  v1. The handshake answers `protocolVersion: 2` and adds
  `capabilities.store: 1` and `capabilities.sql: 1`.
- **Added:** `storeFindById`, `storeFindAll`, `storeSave`, `storePatch`,
  `storeDeleteById`, `storeFind`, `storeCount`, `storeExists`, `storeDelete`,
  `storeScroll`, `storeTransaction`, `transactionBegin`,
  `transactionCommit`, `transactionRollback`, `collectionExists`,
  `listCollections`.
- **Removed** (the Store replaces them; Q5): `getDoc`, `getLatest`,
  `putData`, `batchPutData`, `patchDoc`, `patchDocs`, `delDoc`, `delDocs`,
  `findDocs`. A request for one of them answers `EUNSUPPORTEDOP`.
- **Changed:** `executeSQL` takes optional `parameters` (`api/sql/v1`).
- **Unchanged:** every other v1 operation, with its v1 request and result
  shape.
- The v2 contract is frozen in `api/machine/v2` before SDK-3 lands. A change
  after the freeze needs an amendment to this ADR and a notice to TACHYON's
  and Mesail's Build Coordinators.

### 3. SQL: FYLO's own thin layer (`api/sql/v1`)

- FYLO keeps its own SQL parser and makes it a front door to the Store plan:
  `SELECT`, `SELECT COUNT(*)`, `INSERT`, `UPDATE`, `DELETE`, `CREATE TABLE`,
  `DROP TABLE`, `EXPLAIN [ANALYZE]`.
- It adds `ORDER BY`, `OFFSET`, `COUNT(*)`, `IN`, `IS [NOT] NULL` and bound
  parameters (`?`, `?NNN`, `:name`, `$name`), so callers never render values
  into SQL text.
- It keeps `JOIN`, `GROUP BY`, `OR` and `LIKE` working as today (Q2b).
- No `BEGIN`/`COMMIT` in SQL; transactions are section 5.
- The subset is versioned as `api/sql/v1`. Anything outside it fails with
  `EUNSUPPORTEDSQL`.

### 4. Turso and `fylo link` are removed

- The `fylo` crate's `sql` feature, `vendor/turso_core`, its patch and
  `scripts/vendor-turso-core.sh`, `bindings/turso`, `fylo link` and
  `LinkIdentity` are deleted (Q1). `deny.toml` drops the RUSTSEC-2026-0543
  exception.
- `Root::sql` is replaced by `Database::sql(statement, parameters)` on FYLO's
  own SQL.
- FYLO's crates were and stay `unsafe`-free; no third-party `unsafe` engine
  remains in FYLO's builds.

### 5. Transactions inside one collection

- **In:** `save`, `patch`, `deleteById` and `delete(query)` on **one
  collection** commit as **one** ADR 0013 log record, or not at all.
- **Two shapes, one engine path:** a batch (`storeTransaction { collection,
  writes }`), and begin / commit / rollback (`transactionBegin`, the `store*`
  operations with a `transaction` handle, `transactionCommit`,
  `transactionRollback`). TACHYON maps `@AllOrNothing` on a FYLO-backed
  store to the second shape.
- **Bounds:** at most 1,000 writes and 8 MiB of documents; a time limit from
  begin, default 30,000 ms, at most 300,000 ms. Past a bound, the transaction
  rolls back.
- **Reads** with the handle see the transaction's own writes; reads without
  it see committed state only.
- **Other writes** to the same collection in the same process wait for the
  transaction, up to its time limit, then fail with `ETXNBUSY`.
- **A process that ends before commit wrote nothing**: no record is appended
  before commit, so the next open needs no recovery for it. A crash during
  commit leaves the state before or after, never a mix.
- **Out:** a second collection, queue operations, schema changes,
  `createCollection` and `dropCollection` inside a transaction (all
  `ETXNSCOPE`); savepoints, nesting, isolation levels, cross-process and
  cross-root transactions.
- The segments layout commits a transaction only if it can write the batch as
  one record; otherwise it refuses with `ETXNLAYOUT` (SDK-3 decides, and
  amends this section).

### 6. Clients: TACHYON's languages

- FYLO ships a client with the `Store` model for each TACHYON Yon (server)
  language, one per runtime: JavaScript and TypeScript, Python, Ruby, PHP, the
  JVM (Java, Kotlin, Groovy), .NET (C#, F#), Go, Rust (the `fylo` crate), Dart,
  Lua and C++. Tac (browser) languages need no client; the browser engine
  keeps its `Store` class.
- **Keep:** JavaScript and TypeScript (the Node client and the browser
  engine), Python, Ruby, PHP, the JVM client, the .NET client, Go, the Rust
  crate and Dart. **Add:** Lua and C++. **Drop:** the Rust shim (the crate
  replaces it) and the Swift, Kotlin and Flutter WebView hosts. The user
  approved this table on 2026-11-21: "Approve the list (Rec.)".
- **The native mobile packages stay** (FyloKit for Swift and the Android
  AAR; ADR 0026), each with a `Store` class. The user, on 2026-11-21: "Keep
  both (Rec.)".

### 7. One release, and how users move

- Everything here ships in the next FYLO release, with ADRs 0033 and 0034.
- The old operations go last (SDK-10), only after TACHYON's branch (TAC-FL1,
  TAC-FY2) and Mesail's branch (MSL-K30) have passed against a FYLO
  `develop` build that has both the Store operations and the old ones.
- FYLO publishes `develop` builds for that
  (`~/Developer/AnvilTargets/fylo-builds/<fylo-sha>/<platform>/fylo`, with
  `fylo.sha256`, for `macos-arm64`, `linux-arm64` and `windows-arm64`).
  TACHYON and Mesail keep their `develop` pinned to FYLO 26.45.07 and merge
  their switches together with the pin bump right after the release.

## Consequences

- **Breaking Changes** (CHANGELOG of the next release): machine protocol v2;
  the removed operations listed by name; `fylo link` gone; the crate's `sql`
  feature, `Root::sql`, `TURSO_PIN`, `install_turso_open_hook` and
  `is_fylo_root` gone; the crate's `Root`/`Collection` replaced by
  `Database`/`Store`; the Rust shim and the WebView hosts gone.
- TACHYON's FYLO adapter becomes a pass-through and drops its own scan, sort,
  paging and 10,000 bound (TAC-FY2). Mesail's devicebox test and Floci
  capture steps move to the Store operations (MSL-K30).
- `Cargo.lock` loses 96 packages; `cargo deny` no longer needs the chrono
  exception; Platform may drop LLVM from windows-vm.
- New error codes (`api/store/v1`) enter `api/errors/v1.json` in the slice
  that first emits them.

## Alternatives rejected

| Route | Why |
| ----- | --- |
| Keep Turso SDKs as FYLO's SDK (ADR 0019) | the user chose FYLO's own SDK in TACHYON's shape |
| Drop SQL | breaks `fylo sql`, `executeSQL` and TACHYON's documented SQL datasource on FYLO |
| A new SQL engine (GlueSQL, DataFusion, SQLite virtual tables) | swaps one large third-party engine for another; `unsafe` or size problems |
| Keep the old operations one more release | the user chose to remove them with the SDK (Q5) |
| Transactions across collections | the user chose one collection only (Q6) |
