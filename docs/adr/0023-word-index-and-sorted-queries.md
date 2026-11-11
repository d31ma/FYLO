# ADR 0023: Word indexes (`$match`) and sorted, cursor-paged queries (`$sort`, `$after`)

- Status: **Accepted** (the user's choices, through the Build Coordinator, 2026-11-11)
- Date: **2026-11-11**
- Owners: **FYLO maintainers**
- Request: Mesail (CADUCEUS) feature request F3, "Full-text search and
  ordered, limited queries on an indexed field".
- Builds on: the prefix index (`README.md`, "Index keys"), ADR 0008 (the
  portable WebAssembly engine), ADR 0013 (index write-ahead log), ADR 0016
  (`.vdbx` databases) and ADR 0017 (the encrypted-fields record in the
  collection descriptor, which this ADR copies for word fields).

## Context

Mesail keeps messages and mail in FYLO on its servers and, later, on each
device. It needs two queries that FYLO cannot answer well today:

1. **Search by word prefix** in a body, subject, sender or file name: the
   user types `meet tom` and expects messages with "meeting" and "Tomás".
   FYLO has no full-text search. The prefix index writes `f`, `r` and `g3`
   (trigram) keys only for strings of at most 180 encoded bytes, so a message
   body gets only a hashed `eq` key, and `$like '%meet%'` reads every
   document. Turso's FTS is off (ADR 0019).
2. **"The newest 50 messages of a conversation"**, then the next 50. FYLO has
   no ORDER BY. `findDocs` returns documents in TTID order. The machine
   protocol's `page` option keeps a whole result set in memory per cursor and
   needs `exec --loop`, so it is not available to one-shot calls or the
   browser's WebAssembly engine. The `sql` feature sorts in memory and is
   native only.

What already exists and what this ADR relies on:

- Every field of every document is indexed. A value that JavaScript's
  `Number()` turns into a finite number (numbers, numeric strings, booleans,
  `null`) gets an `n` key (sortable, ascending) and an `nr` key (its
  complement, so ascending `nr` order is descending numeric order). Keys are
  `<field>/<kind>/<value>/<id>`, sorted bytewise in `keys.snapshot` plus
  `keys.wal`.
- Planned queries read only the key ranges they need
  (`index_snapshot_in_ranges`), and the planner intersects fields within an
  `$ops` entry and unions entries.
- The collection descriptor already carries an engine-wide setting the
  storage layer must see without the schema: the encrypted-fields record
  (ADR 0017 section 3).
- The browser has two engines: the Rust engine compiled to
  `fylo-browser.wasm` (`.vdbx` files and Rust-engine OPFS roots), which runs
  the same storage and query code as the native engine, and the older
  JavaScript engine (`BrowserCore`, the default of `createBrowserClient` and
  of the mobile bridge), which reimplements the index in
  `src/browser/core/prefix-index.js`.

## Decision

### 1. Word fields are declared per collection

A collection has an ordered, possibly empty list of **word fields**: field
paths (slash- or dot-separated, as in `$ops`) whose string values get word
keys. The list is stored in the collection descriptor as `"words": [...]`,
beside `"encrypted"`: in `.fylo-catalog/collections/<name>.json` for a
directory root, and in the static part of the collection's directory block
for a `.vdbx` database, rewritten in place exactly as the encrypted-fields
record is. `fylo pack` and `unpack` carry it.

It is set, replaced or cleared by one operation:

| Surface | Form |
| --- | --- |
| Machine protocol | `{"op":"setWordFields","collection":"messages","fields":["body","subject"]}` → `{"collection":"messages","fields":[...],"rebuilt":true}` |
| CLI | `fylo index words <collection> [<field>...]` (no fields clears the list) |
| Inspection | `inspectCollection` reports `wordFields` |

`setWordFields` takes the collection's writer lock, rebuilds the collection's
index with the new list, then writes the descriptor, and only then releases
the lock. No write can interleave, so the index is complete for every listed
field from the moment the descriptor names it. A crash after the rebuild and
before the descriptor leaves extra `w` keys that no query reads and the next
rebuild drops. Clearing the list rebuilds too, which removes the keys.

Rules, checked by `setWordFields` with `EQUERY_INVALID`:

- at most 16 fields, each at most 256 bytes, no duplicates;
- a field in the encrypted-fields record (or the head schema's `$encrypted`)
  is refused: words of a secret would be stored in plain text, and a keyed
  blind index cannot answer a prefix;
- the collection must be a document collection.

Every index writer (the file layout's delta and full rebuild, the segment and
`.vdbx` commit path, recovery's rebuild) reads the list from the descriptor
and passes it to the one key builder, `index_entries_for_document`, so all
paths stay in step. The default list is empty, so nothing changes for a
collection that does not opt in.

**Why the descriptor, not a schema directive (`$words`).** Word indexing is a
property of the index, not of document validity. Many collections have no
schema registry, and the storage layer, which writes the index, cannot see the
schema (the reason ADR 0017 added the encrypted-fields record). A directive
would need a registry version, a schema push and a descriptor copy anyway.

### 2. Normalization

One function turns a string into words. The native engine, the browser
WebAssembly engine (the same Rust code) and the JavaScript engine implement
it identically; a shared fixture (`tests/fixtures/word-normalization-v1.json`)
is checked by both the Rust and the JavaScript test suites.

1. Read the string as Unicode code points.
2. A **word character** is a code point that is Unicode `Alphabetic` or has
   general category `N` (Rust `char::is_alphanumeric`; JavaScript
   `/[\p{Alphabetic}\p{N}]/u`). Combining diacritical marks U+0300 to U+036F
   are part of a word but are dropped. Every other code point ends a word.
3. Lower-case each code point on its own with its full Unicode lower-case
   mapping (Rust `char::to_lowercase`; JavaScript `toLowerCase()` on the
   single code point, which has no context, so `Σ` always becomes `σ`).
   Final `ς` becomes `σ`.
4. Fold Latin diacritics with a fixed table covering U+00C0 to U+017F (Latin-1
   Supplement and Latin Extended-A): `é → e`, `ñ → n`, `ß → ss`, `æ → ae`,
   `œ → oe`, `ø → o`, `ł → l`, `đ → d`, `þ → th`, `ı → i`. Other scripts are
   only lower-cased.
5. A word longer than 64 UTF-8 bytes is cut to its longest prefix of whole
   code points that fits in 64 bytes.
6. Empty words are dropped. Words of one character are kept.

Limits accepted for this version: no stemming, no stop words, no language
detection, and no segmentation of scripts written without spaces (a run of
Chinese or Japanese characters is one word, so it matches only by its leading
characters). The Unicode version is whatever the Rust standard library and
the browser ship; a code point whose properties differ between them can index
differently in the two engines, which a rebuild by either engine repairs.

### 3. Index keys

For each word field and each document, every string value at that path (a
string, or each string item of an array of scalars) is normalized, and each
**distinct** word becomes one key:

```text
<encoded field path>/w/<encodeURIComponent(word)>/<encodeURIComponent(id)>
body/w/meeting/4UUB32VGUDW
body/w/tom%C3%A1s/4UUB32VGUDW   (before folding; after folding it is "tomas")
```

Non-string values at a word field get no `w` keys. Every value of a word
field, string or not, also gets one word witness key (`wv`, section 8). The keys are zero-payload
and rebuilt from documents like every other kind. The words of a field share
the index's 256 MiB cap (`EINDEX_FULL`): a 2 KB message body adds about 150
to 300 keys of 30 to 50 bytes, roughly 6 to 12 KB of index per document, so a
collection of message bodies reaches the cap at roughly 20,000 to 40,000
messages. Index only the fields you search, and split large mailboxes over
several collections until the index redesign (`docs/index-redesign.md`)
removes the cap.

Because a word contains only word characters, `/` never appears in it, and
percent-encoding is per byte, the encoded form of a word's prefix is a prefix
of the encoded word. A prefix search is therefore one key range.

### 4. `$match`

```json
{ "$ops": [ { "conversation": { "$eq": "c-42" }, "body": { "$match": "meet tom" } } ] }
```

- The operand is a string. It is normalized like the document (section 2)
  into **terms**. A document matches when **every** term is a prefix of at
  least one word of the field. `"meet tom"` matches "Meeting with Tomás".
- An operand that yields no terms (empty, or only punctuation), more than 16
  terms, or that is not a string is refused when the query is parsed, with the
  stable code `EQUERY_INVALID` (`QueryErrorCode::InvalidQuery`).
- **Verification** works on the document, not the index, so `$match` answers
  correctly on every field, declared or not, in every engine: an undeclared
  field is answered by reading every document, like `$like` today.
- **Planning**: when the field is a declared word field, each term becomes
  the key range `<field>/w/<term>` (a prefix, so `meet` covers `meeting`).
  Terms of one field are intersected, and the field is intersected with the
  other fields of the same `$ops` entry, as `$eq` already is. Candidates are
  then read and verified against the whole query.
- `$match` on an encrypted field is never planned from the index (it has no
  word keys); it is answered by reading every document, like an undeclared
  field.
- There is no ranking (BM25 or other). Results come in the query's order:
  TTID order, or the `$sort` order below.

### 5. `$sort`, `$limit` and `$after`

```json
{
  "$ops": [ { "conversation": { "$eq": "c-42" } } ],
  "$sort": { "sentAt": "desc" },
  "$limit": 50,
  "$after": "fys1.d.c278a0f4e3c00000.4UUB32VGUDW.9f2c1d7e0a4b5c66"
}
```

- `$sort` is an object with exactly one field path and `"asc"` or `"desc"`.
- **Order** is the field's numeric value under JavaScript `Number()`
  coercion, the same value the `n` keys hold: numbers, numeric strings
  (`"42"`, ISO dates are not numeric), booleans and `null`. Mesail's
  timestamps are epoch milliseconds, which fits. Ties are broken by document
  id, ascending, in both directions. **A document whose field has no numeric
  value (missing, a non-numeric string, an empty array) is not returned by a
  sorted query.** This is stated in the docs; a query that needs those
  documents runs without `$sort`.
- An array field sorts by its smallest value (`asc`) or largest value
  (`desc`); the document appears once.
- `$sort` on an encrypted field is refused with `EENGINE_QUERY`. A `$sort`
  that is not an object with one field and `"asc"` or `"desc"` is refused
  with `EQUERY_INVALID`.
- **Execution** (no in-memory sort of the collection):
  1. Plan the `$ops` filter as today. When it is plannable, the candidates
     are a set of ids from the index; otherwise there is no candidate set.
  2. Read the key range `<field>/n/` for `asc` or `<field>/nr/` for `desc`
     (ascending key order is the requested order), starting after the
     cursor's key when `$after` is given.
  3. Walk the keys in order. Skip an id outside the candidate set, an id
     already seen in this call, and an id whose key is not the document's
     placing value (array rule above). Read and verify the remaining
     documents in batches against the whole query, and stop after `$limit`
     results plus one (to know whether a next page exists).
  The walk reads index keys, not documents, for every document it skips, and
  reads documents only for ids that pass the candidate set. Its cost is at
  most the sort field's keys (bounded by the 256 MiB index cap).
- `$limit` keeps its meaning (0 or absent means no limit). A sorted query
  without `$limit` returns every match in order.
- **Result shape.** A query with `$sort` returns an ordered array and the
  next cursor, because JSON objects do not keep order in every client
  language:

  ```json
  { "items": [ { "id": "4UUB32VGUDW", "doc": { "...": "..." } } ],
    "nextCursor": "fys1.d.c278a0f4e3c00000.4UUB32VGUDW.9f2c1d7e0a4b5c66" }
  ```

  With `$onlyIds: true`, `items` is an array of id strings. `nextCursor` is
  `null` on the last page. A query without `$sort` keeps today's shape
  exactly.
- `$sort` together with the machine request's `page` option is refused with
  `EBADREQUEST`: they are two paging models.

### 6. The cursor

`$after` takes the `nextCursor` of the previous page. It is **stateless**: the
engine keeps nothing between calls, so it works for one-shot processes, the
loop, the `fylo` crate and both browser engines, and never expires.

```text
fys1.<a|d>.<16 hex digits: sortable value>.<encodeURIComponent(id)>.<16 hex digits: query digest>
```

- The value and id are those of the last returned document's placing key, so
  the next page starts strictly after that key.
- The query digest is the first 8 bytes (hex) of SHA-256 over the canonical
  JSON of `{"$ops", "$created", "$updated", "$sort"}` as sent. A cursor used
  with a different filter, sort field or direction is refused with
  `EINVALIDCURSOR`, the code the `page` cursors already use. So is a cursor
  that does not parse.
- **Consistency under writes.** Pages never repeat a document whose sort
  value did not change, and never skip one that existed, unchanged, before
  the first page. A document inserted, deleted or re-valued between pages
  appears on a later page if its new key sorts after the cursor, and not
  otherwise. This is keyset paging, as in SQL `WHERE (v, id) > (?, ?)`.
- The cursor reveals the last document's id and sort value, which the caller
  already received; it carries no secrets.

### 7. The JavaScript browser engine

`BrowserCore` gets the same features so `createBrowserClient` and the mobile
bridge behave like the Rust engine:

- `w` keys from the same normalization (section 2), written by
  `prefix-index.js`;
- the word-field list in the collection's `index/manifest.json`
  (`{"format":"fylo.local-fs.index.v1","words":[...]}`), set by
  `setWordFields(collection, fields)`, which rebuilds the collection's index
  first, as in section 1. The JavaScript engine has no collection descriptor,
  and this file is the one it already owns;
- `$match` in `matchesOperand` and the planner, and `$sort`/`$limit`/`$after`
  with the same cursor format and result shape (`findDocsSorted`, and
  `findDocs` returning `{ items, nextCursor }` from `collect()` when `$sort`
  is present).

ADR 0008 keeps the JavaScript engine until `fylo-wasm` replaces it; this ADR
does not change that plan.

### 8. Compatibility and migration of existing roots

- **No format change and no migration step.** Existing roots open as they
  are. `$sort` works at once, because every root already has `n` and `nr`
  keys. `$match` works at once on any field by reading documents, and from the
  index after `setWordFields` (one rebuild of that collection).
- **Older readers do not refuse the new query fields.** The structured query
  of releases up to 26.45.x ignores unknown operators and fields, so an older
  binary given `$match`, `$sort` or `$after` returns unfiltered, unsorted
  results. Clients are version-matched with their binary, so this needs a
  mismatched deployment; the release notes say so.
- **Older writers: detect and rebuild.** A release that predates this ADR
  ignores the descriptor's `words` field, so its writes add no `w` keys. It
  must never make `$match` silently incomplete, and it must not be refused
  (no new collection-generation format, which would make every older release
  refuse the whole collection). So the index carries a **witness** that older
  releases already keep up to date without knowing about words:
  - Every release writes, for every value of every field, an `eq` key
    `<field>/eq/<token>/<id>`, and removes the old ones on update and delete.
  - For each word field, this release also writes one **word witness** key
    per `eq` key of that field: `<field>/wv/<token>/<id>`, with the same token
    and id. It is written and removed in the same index delta as the field's
    `w` keys, so it says "the `w` keys of this value of this document are
    present".
  - The word index of field F is **complete** exactly when the `(token, id)`
    pairs under `F/eq/` equal those under `F/wv/`. An older release that
    inserts a document, changes F, deletes a document or rebuilds the index
    changes the `eq` keys and not the witnesses, so the two ranges differ. An
    older write that does not touch F leaves both equal, and then F's `w`
    keys are still right, so nothing needs repair.
  - **Before** a `$match` on F is planned from the index, the engine compares
    the two ranges (an index-only read of two key ranges, no documents). The
    answer is cached per collection and generation (every writer of every
    release bumps the generation), so the check runs once after each change,
    not once per query.
  - On a difference, an engine that can write rebuilds the collection's index
    (as `rebuildCollection`, under the writer lock) and then answers from it.
    An engine opened read-only answers that `$match` by reading every
    document. Either way the result is complete.
  - `$sort` reads only `n` and `nr` keys, which every release maintains, so a
    sorted query needs no check unless it also has a `$match`.
  - The witness costs one key per field value, small next to the words.
  The tests simulate an older writer by hiding the descriptor's `words` field
  from one write, then check that `$match` still finds that document.
- `rebuildCollection` and recovery rebuild `w` and `wv` keys from the
  descriptor's list, so a damaged or dropped index comes back complete.
- An older release that rewrites the collection descriptor keeps unknown
  fields in a `.vdbx` database. If a descriptor ever loses `words`, `$match`
  falls back to reading every document, which is slow but complete; running
  `setWordFields` again restores the index.

### 9. Size budget of the browser engine

The Rust code (normalization with its fold table, `$match`, the sorted walk,
the cursor and `setWordFields`) also lands in `fylo-browser.wasm`, whose raw
size was about 1 KiB under its 2,452 KiB budget on 2026-11-08. No dependency
is added (Unicode properties and lower-case tables are already in the
module through `char` methods). The slices measure the module after each
change; if it does not fit, the change trims other code first, and only
raises the budget with a stated reason in the CHANGELOG, as earlier releases
did.

## Consequences

- Mesail's two queries are served from the index in the native engine, in
  `.vdbx` and OPFS roots in the browser's WebAssembly engine, and in the
  JavaScript engine, with one query syntax and one cursor format.
- Word fields cost index space (section 3) and one rebuild to enable.
- Sorted queries leave out documents without a numeric sort value. Sorting
  by strings (for example names) is not covered: the `f` keys are
  percent-encoded and capped at 180 bytes, so their order is not string order.
  A later ADR can add a string sort key.
- Relevance ranking, phrase search, stemming and CJK segmentation are not
  covered.

## Alternatives considered

- **A `$words` schema directive.** Rejected for section 1's reasons; the
  descriptor record is what the storage layer can read.
- **Index words of every string field automatically.** Rejected: it would
  multiply the index size of every collection and hit the 256 MiB cap.
- **Turso FTS or another search library.** Rejected: native only, a large
  dependency and size-budget cost (ADR 0004, ADR 0019), and a second source
  of index truth.
- **Stateful cursors (the existing `page`).** They need `exec --loop`, hold
  the whole result in memory and expire; keyset cursors need none of that.
- **Sorting the matching documents in memory.** Rejected by the request: its
  cost grows with the number of matches, and it would need every match read
  before the first page.

## Verification

- Normalization fixture checked by Rust and JavaScript tests.
- Rust: `$match` and `$sort` answers equal a full scan with an in-memory sort
  for coercions, arrays, ties, nested paths, missing values, puts, patches,
  deletes and restores; a sorted query with a plannable filter reads only the
  documents it returns (plus one); cursors resume across writes and refuse a
  different query; `setWordFields` rebuilds and is refused for encrypted
  fields; `.vdbx` databases answer the same.
- Browser: Playwright tests for the WebAssembly engine and the JavaScript
  engine in Chromium and Firefox; `browser:wasm:budgets` passes.
