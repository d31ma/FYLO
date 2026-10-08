# Filesystem Snapshot and Restore

FYLO is filesystem-only (ADR 0007). Disaster recovery copies a quiesced root
with a platform tool that preserves file bytes, directory structure, ownership,
permissions, timestamps, extended attributes, and NTFS alternate data streams.
Object-storage and cloud-drive synchronization are not FYLO transaction or
restore protocols.

## Snapshot

1. Stop every process that can open the root for writes. Do not snapshot a
   live root merely because its document files look idle.
2. Record the FYLO binary identity, source path, filesystem type, snapshot tool
   and version, wall-clock time, and the root's byte count.
3. Copy the entire root, including `.fylo-catalog`, `.fylo-transactions`,
   `.fylo-queue/v1`, `.fylo-vcs`, `.collections`, and `.buckets`. Queue
   messages, leases, consumer progress, dedupe records, and dead letters are
   durable state; do not copy only documents.
4. Preserve native metadata. Suitable qualified profiles are:
   - Linux: `rsync -aHAX --numeric-ids` on a local filesystem;
   - macOS: an APFS snapshot/clone or `ditto` with resource forks, ACLs, and
     extended attributes preserved;
   - Windows: VSS or `robocopy /COPYALL /DCOPY:DAT /XJ` between NTFS volumes.
5. Generate a read-only inventory of relative paths, types, sizes, content
   hashes, and supported native metadata. Retain it beside the snapshot.
6. Reopen the source with the same binary only after the copy and inventory
   have completed.

Network shares and synchronized folders are not qualified by these profiles.
Copy tools must be tested on the actual source and destination filesystems;
their flags do not prove that a provider preserves xattrs or ADS.

## Restore drill

1. Choose a new, empty local path. Never restore over the source root.
2. Restore the complete snapshot with the corresponding metadata-preserving
   profile.
3. Compare the restored inventory with the retained snapshot inventory before
   opening FYLO.
4. Open the restored root exclusively. Startup recovery may complete a journal
   captured at a documented durable boundary; repeat the open to prove recovery
   is idempotent.
5. Verify every collection index against its documents, rebuild derived indexes
   when required, and verify the reachable version history and object hashes.
6. Verify known queue topic/group pairs with `queueStats` and
   `queueDeadLetters` without claiming, acknowledging, or rejecting messages.
   FYLO does not yet expose a whole-queue offline verifier, so this check proves
   that recorded queue state is readable but does not replace the exact path,
   size, metadata, and content-hash inventory comparison in step 3.
7. Run the representative non-empty query and permission corpus. Encryption
   keys remain outside the snapshot and must be supplied through the normal
   secret boundary.
8. Record elapsed restore time, recovered bytes, verification results, binary
   identity, filesystem, and any metadata that the platform cannot preserve.

A restore is accepted only when integrity and query-equivalence checks pass.
Failure leaves both source and failed restore intact for investigation; retry
into another empty path.

## Release evidence

Production promotion requires retained Linux, macOS, and Windows drill reports
for the exact candidate artifact and qualified filesystem. A local successful
copy documents implementation readiness but does not promote a support tier.

## Unqualified EFS sidecar candidate

`efs-sidecar-v1` adds requirements to this inventory without promoting network
filesystems to a support tier. No actual EFS/Lambda restore drill has qualified
the isolated candidate. Follow its
[qualification plan](efs-profile-qualification.md) before proposing support.

Quiesce every writer, including other functions and old binaries; reserved
concurrency on one Lambda function is insufficient. Preserve the root descriptor
`.fylo-catalog/filesystem-profile.json`, every payload and mandatory
`<payload>.fylo-attrs` manifest, retained tombstones, catalog/index state,
transaction before-images, VCS objects/staging and complete queue state.
Sidecar files are durable metadata even when they contain `{}`. Preserve actual
UID/GID, modes and timestamps for payloads and sidecars. EFS lacks xattrs; that
does not permit dropping sidecars during a snapshot or native migration.

Keep deployment encryption keys, issuer/session material and external restore
configuration under their existing protected boundary. A complete root snapshot
does not authorize exporting these values or replacing them with bare-root mode.

Restore into a separate empty root with the same selected profile and matching
binary. Compare exact path/type/size/hash and supported native metadata before
open, recover twice, verify indexes/history and compare non-empty queries plus
access/revocation results. Queue execution is currently blocked at the native
queue-open seam, so preserve and inventory its bytes without claiming an EFS
queue-readability pass. Retain this gap until the queue adapter is qualified.

Cold migration and any reverse conversion need separate owner-reviewed tooling
and authorization. Retain the original native root and exact prior binary for
rollback; never allow an older writer to open the new marked root. Failed
comparison or uncertainty leaves both roots intact.
