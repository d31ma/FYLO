/**
 * External-storage replication surface. Sync hooks fire on document writes
 * and deletes so consumers can mirror FYLO's local filesystem to S3, GCS, or
 * any other object/file backend they own.
 *
 * The local filesystem write is always the source of truth — sync hooks are
 * a replication aid, not a transactional commit.
 */

/**
 * @typedef {import('../types/vendor.js').TTID} TTID
 * @typedef {import('../observability/events.js').FyloEventHandler} FyloEventHandler
 * @typedef {import('./s3-backup.js').FyloS3BackupOptions} FyloS3BackupOptions
 * @typedef {import('../cache/query.js').FyloCacheOptions} FyloCacheOptions
 */

/** @typedef {'await-sync' | 'fire-and-forget'} FyloSyncMode */
/** @typedef {'off' | 'strict'} FyloWormMode */
/** @typedef {'put' | 'patch' | 'delete' | 'restore' | 'meta' | 'rekey'} FyloAutoCommitOperation */

/**
 * @typedef {(uid: number) => Iterable<number> | Promise<Iterable<number>>} FyloGroupResolver
 *
 * @typedef {object} FyloAccessOptions
 * @property {FyloGroupResolver=} groupsForUid trusted application-owned group
 *   membership resolver; Fylo never accepts group membership from an operation
 *   caller
 */

/**
 * @typedef {object} FyloWormOptions
 * @property {FyloWormMode=} mode
 */

/**
 * @template {Record<string, any>} [T=Record<string, any>]
 * @typedef {object} FyloWriteSyncEvent
 * @property {'put' | 'patch' | 'restore'} operation
 * @property {string} collection
 * @property {TTID} docId
 * @property {string} path
 * @property {T} data
 */

/**
 * @typedef {object} FyloDeleteSyncEvent
 * @property {'delete' | 'patch'} operation
 * @property {string} collection
 * @property {TTID} docId
 * @property {string} path where the record now lives (the `.deleted/` tombstone)
 * @property {string=} previousPath where the record lived before the soft
 *   delete (the live `docs/` path); used by the S3 backup to drop the old object
 */

/**
 * @template {Record<string, any>} [T=Record<string, any>]
 * @typedef {object} FyloSyncHooks
 * @property {(event: FyloWriteSyncEvent<T>) => Promise<void> | void=} onWrite
 * @property {(event: FyloDeleteSyncEvent) => Promise<void> | void=} onDelete
 * @property {FyloS3BackupOptions=} s3 built-in whole-root S3 backup, scoped to
 *   a required prefix unless allowBucketRoot is explicitly enabled
 */

/**
 * @typedef {object} FyloAutoCommitEvent
 * @property {FyloAutoCommitOperation} operation
 * @property {string} collection
 * @property {TTID} docId
 * @property {string} repositoryRoot
 */

/**
 * @typedef {object} FyloVersioningOptions
 * @property {boolean=} resolve
 * @property {boolean=} autoCommit
 * @property {string=} repositoryRoot
 * @property {((event: FyloAutoCommitEvent) => string)=} autoCommitMessage
 */

/**
 * @template {Record<string, any>} [T=Record<string, any>]
 * @typedef {object} FyloOptions
 * @property {FyloSyncHooks<T>=} sync
 * @property {FyloSyncMode=} syncMode
 * @property {FyloWormOptions=} worm
 * @property {FyloCacheOptions=} cache
 * @property {boolean=} queue
 * @property {FyloEventHandler=} onEvent
 * @property {FyloVersioningOptions=} versioning
 * @property {FyloAccessOptions=} access
 */

/**
 * Error raised when an await-sync hook fails after the local write has already
 * committed. The local filesystem remains the source of truth.
 */
export class FyloSyncError extends Error {
    /** @type {string} */
    collection
    /** @type {TTID} */
    docId
    /** @type {string} */
    path
    /** @type {string} */
    operation

    /**
     * @param {{ collection: string, docId: TTID, path: string, operation: string, cause: unknown }} args
     */
    constructor(args) {
        super(
            `FYLO sync failed after the local filesystem operation succeeded for ${args.operation} ${args.collection}/${args.docId}. Local state is already committed at ${args.path}.`,
            { cause: args.cause }
        )
        this.name = 'FyloSyncError'
        this.collection = args.collection
        this.docId = args.docId
        this.path = args.path
        this.operation = args.operation
    }
}

/**
 * @param {FyloSyncMode=} syncMode
 * @returns {FyloSyncMode}
 */
export function resolveSyncMode(syncMode) {
    return syncMode ?? 'await-sync'
}
