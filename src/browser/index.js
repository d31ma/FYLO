export {
    default,
    BrowserDirectCollection,
    BrowserFyloClient,
    createBrowserClient
} from './client.js'
export { FyloBrowser, BrowserCollectionFacade, createBrowserFylo } from './fylo.js'
export { OpfsFilesystem, createOpfsFilesystem } from './opfs-filesystem.js'
export { FyloWorkerClient, createWorkerClient } from './worker/client.js'
export { BrowserCore } from './core/engine.js'
export { createMemoryFilesystem, MemoryFilesystem } from './core/memory-filesystem.js'
export { CollectionNotFoundError } from '../core/collection.js'
// Local-first synced client: OPFS/memory primary + REST/SSE backend sync.
export { createSyncedClient, SyncEngine } from './sync/index.js'
