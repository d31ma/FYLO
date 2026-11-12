# FYLO Machine Protocol v1

FYLO's machine interface is a local, bounded NDJSON protocol. One UTF-8 JSON
request and one UTF-8 JSON response occupy one LF-delimited frame.

The canonical artifacts are:

- [`schema.json`](schema.json): request and response envelope;
- [`operations.json`](operations.json): operation names, mutability, and retry
  classification;
- [`fixtures.ndjson`](fixtures.ndjson): transport-level conformance examples;
- [`../../errors/v1.json`](../../errors/v1.json): stable public error codes.

## Framing

- Protocol version: `1`
- Encoding: UTF-8
- Delimiter: LF (`0x0a`)
- Delimiter counts toward limit: no
- Default maximum request: 1 MiB
- Default maximum response: 8 MiB
- Maximum configured frame: 64 MiB
- Duplicate JSON keys: rejected
- Truncated final frame: error and terminate
- Malformed complete frame: error and resume at the next LF

Requests do not include `protocolVersion`; compatibility is established through
`handshake`. Every response includes `protocolVersion`.

## Errors

Error responses have a stable `error.code`. The human-readable
`error.message` is diagnostic and must not be parsed. Clients may retry only
when the operation registry says retry is safe and the concrete error is
documented as transient.

A `putData` that names an `id` the collection already has (a document, or a
file in a document bucket) fails with `EDOCEXISTS` (not retryable) and
changes nothing, in every layout: per-document files, segments and `.vdbx`
databases. Releases
up to 26.45.07 reported it as `ENATIVE_METADATA`. An `exec --invocation`
retry of a create that had already committed answers `EDOCEXISTS`, which a
host can treat as "already done".

## Compatibility

Within protocol v1:

- operation names and stable error codes are preserved;
- additive result fields are allowed;
- clients must ignore unknown result fields;
- request fields remain operation-specific;
- changing idempotency, durability, or retry behavior requires a new protocol
  version or an explicit compatibility RFC.

## Repository history

The `log` operation returns its commit array directly in `result`, in
newest-to-oldest first-parent order. Each commit has `id`, `branch`, `parents`,
`message`, `createdAt` and `root`. `limit` defaults to 50 and must be between 1
and 1000. `restoreCommit` moves the active branch head to the selected commit;
subsequent `log` requests traverse that commit's first-parent history. Stored
commit manifests remain immutable.

## Embedded serverless queue

Protocol v1 exposes a brokerless filesystem queue through `queuePublish`,
`queueClaim`, `queueAck`, `queueNack`, `queueExtend`, `queueStats`, and
`queueDeadLetters`. Delivery is at least once. A claim is durable before its
receipt is returned; workers must acknowledge with that receipt or the message
becomes visible after its lease. Producer retries are idempotent only when the
same `idempotencyKey`, topic, and payload are supplied.
Topic and consumer-group names contain at most 127 UTF-8 bytes. Queue reads are
bounded before the engine leases or aggregates records so the response frame
limit cannot strand an unreturnable delivery. Header discovery and queue stats
also stop at 64 MiB of message-file scan work per request.

## Candidate filesystem profile reporting

The isolated adapter adds explicit CLI selection
`--filesystem-profile efs-sidecar-v1` and an additive embedded `serve_profiled`
entry point. Legacy wrappers and unselected calls keep the native profile.
Profile selection is immutable for the session; request fields cannot enable a
different backend or authorize another root. EFS admission occurs before startup
recovery and every subsequent engine or writer construction, even without
`--exclusive-root`. Unsupported locking fails before owner metadata publication.

