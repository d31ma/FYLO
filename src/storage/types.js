/**
 * @typedef {import('../types/vendor.js').TTID} TTID
 */

/**
 * @typedef {'filesystem'} FyloStorageEngineKind
 */

/**
 * @typedef {object} StorageEngine
 * @property {(path: string) => Promise<string>} read
 * @property {(path: string) => Promise<Uint8Array>} readBytes
 * @property {(path: string) => ReadableStream<Uint8Array>} readStream
 * @property {(path: string, data: string) => Promise<void>} write
 * @property {(path: string, stream: ReadableStream<Uint8Array>, options?: { maxBytes?: number }) => Promise<{ contentLength: number, checksumSHA256: string }>} writeStream
 * @property {(source: string, target: string) => Promise<void>} move
 * @property {(path: string, mode: number) => Promise<void>} chmod
 * @property {(path: string, mtimeMs: number) => Promise<void>} setModifiedTime
 * @property {(path: string) => Promise<{ mtimeMs: number, size: number }>} metadata
 * @property {(path: string) => Promise<void>} delete
 * @property {(path: string) => Promise<string[]>} list
 * @property {(path: string) => Promise<void>} mkdir
 * @property {(path: string) => Promise<void>} rmdir
 * @property {(path: string) => Promise<boolean>} exists
 */

/**
 * @typedef {object} LockManager
 * @property {(collection: string, docId: TTID, owner: string, ttlMs?: number) => Promise<boolean>} acquire
 * @property {(collection: string, docId: TTID, owner: string) => Promise<void>} release
 * @property {(collection: string, owner: string, options?: { ttlMs?: number, waitTimeoutMs?: number, onTakeover?: (info: { lockPath: string, newOwner: string, previousOwner?: string }) => void }) => Promise<void>} acquireCollectionWrite
 * @property {(collection: string, owner: string) => Promise<void>} releaseCollectionWrite
 */

/**
 * @template T
 * @typedef {object} EventBus
 * @property {(collection: string, event: T) => Promise<void>} publish
 * @property {(collection: string) => AsyncGenerator<T, void, unknown>} listen
 */

/**
 * @template {Record<string, any>} T
 * @typedef {Record<TTID, T>} FyloRecord
 */

/**
 * @template {Record<string, any>} T
 * @typedef {TTID | FyloRecord<T> | Record<string, TTID[]> | Record<string, Record<TTID, Partial<T>>> | Record<TTID, Partial<T>>} FilesystemQueryResult
 */

/**
 * @template {Record<string, any>} T
 * @typedef {object} FilesystemEvent
 * @property {number} ts
 * @property {'insert' | 'delete'} action
 * @property {TTID} id
 * @property {T=} doc
 * @property {number=} createdAt
 * @property {number=} updatedAt
 */

/**
 * @template {Record<string, any>} T
 * @typedef {object} StoredDoc
 * @property {TTID} id
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {number=} deletedAt
 * @property {T} data
 */

/**
 * @typedef {object} PrefixIndexStore
 * @property {(collection: string) => Promise<void>} ensureCollection
 * @property {(collection: string) => Promise<void>} resetCollection
 * @property {(collection: string, docId: TTID, doc: Record<string, any>) => Promise<void>} putDocument
 * @property {(collection: string, docId: TTID, doc: Record<string, any>) => Promise<void>} removeDocument
 * @property {(collection: string) => Promise<number>} countDocuments
 * @property {(collection: string, fieldPath: string, operand: import('../query/types.js').Operand) => Promise<Set<TTID> | null>} candidateDocIds
 */

/**
 * @typedef {object} FyloS3IndexOptions
 * @property {string=} accessKeyId
 * @property {string=} secretAccessKey
 * @property {string=} sessionToken
 * @property {string=} endpoint
 * @property {string=} region
 */

/**
 * @typedef {{ backend?: 'local-fs' } | { backend: 's3-client', s3?: FyloS3IndexOptions }} FyloIndexOptions
 */

/**
 * @typedef {'document' | 'file'} FyloCollectionKind
 */

/**
 * @typedef {object} CollectionCreateOptions
 * @property {FyloCollectionKind=} kind
 */

/**
 * @typedef {object} CollectionRebuildResult
 * @property {string} collection
 * @property {FyloCollectionKind} kind
 * @property {boolean} worm
 * @property {number} docsScanned
 * @property {number} indexedDocs
 */

/**
 * @typedef {object} CollectionInspectResult
 * @property {string} collection
 * @property {FyloCollectionKind} kind
 * @property {boolean} exists
 * @property {boolean} worm
 * @property {number} docsStored
 * @property {number} deletedDocs
 * @property {number} indexedDocs
 */

export {}
