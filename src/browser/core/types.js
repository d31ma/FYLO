/**
 * Runtime-neutral contracts for FYLO's browser-targeted browser seam.
 *
 * The current implementation is a JavaScript reference core. Its public shape
 * is intentionally narrow so a future browser module can replace the
 * internals without changing browser or Bun host adapters.
 */

/**
 * @typedef {import('../../types/vendor.js').TTID} TTID
 * @typedef {import('../../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} StoreJoin
 * @typedef {import('../../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 * @typedef {import('../../query/types.js').StoreUpdate<Record<string, any>>} StoreUpdate
 * @typedef {import('../../query/types.js').StoreDelete<Record<string, any>>} StoreDelete
 */

/**
 * @typedef {object} BrowserStoredDoc
 * @property {TTID} id
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {Record<string, any>} data
 */

/**
 * @typedef {object} BrowserDeletedDoc
 * @property {TTID} id
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {number} deletedAt
 * @property {Record<string, any>} data
 */

/**
 * @typedef {object} BrowserEvent
 * @property {number} ts
 * @property {'insert' | 'delete' | 'meta'} action
 * @property {TTID} id
 * @property {Record<string, any>=} doc
 * @property {number=} createdAt
 * @property {number=} updatedAt
 * @property {Record<string, any>=} meta
 */

/**
 * @typedef {'off' | 'strict'} BrowserWormMode
 */

/**
 * @typedef {import('../storage.js').BrowserStorage} BrowserStorage
 */

/**
 * @typedef {object} BrowserCoreOptions
 * @property {import('./filesystem.js').FyloFilesystem} fs
 * @property {string=} root
 * @property {{ mode?: BrowserWormMode }=} worm
 * @property {{ ready(): Promise<void>, create(): Promise<{ loadSnapshot(snapshot: Uint8Array): void | Promise<void>, scanQueries(queries: Array<{ prefix: string, range?: { op: '$gt' | '$gte' | '$lt' | '$lte', value: string } }>): string[] | Promise<string[]>, close?(): void | Promise<void> }> }=} indexScannerFactory
 */

/**
 * @typedef {'executeSQL' | 'createCollection' | 'dropCollection' | 'inspectCollection' | 'rebuildCollection' | 'getDoc' | 'getLatest' | 'getMeta' | 'setMeta' | 'findDocs' | 'findDeletedDocs' | 'restoreDoc' | 'joinDocs' | 'putData' | 'batchPutData' | 'patchDoc' | 'patchDocs' | 'delDoc' | 'delDocs'} BrowserOperation
 */

/**
 * @typedef {object} BrowserRequest
 * @property {BrowserOperation} op
 * @property {string=} requestId
 * @property {string=} collection
 * @property {TTID=} id
 * @property {boolean=} onlyId
 * @property {string=} sql
 * @property {StoreQuery=} query
 * @property {StoreJoin=} join
 * @property {Record<string, any>=} data
 * @property {Record<string, any>=} meta
 * @property {Record<string, any>[]=} batch
 * @property {Record<TTID, Record<string, any>>=} newDoc
 * @property {Record<TTID, Record<string, any>>=} oldDoc
 * @property {StoreUpdate=} update
 * @property {StoreDelete=} delete
 */

/**
 * @typedef {object} BrowserSuccessResponse
 * @property {1} protocolVersion
 * @property {true} ok
 * @property {BrowserOperation} op
 * @property {string | null} requestId
 * @property {number} durationMs
 * @property {unknown} result
 */

/**
 * @typedef {object} BrowserErrorResponse
 * @property {1} protocolVersion
 * @property {false} ok
 * @property {BrowserOperation | null} op
 * @property {string | null} requestId
 * @property {number} durationMs
 * @property {{ name: string, message: string, code?: string }} error
 */

export {}