The candidate handshake adds `capabilities.filesystemMetadata` with `version:1`,
`profiles:["native","efs-sidecar-v1"]` and `efsQualified:false`. These identify
candidate implementations, not a filesystem support tier. `result.storage`
reports `filesystemProfile` and `rootLock` fields `owned`, `kernelEnforced` and
`remoteQualified:false`. `owned` describes current owner-metadata verification;
`kernelEnforced` means the actual kernel-mediated advisory-lock acquisition
succeeded. Neither proves mandatory fencing, current remote lock ownership or
independent-client cache coherence. Native sessions without an owning lease
report both local lock flags false.

`rootLock` also reports `filesystem`, the name the OS gives the root's
filesystem (`apfs`, `ext4`, `NTFS`, `nfs`, `nfs4.1`, `unknown`, ...), and
`remote`, whether it is a network filesystem, FUSE or a virtual machine's
host share ([ADR 0029](../../../docs/adr/0029-no-root-on-a-network-filesystem.md)).
Before the lease is taken they describe the default root's path; both are
`null` without a default root. `kernelEnforced` is never `true` when `remote`
is `true`, the EFS profile included. A native session refuses a remote root
with `EREMOTEROOT` (not retryable) before it writes anything: at startup with
`--exclusive-root`, otherwise on the first request that opens the root. The
EFS profile is admitted there as before.

Frame limits, schema/encryption configuration, operation names and stable errors
retain their existing contracts. EFS queue operations currently fail closed at
the native queue-open seam; its `serverlessQueue` capability is false while the
native default remains unchanged. Queue support needs a scoped source extension
and later qualification. See the
[EFS candidate plan](../../../docs/operations/efs-profile-qualification.md).

These storage fields do not describe an ephemeral invocation lifetime.
`persistent=false` disables pagination cursors; the current streaming loop still
reads until EOF and flushes while its session owns leases. The additive isolated
`InvocationOptions`/`InvocationRuntime` source candidate accepts exactly one
bounded nonempty LF core frame. The host selects immutable root/profile/application
configuration, an absolute monotonic deadline and a nonzero cleanup reserve.
The host root/repository must already exist and resolve to the same directory.
External leases, unrelated request roots and request schema overrides are refused.
Canonical aliases of the admitted root are allowed.

This invocation slice refuses `log`, `commit`, `checkout`, `branch`, `status`,
`diff`, `restoreCommit` and `merge` before admission. The reused core handshake
retains legacy capability fields; those fields do not qualify these operations
for invocation use. Malformed input and forbidden configuration return I/O errors.

The invocation candidate also refuses `schemaValidate` and host configuration
with `strict_schema=true` before acquiring a lease. External validator deadlines
and checked descendant cleanup are not qualified. Embedded `schemaMaterialize`,
non-strict schema inspection and encryption configuration retain their existing
scope. Persistent streaming and native schema validation remain available.

The runtime acquires its own kernel-admitted cooperative lease, recovers before
dispatch and keeps engines/handles private. Recovery enumerates at most 10,000
unique collection names per main/active root. It buffers the existing response and
requires unique checked teardown before returning bytes. Cleanup or unlock
uncertainty returns an error without completed response bytes; mutation retry is
never automatic. Checks cannot preempt blocking I/O or prove distributed fencing.
Returned output is bounded; intermediate serializer allocations have no hard cap.
Existing streaming APIs retain their behavior. Native behavior tests and remote
qualification remain pending. See
[ADR 0011](../../../docs/adr/0011-serverless-invocation-contract.md).

The `fylo` binary exposes this runtime as `fylo exec --invocation --request
<json|@path|-> --deadline-ms <n> --cleanup-reserve-ms <n>`: one request per
process, the same answer frame and exit codes as `exec --request`, and a
failure frame (`EROOTLOCKED`, `EREMOTEROOT`, `EDEADLINE`, `EBADREQUEST`,
`EFRAME_REQUEST_TOO_LARGE`, `EUNSUPPORTEDOP`, `EINVOCATION`) where the library
returns an I/O error. See
[ADR 0028](../../../docs/adr/0028-cli-invocation-mode.md).

