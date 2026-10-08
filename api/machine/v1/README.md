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

## Compatibility

Within protocol v1:

- operation names and stable error codes are preserved;
- additive result fields are allowed;
- clients must ignore unknown result fields;
- request fields remain operation-specific;
- changing idempotency, durability, or retry behavior requires a new protocol
  version or an explicit compatibility RFC.

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
