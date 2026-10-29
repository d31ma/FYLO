# ADR 0012: Opt-in network server for one root

- Status: **Withdrawn** (2026-10-29)
- Date: **2026-10-28**
- Owners: **FYLO maintainers**
- Amended: ADR 0007 (FYLO no longer "opens no server"); withdrawn, so ADR 0007
  stands as written

> **Withdrawn 2026-10-29.** FYLO is an embedded library, like SQLite: apps open
> the database files directly in their own process. `fylo serve`, its flags,
> `Fylo.connect` and the `fyloServe` hello were removed before release, and no
> network server ships. The hot set moved to embedded sessions that hold the
> root's exclusive lease (`exec --loop --exclusive-root`; see ADR 0014). A
> server exists only as a test fixture that simulates an app server reading the
> database. The rest of this record is kept as history.

## Context

A FYLO root on a network filesystem is slow because every operation becomes
hundreds of remote file calls. The 2026-10-28 ATLAS measurements put a durable
put at about 490 file calls, each one an SMB round trip. Network filesystems
also weaken the lock, permission and path-identity guarantees FYLO relies on.

The remedy is to run FYLO next to its disk and move the network to the other
side of the engine: one round trip per operation, with every file call local.
The machine protocol is already a versioned, bounded NDJSON stream, so it can
cross a socket unchanged.

## Decision

Add `fylo serve`, an opt-in, long-running process that owns one root and
answers the machine protocol v1 over TCP.

- **One owner.** The server takes the root lease at startup and holds it until
  it stops, so no other process can open the root meanwhile. It recovers the
  root once at startup, and each connection's session admits it again, exactly
  as `exec --loop --exclusive-root` does.
- **One frame at a time.** Each connection has its own persistent session (its
  own query cursors), and frames from all connections run through one executor.
  The engine has one writer per root inside a process; this keeps that
  invariant instead of relaxing it. Reads are serialized too, for now
  (ADR 0014 later let reads run together and serve them from memory).
- **No open listener.** The operator must name the address (`--listen`); there
  is no default. Every connection must first send a transport hello,
  `{"fyloServe":1,"token":"..."}`, carrying a shared secret of at least 32 bytes
  read from a private file (`--token-file`, mode 600 on Unix, never a
  command-line argument). The comparison takes the same time for every
  candidate of the right length. A wrong or missing token closes the
  connection before any machine frame is read.
- **Bounds.** Request and response frames keep the machine limits
  (`--max-request-bytes`, `--max-response-bytes`). Connections are capped
  (`--max-connections`, default 16; extra ones get `ESERVERBUSY`). A connection
  must authenticate within 5 seconds and is closed after `--idle-timeout-ms`
  (default 5 minutes) without a frame.
- **A remote client runs as the server, so it gets less.** It cannot choose
  another root, override the schema directory, read a server path or URL as
  file input, write a file on the server (`getFileData` with `path`), or make
  the server fetch a URL (`importBulkData`). File bytes travel inline as base64
  only. The handshake says so in `capabilities.networkServer` and in
  `capabilities.documentBuckets`.
- **Shutdown.** `fylo serve --stop` (a hello with `"shutdown":true`) stops
  accepting, lets running frames finish and write their responses, closes idle
  connections within 200 ms, and releases the root with the checked release
  that removes its owner record. Killing the process is a crash: the kernel
  drops the lock, and the next owner recovers the root as after any FYLO crash.
- **Clients.** The Node and Rust shims gain a TCP transport beside the
  subprocess one (`Fylo.connect`). Other shims follow the same hello.

## Consequences

Benefits:

- a remote operation costs one network round trip plus local server time;
- the root lives on a local filesystem with real locks and fsync, instead of
  being run live over SMB or NFS;
- the server is the natural home for an in-memory hot set.

Costs and limits:

- **No transport encryption yet.** The token and every document cross the
  network in clear text. Run `fylo serve` on loopback, a trusted LAN, or behind
  a tunnel (WireGuard, SSH). TLS or mTLS is the next step and needs no protocol
  change, since the hello is transport-level.
- A token holder has full read and write access to the served root, including
  `dropCollection`. Per-actor access descriptors still apply to the requests
  that carry them, but the token itself is not an identity.
- Serialized execution caps throughput at one frame at a time. A slow query
  delays every other client.
- Remote clients cannot use file path or URL inputs.

## Rollback

`fylo serve` is a new command; nothing else changes behavior. Removing it
removes the network surface entirely, and roots it served are ordinary roots
any local process can open.

## Verification

- `crates/fylo-machine/tests/network_server_contract.rs`: put, patch, read and
  delete over TCP; the lease held until shutdown and released after; wrong and
  missing tokens; remote-policy refusals; oversized frames; the connection cap;
  concurrent writers on one collection all committing; graceful shutdown with
  an idle session; the idle timeout.
- `tests/interop/network-server.test.js`: the Node and Rust shims against the
  built binary, a server killed with `SIGKILL` and its root recovered by the
  next server, and `fylo serve --stop` releasing the root to the next owner.
