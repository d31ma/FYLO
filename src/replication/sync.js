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
 * @typedef {import('../storage/types.js').FyloIndexOptions} FyloIndexOptions
 */

/** @typedef {'await-sync' | 'fire-and-forget'} FyloSyncMode */
/** @typedef {'off' | 'strict'} FyloWormMode */

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
 * @property {string} path
 */

/**
 * @template {Record<string, any>} [T=Record<string, any>]
 * @typedef {object} FyloSyncHooks
 * @property {(event: FyloWriteSyncEvent<T>) => Promise<void> | void=} onWrite
 * @property {(event: FyloDeleteSyncEvent) => Promise<void> | void=} onDelete
 */

/**
 * @template {Record<string, any>} [T=Record<string, any>]
 * @typedef {object} FyloOptions
 * @property {string=} root
 * @property {boolean=} rls
 * @property {FyloSyncHooks<T>=} sync
 * @property {FyloSyncMode=} syncMode
 * @property {FyloWormOptions=} worm
 * @property {FyloIndexOptions=} index
 * @property {boolean=} queue
 * @property {FyloEventHandler=} onEvent
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
