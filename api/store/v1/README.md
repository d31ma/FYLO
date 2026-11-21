# FYLO Store Contract v1 (FROZEN)

**Frozen on 2026-11-21 by [ADR 0035](../../../docs/adr/0035-fylos-own-sdk-in-the-store-shape.md).
Not built yet; slices SDK-2 and SDK-3 build it, and it ships in the next FYLO
release.** TACHYON and Mesail code against this text. A change needs an
amendment to ADR 0035 and a notice to their Build Coordinators.
Amendment 1 (2026-11-21, ADR 0035): `ne` never matches `null` or a missing
field, and `storeSave` takes `requireExisting`.

The Store is FYLO's document API in the shape of TACHYON's `Store`: a
Repository base and queries with AND-joined predicates, an order and a page.
The machine operations that carry it are in
[`api/machine/v2`](../../machine/v2/README.md). The JSON shapes are in
[`schema.json`](schema.json).

## Names

| Concept | Machine operation (v2) | Rust | JS / TS | Python, Ruby |
| ------- | ---------------------- | ---- | ------- | ------------ |
| Find one by id | `storeFindById` | `find_by_id` | `findById` | `find_by_id` |
| First or next page, ordered by id | `storeFindAll` | `find_all` | `findAll` | `find_all` |
| Insert or replace | `storeSave` | `save` | `save` | `save` |
| Change some fields | `storePatch` | `patch` | `patch` | `patch` |
| Delete one by id | `storeDeleteById` | `delete_by_id` | `deleteById` | `delete_by_id` |
| Find by query | `storeFind` | `find` | `find` | `find` |
| Count by query | `storeCount` | `count` | `count` | `count` |
| Exists by query | `storeExists` | `exists` | `exists` | `exists` |
| Delete by query | `storeDelete` | `delete` | `delete` | `delete` |
| Cursor pages | `storeScroll` | `scroll` | `scroll` | `scroll` |
| Batch transaction | `storeTransaction` | `transaction` | `transaction` | `transaction` |
| Begin / commit / rollback | `transactionBegin`, `transactionCommit`, `transactionRollback` | `begin`, `commit`, `rollback` | same | same |
| Collection exists | `collectionExists` | `Collections::exists` | `collections.exists` | `collections.exists` |
| List collections | `listCollections` | `Collections::list` | `collections.list` | `collections.list` |

Other languages use the same words in their own style.

## Common request fields

Every Store operation takes:

| Field | Type | Required | Meaning |
| ----- | ---- | -------- | ------- |
| `collection` | string | yes (not for `listCollections`) | FYLO collection name rules |
| `namespace` | string | no, default `public` | ADR 0033 namespace |
| `idField` | string | no, default `"id"` | the field of the entity that holds the document id, in requests and results |
| `transaction` | string | no | a handle from `transactionBegin` (section "Transactions") |

## Documents and ids

- A result **entity** is the document's JSON object with its id added under
  `idField`. The id is FYLO's TTID string.
- The stored document never contains the `idField` key; FYLO removes it on
  write and adds it on read.
- **`save(entity)`**: when `entity[idField]` is missing or `null`, FYLO
  creates a new document with a new TTID. When it names an id that exists,
  FYLO replaces the whole document (same id). When it names a valid TTID that
  does not exist, FYLO creates the document with that id. An id that is not a
  valid TTID fails with `EINVALIDDOCID`. The answer is the saved entity.
- **`save(entity, { requireExisting: true })`** (machine field
  `requireExisting`, default `false`; Amendment 1): the save only replaces.
  When `entity[idField]` is missing, `null` or names an id that does not
  exist, it fails with `ENATIVE_NOT_FOUND`, writes nothing, and the message
  names the collection and the id. This is TACHYON's update contract (TACHYON
  ADR 0032).
- **`patch(id, fields)`**: a shallow merge of top-level fields into the
  existing document (same id); a field set to `null` is stored as `null`.
  The answer is the patched entity, or `null` when the id does not exist.
- **`findById(id)`**: the entity, or `null` when the id does not exist (not
  an error).
- **`deleteById(id)`**: the number deleted, `0` or `1`.
- Writes keep every check FYLO makes today: schemas (`_v` stamp), encrypted
  fields, word fields, identities and policies, the drive rule, the lease.

## Queries

