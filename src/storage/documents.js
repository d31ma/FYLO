import path from 'node:path'
import TTID from '../vendor/ttid.js'
import { assertPathInside, validateDocId, filterTTIDs } from '../core/doc-id.js'

/**
 * @typedef {string} TTIDValue
 * @typedef {object} FyloStorage
 * @property {(path: string) => Promise<string>} read
 * @property {(path: string, data: string) => Promise<void>} write
 * @property {(source: string, target: string) => Promise<void>} move
 * @property {(path: string, mode: number) => Promise<void>} chmod
 * @property {(path: string, mtimeMs: number) => Promise<void>} setModifiedTime
 * @property {(path: string) => Promise<{ mtimeMs: number }>} metadata
 * @property {(path: string) => Promise<void>} delete
 * @property {(path: string) => Promise<string[]>} list
 * @property {(path: string) => Promise<void>} mkdir
 * @property {(path: string) => Promise<void>} rmdir
 * @property {(path: string) => Promise<boolean>} exists
 *
 * @typedef {object} StoredDocRecord
 * @property {TTIDValue} id
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {Record<string, any>} data
 *
 * @typedef {object} DeletedDocRecord
 * @property {TTIDValue} id
 * @property {number} createdAt
 * @property {number} deletedAt
 * @property {Record<string, any>} data
 */

/**
 * Document body persistence for filesystem-backed FYLO collections.
 */
export class FilesystemDocuments {
    /** @type {FyloStorage} */
    storage
    /** @type {(collection: string) => string} */
    docsRoot
    /** @type {(collection: string, docId: TTIDValue) => string} */
    docPath
    /** @type {(collection: string) => string} */
    deletedRoot
    /** @type {(collection: string, docId: TTIDValue) => string} */
    deletedPath
    /** @type {(collection: string) => Promise<void>} */
    ensureCollection
    /** @type {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} */
    encodeEncrypted
    /** @type {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} */
    decodeEncrypted
    /**
     * Coordinates low-level live, deleted, and read-only document persistence.
     * @param {FyloStorage} storage
     * @param {(collection: string) => string} docsRoot
     * @param {(collection: string, docId: TTIDValue) => string} docPath
     * @param {(collection: string) => string} deletedRoot
     * @param {(collection: string, docId: TTIDValue) => string} deletedPath
     * @param {(collection: string) => Promise<void>} ensureCollection
     * @param {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} encodeEncrypted
     * @param {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} decodeEncrypted
     */
    constructor(
        storage,
        docsRoot,
        docPath,
        deletedRoot,
        deletedPath,
        ensureCollection,
        encodeEncrypted,
        decodeEncrypted
    ) {
        this.storage = storage
        this.docsRoot = docsRoot
        this.docPath = docPath
        this.deletedRoot = deletedRoot
        this.deletedPath = deletedPath
        this.ensureCollection = ensureCollection
        this.encodeEncrypted = encodeEncrypted
        this.decodeEncrypted = decodeEncrypted
    }
    /**
     * Reads and decrypts one stored document version.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<StoredDocRecord | null>}
     */
    async readStoredDoc(collection, docId) {
        await validateDocId(docId)
        const target = this.docPath(collection, docId)
        assertPathInside(this.docsRoot(collection), target)
        try {
            const raw = JSON.parse(await this.storage.read(target))
            const decoded = await this.decodeEncrypted(collection, raw)
            const { createdAt } = await TTID.decodeTime(docId)
            const { mtimeMs } = await this.storage.metadata(target)
            return {
                id: docId,
                createdAt,
                updatedAt: mtimeMs,
                data: decoded
            }
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code === 'ENOENT') return null
            throw err
        }
    }
    /**
     * Reads a retained soft-deleted document. Its file modification time is
     * the deletion timestamp, not the previous live update timestamp.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<DeletedDocRecord | null>}
     */
    async readDeletedDoc(collection, docId) {
        await validateDocId(docId)
        const target = this.deletedPath(collection, docId)
        assertPathInside(this.deletedRoot(collection), target)
        try {
            const raw = JSON.parse(await this.storage.read(target))
            const decoded = await this.decodeEncrypted(collection, raw)
            const { createdAt } = await TTID.decodeTime(docId)
            const { mtimeMs } = await this.storage.metadata(target)
            return { id: docId, createdAt, deletedAt: mtimeMs, data: decoded }
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code === 'ENOENT') return null
            throw err
        }
    }
    /**
     * Writes one encrypted document version to the document tree.
     * @template {Record<string, any>} T
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {T} data
     * @returns {Promise<void>}
     */
    async writeStoredDoc(collection, docId, data) {
        await validateDocId(docId)
        await this.ensureCollection(collection)
        const encoded = await this.encodeEncrypted(collection, data)
        const target = this.docPath(collection, docId)
        assertPathInside(this.docsRoot(collection), target)
        await this.storage.write(target, JSON.stringify(encoded))
    }
    /**
     * Removes one stored document version.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<void>}
     */
    async removeStoredDoc(collection, docId) {
        await validateDocId(docId)
        const target = this.docPath(collection, docId)
        assertPathInside(this.docsRoot(collection), target)
        await this.storage.delete(target)
    }
    /**
     * Moves a normal-mode document out of the queryable tree while retaining
     * its original TTID identity in the deleted namespace.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {number} deletedAt
     * @returns {Promise<string>}
     */
    async softDeleteStoredDoc(collection, docId, deletedAt) {
        await validateDocId(docId)
        const source = this.docPath(collection, docId)
        const target = this.deletedPath(collection, docId)
        assertPathInside(this.docsRoot(collection), source)
        assertPathInside(this.deletedRoot(collection), target)
        await this.storage.move(source, target)
        await this.storage.setModifiedTime(target, deletedAt)
        await this.storage.chmod(target, 0o444)
        return target
    }
    /**
     * Restores a retained tombstone to the live document namespace.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {number} restoredAt
     * @returns {Promise<string>}
     */
    async restoreStoredDoc(collection, docId, restoredAt) {
        await validateDocId(docId)
        const source = this.deletedPath(collection, docId)
        const target = this.docPath(collection, docId)
        assertPathInside(this.deletedRoot(collection), source)
        assertPathInside(this.docsRoot(collection), target)
        await this.storage.move(source, target)
        await this.storage.chmod(target, 0o644)
        await this.storage.setModifiedTime(target, restoredAt)
        return target
    }
    /**
     * Applies local defense-in-depth permissions to a strict WORM document.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<void>}
     */
    async makeStoredDocReadOnly(collection, docId) {
        await validateDocId(docId)
        const target = this.docPath(collection, docId)
        assertPathInside(this.docsRoot(collection), target)
        await this.storage.chmod(target, 0o444)
    }
    /**
     * Lists all stored document version ids.
     * @param {string} collection
     * @returns {Promise<TTIDValue[]>}
     */
    async listDocIds(collection) {
        const files = await this.storage.list(this.docsRoot(collection))
        const keys = files
            .filter((file) => file.endsWith('.json'))
            .map((file) => path.basename(file, '.json'))
        return await filterTTIDs(keys)
    }
    /**
     * Lists soft-deleted document IDs retained in the tombstone namespace.
     * @param {string} collection
     * @returns {Promise<TTIDValue[]>}
     */
    async listDeletedDocIds(collection) {
        const files = await this.storage.list(this.deletedRoot(collection))
        const keys = files
            .filter((file) => file.endsWith('.json'))
            .map((file) => path.basename(file, '.json'))
        return await filterTTIDs(keys)
    }
}
