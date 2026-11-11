# ADR 0022: An IndexedDB host backend for `.vdbx` working copies

- Status: **Accepted**
- Date: **2026-11-11**
- Owners: **FYLO maintainers**
- Builds on: ADR 0008 (the portable WebAssembly engine) and ADR 0016
  section 10 (single-file hosts in the browser).
- Requested by: Mesail (CADUCEUS), request F1.

## Context

`fylo-browser.wasm` keeps a writable `.vdbx` working copy in the origin-private
file system (OPFS), through one `FileSystemSyncAccessHandle` in a dedicated
Worker (ADR 0016 section 10). OPFS exists only in a secure context. TACHYON's
macOS and iOS shells serve pages from `tachyon-app://bundle` in WKWebView,
which is not a secure context, so `navigator.storage` is missing there and a
working copy cannot be opened at all. A page on plain `http://` from a LAN
address is in the same position. IndexedDB is available in all of these.

The engine calls its host synchronously: `read_at`, `write_at`, `truncate` and
`flush` must return before the engine goes on. IndexedDB is asynchronous only,
and a Worker cannot block on it without a `SharedArrayBuffer`, which needs
cross-origin isolation, which also needs a secure context. So the backend
cannot answer each engine call from IndexedDB directly.

The single-file Worker already runs the engine one call at a time: one
`exec` message runs one NDJSON batch to the end, synchronously, and only then
posts its reply. Between two messages the Worker is free to wait for a
promise.

## Decision

Add an IndexedDB backend for `.vdbx` working copies, beside the OPFS one.

### 1. Choosing the backend at open time

The Worker takes two new sources:

- `{ kind: 'indexeddb', name, copyFrom? }` always uses IndexedDB;
- `{ kind: 'local', name, copyFrom? }` uses OPFS when the Worker can open
  `navigator.storage.getDirectory()` and files there have
  `createSyncAccessHandle`, and IndexedDB otherwise.

`{ kind: 'opfs' }` is unchanged. The open result says which one was used
(`storage: 'opfs' | 'indexeddb'`). The two stores are separate: a copy made in
IndexedDB is not visible through OPFS. An origin's answer does not change from
one page load to the next, so `local` finds the same copy each time.

Folder roots (`fylo-wasm-worker.mjs`) are out of scope. They need the
`SharedArrayBuffer` bridge, so they cannot run where OPFS is missing anyway.

### 2. Format in IndexedDB

One IndexedDB database per origin, named `fylo-vdbx`, version 1, with three
object stores:

| Store    | Key              | Value |
| -------- | ---------------- | ----- |
| `files`  | the copy's name  | `{ name, size, pageSize, state, modifiedMs }` |
| `pages`  | `[name, index]`  | an `ArrayBuffer` of exactly `pageSize` bytes |
| `owners` | the copy's name  | `{ name, owner, expiresAt }` |

- The file is cut into pages of 16 KiB (`pageSize` is stored, so it can
  change later without a migration). Page `index` holds the bytes
  `[index * pageSize, (index + 1) * pageSize)`.
- A page that is all zeros is not stored. A missing page reads as zeros. A
  `.vdbx` preallocates whole extents, so most of a fresh file is zeros.
- `size` is the file's length. Bytes past `size` do not exist; pages past it
  are deleted when the file shrinks, and the tail of the last page is zeroed.
- `state` is `ready`, or `copying` while `copyFrom` streams a file in. A copy
  left in `copying` (the tab died during the copy) does not open; opening it
  with `copyFrom` again starts the copy over.

The bytes are the `.vdbx` file, unchanged. A snapshot of an IndexedDB copy is
a normal `.vdbx` that the native engine opens.

### 3. Durability: one transaction per Worker call

At open, the Worker reads every stored page of the copy into memory. The
engine's synchronous calls then work on that memory. Every page a call
changes is marked dirty.

After every Worker call that can change the file (`exec`, and the `host`
calls `write`, `truncate` and `flush`), and before the Worker replies, it
writes all dirty pages, the new `size` and the deletions in **one** IndexedDB
`readwrite` transaction, opened with `{ durability: 'strict' }`, and waits for
its `complete` event. Only then does it post the reply. This keeps FYLO's rule
that a write is reported done only when it is stored.

- One transaction is all or nothing. After a crash the stored file is either
  the file before the call or the file after it, never a mix. Both are states
  the engine has already left consistent, so recovery has less to do than on
  a real disk, where the engine's own flush order matters.
