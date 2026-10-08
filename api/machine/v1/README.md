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
reads until EOF and flushes while its session owns leases. FYLO's proposed general
serverless entry point must accept exactly one bounded core frame, acquire its
own lease, keep leased handles private and buffer its result until checked
teardown. Existing streaming APIs retain their behavior. That runtime API is a
separate scoped source/compatibility slice, not implemented by this flag.
