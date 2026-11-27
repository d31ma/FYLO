# BYCKR Machine Protocol v2 (FROZEN)

**Frozen on 2026-11-21 by [ADR 0035](../../../docs/adr/0035-fylos-own-sdk-in-the-store-shape.md).
Not built yet. It replaces [v1](../v1/README.md) in the next BYCKR release:
`fylo exec --loop` and `fylo exec --invocation` then speak v2 only.** TACHYON
and Mesail code against this text. A change needs an amendment to ADR 0035 and
a notice to their Build Coordinators.

The canonical artifacts are:

- [`schema.json`](schema.json): request and response envelope;
- [`operations.json`](operations.json): operation names, mutability and retry
  classification (63 operations);
- [`../../store/v1/README.md`](../../store/v1/README.md) and
  [`schema.json`](../../store/v1/schema.json): the Store operations' fields,
  results, query, pages, transactions and error codes;
- [`../../sql/v1/README.md`](../../sql/v1/README.md): the SQL that
  `executeSQL` accepts;
- [`../../errors/v1.json`](../../errors/v1.json): stable error codes (the new
  codes of `api/store/v1` join it in the slice that first emits them).

## What is the same as v1

Framing (UTF-8, one JSON object per LF-delimited frame), the request and
response size limits (1 MiB and 8 MiB by default, up to 64 MiB), duplicate-key
and truncated-frame handling, `requestId`, the response envelope (`ok`, `op`,
`requestId`, `durationMs`, `result` or `error`), and the rule that clients
ignore unknown result fields. Every response carries `protocolVersion: 2`.

## Handshake

`{ "op": "handshake" }` answers, beside the v1 fields:

- `protocolVersion: 2`;
- `capabilities.store: { "version": 1, "multiCollectionTransactions": true }`
  (this contract and `api/store/v1`; a transaction may span several
  collections of one namespace);
- `capabilities.sql: 1` (`api/sql/v1`);
- the ADR 0033 fields (`storage.layout`, `storage.volume`).

A client that needs the Store checks `protocolVersion == 2` and
`capabilities.store.version >= 1`, and refuses an older `fylo` with its own
error naming the needed version (TACHYON: a TY error). A client that opens a
transaction over several collections checks
`capabilities.store.multiCollectionTransactions == true`.


## Common request fields

Every operation that names a collection also takes the optional `namespace`
(ADR 0033; default `public`, or the `--namespace` of the session). The Store
operations also take `idField` and `transaction` (`api/store/v1`).

## Added operations

| Operation | Request fields (beside `collection`, `namespace`, `idField`, `transaction`) | `result` |
| --------- | --------------------------------------------------------------------------- | -------- |
| `storeFindById` | `id` | entity or `null` |
| `storeFindAll` | `page?` | list of entities |
| `storeSave` | `entity`, `requireExisting?` | the saved entity |
| `storePatch` | `id`, `fields` | the patched entity or `null` |
| `storeDeleteById` | `id` | `0` or `1` |
| `storeFind` | `query?`, `page?` | list of entities |
| `storeCount` | `query?` | integer |
| `storeExists` | `query?` | boolean (`false` for a missing collection) |
| `storeDelete` | `query?` | integer (one log record, all or none) |
| `storeScroll` | `query?`, `page?` (`size`, `cursor`) | `{ content, nextCursor }` |
| `storeTransaction` | `writes` (1 to 1,000) | list of results, in order |
| `transactionBegin` | `timeoutMs?` (default 30,000, max 300,000) | `{ transaction }` |
| `transactionCommit` | `transaction` (no `collection`) | `{ committed: true, writes }` |
| `transactionRollback` | `transaction` (no `collection`) | `{ rolledBack: true }` |
| `collectionExists` | none | boolean; reads the catalog only |
| `listCollections` | none (no `collection`) | sorted list of names |

## Removed operations

A request for one of these answers `EUNSUPPORTEDOP`.

| v1 operation | Use in v2 |
| ------------ | --------- |
| `getDoc` | `storeFindById` |
| `getLatest` | `storeFindById` (an id keeps its id across patches; the answer is the latest version) |
| `putData` | `storeSave` (no `id`: a new document; an `id`: replace or create with that id) |
| `batchPutData` | `storeTransaction` with `save` writes (up to 1,000), or `importBulkData` |
| `patchDoc` | `storePatch` |
| `patchDocs` | `storeTransaction` with `patch` writes, or SQL `UPDATE ... WHERE` |
| `delDoc` | `storeDeleteById` |
| `delDocs` | `storeDelete` |
| `findDocs` | `storeFind`, `storeCount`, `storeExists`, `storeScroll` (word search: the `match` operator; OR: `anyOf`) |

Result shapes change with the names: v1 answered `{ "<id>": { ...document } }`
maps; v2 answers entities with the id under `idField`.

## Changed operation

- `executeSQL { sql, parameters? }`: `parameters` is new (a list for `?` and
  `?NNN`, an object for `:name` and `$name`; at most 64 scalars). The accepted
  SQL is `api/sql/v1`. Callers must stop rendering values into the SQL text.

## Unchanged operations

Every other v1 operation keeps its v1 request and result shape:
`handshake` (plus the fields above), `executeSQL` (plus `parameters`),
`createCollection`, `dropCollection`, `createNamespace`, `dropNamespace`,
`listNamespaces`, `inspectCollection`,
`rebuildCollection`, `setWordFields`, `reshardCollection`, `migrateLayout`,
`compactCollection`, `verifyCollection`, `getFileData`, `getMeta`, `setMeta`,
`findDeletedDocs`, `changes`, `restoreDoc`, `joinDocs`, `importBulkData`,
`checkout`, `branch`, `commit`, `log`, `status`, `diff`, `restoreCommit`,
`merge`, the `schema*` operations, the `queue*` operations,
`inspectDatabase`, `verifyDatabase`.

`inspectCollection` stays an inspection tool (counts, index state). To ask
"does this collection exist", use `collectionExists`.

## Retry classes

As in v1 (`operations.json`). For the new operations: reads are `safe`;
`storeSave`, `storePatch` and `storeDeleteById` are `conditional` (safe when
the request names the id); `storeDelete`, `storeTransaction`,
`transactionBegin` and `transactionCommit` are `unsafe`;
`transactionRollback` is `safe`.

## Compatibility within v2

- Operation names, request fields and stable error codes are kept.
- Additive result fields and additive optional request fields are allowed;
  clients ignore unknown result fields.
- A change to idempotency, durability, retry behavior, sort or null order, or
  a page limit needs a new protocol version or an amendment to ADR 0035.
