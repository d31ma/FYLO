# FYLO SQL v1 (FROZEN)

**Frozen on 2026-11-21 by [ADR 0035](../../../docs/adr/0035-fylos-own-sdk-in-the-store-shape.md).
Not built yet; slice SDK-4 builds it, and it ships in the next FYLO release.**
TACHYON's documentation points here for "the SQL subset of FYLO". A change
needs an amendment to ADR 0035 and a notice to TACHYON's Build Coordinator.

FYLO's SQL is FYLO's own small dialect. It is a front door to the
[Store plan](../../store/v1/README.md): the same filters, order, pages, limits,
results and error codes. It runs in `fylo sql`, the machine operation
`executeSQL` ([`api/machine/v2`](../../machine/v2/README.md)), the JavaScript
`sql` tag and `Database::sql` in the Rust crate. There is no other SQL engine
in FYLO (Turso was removed by ADR 0035).

## Calls

- One statement per call. A `;` that ends the text is allowed; a second
  statement fails with `EUNSUPPORTEDSQL`.
- `executeSQL { "sql": "<text>", "parameters": [...] | {...}, "namespace"? }`.
  `parameters` is optional. It is a list for `?` and `?NNN` (1-based), or an
  object for `:name` and `$name`; not both kinds in one statement.
- At most 64 parameters. A parameter is a scalar: string, finite number,
  boolean or `null`.
- A parameter is always a value, never a name or SQL text.
- Keywords, and the literals `TRUE`, `FALSE` and `NULL`, are
  case-insensitive (`true`, `Select`, `null` are the same as `TRUE`,
  `SELECT`, `NULL`). Collection and field names are case-sensitive.
- The handshake answers `capabilities.sql: 1` when this contract is built.

## Statements

| Statement | Clauses | `result` |
| --------- | ------- | -------- |
| `CREATE TABLE <c>` | none | `true` (created) or `false` (existed) |
| `DROP TABLE <c>` | none | `true` |
| `INSERT INTO <c> (<f>, ...) VALUES (<v>, ...)[, (...)]` | none | the new id (one row) or the list of new ids |
| `SELECT * \| <f>, ... FROM <c>` | `WHERE`, `ORDER BY`, `LIMIT`, `OFFSET` | a list of rows, each with `id` |
| `SELECT COUNT(*) FROM <c>` | `WHERE` | a non-negative integer |
| `UPDATE <c> SET <f> = <v>, ...` | `WHERE` | the number of rows changed |
| `DELETE FROM <c>` | `WHERE` | the number of rows deleted |
| `EXPLAIN <statement>` | | the plan, not executed |
| `EXPLAIN ANALYZE <statement>` | | the plan, executed, with `elapsedMs` and `result` |

Kept from FYLO's dialect as today (ADR 0035, Q2b): `JOIN` (`INNER`, `LEFT`,
`RIGHT`, `OUTER`, `ON`), `GROUP BY`, `OR` and `LIKE`. They behave exactly as
in FYLO 26.45.07; they are not part of the Store plan.

- `<c>` is a collection, or `<namespace>.<collection>` (ADR 0033). Without a
  namespace, the request's `namespace`, else `public`.
- `<f>` is a field name, or a dotted path into nested objects such as
  `waiting.device42` (ADR 0035, Amendment 2; FYLO's SQL accepts this today).
  A path walks objects only: a step that is missing, or that is not an
  object, makes the value missing. A dotted path may appear in `WHERE`, in
  the select list and in `ORDER BY`. `id` is the document id. (The Store's
  queries name top-level fields only.)
- `UPDATE` and `DELETE` with a `WHERE` change every matching row in one log
  record, all or none (as today). Without `WHERE` they change every row.

## Expressions in `WHERE`

- Comparisons: `=`, `!=`, `<>`, `<`, `<=`, `>`, `>=`.
- `<f> IN (<v>, ...)`, at most 100 values.
- `<f> IS NULL`, `<f> IS NOT NULL`.
- `<f> LIKE <pattern>` (`%` and `_`, case-sensitive).
- `AND`; `OR` and parentheses as today.
- Values: string literals in single quotes (`''` for a quote), numbers,
  `TRUE`, `FALSE`, `NULL`, or a parameter.
- The meaning of every comparison and of the sort order is the Store's
  ("Equality" and "Sort order" in `api/store/v1`), except for `NULL`, which
  follows SQL (ADR 0035, Amendment 1):
  - `=`, `<>` and `!=` with `NULL` match no row, whether `NULL` is written
    as a literal or given as a bound parameter whose value is `null`.
    `IS NULL` and `IS NOT NULL` are the only tests for `NULL`.
  - `<f> <> v` and `<f> != v` leave out rows where `<f>` is `NULL` or
    missing, as in standard SQL.
  - This differs from the Store on purpose: there `eq null` is `isNull` and
    `ne null` is `isNotNull`, as in TACHYON's query methods.

## `ORDER BY`, `LIMIT`, `OFFSET`

- `ORDER BY <f> [ASC | DESC], ...`, up to 4 fields; `id` ascending is always
  added as the last tie-breaker.
- `LIMIT n`: 1 to 1,000 (default: all matching rows, up to the response
  limit). `OFFSET k`: 0 or more.
- `n` and `k` may be bound parameters (`LIMIT ?2 OFFSET ?3`, `LIMIT :size`;
  Amendment 2). The value must be a non-negative integer within the same
  bounds; any other value fails with `EUNSUPPORTEDSQL`.

## Types

String, number (finite; integers within +/- 9,007,199,254,740,991), boolean
and `NULL`. Dates are strings. There are no blob, decimal or date types.

## Not in v1

Sub-queries, `HAVING`, functions other than `COUNT(*)`, `BEGIN`/`COMMIT`
(transactions are Store operations), `ALTER`, `CREATE INDEX`, views, more
than one statement. Each fails with `EUNSUPPORTEDSQL`, and the message names
the clause.

## Error codes

| Code | When |
| ---- | ---- |
| `EUNSUPPORTEDSQL` (new) | a statement or clause outside this contract |
| `EQUERY_INVALID` | a malformed statement, an unknown parameter name, a missing parameter, or a parameter that is not a scalar |
| `EQUERYLIMIT` (new) | more than 100 `IN` values, more than 4 `ORDER BY` fields, or more than 64 parameters |
| `ENATIVE_NOT_FOUND` | the collection does not exist (`CREATE TABLE` excepted) |
| every Store write error | the same as the matching Store operation |
