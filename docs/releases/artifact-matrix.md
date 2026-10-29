# Artifact Matrix

FYLO ships one engine as three artifacts. They share the source, the on-disk
format, and the NDJSON machine protocol; they differ only where a platform
cannot supply something.

| | `fylo` | `fylo.wasm` | `fylo-browser.wasm` |
| --- | --- | --- | --- |
| Target | native triples | `wasm32-wasip1` | `wasm32-unknown-unknown` |
| Storage | `std::fs` | `std::fs` over WASI preopens | host `HostVfs` table |
| Driven by | a spawned process | any WASI runtime | an embedder that fills the table |
| Release build (develop, 2026-10-10) | ~7.97 MiB (macOS arm64) | ~2.04 MiB | ~2.23 MiB, ~735 KiB gzip with the CHEX validator and `.vdbx` databases with their record codecs and dictionaries, built at `opt-level = "z"` (2026-10-29; 2.38 MiB / 800 KiB budget) |
| Verified by | crash matrix, soak, interchange | native/WASI interchange | `browser:engine:e2e` (Chromium, Firefox), host-table corpus |

`fylo` and `fylo.wasm` are **interchangeable**. A shim spawns one or the other,
writes NDJSON to stdin, reads NDJSON from stdout, and cannot tell which it got.
`bun run rust:interchange` drives 25 frames through both — CRUD, query, SQL,
buckets, version control, and five deliberate failures — and fails on any
difference outside the declared list below.

## The browser host table

`fylo-browser.wasm` imports seventeen functions from one module, `fylo_host`,
and nothing else — no binding-generator glue. Twelve are ordinary filesystem
operations, two read and write the host-owned attribute manifest, and the other
three are entropy, the wall clock, and a diagnostic channel, because
`SystemTime::now` panics on this target and a module with no stdio otherwise
reports every failure as a bare `unreachable` trap.

Keeping all fifteen in one plain C table is what lets a Swift, Kotlin, or Dart
embedder fill the same slots. `src/browser/host-vfs.mjs` is the JavaScript
implementation; in a browser its backend is OPFS through
`FileSystemSyncAccessHandle`, which is synchronous and therefore available only
inside a dedicated Worker.

## Where a record's attributes live

FYLO hangs a record's logical `key`, checksum stamp, access descriptor, and
developer metadata off the file itself. Only unix has somewhere to put them.

| Target | Location |
| --- | --- |
| unix | native extended attributes |
| Windows | the `:fylo.xattrs` alternate data stream |
| `wasm32-wasip1` | a `<record>.fylo-attrs` sidecar file |
| browser | the host's manifest, through `read_attrs` / `write_attrs` |

The browser writes nothing beside a record. The host owns the manifest and
chooses where it goes, which lets a root keep **one** manifest rather than
doubling its file count — OPFS charges per handle, and a 100k-document root
would otherwise carry 100k extra files. The bytes are the same
JSON-and-base64 map every other platform uses, so the engine's view of an
attribute never changes.

Transaction rollback still covers them: on a sidecar target the sidecar is
captured as a file in its own right, and on the browser the manifest is
captured and restored through the same table.

**One consequence to know about.** File System Access lets a browser write into
a real folder the user picked. A native binary opening that folder reads a
sidecar when a record has no extended attributes of its own — so a WASI-written
root is portable — but it cannot see a browser host's manifest, which by design
lives wherever the host put it. Moving a browser root to a server therefore
needs an export, not a copy.

## Browser: how the engine reaches OPFS

`bun run browser:engine:e2e` stores and queries documents through the real
engine in a real browser. Chromium and Firefox pass; WebKit cannot start a
module Worker over OPFS in this harness and is skipped.

**OPFS is only half synchronous.** Inside a Worker `createSyncAccessHandle`
gives synchronous file read, write, truncate, and flush — but every directory
operation returns a promise. `getDirectoryHandleSync`, `getFileHandleSync`,
`keysSync`, and `removeEntrySync` do not exist. The engine calls the host
synchronously, so the async half has to be made to block.

Two Workers and one `SharedArrayBuffer`, the design `sqlite-wasm` uses:

- **the bridge Worker** owns every OPFS handle and runs the promises. It waits
  with `Atomics.waitAsync`, never `Atomics.wait`, because parking this thread
  would stop the very work it exists to do;
- **the engine Worker** writes a request into shared memory, notifies, and
  parks in `Atomics.wait` until the reply lands.

It has to be *all* of OPFS rather than only directories: a Worker parked in
`Atomics.wait` cannot receive a `postMessage`, and a `FileSystemSyncAccessHandle`
is not transferable, so the answer cannot come back any other way.

This requires the page to be cross-origin isolated —
`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp` — or `SharedArrayBuffer` does not
exist. `scripts/serve-isolated.mjs` sends both. Without them the Worker refuses
with a message naming the headers rather than failing as a disk error.

Three behaviours worth knowing, each of which cost a debugging cycle:

- `TextDecoder` refuses a view backed by shared memory, so bytes are copied out
  with `slice` before decoding;