- The guarantee is per Worker call. Inside one `exec` batch nothing is stored
  until the whole batch has run. A caller that wants each write stored on its
  own sends it in its own batch, as with every other host.
- When the commit fails (quota, the lease was lost, the browser closed the
  database), the Worker answers that call with an error (`ENOSPC`,
  `ELOCKLOST` or `EIO`), even when the engine's own answer was a success, and
  refuses every later call on that copy until it is closed. Memory and storage
  no longer agree, so the only safe next step is to open the copy again, which
  reads the last stored state.

### 4. One owner per copy

A copy has one writer, as an OPFS copy does. The owner is recorded in
`owners` as a lease: `{ owner: <random id>, expiresAt }`.

- Opening claims the lease in a `readwrite` transaction. It succeeds when no
  lease exists, when the lease has expired, or when the Web Lock below says
  the old owner is gone. Otherwise the open fails at once with `EBUSY`, as an
  OPFS copy does.
- The owner renews the lease every 2 seconds and in every commit. A lease runs
  for 6 seconds.
- **Fencing.** Every commit transaction first reads the lease, in the same
  transaction, and aborts with `ELOCKLOST` when the owner is not this Worker.
  IndexedDB runs `readwrite` transactions on the same stores one after
  another, so an old owner that wakes up after its lease was taken can never
  write over the new owner's pages.
- **Web Locks, when present.** In a secure context the owner also holds the
  exclusive Web Lock `fylo-vdbx-idb:<name>` for its whole life. A contender
  that gets that lock knows the old owner has ended, so it takes the lease at
  once, without waiting for it to expire. `navigator.locks` does not exist
  where OPFS is missing (it is also secure-context only), so there a copy
  whose owner was killed opens again once the lease expires, at most 6
  seconds later.
- `close` deletes the lease, so a clean close hands the copy over at once
  everywhere.

### 5. Removing and listing copies

`listWorkingCopies` and `removeWorkingCopy` take `{ storage: 'opfs' |
'indexeddb' }`. Removing an IndexedDB copy deletes its `files`, `pages` and
`owners` records in one transaction, and is refused with `EBUSY` while
another Worker holds a live lease.

## Accepted losses

| Loss | Why |
| --- | --- |
| The copy's non-zero bytes stay in the Worker's memory while it is open. | IndexedDB cannot be read synchronously. A copy is limited by the memory the browser gives a Worker; OPFS has no such limit. |
| Nothing inside a batch is stored before the batch ends. | Commits happen between Worker calls. This is stronger than a disk (all or nothing), but a batch is the unit of durability. |
| `strict` durability is a request. | Chromium and Firefox flush before `complete` with `strict`. WebKit stores IndexedDB in SQLite and decides its own sync policy. A killed tab or Worker never loses a committed call; a power cut on WebKit may lose the last calls, as it can lose any IndexedDB write there. |
| Eviction. | `navigator.storage.persist()` is secure-context only. Where IndexedDB is the fallback, the browser may evict the origin's data under storage pressure, as it may evict OPFS data in a non-persistent origin. |
| A killed owner blocks the copy for up to 6 seconds without Web Locks. | Without Web Locks the only proof that an owner has gone is its silence. |
| `webcrypto:` identities still need cross-origin isolation. | Unchanged from ADR 0021; a protected database cannot open where OPFS is missing either. |

## Consequences

Working copies open in Safari, in WKWebView pages on custom schemes, and on
plain `http://` pages, with the same engine and the same file format. Each
`exec` costs one IndexedDB transaction, which bounds write throughput by
batch rather than by document; callers that need speed batch their writes.

The browser e2e suites run every working-copy scenario on both backends, in
Chromium, Firefox and WebKit, and also on a page that is not a secure context
(the server is reached by a name that is not `localhost`), where OPFS and Web
Locks are both missing and the `local` source must pick IndexedDB.

## Acceptance evidence

- Complete when merged: the working-copy, crash and soak specs pass on both
  backends in Chromium, Firefox and WebKit, and the `local` source picks
  IndexedDB on a page that is not a secure context.
- Not covered: a real WKWebView page on `tachyon-app://`. Playwright's WebKit
  is the same engine, and the non-secure-context page has the same missing
  APIs, but the shell itself is tested by TACHYON, not here.

## Related decisions

- [ADR 0008](0008-fylo-wasm-browser-and-mobile-engine.md)
- [ADR 0016](0016-single-file-vdbx-database.md)
- [ADR 0021](0021-application-identities-and-signed-policies.md)