```json
{
  "where": [
    { "field": "status", "operator": "eq", "value": "open" },
    { "field": "priority", "operator": "gte", "value": 3 }
  ],
  "orderBy": [{ "field": "priority", "direction": "desc" }]
}
```

- `where`: a list of predicates, **joined by AND**. Empty or missing means
  every document.
- `field`: a top-level field name, or the `idField`. Nested paths: not in v1.
- `operator`:

| Operator | `value` | Matches when |
| -------- | ------- | ------------ |
| `eq` | a scalar | equal (see "Equality") |
| `ne` | a scalar | the field is present, not `null`, and not equal (as SQL `<>`); a `null` or missing field never matches. Use `isNull` and `isNotNull` to ask for `null` |
| `lt`, `lte`, `gt`, `gte` | a number or a string | ordered compare within the same type; other types never match |
| `in` | a list of 1 to 100 scalars | equal to one of them |
| `isNull` | none | the field is `null` or missing |
| `isNotNull` | none | the field is present and not `null` |

- **FYLO extensions** (TACHYON does not use them): `like` (string pattern
  with `%` and `_`, as SQL `LIKE`, case-sensitive) and `match` (word search on
  word fields, ADR 0023; `value` is the search text). A top-level `anyOf`
  (a list of up to 8 predicate lists, OR between lists, AND inside) gives OR.
- A **scalar** is a string, a finite number, a boolean or `null`.
- `orderBy`: up to 4 `{ field, direction }` items, `direction` `asc` (default)
  or `desc`. FYLO always adds the id ascending as the last tie-breaker.

### Equality

- Numbers compare by numeric value (`1` equals `1.0`).
- Strings compare by exact Unicode code points: no case folding, no locale,
  no normalization.
- A value never equals a value of another type (`"1"` is not `1`).
- `ne` is not the opposite of `eq` for `null`: `ne X` leaves out a `null` or
  missing field, as SQL `<> X` does (Amendment 1). `ne null` matches like
  `isNotNull`, just as `eq null` matches like `isNull`.
- `eq null` matches `null` and a missing field, like `isNull`.

### Sort order

- Stable. Equal keys keep id order (ascending TTID, which is creation order).
- Across types, ascending: missing and `null` first, then booleans (`false`
  before `true`), then numbers, then strings. Descending reverses this, so
  `null` comes last.
- Objects and arrays cannot be sort keys: `EQUERY_INVALID`.

## Pages

- **Offset pages** (`storeFind`, `storeFindAll`): `page` is
  `{ "size": n, "offset": k }`. Default size 20. Maximum size 100, or the
  call's `maxSize` when given, never above 1,000. There is no bound on
  `offset`. A size above the maximum fails with `EPAGESIZE`.
- **Cursor pages** (`storeScroll`): `{ "size": n, "cursor": "<opaque>" }`.
  The first call has no cursor. The answer is
  `{ "content": [...], "nextCursor": "<opaque>" | null }`; `null` means the
  end. A cursor is tied to its collection, namespace, query and order; any
  other use fails with `ECURSOR`. Writes between pages may be seen or not;
  no entity appears twice and none is skipped that existed for the whole scan.
- `findAll(page)` is `find` with an empty `where`, ordered by id.

## Results

| Operation | `result` |
| --------- | -------- |
| `storeFindById` | an entity or `null` |
| `storeFindAll`, `storeFind` | a list of entities, at most the page size |
| `storeScroll` | `{ content, nextCursor }` |
| `storeSave` | the saved entity |
| `storePatch` | the patched entity or `null` |
| `storeDeleteById` | `0` or `1` |
| `storeCount` | a non-negative integer (exact at the moment of the call) |
| `storeExists` | `true` or `false` |
| `storeDelete` | the number deleted (all in one log record, all or none) |
| `collectionExists` | `true` or `false` (reads the catalog only) |
| `listCollections` | a list of collection names in the namespace, sorted |

## Limits

| Limit | Value | Error |
| ----- | ----- | ----- |
| Predicates in `where` (all `anyOf` lists counted) | 8 | `EQUERYLIMIT` |
| `orderBy` items | 4 | `EQUERYLIMIT` |
| Values in one `in` | 100 | `EQUERYLIMIT` |
| `anyOf` lists | 8 | `EQUERYLIMIT` |
| Query JSON size | 64 KiB | `EQUERY_INPUT_SIZE` |
| Page size | 20 default, 100 max (caller `maxSize` up to 1,000) | `EPAGESIZE` |
| Response size | the session's `--max-response-bytes` | `EFRAME_RESPONSE_TOO_LARGE`; never a cut list |