- `Atomics.waitAsync` returns *synchronously* when the observed value is not the
  one awaited, so waiting on a fixed value spins the microtask queue and starves
  the OPFS promises;
- the engine opens a **directory** to flush it after a rename, as POSIX
  requires. OPFS has no directory handle to flush, so a directory opens to a
  handle whose flush and close do nothing — the rename it follows is already
  durable.

## What every artifact does

All 38 machine operations are compiled into all three. Documents, buckets,
prefix indexes, queries, SQL, joins, pagination, transactions, crash recovery,
tombstones and restore, resharding, version control, and encryption behave
identically, because they are filesystem and CPU work and nothing else.

The recovery implementation is shared by all three artifacts, but the release
evidence is deliberately named rather than implied: the native artifact runs
all 15 injected crash points, native and WASI run the interchange corpus, and
the browser artifact runs the host-table and real-OPFS corpora. Browser/WASI
failpoint and soak runs remain qualification work before the browser artifact
can replace the JavaScript engine.

## What differs, and why

| Capability | `fylo` | `fylo.wasm` | `fylo-browser.wasm` | Cause |
| --- | --- | --- | --- | --- |
| Kernel-enforced single writer (`exclusiveRoot`) | yes | **no** | **no** | WASI preview 1 has no advisory locking. The lease still records its owner, so a supervisor can see who holds the root, but nothing refuses a second writer. |
| POSIX uid/gid/mode (`machineAccess`) | macOS, Linux only | **no** | **no** | Needs `chown`/`chmod`. Already absent on Windows, so the capability set already expressed this. |
| Schema validation (`schemaValidate`, `schemaCheck`, `strict` and `warn` writes) | yes, compiled from each version's CHEX source | yes, from the schema registry's stored DFAs; otherwise refuses | yes, from the schema registry's stored DFAs; otherwise refuses | A regex compiler would push the browser engine past its size budget, so the Wasm builds link only `regex-automata`'s DFA search (ADR 0017 section 4). They validate with the DFAs `fylo schema push` stored in each version's `compiled` field, loaded through the validating `from_bytes`, and give the native binary's results and messages. They fail closed: a head version that is not portable (a Unicode `\b`), whose `compiled` bytes are missing or refused, or a collection with no registry whose schema is in a `fylo.schema` folder or whose descriptor records `strict` or `warn`, refuses `strict` and `warn` writes with `ESCHEMAUNAVAILABLE`; `off` writes as before. Schemas are compiled, so pushed, natively. No build has an upgrader runtime: migrations are JSON steps (ADR 0018). |
| URL ingestion (`putData` with `file.url`, `importBulkData` with a URL) | yes | **no** | **no** | No TLS stack in the module. A browser already has a network stack carrying the origin's CORS, cookies, and policy; bundling one would add megabytes *and* route around those protections. The host fetches and supplies the bytes. |
| Extended attributes | native xattrs (unix), alternate data stream (Windows) | `.fylo-attrs` sidecar | host-owned manifest through `read_attrs` / `write_attrs` | No xattrs under WASI or in a browser. Same JSON-and-base64 manifest, different location. |
| Inode identity check on open | yes | **no** | **no** | Catches a symlink or rename swapped in between the path check and the read. WASI scopes the guest to preopened directories it cannot escape; a browser host has no inodes. |
| Directory `fsync` | yes | best effort | best effort | Tolerated when the platform reports `Unsupported`. The preceding rename is still atomic. |
| Process identity in lock records | real pid | `0` | `0` | No pids on WebAssembly. `lock_owner_alive` already treats an unverifiable owner as stale. |
| Environment configuration | `FYLO_*` variables | `FYLO_*` variables, if the runtime passes them | **none** | A browser has no environment. Every knob is also a `RootConfig` value, and `serve_configured` takes it directly. |

A supervisor should read these from the handshake rather than from this table:
`exclusiveRoot`, `machineAccess`, `documentBuckets.putInputs`, and
`schemaValidation` each report the truth for the running artifact; a Wasm
build's `schemaValidation` carries `compiledOnly: true`.

## Does WebAssembly replace the binary?

For a service that stores and queries documents: yes. Storage, indexing,
queries, SQL, transactions, recovery, buckets, and version control are all
present and byte-compatible, and the module is a quarter the size.

Choose the native binary when you need any of:

- **more than one writer process against one root** — the kernel lease is the
  only thing that makes that safe, and WebAssembly has no equivalent;
- **POSIX ownership and permissions** on documents;
- **field encryption**, which only the native binary includes;
- **schema authoring**: `fylo schema push` compiles CHEX, which needs the
  regex compiler only the native binary links, and only the native binary
  validates a schema that is not portable;
- **ingestion straight from a URL** rather than from bytes the caller supplies.

Everything else is a swap.

## Running the WASI artifact

```bash
wasmtime --dir "$ROOT" fylo.wasm exec --loop --root "$ROOT"
```

The guest sees only what is preopened, so the root must be granted explicitly.
`scripts/run-wasi-machine.mjs` does the same under Node for environments that
already have it; any WASI preview 1 runtime works.
