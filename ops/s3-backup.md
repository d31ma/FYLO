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

## Recover a failed pass

1. Confirm local storage is healthy and has capacity.
2. Check the last error and retry events. Credential, authorization, invalid
   prefix, and file-size failures require configuration repair; they are not
   retried automatically.
3. Correct the cause, then run `await fylo.reconcile()`. A failed pass does not
   wedge the scheduler; a later manual or scheduled pass can recover.
4. Confirm `lastSuccessAt` advances and the failed alert clears.

With the default `syncMode: 'await-sync'`, a mirror failure is reported to the
writer after the local filesystem change has committed. With
`syncMode: 'fire-and-forget'`, monitor `sync.failed` because the write caller
does not wait for the mirror. A later reconcile repairs either case.

## Capacity and shutdown

- `concurrency` is the hard ceiling for simultaneous S3 requests.
- `maxFileBytes` is the hard per-file memory/request boundary. Increase it only
  after accounting for the process memory budget and configured concurrency.
- `maxManifestBytes` bounds metadata fetched from the remote trust boundary.
- `retry.attempts`, `baseDelayMs`, and `maxDelayMs` bound transient retries.
- Always `await fylo.close()` during graceful shutdown. It refuses new backup
  work, cancels retry waits and the one pending reconcile, drains active remote
  requests, and only then releases the pinned root descriptor.

Reconciliation deletes remote objects that have no local counterpart. Keep the
default requirement for a unique `prefix` per FYLO root. Use
`allowBucketRoot: true` only for a dedicated bucket with a least-privilege IAM
identity; never point two roots at the same reconciliation scope.

Built-in backup and S3 restore both fail closed on Windows because ADS metadata
cannot yet be captured and restored with the required descriptor guarantees.

For disaster recovery from S3, follow the recovery runbook in the main README
and verify into a new destination before promotion.
