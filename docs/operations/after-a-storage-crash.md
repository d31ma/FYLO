# After a storage crash

Use this runbook when the storage under a FYLO root lost power or crashed
while FYLO wrote to it. The main case is a network share exported with
`async` (for example an NFS server that acknowledges a flush before the data
is on its disks): such a crash can lose the last seconds of writes, possibly
out of order, so a file can lose its tail, keep its size and time but not its
bytes, or appear under its new name without its data.

FYLO cannot see this loss when it happens. It finds it when it reads:

- A document that is no longer valid JSON is refused. The error names the
  file, for example
  `damaged FYLO document file /data/app/.collections/items/docs/4V/4VRNF530001.json: ...`.
- A stored file whose bytes no longer match its recorded checksum is refused
  with `ENATIVE_METADATA` and its path.
- An index that no longer matches its documents makes `fylo verify` report
  `ENATIVE_INDEX` for that collection.

FYLO never returns damaged data and never guesses its content. Documents are
the source of truth: indexes and other derived data can be rebuilt from them,
a lost document cannot.

## Steps

1. **Stop every writer of the root.** No restore is ever done under a running
   owner.
2. **Open the root once and verify it.**

    ```bash
    fylo verify --root /data/app
    ```

    The command first finishes any write the crash interrupted (an unfinished
    transaction rolls back, a committed one rolls forward), then reads every
    document and file. It prints a JSON report and exits with a non-zero code
    when any file is damaged:

    ```json
    {
        "root": "/data/app",
        "ok": false,
        "collectionsChecked": 2,
        "filesChecked": 5120,
        "damaged": [
            {
                "collection": "items",
                "file": "/data/app/.collections/items/docs/4V/4VRNF530001.json",
                "code": "ENATIVE_DOCUMENT",
                "message": "damaged FYLO document file ...: EOF while parsing ..."
            }
        ]
    }
    ```

    A health check or a script can use the exit code alone.

3. **Restore the named files** from a backup taken before the crash (for a
   btrfs pool: a snapshot; otherwise another copy). Restore only the files the
   report names, to the same paths.
4. **Rebuild each collection the report named**, so its index matches its
   documents again:

    ```bash
    fylo rebuild <collection> --root /data/app
    ```

5. **Verify again** with `fylo verify --root /data/app`. Start the writers only
   when it exits with code 0.

## Restoring a whole snapshot

A snapshot of a whole storage pool taken at one instant (a btrfs or ZFS
snapshot) looks to FYLO exactly like a power cut at that instant, so no pause
is needed while it is taken. To restore one:

1. Stop every writer of the root.
2. Put the snapshot in place.
3. Run `fylo verify --root <root>` (step 2 above). `fylo rebuild` is needed
   only for a collection the report names.

The restored lease record still names the owner that ran when the snapshot
was taken. The first new owner takes the root over after the takeover time
(20 seconds by default, `FYLO_LEASE_TAKEOVER_SECONDS`), or at once when the
record names this machine and that owner's process has ended.

## What is out of FYLO's scope

Backups. A mirrored pool protects against a failed disk, not against lost
writes, so a root on an `async` export needs its own snapshots or copies.
