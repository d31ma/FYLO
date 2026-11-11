# ADR 0024: Collection change feed

- Status: **Accepted** (built 2026-11-11; the subscription in section 6 is designed, not built)
- Date: **2026-11-11**
- Owners: **FYLO maintainers**
- Request: Mesail's F4 (CADUCEUS), part 1
- Related: ADR 0007 (replication stays outside FYLO), ADR 0013, ADR 0015,
  ADR 0016

## Context

Mesail syncs devices from a log of changes per conversation. Today its
`messaging` service writes that log itself, because FYLO has no way to ask
"what changed in this collection since I last looked?".

What FYLO had when this was written:

- The README described an `events/<collection>.ndjson` journal. Only the
  separate JavaScript browser engine (`fylo-web.mjs`) writes it. The Rust
  engine reads its size in the undo journal and never writes it.
- Document updates keep the document's TTID. So TTID order is creation
  order, not change order, and a TTID cannot mark a position in a feed.
- Ordered, durable history exists only where records carry log sequence
  numbers (LSNs) that outlive a checkpoint: the segments layout (ADR 0015)
  and `.vdbx` databases (ADR 0016), which share the code. Each such
  collection keeps a location table: for every document id, the location,
  state (live or deleted) and LSN of its newest entry.
- The files layout's write-ahead log (ADR 0013) is deleted at every
  checkpoint (by default every 4,096 records). Raw-file collections, EFS
  roots and folder roots in WASI or the browser use the undo journal, which
  keeps nothing after a commit.

## Decision

### 1. The operation

`changes` is a read operation of the machine protocol, in every language
client and in both browser engines:

```json
{ "op": "changes", "collection": "notes", "after": null, "limit": 256 }
```

The result is `{ changes: [{ id, kind }], cursor, more }`:

- `changes`: every document whose **newest** change follows the cursor,
  once, ordered by that change. `kind` is `put` (created, changed, restored
  or its metadata set) or `delete` (soft-deleted).
- `cursor`: an opaque string to pass as `after` next time. A page with no
  changes returns the cursor it was given.
- `more`: whether more changes follow this page.
- `limit`: 1 to 1000, default 256.

The draft asked for `changes(collection, afterTtid, limit)`. The parameter is
`after` and holds a cursor, because a TTID does not say where a document's
latest change is (section "Context").

### 2. A coalesced feed, served from the location table

The feed reports each document's newest change, not every change. It is
read from the location table, filtered to LSNs after the cursor and sorted
by LSN, then by id. Nothing new is written and no flush is added to any
commit.

Why coalesced, not a full history:

- `fylo compact` keeps only the newest entry of each document, at its
  original LSN. A full per-change history would need a second log that
  compaction does not touch, and a retention rule to bound it. A coalesced
  feed gives the same answer before and after compaction.
- A device that syncs needs to know which documents to fetch again and
  which to remove. It reads the current document with `getDoc` either way,
  so intermediate versions add nothing to it.
- `put` does not distinguish a new document from a changed one. After
  compaction the earlier entry that would tell them apart is gone, so the
  feed does not claim it.

A reader that pages through a changing collection loses nothing: a document
changed after the reader passed it gets a new, larger LSN and appears again
later. Entries of one commit share an LSN; the cursor holds the LSN and the
id of the last change returned, so a page boundary inside one commit skips
nothing.

### 3. The cursor and when it expires

The cursor is `fch1.<lineage>.<lsn>.<id>`. It holds no state in the process,
so it survives restarts and works across processes and languages, unlike
the 15-minute `findDocs` cursors.

**Nothing expires by time.** A cursor expires only when the collection's
LSNs start again from 1, because then an old LSN would point into a
different history:

