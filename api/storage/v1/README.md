# FYLO Storage Contract v1

FYLO v1 stores one compact JSON object per document. The local filesystem is
authoritative; indexes, caches, and query snapshots are derived.

Canonical identifiers:

| Boundary             | Identifier                           |
| -------------------- | ------------------------------------ |
| Document body        | `fylo.document.json.v1`              |
| Prefix index         | `fylo.local-fs.index.v1`             |
| Transaction journal  | `fylo.collection-transaction.v1`     |
| Generation state     | `fylo.collection-generation.v1`      |
| Backup file manifest | numeric version `2`, platform tagged |

The schemas in this directory describe envelopes with explicit manifests.
Existing v1 document bodies do not contain a wrapper or version field; their
format identity comes from the release/storage compatibility manifest.

## Document rules

- root must be a JSON object;
- nested objects are allowed;
- arrays may contain scalar values;
- arrays of objects are rejected with `EARRAYOFOBJECTS`;
- encoded and decoded resource limits are applied before storage/query work;
- JSON object insertion order is preserved when unchanged bytes are required;
- encrypted values remain part of the document representation and fail closed
  when they cannot be decrypted.

## Canonical metadata

Custom metadata and canonical storage metadata are returned together.
Canonical fields win collisions:

- `id`;
- `createdAt`;
- `updatedAt`;
- `mtime`;
- raw-file descriptors;
- supported native `uid`, `gid`, and `mode`.

## Compatibility

Any new writer format requires an ADR, migration RFC, interrupted-upgrade
tests, rollback/restore policy, and golden fixtures. A Rust language migration
does not by itself change these formats.

## Explicit EFS sidecar candidate

Native remains the default. `efs-sidecar-v1` is an explicit, unqualified source
candidate, selected through additive profiled constructors or the CLI flag.
Metadata representation is separate from FYLO's planned provider-independent
invocation lifetime, retry and concurrency contracts; it does not by itself
make a backend serverless-safe.
It never infers a backend from an xattr error. The
[descriptor schema](filesystem-profile.schema.json) describes
`.fylo-catalog/filesystem-profile.json`: `format`, `profile`, and actual immutable
root `uid`/`gid`, with unknown/missing/mismatched fields rejected. Only a selected
writer initializes a genuinely empty root under its verified owning lease;
readers do not initialize. Worktrees use the repository descriptor and lease.

Every payload requires a regular private `<payload>.fylo-attrs` manifest using
the existing sorted attribute-name/base64 map. Attribute removal leaves `{}`.
Reads reject missing, malformed, unsafe or oversized manifests and verify both
file identities inside the same stable-generation envelope as metadata and
authorization. Moves, deletes, SQL mutations, reshard and journal recovery
treat payload and sidecar as a logical pair, including destination before-images.
Two filesystem renames alone do not supply joint atomicity.

Existing version-2 metadata objects and transaction captures keep their formats.
No general UID/GID capture expansion is included. Root ownership is fixed;
unsupported transitions fail before mutation. The initial sidecar backend also
refuses modes lacking owner-read capability before mutation. Payload mtime remains
canonical and recorded mtime must be restored during recovery. Sidecars do not appear in
logical record counts, staged lookup or custom document bodies.

The version-2 metadata object has no historical payload-mtime or standalone
mode field. Commit materialization retains native VCS time semantics; this
profile does not invent historical timestamps. Journal rollback and full-root
snapshot/restore still require recorded-time fidelity, and unchanged pair moves
and branch copies preserve current payload time.

Identical payloads with matching timestamps cannot be distinguished after a
completed manifest swap by the pinned checksum representation. Record binding
requires a reviewed extension. Queue execution also remains unqualified and
fails closed at the native queue-open seam. Neither limitation may be hidden
by fallback or a deployment support claim.

Old readers are not guaranteed to inspect this descriptor. Keep old binaries on
the original root. Migration must be a separately authorized cold copy with
complete metadata/history/query/permission comparison; retain the original root
and binary for rollback. Backups include descriptor, every sidecar, journals,
VCS, queues and native modes/times, plus separately protected keys. See
[ADR 0010](../../../docs/adr/0010-explicit-efs-metadata-profile.md) and the
[snapshot runbook](../../../docs/operations/filesystem-snapshot-restore.md).
