/**
 * @fileoverview Server side of local-first browser sync.
 *
 * Applies a browser client's change-set to the backend with **document-level
 * three-way merge**: a change applies cleanly when the backend has not advanced
 * past the version the client last saw (`baseUpdatedAt`); when both sides changed
 * the same document it is a genuine conflict, resolved last-write-wins by
 * timestamp. Every applied write auto-commits, so the version history records the
 * reconciliation. The pull side (SSE `/v1/:collection/events`) delivers changes
 * the other direction.
 */

/**
 * @typedef {object} SyncChange
 * @property {string} id
 * @property {Record<string, any>=} doc     The client's version (omit for a delete).
 * @property {boolean=} deleted
 * @property {number=} baseUpdatedAt         `updatedAt` the client last synced for this doc.
 * @property {number=} clientUpdatedAt       Client mtime, used as the LWW tiebreak.
 *
 * @typedef {object} SyncPushResult
 * @property {string[]} applied
 * @property {{ id: string, resolved: 'local' | 'remote' }[]} conflicts
 * @property {Record<string, { doc: Record<string, any> | null, updatedAt: number | null }>} results
 * @property {number} offset
 */

/**
 * @param {any} fylo  Fylo facade from the machine layer (exposes `.engine` and `[collection]`).
 * @param {string} collection
 * @param {SyncChange[]} changes
 * @returns {Promise<SyncPushResult>}
 */
export async function syncPush(fylo, collection, changes) {
    const engine = fylo.engine
    // The client may have created this collection while offline.
    try {
        await fylo[collection].create()
    } catch {
        // already exists
    }

    /** @type {string[]} */
    const applied = []
    /** @type {{ id: string, resolved: 'local' | 'remote' }[]} */
    const conflicts = []
    /** @type {Record<string, { doc: Record<string, any> | null, updatedAt: number | null }>} */
    const results = {}

    for (const change of Array.isArray(changes) ? changes : []) {
        const id = change?.id
        if (typeof id !== 'string') continue

        const current = await engine.readStoredRecord(collection, id)
        const backendChanged = current ? current.updatedAt !== change.baseUpdatedAt : false

        let takeLocal
        if (!current || !backendChanged) {
            // New to the backend, or only the client changed it — apply cleanly.
            takeLocal = true
        } else {
            // Both sides changed the same doc since the client's base → conflict.
            const clientTs = change.clientUpdatedAt ?? Date.now()
            takeLocal = clientTs > current.updatedAt
            conflicts.push({ id, resolved: takeLocal ? 'local' : 'remote' })
        }

        if (takeLocal) {
            if (change.deleted) {
                if (current) await fylo.executeDelDocDirect(collection, id)
            } else if (change.doc) {
                // previousId set → in-place update; undefined → create with the
                // client's id. Both auto-commit and update indexes/events.
                await fylo.executePutDataDirect(
                    collection,
                    id,
                    change.doc,
                    current ? id : undefined
                )
            }
            applied.push(id)
        }

        const record = await engine.readStoredRecord(collection, id)
        results[id] = record
            ? { doc: record.data, updatedAt: record.updatedAt }
            : { doc: null, updatedAt: null }
    }

    const offset = await engine.events.currentOffset(collection)
    return { applied, conflicts, results, offset }
}
