# ADR 0014: In-memory hot set for `fylo serve`

- Status: **Accepted**
- Date: **2026-10-28**
- Owners: **FYLO maintainers**
- Amends: ADR 0012 (reads no longer serialized), `docs/QUERY_INDEX_REUSE.md`
  (a served process may now keep document bodies in memory)

## Context

`fylo serve` (ADR 0012) moved the network to the far side of the engine, so a
remote read costs one round trip plus the server's own time. That server time
was still file calls: every `getDoc` and `findDocs` opened the root and the
collection, read `state.json` twice for a stable generation, listed every
document and tombstone in the collection only to learn whether it holds
documents or files, re-read the index snapshot to verify its bytes, and then
made about six calls per row. On the raw ATLAS share each of those calls is an
SMB round trip.

`docs/QUERY_INDEX_REUSE.md` explains why a generation number alone was never
promoted to a cache key: any process may write the root, a missing state file
reads as generation zero, and replacement, restore and branch copies can leave
or reuse a generation. Those objections hold for an engine that shares its
root. They do not hold for `fylo serve`, which holds the root lease for its
whole life and through which every write passes.

## Decision

`fylo serve` keeps a **hot set**: parsed documents, index snapshots, document
id lists and collection headers in memory, under a byte budget, shared by all
of its connections.

- **Scope.** The hot set lives in `fylo-engine` (`HotSet`), and a read-only
  engine uses it only when given one (`ReadOnlyEngine::with_hot_set`). Only
  `fylo serve` gives one. The CLI, `exec --loop`, invocations and the browser
  engine are unchanged.
- **What is resident.** For each collection: a header (kind, generation,
  write-ahead log end and the schema's encrypted fields), the parsed index
  snapshot, the list of live ids for queries the index cannot narrow, and each
  document read so far, parsed and decoded, with its stored access
  descriptor. Raw-file collections and tombstones are not resident.
- **Authorization is never cached.** Each read checks its own actor against
  the stored descriptor, as before. A document the reading actor may not see
  is not parsed and not admitted, as before.
- **Filled only from a stable, known state.** A header is read under the
  existing stable-read protocol. Any later miss opens the collection, checks
  that `state.json` is still stable at the header's generation and log end,
  reads, and checks again before admitting anything. A mismatch means storage
  moved without the server knowing, so the whole hot set is cleared and that
  read is answered from storage. The log end is the write-ahead log's segment
  identity, length and record count (ADR 0013), so a reused generation number
  alone cannot pass the check.
- **Invalidation.** Reads (`handshake`, `getDoc`, `getLatest`, `getFileData`,
  `getMeta`, `findDocs`, `findDeletedDocs`, `joinDocs`, `inspectCollection`,
  `log`) share the executor. Every other frame, including one that cannot be
  parsed, runs alone and clears the hot set before and after it. Opening a
  session recovers the root, so it runs alone and clears too, as does the
  recovery of a stranded transaction that a read triggers. A clear bumps an
  epoch, and a read that started before it cannot admit what it read.
- **Budget.** `--hot-set-bytes` (default 64 MiB, the shared index cache's
  admission budget; 0 turns the hot set off). Sizes are estimated from the
  parsed values: every JSON node, string and map slot, plus a fixed cost per
  entry. An entry larger than the whole budget is served and not kept. Eviction
  is a second-chance clock over all entries. The budget bounds what is
  retained; a reader still holding an evicted entry keeps it alive until it
  finishes, and the shared index cache keeps its own one entry.
- **No listing for a kind.** `getDoc`, `findDocs` and the other operations that
  only needed a collection's kind ask `ReadOnlyEngine::collection_kind`, which
  opens the collection descriptor instead of listing every document. This
  applies with or without a hot set.
- **Observability.** A served session's handshake reports
  `capabilities.networkServer.hotSet` with the budget, resident bytes and
  counters, or `false`, and `execution` is now `shared-reads`.

## Consequences

Benefits:

- a warm read makes no file call at all;
- concurrent readers no longer wait for each other;
- results are byte-for-byte those of the storage path, which the tests compare.

Costs and limits:

- **Out-of-band edits are not seen.** While `fylo serve` owns a root, the
  lease already forbids other FYLO writers; an edit made to the root's files by
  some other tool is now invisible to warm reads until the next write frame or
  the next miss that sees the generation move. That was always unsupported,
  and it is now also unobserved.
- Every write clears every collection's entries, so a mixed workload refills
  after each write. Per-collection invalidation from the write-ahead log's
  commit records is the next step and needs no format change.
- Writes now wait for running reads to finish, and a long query still delays
  the writes behind it.
- Memory: up to the budget, plus transient copies for the reads in flight.

## Rollback

`fylo serve --hot-set-bytes 0` turns the hot set off without a release.
Removing the code removes nothing on disk: the hot set writes no file and
changes no format.

## Verification

- `crates/fylo-engine/tests/hot_set.rs`: warm results equal storage results
  for planned, limited, range and unplanned queries and for gets, and a warm
  pass reads nothing; a cleared hot set sees the next write; a write it was not
  told about is caught on the next miss and the hot set is cleared; the budget
  holds while evicting; an entry over budget is served and not kept; file
  collections; per-actor access with denied documents left unparsed.
- `crates/fylo-machine/tests/hot_set_server.rs`: four rounds of three readers
  and two writers over TCP with coherence, monotonic and settled checks after
  every acknowledged write (768 settled checks, no stale row); the same suite
  under a 4 KiB budget; warm reads counted as hits with no misses; the budget
  sampled from another connection under load; `--hot-set-bytes 0`.
- Measurements: `docs/QUERY_RELEASE_QUALIFICATION.md`, section "Hot set in
  fylo serve, 2026-10-28".