- a drop and re-create of the collection;
- `fylo pack`, `fylo rekey` (which packs) and import into a new `.vdbx`;
- a move to the segments layout (`fylo layout --to segments`);
- an older copy of the files put back (the cursor's LSN is then past the
  collection's next LSN).

The answer is **`ECURSOR_EXPIRED`** (not retryable): read the collection
again (for example with `findDocs`) and continue from `"after": null`.

To detect a new history, every operation that starts LSNs again writes a
fresh random **change lineage** (128 bits, hex) into the collection's
catalog descriptor (`changeLineage`): the directory catalog for directory
roots and the descriptor in the collection's directory block for `.vdbx`.
The cursor carries it, and a cursor whose lineage differs is expired.

- Older FYLO builds read a descriptor with this key: the descriptor parser
  has no `deny_unknown_fields`.
- A collection made before this change has no lineage until one of those
  operations runs; its cursors carry `0`, and only the LSN bound protects
  them. An older build that drops and re-creates such a collection writes
  no lineage, so a cursor from before that is not detected. This is stated,
  not fixed.
- A move back to the files layout removes the lineage; that collection
  then refuses `changes` anyway.

### 4. Where the feed works

- **Segments layout and `.vdbx`:** yes, natively, in WASI and in
  `fylo-browser.wasm` (which reads `.vdbx`).
- **Files layout:** refused with `ELAYOUTUNSUPPORTED`, naming
  `fylo layout <collection> --to segments`. Keeping a feed there would need
  a new durable log beside the write-ahead log, which this decision avoids.
- **Raw-file collections and buckets:** refused the same way.
- **The JavaScript browser engine (`fylo-web.mjs`):** served from its own
  `events/<collection>.ndjson` journal, with the same coalescing, cursor
  format and error codes. Its lineage is the journal's first event (time and
  id), so a dropped and re-created collection expires old cursors; its
  sequence numbers are the journal's line numbers. A cursor taken while the
  journal was empty stays valid after the first write.

### 5. Retention

The feed keeps every document's newest change for as long as the document
(or its soft-deleted tombstone) is kept. Nothing is trimmed by age or count.
A document removed for good, with no tombstone, would leave the feed
without a trace of it; FYLO has no such operation today, and one would need
to record its removal here first.

### 6. Subscription (designed, not built)

A later step can push changes instead of being polled. FYLO opens no
listener and runs no server, so a subscription lives inside a session the
application already runs:

- `subscribeChanges {collection, after}` in an `exec --loop` session (or the
  browser worker) returns a subscription id. `unsubscribeChanges {id}` ends
  it.
- The session checks the collection's generation slot, a small read it
  already does for stable reads, every 250 ms and after each of its own
  commits. When the generation has moved, it runs the same query as
  `changes` from the subscription's cursor and writes an unsolicited frame
  `{"type":"changes","subscription":<id>,"changes":[...],"cursor":...}`,
  then moves the cursor.
- Unsolicited frames need a protocol rule: clients must tell them from
  replies by `type`, and a client that did not subscribe never sees one. A
  handshake capability (`changeSubscriptions: 1`) advertises it.
- An expired cursor ends the subscription with one frame carrying
  `ECURSOR_EXPIRED`.
- In the browser the worker posts the same object as a message.

It is not built yet because the unsolicited frame changes the request and
reply rule every client relies on, and each client needs a reader for it.

## Consequences

- One new operation (55 in the registry), one new error code
  (`ECURSOR_EXPIRED`), and one optional descriptor key (`changeLineage`).
- The feed adds no write and no flush. A request reads the location table,
  which a reader holds in memory, and sorts the changes after the cursor:
  O(n) in the collection's document count per page.
- Applications on the files layout must move to segments to use it.

## Verification

- `crates/fylo-machine/tests/change_feed.rs`: pages, coalescing, delete
  and restore, a commit of many documents across pages, compaction, drop and
  re-create (`ECURSOR_EXPIRED`), a cursor past the end, bad cursors and
  limits, the files layout refused, and a move to segments and back to files.
- `tests/browser/change-feed.test.js`: the JavaScript engine's feed.
- `tests/browser/e2e/engine-vdbx.spec.mjs`: the feed in `fylo-browser.wasm`
  over a `.vdbx` working copy, in Chromium and Firefox.
- `scripts/verify-rust-release-machine-parity.mjs` covers the operation.
