# S3 backup operations

FYLO's local root remains authoritative. The S3 target is a recoverable mirror,
so an S3 failure never means the local write was rolled back.

## Inspect

Call `fylo.backupStatus()` for the current state (`idle`, `running`, `failed`,
or `closed`), attempted-pass count (`runs`), and last start, success, or failure
details. `runs` includes failed attempts; use `lastSuccessAt`, `lastFailureAt`,
and `lastError` to interpret it. The method returns `undefined` when built-in S3
backup is not configured.
Forward `backup.reconcile.*` and `backup.retry` events from `onEvent` to the
service's metrics/log pipeline. Alert when scheduled passes remain failed past
the recovery-point objective or retry volume increases materially.

A compiled supervisor exposes the same state without importing JavaScript:

```sh
fylo exec --loop --root /srv/fylo \
  --exclusive-root \
  --backup-bucket fylo-backup \
  --backup-prefix production/root-a \
  --backup-endpoint https://s3.example.internal
```

Send `{"op":"backupStatus"}` for non-mutating inspection and
`{"op":"backupReconcile"}` to run and await one coalesced pass. The handshake
reports whether backup is configured and advertises these operations. Bucket,
prefix, endpoint, region, and limits are process startup configuration;
credentials remain exclusively in `AWS_*`/`FYLO_S3_*` environment variables.
An unconfigured reconcile returns `EBACKUPNOTCONFIGURED`.

## Recover a failed pass

1. Confirm local storage is healthy and has capacity.
2. Check the last error and retry events. Credential, authorization, invalid
   prefix, and file-size failures require configuration repair; they are not
   retried automatically.
3. Correct the cause, then run `await fylo.reconcile()`. A failed pass does not
   wedge the scheduler; a later manual or scheduled pass can recover.
4. Confirm `lastSuccessAt` advances and the failed alert clears.

With the default `syncMode: 'await-sync'`, mirroring begins only after the local
collection transaction commits. A mirror failure is reported to the
writer after the local filesystem change has committed. With
`syncMode: 'fire-and-forget'`, monitor `sync.failed` because the write caller
does not wait for the mirror. A later reconcile repairs either case.

## Capacity and shutdown

- `concurrency` is the hard ceiling for simultaneous S3 requests.
- `maxFileBytes` is the hard per-file memory/request boundary. Increase it only
  after accounting for the process memory budget and configured concurrency.
- `maxManifestBytes` bounds metadata fetched from the remote trust boundary.
- `maxReconcileSnapshotBytes` caps the immutable local snapshot materialized by
  a reconcile pass. It defaults to 512 MiB and counts file bytes plus encoded
  xattr values across the whole root.
- `retry.attempts`, `baseDelayMs`, and `maxDelayMs` bound transient retries.
- Always `await fylo.close()` during graceful shutdown. It refuses new backup
  work, cancels retry waits and the one pending reconcile, drains active remote
  requests, and only then releases the pinned root descriptor.

`maxReconcileSnapshotBytes` is not an RSS limit. Size the process for the cap
plus map/object overhead, one additional validation snapshot up to
`maxFileBytes`, concurrent request buffers, and the Bun runtime. Measure peak
RSS with production-like file counts and xattr density before raising it. If a
pass fails with `S3 reconcile snapshot exceeds
sync.s3.maxReconcileSnapshotBytes`, confirm the failure in `backupStatus()` and
the `backup.reconcile.failed` event. Then reduce the root's retained data or
split independent datasets into separate FYLO roots; lower `concurrency` if
request buffers are the pressure source. Raise the cap only after increasing
the service/container memory limit and validating headroom. A later manual
`await fylo.reconcile()` retries the entire immutable snapshot; no partial
remote mutation is promoted from the rejected pass.

Reconciliation deletes remote objects that have no local counterpart. Keep the
default requirement for a unique `prefix` per FYLO root. Use
`allowBucketRoot: true` only for a dedicated bucket with a least-privilege IAM
identity; never point two roots at the same reconciliation scope.

Manifest version 2 records the source platform, SHA-256 and size, native
mode/mtime, and FYLO metadata captured from the same pinned descriptor as the
bytes. POSIX xattrs and NTFS alternate data streams are restored only on the
same platform family. Cross-family recovery fails explicitly instead of
silently discarding UID/GID or NTFS access semantics.

For disaster recovery from S3, follow the recovery runbook in the main README.
Use `fylo backup verify` before `fylo backup restore`, always target a new
destination, and retain the old root until application verification succeeds.
Restore acquires a canonical root reservation before listing S3, stages with
private permissions, and removes staging data after failure. A competing
exclusive machine owner returns `EROOTLOCKED`.

## Release gate

Release publication depends on the reusable live S3-compatible workflow. It
builds a release-identified native executable, exercises more than one provider
listing page against pinned MinIO, covers documents, raw files, developer
metadata, tombstones, reconcile/status, offline verify/restore, and proves that
provider-side corruption is detected. The workflow retains the binary digest,
runtime identity, provider version, filesystem type, isolated prefix, and test
log as release evidence.
