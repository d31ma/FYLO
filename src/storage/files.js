import path from 'node:path'
import TTID from '../vendor/ttid.js'
import { assertPathInside, validateDocId } from '../core/doc-id.js'
import {
    rawFileContentType,
    rawFileExtension,
    rawFileId,
    rawFileKey,
    rawFileMetadata
} from '../core/raw-file.js'

/**
 * @typedef {import('./types.js').StorageEngine} StorageEngine
 * @typedef {import('../core/raw-file.js').RawFileMetadata} RawFileMetadata
 *
 * @typedef {object} RawFileSource
 * @property {ReadableStream<Uint8Array>} stream
 * @property {string=} name
 * @property {string=} contentType
 * @property {number=} maxBytes
 * @property {string=} key
 *
 * @typedef {object} StoredRawFile
 * @property {string} id
 * @property {string} path
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {number=} deletedAt
 * @property {RawFileMetadata} data
 */

/**
 * Raw-file persistence for file collections. File bytes are never wrapped,
 * encoded, or rewritten; only the TTID-based filename is assigned by FYLO.
 */
export class FilesystemFiles {
    /**
     * @param {StorageEngine} storage
     * @param {(collection: string) => string} docsRoot
     * @param {(collection: string) => string} deletedRoot
     * @param {(collection: string) => string} metadataRoot
     * @param {(collection: string) => Promise<void>} ensureCollection
     */
    constructor(storage, docsRoot, deletedRoot, metadataRoot, ensureCollection) {
        this.storage = storage
        this.docsRoot = docsRoot
        this.deletedRoot = deletedRoot
        this.metadataRoot = metadataRoot
        this.ensureCollection = ensureCollection
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @returns {Promise<string>}
     */
    async metadataPath(collection, docId) {
        await validateDocId(docId)
        const root = this.metadataRoot(collection)
        const target = path.join(root, docId.slice(0, 2), `${docId}.json`)
        assertPathInside(root, target)
        return target
    }

    /**
     * @param {string} docId
     * @param {RawFileSource} source
     * @returns {{ extension: string, contentType: string, key: string }}
     */
    resolveMetadata(docId, source) {
        const extension = rawFileExtension(source.name, source.contentType)
        return {
            extension,
            contentType: rawFileContentType(extension, source.contentType),
            key: rawFileKey(source.key, docId, extension)
        }
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @param {string} key
     * @returns {Promise<void>}
     */
    async writeKey(collection, docId, key) {
        await this.storage.write(
            await this.metadataPath(collection, docId),
            `${JSON.stringify({ version: 1, key })}\n`
        )
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @returns {Promise<string>}
     */
    async readKey(collection, docId) {
        const target = await this.metadataPath(collection, docId)
        if (!(await this.storage.exists(target))) {
            throw new Error(`Raw file system metadata is missing: ${docId}`)
        }
        const parsed = /** @type {{ version?: unknown, key?: unknown }} */ (
            JSON.parse(await this.storage.read(target))
        )
        if (parsed.version !== 1 || typeof parsed.key !== 'string') {
            throw new Error(`Raw file system metadata is corrupt: ${docId}`)
        }
        return parsed.key
    }

    /**
     * @param {string} root
     * @param {string} docId
     * @returns {Promise<string | null>}
     */
    async findPath(root, docId) {
        await validateDocId(docId)
        const bucket = path.join(root, docId.slice(0, 2))
        const matches = (await this.storage.list(bucket)).filter((file) => {
            const filename = path.basename(file)
            return rawFileId(filename) === docId
        })
        if (matches.length > 1) {
            throw new Error(`Multiple raw files found for document ID: ${docId}`)
        }
        const target = matches[0]
        if (!target) return null
        assertPathInside(root, target)
        return target
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @param {RawFileSource} source
     * @returns {Promise<StoredRawFile>}
     */
    async writeStoredFile(collection, docId, source) {
        await validateDocId(docId)
        await this.ensureCollection(collection)
        const { extension, contentType, key } = this.resolveMetadata(docId, source)
        const target = path.join(
            this.docsRoot(collection),
            docId.slice(0, 2),
            `${docId}${extension}`
        )
        assertPathInside(this.docsRoot(collection), target)
        let written
        try {
            written = await this.storage.writeStream(target, source.stream, {
                maxBytes: source.maxBytes
            })
            await this.writeKey(collection, docId, key)
        } catch (error) {
            await this.storage.delete(target)
            await this.storage.delete(await this.metadataPath(collection, docId))
            throw error
        }
        const metadata = await this.storage.metadata(target)
        const { createdAt } = await TTID.decodeTime(docId)
        return {
            id: docId,
            path: target,
            createdAt,
            updatedAt: metadata.mtimeMs,
            data: rawFileMetadata(
                docId,
                key,
                extension,
                contentType,
                written.contentLength,
                written.checksumSHA256,
                createdAt,
                metadata.mtimeMs
            )
        }
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @returns {Promise<StoredRawFile | null>}
     */
    async readStoredFile(collection, docId) {
        return await this.readFileAtRoot(collection, this.docsRoot(collection), docId)
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @returns {Promise<StoredRawFile | null>}
     */
    async readDeletedFile(collection, docId) {
        const stored = await this.readFileAtRoot(collection, this.deletedRoot(collection), docId)
        return stored ? { ...stored, deletedAt: stored.updatedAt } : null
    }

    /**
     * @param {string} collection
     * @param {string} root
     * @param {string} docId
     * @returns {Promise<StoredRawFile | null>}
     */
    async readFileAtRoot(collection, root, docId) {
        const target = await this.findPath(root, docId)
        if (!target) return null
        const filename = path.basename(target)
        const extension = filename.slice(docId.length).toLowerCase()
        const metadata = await this.storage.metadata(target)
        const checksumSHA256 = await this.hash(target)
        const key = await this.readKey(collection, docId)
        const { createdAt } = await TTID.decodeTime(docId)
        return {
            id: docId,
            path: target,
            createdAt,
            updatedAt: metadata.mtimeMs,
            data: rawFileMetadata(
                docId,
                key,
                extension,
                rawFileContentType(extension, undefined),
                metadata.size,
                checksumSHA256,
                createdAt,
                metadata.mtimeMs
            )
        }
    }

    /**
     * @param {string} target
     * @returns {Promise<string>}
     */
    async hash(target) {
        const hasher = new Bun.CryptoHasher('sha256')
        for await (const chunk of /** @type {AsyncIterable<Uint8Array>} */ (
            /** @type {unknown} */ (this.storage.readStream(target))
        )) {
            hasher.update(chunk)
        }
        return hasher.digest('hex')
    }

    /** @param {string} collection @returns {Promise<string[]>} */
    async listFileIds(collection) {
        const files = await this.storage.list(this.docsRoot(collection))
        return this.idsFromFiles(files)
    }

    /** @param {string} collection @returns {Promise<string[]>} */
    async listDeletedFileIds(collection) {
        const files = await this.storage.list(this.deletedRoot(collection))
        return this.idsFromFiles(files)
    }

    /**
     * @param {string[]} files
     * @returns {Promise<string[]>}
     */
    async idsFromFiles(files) {
        /** @type {string[]} */
        const ids = []
        for (const file of files) {
            const id = rawFileId(path.basename(file))
            if (id !== null && (await TTID.isTTID(id))) ids.push(id)
        }
        return ids
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @param {number} deletedAt
     * @returns {Promise<string>}
     */
    async softDeleteStoredFile(collection, docId, deletedAt) {
        const source = await this.findPath(this.docsRoot(collection), docId)
        if (!source) throw new Error(`Raw file not found: ${docId}`)
        const target = path.join(
            this.deletedRoot(collection),
            docId.slice(0, 2),
            path.basename(source)
        )
        assertPathInside(this.deletedRoot(collection), target)
        await this.storage.move(source, target)
        await this.storage.setModifiedTime(target, deletedAt)
        await this.storage.chmod(target, 0o444)
        return target
    }

    /**
     * @param {string} collection
     * @param {string} docId
     * @param {number} restoredAt
     * @returns {Promise<string>}
     */
    async restoreStoredFile(collection, docId, restoredAt) {
        const source = await this.findPath(this.deletedRoot(collection), docId)
        if (!source) throw new Error(`Deleted raw file not found: ${docId}`)
        const target = path.join(
            this.docsRoot(collection),
            docId.slice(0, 2),
            path.basename(source)
        )
        assertPathInside(this.docsRoot(collection), target)
        await this.storage.move(source, target)
        await this.storage.chmod(target, 0o644)
        await this.storage.setModifiedTime(target, restoredAt)
        return target
    }

    /** @param {string} collection @param {string} docId @returns {Promise<void>} */
    async makeStoredFileReadOnly(collection, docId) {
        const target = await this.findPath(this.docsRoot(collection), docId)
        if (!target) throw new Error(`Raw file not found: ${docId}`)
        await this.storage.chmod(target, 0o444)
    }

    /** @param {string} collection @param {string} docId @returns {Promise<void>} */
    async makeSystemMetadataReadOnly(collection, docId) {
        await this.storage.chmod(await this.metadataPath(collection, docId), 0o444)
    }

    /** @param {string} collection @param {string} docId @returns {Promise<Uint8Array>} */
    async readBytes(collection, docId) {
        const target = await this.findPath(this.docsRoot(collection), docId)
        if (!target) throw new Error(`Raw file not found: ${docId}`)
        return await this.storage.readBytes(target)
    }

    /** @param {string} collection @param {string} docId @returns {Promise<ReadableStream<Uint8Array>>} */
    async readStream(collection, docId) {
        const target = await this.findPath(this.docsRoot(collection), docId)
        if (!target) throw new Error(`Raw file not found: ${docId}`)
        return this.storage.readStream(target)
    }
}
