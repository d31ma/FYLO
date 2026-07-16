/**
 * Structured event surface for metrics, logging, and observability tooling.
 *
 * The Fylo constructor accepts an `onEvent` handler which receives a
 * discriminated union of internal events. Handlers run inline in the calling
 * code path; throws are caught and logged so a misbehaving consumer cannot
 * break the underlying operation.
 */

/**
 * @typedef {(
 *   | { type: 'import.blocked', reason: 'protocol' | 'host' | 'private-network' | 'redirect', url: string, detail?: string }
 *   | { type: 'cipher.configured', collection: string }
 *   | { type: 'index.rebuilt', collection: string, docsScanned: number, indexedDocs: number, worm: boolean }
 *   | { type: 'file.key-repaired', collection: string, docId: string, key: string }
 *   | { type: 'file.checksum-mismatch', collection: string, docId: string, expected: string, actual: string }
 *   | { type: 'lock.takeover', lockPath: string, newOwner: string, previousOwner?: string }
 *   | { type: 'sync.failed', collection: string, docId: string, operation: string, path: string, detail: string }
 *   | { type: 'backup.retry', operation: string, key: string, attempt: number, delayMs: number, detail: string }
 *   | { type: 'backup.reconcile.started', source: 'manual' | 'scheduled', startedAt: string }
 *   | { type: 'backup.reconcile.succeeded', source: 'manual' | 'scheduled', startedAt: string, completedAt: string }
 *   | { type: 'backup.reconcile.failed', source: 'manual' | 'scheduled', startedAt: string, failedAt: string, detail: string }
 * )} FyloEvent
 * @typedef {(event: FyloEvent) => void | Promise<void>} FyloEventHandler
 */

/**
 * Invokes a user-supplied event handler, swallowing any thrown errors and
 * rejected promises so a misbehaving consumer cannot break FYLO operations.
 * Returning a Promise from the handler is supported; the rejection is
 * captured and logged but never propagated.
 *
 * @param {FyloEventHandler | undefined} handler
 * @param {FyloEvent} event
 */
export function emitFyloEvent(handler, event) {
    if (!handler) return
    try {
        /** @type {any} */
        const result = handler(event)
        if (result && typeof result.then === 'function') {
            result.catch((/** @type {unknown} */ err) => {
                console.error('FYLO onEvent handler rejected:', err)
            })
        }
    } catch (err) {
        console.error('FYLO onEvent handler threw:', err)
    }
}