Invocations that wait for the same root take turns. A waiting process first
takes a second lock, kept in `.<root>.fylo-root-turn.lock` beside the root's
owner lock, and keeps it until it owns the root. A process that starts later
waits behind it, so a stream of new invocations cannot keep a waiting one out
until its deadline. `exec --loop --exclusive-root` does not wait and does not
take turns.

Every handshake reports `capabilities.invocation`, so a host can check for
the mode before it uses it. On a native build it is:

```json
{
    "version": 1,
    "command": "exec --invocation",
    "options": ["--request", "--deadline-ms", "--cleanup-reserve-ms"],
    "excludedOperations": ["log", "commit", "checkout", "branch", "status", "diff", "restoreCommit", "merge"],
    "createRequiresId": true,
    "waitsForRoot": true
}
```

It is `false` in the browser (WebAssembly) engine. A binary without the key
predates the mode.

`fylo exec` refuses an option it does not read, and a value option with no
value, as a configuration error: a message on stderr, exit code 1 and no
frame. Releases up to 26.45.07 ignored such options, so an old binary given
`--invocation` ran the request as a plain `exec --request`.

## In-memory hot set (`capabilities.hotSet`)

Every handshake reports top-level `capabilities.hotSet`. A session that holds
the root's exclusive lease (`exec --loop --exclusive-root`, or the Rust
`FrameSession` with that lease) keeps parsed documents, index snapshots,
document id lists and collection headers in memory, 64 MiB by default
(ADR 0014). `--hot-set-bytes <n>` sets the budget and `0` turns it off.

`hotSet` is `false` when the session has no hot set: without the exclusive
lease, or with `--hot-set-bytes 0`. Otherwise it is an object of counters:
`budgetBytes`, `residentBytes`, `collections`, `documents`, `hits`, `misses`,
`evictions`, `refusals` and `clears`. The counters are for operators; results
never depend on them. Frames in one session run one at a time, reads
included; a frame that is not a read clears the hot set before and after it
runs.

## Application identity (`identity`)

Access control is per application identity on protected `.vdbx` databases
(ADR 0021). The session opens databases as the identity its configuration
names (`fylo.identity` or `FYLO_IDENTITY`, pinned with `fylo.trustAnchor` or
`FYLO_TRUST_ANCHOR`, or `--identity` and `--trust-anchor`); a request never
names one.

Every handshake reports top-level `identity`: `null` when no identity is
configured, otherwise an object with

- `id`: the identity's `fid1:` identifier, and `publicKey`, its `fpk1:` key;
- `trust`: `pinned` (with `trustAnchor`, the pinned `fid1:` admin),
  `firstUse` or `unpinned`;
- `database`: `null` unless the session's root is a protected `.vdbx`
  database, otherwise what this identity may do there: `trustAnchor` (the
  admin that created it), `trustedOnFirstUse`, `admin`, `grants`, each
  collection the identity may use mapped to `read`, `write` or `admin`, and
  `catalogRotationPendingSince`: `null`, or the policy version of a
  revocation whose catalog-key rotation stopped before it finished, which
  an admin settles with `fylo access settle` or its next write. When
  the database refuses the identity, `database` holds only `error`, with the
  stable `code` (`EACCES`, `EPOLICY`, `ESIGNATURE`) and a `message`.

The handshake opens the root to report this; it takes no write lease.

## Removed: per-request `access` (POSTIX)

The per-request `access` object (`uid`, `gid`, `mode`, `groups`) and the
`capabilities.machineAccess` capability were removed with POSTIX (ADR 0021,
decision Q1). A request that still carries `access` is refused with
`EBADREQUEST`, so a client that relied on per-record rules learns of the
change instead of losing them silently. Records an earlier release wrote
with a descriptor are read in full; the descriptor is ignored, and the
first such read in a process prints a one-time warning to stderr. See
[migrating to protected databases](../../../docs/migrating-to-protected-databases.md).
