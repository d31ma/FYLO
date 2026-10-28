# EFS profile qualification

`efs-sidecar-v1` is an isolated source candidate. Its authored tests establish
neither native execution nor EFS/Lambda support. This plan specifies required
evidence; it authorizes no resources, IAM changes, credentials, production reads
or migration. See [ADR 0010](../adr/0010-explicit-efs-metadata-profile.md).

## Local candidate gate

Pin the synthetic baseline, tests-only checkpoint, adapter patch, source hashes,
tool versions and offline dependency closure. Obtain a coordinated bounded
native lane with the existing supervisor, owned cleanup, log/disk budgets and
truthful memory policy. Do not install dependencies to repair a missing closure.
Source review and parsing are not compilation or actual failing behavior tests.

Run storage, engine, machine and main-CLI regressions on disposable roots. Cover
explicit selection, empty-root initialization, native defaults, mismatch refusal
before mutation, actual-root contention/loss, private mandatory manifests,
malformed/unsafe paths, immutable UID/GID and schema/encryption/frame compatibility.
Preserve the native crash gate. Run the separate EFS-profile main-CLI gate with
interruptions between paired transitions and before/after commit. Compare full
payload, metadata, modes and recorded mtime; recover twice. Include retained
deletes, SQL deletion, reshard, staged version operations, history and full-root
backup/restore.

Distinguish exact journal/snapshot time recovery from historical commit
materialization. Existing version-2 metadata objects do not store historical
payload mtime; require existing native VCS semantics rather than inventing fields
or silently weakening rollback and snapshot comparisons.

The v2 binding contract is accepted only for this isolated unreleased candidate.
Require reciprocal v1/v2 reader refusal without mutation, same-payload/time
cross-record and cross-collection swaps, every binding field/encoding, payload
drift, missing binding and metadata-write laundering. Verify reserved-binding
retention and unchanged custom-key/byte limits. Exercise valid and corrupted
before-image pairs, partial capture, VCS staging/rollback/rollforward, deleted
records, idempotent recovery and quiesced copies. Native defaults and metadata
bytes must remain compatible. Source checks do not qualify these behaviors;
the historical local pass applies only to its pinned older checkpoint.
Unsupported-lock refusal needs an actual
controlled filesystem fixture; supported local contention is not that test.

CHEX schema validation runs in-process, so FYLO starts no validator process
and there are no validator children or grandchildren to tear down. The local
fixture now checks in-process validation, machine exit, and stdio closure.
Keep the native supervisor's zero-survivor check throughout qualification.

The uncompiled invocation successor contains this risk by refusing explicit
`schemaValidate` requests and strict-schema host configuration before lease
acquisition. These refusal and native-compatibility regressions need their own
admitted lane. Embedded schema materialization remains separate from external
validator lifetime qualification.

## Actual EFS gate

A separately authorized disposable environment is required. Lambda can mount
EFS; this alone does not establish FYLO compatibility.
[AWS file-system configuration](https://docs.aws.amazon.com/lambda/latest/dg/configuration-filesystem.html).

Retain actual region, filesystem/access-point identity, mount/client/kernel
versions, mount options, function image digest, Lambda settings, IAM policy and
network scope. Use synthetic data and the reviewed access-point identity.
Confirm object ownership/modes and unchanged state after unsupported ownership
transitions.

Use genuinely independent clients to verify contention, visibility, canonical
aliases, lock loss/reclaim and client death. All writers must cooperate: EFS
advisory locks do not prevent bypassing ordinary writes.
[AWS EFS limits](https://docs.aws.amazon.com/efs/latest/ug/limits.html).
Owner metadata alone cannot establish current NFS lock fencing.

Test the collection-lock `rootOwner` generation: a held current root lease may
recover prior-generation locks, while a same-generation contender must refuse.
Confirm no remote reclaim decision relies on a local PID or boot identifier.

Exercise mounted-cache visibility of generation markers, payloads and manifests.
No reader may combine public authorization with a later private payload or
metadata. Stable missing/corrupt metadata is an error. Record retries/failures;
uncertainty cannot become success. Interrupt every pair and commit boundary,
recover repeatedly, terminate clients and measure bounded admission failure.
Network/lock-loss drills need their own specific safe scope.

Verify the exact client/server directory-flush and rename durability behavior.
The inherited sync helper tolerates `Unsupported`; a successful local call or
source inspection cannot establish durable remote publication.

Measure cold/warm duration, peak memory, admission delay, filesystem latency,
read/write volume, request/response sizes and recovery time against the actual
invocation deadline. Prove work completes before response freeze. Background
callbacks and process-local loops need separate bounded durable designs.
Reserved concurrency supplies no cross-function ownership guarantee.

## Restore and promotion

Quiesce every writer and snapshot the complete root: descriptor, sidecars,
catalog, journals, VCS and queues. Preserve modes/times and separately protected
keys. Restore into a different empty root and compare inventories before opening
with the matching profile/binary. Recover twice; verify indexes, reachable
history, a non-empty query/revocation corpus and queue readability without
claiming or acknowledging messages. Follow the
[snapshot runbook](filesystem-snapshot-restore.md).

Retain a structured receipt with exact inputs, attempted checks, failures/skips,
cost/latency measurements and bounded logs. Local tests, provider validation,
cached images and successful mounts do not promote support. Deployment remains
an independent review of concrete data, IAM and cost scope.
