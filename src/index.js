/**
 * Public package entry. Re-exports the {@link Fylo} default class plus the
 * named error types consumers can catch on.
 */
import Fylo from './api/fylo.js'
export { CollectionFacade } from './api/fylo.js'
export { AuthenticatedFylo } from './api/fylo.js'
export { FyloBatchWriteError } from './api/fylo.js'

export { CollectionNotFoundError } from './core/collection.js'
export { LocalQueue, QueueMessageContext, consume, publish } from './queue/local.js'
export { FyloAuthError } from './security/auth.js'
export { FyloSyncError } from './replication/sync.js'
export { getXattr, setXattr, listXattr, removeXattr } from './storage/xattr.js'

const globalScope = /** @type {typeof globalThis & { Fylo?: typeof Fylo }} */ (globalThis)
globalScope.Fylo ??= Fylo

export default Fylo
