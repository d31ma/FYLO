import path from 'node:path'
import TTID from '@d31ma/ttid'
import { assertPathInside, validateDocId } from '../core/doc-id.js'

/**
 * @typedef {string} TTIDValue
 * @typedef {object} FyloStorage
 * @property {(path: string) => Promise<string>} read
 * @property {(path: string, data: string) => Promise<void>} write
 * @property {(path: string) => Promise<void>} delete
 * @property {(path: string) => Promise<string[]>} list
 * @property {(path: string) => Promise<void>} mkdir
 * @property {(path: string) => Promise<void>} rmdir
 * @property {(path: string) => Promise<boolean>} exists
 *
 * @typedef {object} StoredHeadRecord
 * @property {1} version
 * @property {string} lineageId
 * @property {TTIDValue} currentVersionId
 * @property {boolean=} deleted
 * @property {number=} deletedAt
 *
 * @typedef {object} StoredVersionMetaRecord
 * @property {1} version
 * @property {TTIDValue} versionId
 * @property {string} lineageId
 * @property {TTIDValue=} previousVersionId
 * @property {number=} supersededAt
 * @property {number=} deletedAt
 *
 * @typedef {object} StoredDocRecord
 * @property {TTIDValue} id
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {Record<string, any>} data
 */

/**
 * Document body and WORM metadata persistence for filesystem-backed FYLO
 * collections.
 */
export class FilesystemDocuments {
    /** @type {FyloStorage} */
    storage
    /** @type {(collection: string) => string} */
    docsRoot
    /** @type {(collection: string, docId: TTIDValue) => string} */
    docPath
    /** @type {(collection: string) => string} */
    headsRoot
    /** @type {(collection: string, lineageId: string) => string} */
    headPath
    /** @type {(collection: string) => string} */
    versionsRoot
    /** @type {(collection: string, docId: TTIDValue) => string} */
    versionMetaPath
    /** @type {(collection: string) => Promise<void>} */
    ensureCollection
    /** @type {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} */
    encodeEncrypted
    /** @type {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} */
    decodeEncrypted
    /**
     * Coordinates low-level document, head, and version metadata persistence.
     * @param {FyloStorage} storage
     * @param {(collection: string) => string} docsRoot
     * @param {(collection: string, docId: TTIDValue) => string} docPath
     * @param {(collection: string) => string} headsRoot
     * @param {(collection: string, lineageId: string) => string} headPath
     * @param {(collection: string) => string} versionsRoot
     * @param {(collection: string, docId: TTIDValue) => string} versionMetaPath
     * @param {(collection: string) => Promise<void>} ensureCollection
     * @param {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} encodeEncrypted
     * @param {<T extends Record<string, any>>(collection: string, value: T, parentField?: string) => Promise<T>} decodeEncrypted
     */
    constructor(
        storage,
        docsRoot,
        docPath,
        headsRoot,
        headPath,
        versionsRoot,
        versionMetaPath,
        ensureCollection,
        encodeEncrypted,
        decodeEncrypted
    ) {
        this.storage = storage
        this.docsRoot = docsRoot
        this.docPath = docPath
        this.headsRoot = headsRoot
        this.headPath = headPath
        this.versionsRoot = versionsRoot
        this.versionMetaPath = versionMetaPath
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
        validateDocId(docId)
        const target = this.docPath(collection, docId)
        assertPathInside(this.docsRoot(collection), target)
        try {
            const raw = JSON.parse(await this.storage.read(target))
            const decoded = await this.decodeEncrypted(collection, raw)
            const { createdAt, updatedAt } = TTID.decodeTime(docId)
            return {
                id: docId,
                createdAt,
                updatedAt: updatedAt ?? createdAt,
                data: decoded
            }
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
        validateDocId(docId)
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
        validateDocId(docId)
        const target = this.docPath(collection, docId)
        assertPathInside(this.docsRoot(collection), target)
        await this.storage.delete(target)
    }
    /**
     * Reads append-only lineage metadata for a document version.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<StoredVersionMetaRecord | null>}
     */
    async readVersionMeta(collection, docId) {
        validateDocId(docId)
        const target = this.versionMetaPath(collection, docId)
        assertPathInside(this.versionsRoot(collection), target)
        try {
            return JSON.parse(await this.storage.read(target))
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code === 'ENOENT') return null
            throw err
        }
    }
    /**
     * Persists append-only lineage metadata for a document version.
     * @param {string} collection
     * @param {StoredVersionMetaRecord} meta
     * @returns {Promise<void>}
     */
    async writeVersionMeta(collection, meta) {
        validateDocId(meta.versionId)
        await this.ensureCollection(collection)
        const target = this.versionMetaPath(collection, meta.versionId)
        assertPathInside(this.versionsRoot(collection), target)
        await this.storage.write(target, JSON.stringify(meta))
    }
    /**
     * Reads the active head pointer for a lineage.
     * @param {string} collection
     * @param {string} lineageId
     * @returns {Promise<StoredHeadRecord | null>}
     */
    async readHead(collection, lineageId) {
        const target = this.headPath(collection, lineageId)
        assertPathInside(this.headsRoot(collection), target)
        try {
            return JSON.parse(await this.storage.read(target))
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code === 'ENOENT') return null
            throw err
        }
    }
    /**
     * Writes the active head pointer for a lineage.
     * @param {string} collection
     * @param {StoredHeadRecord} head
     * @returns {Promise<void>}
     */
    async writeHead(collection, head) {
        await this.ensureCollection(collection)
        const target = this.headPath(collection, head.lineageId)
        assertPathInside(this.headsRoot(collection), target)
        await this.storage.write(target, JSON.stringify(head))
    }
    /**
     * Resolves either a lineage id or version id to its current head.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<StoredHeadRecord | null>}
     */
    async resolveHead(collection, docId) {
        const directHead = await this.readHead(collection, docId)
        if (directHead) return directHead
        const meta = await this.readVersionMeta(collection, docId)
        if (!meta) return null
        return await this.readHead(collection, meta.lineageId)
    }
    /**
     * Lists all stored document version ids.
     * @param {string} collection
     * @returns {Promise<TTIDValue[]>}
     */
    async listDocIds(collection) {
        const files = await this.storage.list(this.docsRoot(collection))
        return (
            files
                /** @param {string} file */
                .filter((file) => file.endsWith('.json'))
                /** @param {string} file */
                .map((file) => path.basename(file, '.json'))
                /** @param {string} key */
                .filter((key) => TTID.isTTID(key))
        )
    }
    /**
     * Lists current non-deleted head document ids.
     * @param {string} collection
     * @returns {Promise<TTIDValue[]>}
     */
    async listActiveDocIds(collection) {
        const files = await this.storage.list(this.headsRoot(collection))
        const ids = []
        for (const file of files) {
            if (!file.endsWith('.json')) continue
            const head = JSON.parse(await this.storage.read(file))
            if (head.deleted || !TTID.isTTID(head.currentVersionId)) continue
            ids.push(head.currentVersionId)
        }
        return ids
    }
}