## Transactions

One collection only (ADR 0035 section 5).

- **Batch:** `storeTransaction { collection, writes }` where each write is one
  of `{ "save": entity }`, `{ "patch": { "id", "fields" } }`,
  `{ "deleteById": id }`, `{ "delete": query }`. The answer is the list of
  results in order (as the single operations answer). All writes commit as one
  log record, or none does. A failing write answers its own error code, with
  `error.details.index` set to its position; nothing is written.
- **Begin / commit / rollback:** `transactionBegin { collection, timeoutMs? }`
  answers `{ "transaction": "<handle>" }`. Every Store operation then takes
  `transaction: "<handle>"`. `transactionCommit { transaction }` answers
  `{ "committed": true, "writes": n }`; `transactionRollback { transaction }`
  answers `{ "rolledBack": true }`.
- **Handle:** an opaque string, valid only in the `fylo` process that gave
  it. After that process ends, or for any other string: `ETXNUNKNOWN`.
- **Time limit:** `timeoutMs` from begin, default 30,000, maximum 300,000
  (a FYLO setting may lower the maximum). After it the transaction is rolled
  back; the next call with its handle answers `ETXNEXPIRED`. TACHYON passes
  the rest of its own store-time budget, so a transaction ends before TACHYON
  stops `fylo`.
- **Size limits:** 1,000 writes, 8 MiB of documents: `ETXNLIMIT`, and the
  transaction rolls back.
- **Reads** with the handle see the transaction's writes; reads without it
  see committed state only.
- **Other writes** to the same collection in the same process wait for the
  transaction up to its time limit, then fail with `ETXNBUSY`. Writes to
  other collections do not wait.
- **A process that ends before commit** (exit, kill, crash) wrote nothing.
  The next open needs no recovery for it. A crash during commit leaves the
  state before or after, never a mix.
- **Refused inside a transaction** with `ETXNSCOPE` (and the transaction
  rolls back): a write to another collection; queue operations; schema
  operations; `createCollection`, `dropCollection`.
- **Layouts:** files layout and `.vdbx`. The segments layout answers
  `ETXNLAYOUT` if it cannot write the batch as one record (decided in SDK-3).

## Error codes

Existing codes keep their meaning. Codes marked "new" enter
`api/errors/v1.json` in the slice that first emits them.

| Code | When | Retry |
| ---- | ---- | ----- |
| `ENATIVE_NOT_FOUND` | the collection does not exist (every operation except `collectionExists` and `storeExists`, which answer `false`) | no |
| `ESYSTEMDISK` | the root is on the system disk (ADR 0032) | no |
| `EBADREQUEST` | a request field has the wrong type or is missing | no |
| `EUNSUPPORTEDOP` | a v1 operation removed in v2 (`getDoc`, `putData`, ...) | no |
| `EINVALIDDOCID` | an id is not a valid TTID | no |
| `EDOCEXISTS` | not used by `storeSave` (it replaces); kept for `importBulkData` | no |
| `EQUERY_INVALID` | a query names an unknown operator or field shape, a bad value type, or an object or array as a sort key | no |
| `EQUERY_INPUT_SIZE` | the query JSON is above 64 KiB | no |
| `EFRAME_RESPONSE_TOO_LARGE` | the answer is above the response limit | no |
| `EQUERYLIMIT` (new) | too many predicates, sort fields, `in` values or `anyOf` lists | no |
| `EPAGESIZE` (new) | a page size above its maximum, or below 1 | no |
| `ECURSOR` (new) | a cursor that is malformed or belongs to another query | no |
| `ETXNSCOPE` (new) | another collection, a queue, schema or collection operation inside a transaction | no |
| `ETXNEXPIRED` (new) | the transaction's time limit passed | no (begin again) |
| `ETXNUNKNOWN` (new) | an unknown handle, or one from an ended process | no (begin again) |
| `ETXNBUSY` (new) | a write waited for another transaction on its collection past the limit | yes |
| `ETXNLIMIT` (new) | a transaction passes 1,000 writes or 8 MiB | no |
| `ETXNLAYOUT` (new) | the collection's layout cannot hold a transaction | no |

`storeExists` on a missing collection answers `false`, not an error, so a
caller can check without a separate `collectionExists` call.
