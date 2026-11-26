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
- **Layouts (decided in SDK-3):** a transaction commits only on a document
  collection whose writes go through the commit log, which writes the whole
  batch as one record with one durable flush. The files layout, the segments
  layout and `.vdbx` all do this, so all three take transactions. `ETXNLAYOUT`
  stays only for a collection whose documents do not go through the commit
  log: a raw-file collection, or a sidecar-store root on Windows.

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

### Amendment 1 (2026-11-21): `ne` and `null`; `storeSave` with `requireExisting`

TACHYON's TAC-FY2 checked the frozen contracts on `develop` 88c31fa and asked
for two changes. The FYLO Build Coordinator accepted both as maintainer. Both
ship in the same release (DB-2 builds).

1. **`ne` never matches `null` or a missing field**, as SQL `<> X` does.
   The draft said a missing field counts as `null` and `ne` was the opposite
   of `eq`; now `ne X` matches only a present, non-`null` value that is not
   equal to `X`. `isNull` and `isNotNull` stay the way to ask for `null`.
   `ne null` matches like `isNotNull`, as `eq null` matches like `isNull`.
   FYLO's SQL also leaves out `NULL` and missing fields for `<> v` and
   `!= v`; its comparisons with `NULL` itself follow part 3. Built in SDK-2
   (Store) and SDK-4 (SQL).
2. **`storeSave` takes `requireExisting`** (default `false`). With `true`,
   the save only replaces: an `entity[idField]` that is missing, `null` or
   names no existing document fails with `ENATIVE_NOT_FOUND`, writes nothing,
   and the message names the collection and the id. This is TACHYON's update
   contract (TACHYON ADR 0032). The default stays create-or-replace. Built in
   SDK-3.

3. **SQL is strict about `NULL`** (Amendment 1, part 2, the same day).
   TACHYON passes users' raw SQL to `executeSQL`, and on PostgreSQL, MySQL,
   SQLite and SQL Server a comparison with `NULL` matches no row. So in
   `api/sql/v1`, `=`, `<>` and `!=` with `NULL`, written as a literal or
   given as a bound parameter whose value is `null`, match nothing; `IS
   NULL` and `IS NOT NULL` are the only tests for `NULL`. Today FYLO's SQL
   treats `= NULL` like `IS NULL`, so this is a breaking change (CHANGELOG).
   **The Store and SQL now differ on purpose:** the Store follows TACHYON's
   query methods (`eq null` is `isNull`, `ne null` is `isNotNull`), and SQL
   follows SQL. Built in SDK-4.

`api/store/v1` (README and `schema.json`), `api/machine/v2` and
`api/sql/v1` carry these changes.

### Amendment 2 (2026-11-21): SQL that works today keeps working

Mesail's devicebox app sends raw SQL to `fylo://` through TACHYON, and it
works on FYLO today. What a user depends on today must keep working, so
`api/sql/v1` gains three rules. Built in SDK-4, whose tests run Mesail's
statements unchanged.

1. **Dotted field paths.** `<f>` may be a dotted path into nested objects
   (`waiting.<deviceKey> = true`), in `WHERE`, the select list and `ORDER
   BY`, as FYLO's SQL accepts today. The draft allowed top-level names only.
2. **Bound `LIMIT` and `OFFSET`.** Both accept a parameter (TACHYON sends
   `LIMIT ?2`), checked against the same bounds.
3. **Case.** Keywords and the literals `TRUE`, `FALSE` and `NULL` are
   case-insensitive (Mesail writes `true`).

### Amendment 3 (2026-11-21): additions to the frozen v2 contract

Additive; no existing operation, request or result changes.

1. **`verifyRoot` is in v2.** L1-3 added it to v1 (class `read`, retry
   `safe`) after the v2 freeze. It joins `api/machine/v2` (operations and
   schema enum); v2 has 64 operations.
2. **Rule:** every v1 operation that stays after SDK-10 is also in v2.
   `contracts:verify` checks that each v1 operation is in v2 or in
   `keptUntilSdk10`. L1-3b's `createNamespace`, `listNamespaces` and
   `dropNamespace` join v2 the same way when they land (67 operations).
3. **Transactions on the sidecar store.** With ADR 0013 Amendment 2 the
   `efs-sidecar-v1` profile writes its documents through the commit log on
   Unix, so it takes transactions there. `ETXNLAYOUT` remains for a raw-file
   collection and for a sidecar-store root on Windows (`api/store/v1`,
   "Profiles and layouts").

### Amendment (FYLO-TXN2, 2026-11-21, Accepted): one transaction over several collections of one namespace

The user answered "Yes, several": one transaction may write several
collections of the same namespace, all or nothing, in the coordinated
release. TACHYON and Mesail want one commit per user action (for example a
message and its outbox entry), which removes their repair code for
half-saved actions. On a network share each commit costs about 30-230 ms, so
one flush per action matters. This replaces "one collection" in section 5
and the "Transactions across collections" row of the rejected routes.

1. **Scope.** `save`, `patch`, `deleteById` and `delete(query)` on any
   document collections of **one namespace** commit together or not at all.
   Each write in `storeTransaction` may carry its own `collection` (default:
   the frame's `collection`). After `transactionBegin`, a Store operation
   with the handle may name any collection of the begin frame's namespace.
2. **One record, one flush.** A transaction over two or more collections
   commits as one record in the namespace's own log
   (`.fylo-transactions/.namespace/log/segment.log`, the ADR 0013 record
   format, manifest `fylo.log-namespace-commit.v1`) with one flush. Each
   collection then applies its part and gets a copy of it in its own log
   without a flush; a later flush or checkpoint of that log makes the copy
   durable, and a checkpoint records a mark in the namespace log first.
   Recovery applies a part only when the collection's own durable state is
   older than it, and only after every record of the collection's log. A
   transaction over one collection is unchanged (no namespace log).
3. **No deadlock.** The collections are locked in name order, under each
   collection's in-process commit queue and file lock; the namespace log's
   mutex is always taken last.
4. **Limits** stay per transaction, over all its collections: 1,000 writes,
   8 MiB of documents (`ETXNLIMIT`), and the time limit. `ETXNBUSY` covers
   every collection the transaction writes.
5. **Refused** (the transaction rolls back, `error.details.collection`
   names the collection): a collection of another namespace with
   `ETXNSCOPE`; a raw-file collection, a segment collection, or any
   collection whose documents do not go through the commit log
   (`NativeWriteRoot::collection_uses_log` is false) with `ETXNLAYOUT`.
6. **Reads.** There is **no read snapshot across collections**: a reader
   without the handle may see one collection's new state before another's.
   Each collection's readers wait while it is `writing`, as today. The
   collections are applied and become `stable` in the order of each one's
   first write in the transaction, so a pointer written last (an outbox
   entry, a head row) never points at a document a reader finds missing.
   No limit on the number of collections; `createCollection` stays out
   (`ETXNSCOPE`, create before `begin`). Storage design: ADR 0013,
   Amendment "one commit over several collections".
7. **Capability.** The handshake reports
   `capabilities.store.multiCollectionTransactions: true`.

Additive: no existing request changes meaning, except that a second
collection inside a transaction of the same namespace is now accepted
instead of `ETXNSCOPE`. `api/store/v1` and `api/machine/v2` carry it.

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
| Transactions across namespaces | each namespace has its own lease and log (ADR 0033, N9); a cross-namespace commit would need both leases and a second commit point |
