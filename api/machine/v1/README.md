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

## Namespace (`namespace`, ADR 0033)

Every request may carry the optional string field `namespace`: the
namespace (like a Postgres schema) the request works in. A request without it
uses the session's default: `fylo exec --namespace <ns>`, else `public`. The
handshake reports the default as `storage.namespace`.

A name is 2-64 bytes of lowercase `a-z`, digits and `-`, starting and ending
with a letter or digit; Windows device names and FYLO's reserved words are
refused. A bad name fails the frame with `ENAMESPACE` (not retryable) before
the operation runs; a value that is not a string fails with `EBADREQUEST`.
Until the layout release the engine stores every collection in the root's
existing layout, so the field is checked but does not yet select a folder.

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

A local root on the system disk is refused the same way, with `ESYSTEMDISK`
(not retryable), unless the opt-out (`--allow-system-disk`,
`FYLO_ALLOW_SYSTEM_DISK=1`, `fylo.allowSystemDisk`) or app mode (`--app-id`,
`FYLO_APP_ID`, `fylo.appId`) admits it
([ADR 0032](../../../docs/adr/0032-primary-root-on-a-data-disk.md)). The
refusal is always an error frame on stdout: the first frame with
`--exclusive-root`, otherwise the first request that opens the root; a
handshake alone opens no root and answers `ok`. `rootLock` reports
`systemDisk` (`true`, `false`, or `null` when it was not looked up: no
default root, a remote root, the opt-out, app mode, or FYLO could not tell)
and `placement`, why the root is admitted (`dataDisk`, `systemDiskAllowed`,
`appOwned`), or `null` when it would be refused. An invalid
`FYLO_ALLOW_SYSTEM_DISK` value or app id stops the CLI at start with a
message on stderr and exit code 1.

A root whose parent folder is not a mount point is refused the same way,
with `ENOTVOLUMEROOT` (not retryable), before its disk is read
([ADR 0033](../../../docs/adr/0033-primary-layout-on-a-data-drive.md)
section 5). The message names the nearest mount point above the root. The
opt-out waives it; app mode admits the app's own folder. `storage.volume`
reports the mount point of the default root's parent folder, or `null`
without a default root or when FYLO cannot tell.

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
<json|@path|-> --deadline-ms <n> --cleanup-reserve-ms <n>`, with optional
`--max-request-bytes <n>` and `--max-response-bytes <n>` frame limits: one
request per process, the same answer frame and exit codes as `exec --request`,
and a failure frame (`EROOTLOCKED`, `EREMOTEROOT`, `ESYSTEMDISK`, `EDEADLINE`,
`EBADREQUEST`, `EFRAME_REQUEST_TOO_LARGE`, `EUNSUPPORTEDOP`, `EINVOCATION`)
where the library
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
    "options": [
        "--request",
        "--deadline-ms",
        "--cleanup-reserve-ms",
        "--max-request-bytes",
        "--max-response-bytes"
    ],
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
`evictions`, `refusals`, `clears`, and for kept query results (ADR 0030)
`queries`, `queryHits` and `queryMisses`.

The hot set also keeps the complete match list of each `findDocs` query
without `$sort` that it evaluated to the end: the ids of every matching
document, in order, under the same budget and clear rules. A repeated query,
with any `$limit`, reads its first ids from that list instead of the index,
then reads each document as before. A query cut short by its `$limit` is not
kept. Match lists live only in memory. The counters are for operators; results
never depend on them. Frames in one session run one at a time, reads
included; a frame that is not a read clears the hot set before and after it
runs.

## Temp-disk read tier (`capabilities.tempCache`)

ADR 0030 adds a read cache in the OS temp folder for roots on slow storage.
It keeps the stored records of files-layout document collections exactly as
the root holds them (encrypted fields stay encrypted) and checks every entry
against the collection before it uses one: an entry read under the
collection's current stamp (its `state.json`, that file's identity and the
collection's incarnation) is used as it is; one read under an older stamp is
used only when the document file on the root (and, in the EFS profile, its
`.fylo-attrs` manifest) is still the same file. An EFS-profile entry also
keeps the record's verified manifest, and a hit checks that the manifest
binds the payload to the record, as a root read does. Any problem with an
entry is counted and the read goes to the root. In the default `auto` mode
the tier is active only for a root opened with the EFS profile (the only
way a root may sit on a network filesystem) whose cache folder is private,
local, on another device and has 64 MiB free; `on` turns it on for any
folder root.

`--temp-cache <off|auto|on>` (default `auto`), `--temp-cache-bytes <n>`
(default 256 MiB, never more than half the free space) and
`--temp-cache-dir <path>` set it for every `exec` mode, as do
`fylo.tempCache.mode`, `.bytes` and `.dir` in `application.json`. A request
never changes them. The cache folder is `fylo-cache-v1` in the base folder
(`fylo-cache-v1-<uid>` on Linux and Android). It is private to the OS user:
mode `0700` (Windows: a protected DACL for the current user only). A folder
that is a link, belongs to another user or that other users may reach is
refused and left unchanged.

Every handshake reports top-level `capabilities.tempCache`: `false` under
`--temp-cache off`, otherwise an object with `mode`, `active`, `reason`,
`dir`, `budgetBytes`, `usedBytes` and `entries` (from the last sweep), and
the counters `hits`, `misses`, `revalidations`, `rejected`, `writes`,
`evictions` and `errors`. `reason` says why the tier is not active:
`no root`, `vdbx`, `root is local` (`auto` with a root not opened with the
EFS profile), `unsafe folder`, `cache folder is remote`, `same device`,
or `low space`. `auto` creates nothing for a local root.
The counters count this process's reads: `hits` answered from the tier,
`misses` it could not answer, `revalidations` of entries read under an older
stamp, `rejected` entries refused and deleted, `writes` of new entries, and
`errors` of failed cache reads and writes.

`fylo cache status [--json]` prints the folder, its entries and bytes, its
partial files and the last sweep; `fylo cache clear` deletes the folder.

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
