# ADR 0010: Explicit EFS metadata profile

Status: accepted for isolated candidate source preparation. Native execution,
format qualification, migration and production support remain pending.

## Context

FYLO's native Unix profile uses xattrs. EFS lacks xattrs and provides advisory
locks, so a mount alone cannot establish FYLO's metadata or transaction
guarantees. [AWS EFS limits](https://docs.aws.amazon.com/efs/latest/ug/limits.html).
The selected deployment direction is EFS. This decision preserves native
defaults and public `RootConfig` struct literals; it authorizes no live conversion.

FYLO owns the broader provider-independent serverless runtime contracts. This
inside-FYLO metadata backend is one first slice; it is not an external AWS shim
or a complete invocation/fencing/retry contract. PaaS retains AWS infrastructure.

## Decision

`FilesystemProfile` selects `native` or `efs-sidecar-v1`. Unknown values fail
before storage work. Additive `RootOpenOptions` constructors carry the payload
root, optional repository root, profile and owning `RootLease`; existing opens
retain native behavior. CLI selection is explicit through
`--filesystem-profile efs-sidecar-v1`. A machine session keeps that profile for
recovery, queries, writes and version operations.

An EFS root has `.fylo-catalog/filesystem-profile.json` with exactly `format`,
`profile`, `uid` and `gid`, described by the
[profile schema](../../api/storage/v1/filesystem-profile.schema.json).
`format` is `fylo.filesystem-profile.v2`; `profile` remains `efs-sidecar-v1`.
The isolated bound candidate refuses all unbound experimental v1 descriptors
before lease publication or recovery. Fresh empty roots and matching v2 roots
are its only admissions; no adoption or conversion is implemented.
UID/GID record the actual immutable root identity, not replacements for OS
ownership. The deployment proposes EFS access-point identity 1001/1001; storage
does not hard-code it or claim it as a measured production property.

Only an explicitly selected writer initializes a genuinely empty root under
its verified owning lease. Readers never initialize it. Marked roots refuse
native or mismatched opens; existing unconverted roots are not adopted.
Worktrees inherit the repository descriptor and owning lease. EFS acquisition
rejects unsupported kernel locking before owner metadata publication.
Operations verify owning identity; remote recovery cannot infer client liveness
from a local PID.

EFS collection locks carry the optional `rootOwner` generation. A currently
asserted cooperative root lease permits reclaiming a prior generation; it does
not permit reclaiming a collection lock from the same live generation. Native
lock serialization omits this field and retains its existing liveness policy.

Every record has a mandatory `<payload>.fylo-attrs` file using the existing
sorted attribute-name/base64 JSON representation. Reserved
`user.fylo.record-identity` contains canonical compact UTF-8 JSON
`["fylo.record-identity.v1",namespace,collection,ttid,sha256]` inside its base64
value. Namespace is `.collections` or `.buckets`; the digest covers persisted
payload bytes, including ciphertext. Canonical collection and TTID bind a
record independently of root, shard, deleted position and branch/staging path.
Removing the final user attribute retains this protected binding rather than
literal `{}`. The reserved value has its own 512-byte bound and does not reduce
existing custom-attribute budgets; the outer manifest limit remains. EFS errors never trigger native
or empty-metadata fallback. Missing, malformed, oversized, duplicate-name,
noncanonical-base64, nonregular or symlinked manifests are errors. Reads verify
payload and manifest file identities, identity binding and actual returned
payload bytes before access decisions or metadata exposure. Hashing uses
bounded offset-preserving reads. Metadata scratch and manifest files use
mode 0600 under the trusted engine identity. Payload scratch starts with restricted
owner permissions and gains its intended mode before publication.

Payload, canonical metadata, custom attributes and authorization belong to one
stable-generation read attempt. Selected attribute operations, capture/restore
and version materialization use one backend. Moves, deletes and reshard capture
source and destination payload/sidecar pairs. The existing journal, commit
marker and generation barrier provide logical atomicity; two renames do not.
Recovery restores recorded payload mtime and must be idempotent. The existing
capture format gains no UID/GID fields.

Retained payload/manifest descriptors and the engine's before/after generation
checks detect changes across that read attempt. They do not make two files an
OS-atomic read. The existing directory-sync primitive tolerates `Unsupported`;
remote durable publication and directory-flush behavior remain qualification
requirements rather than guarantees established by this source slice.

The initial profile requires uniform immutable root UID/GID and refuses
unsupported cross-owner changes before mutation. Modes retain their meanings;
the initial sidecar backend refuses projections without owner-read capability
before record mutation, rather than publishing an unreadable payload and failing
its metadata reopen. Native mode behavior is unchanged. An access marker does
not replace OS enforcement. Sidecars are excluded from
logical counts and staged TTID lookup. Existing version-2 metadata objects retain
their representation, excluding the checksum cache. Backups include the entire
root, sidecars and native modes/times.

Existing version-2 metadata objects contain attributes, not historical payload
mtime or an independent native mode field. Branch-copy paths preserve current
metadata/times; commit materialization retains the existing native VCS time
semantics. Exact rollback mtime and full-snapshot restore fidelity remain required.
This candidate adds no historical timestamp guarantee to VCS.

## Compatibility and unresolved gates

The prior candidate's exact v1 check rejects v2; the new candidate rejects v1.
This is a stored-format refusal boundary, not a runtime compatibility pass.
Unrelated old binaries cannot be assumed to honor the descriptor. Restrict them
to their original native roots. A new v2 reader would refuse an existing v1
experimental root, even if it contains real user data; its bytes remain intact.
No live-root inventory, in-place upgrade or automatic reverse conversion exists.
Separately authorized cold migration must compare payloads, metadata, history,
queries and permissions, keeping the original root and prior binary for rollback.

Kernel-mediated advisory-lock acquisition and owner metadata are admission
checks, not remote fencing. Independent-client visibility, lock loss, interrupted
mutation, restore fidelity and Lambda latency need actual qualification.
Reserved concurrency 1 does not fence another function, process or old binary.

The accepted isolated v2 source adds record/payload binding. Old pairs are
verified before overwrites, then the digest is refreshed within the existing
transaction. Valid delete/restore/reshard moves preserve identity. Version
export/materialization and before-image recovery verify pairs before publication
or restoration; missing/unbound history is refused instead of inferred.
No journal schema, native metadata bytes or stored revisions are silently
rewritten. Binding is unkeyed consistency checking, not authentication against
an owner changing both halves. Native and remote behavior remain unqualified.

Retain tests before repair. Authored tests are not an actual RED run. Compilation,
native regressions and crash gates require their own qualified lane. Follow the
[EFS qualification plan](../operations/efs-profile-qualification.md) before
claiming remote support or proposing deployment. Failure preserves state and
evidence and never falls back to absent metadata.

Queue execution currently fails closed at the native queue-open seam. Its source
file is outside the initial adapter grant, so the EFS handshake reports queue
capability false. Preserve the full queue tree in backups while scoped queue
integration and its qualification remain pending.
