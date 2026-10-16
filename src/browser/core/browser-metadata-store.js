import TTID from '../vendor/ttid.mjs'
import { legacyShardOf, shardOf } from '../../core/shard.js'
import { copySafeJson, safeRecord } from '../../query/safe-record.js'
import { BrowserPath } from './browser-path.js'
import { MetadataRecord } from './metadata-record.js'

/** @typedef {import('./filesystem.js').FyloFilesystem} FyloFilesystem */

export class BrowserMetadataStore {
    /**
     * @param {FyloFilesystem} fs
     * @param {(collection: string) => string} collectionRoot
     */
    constructor(fs, collectionRoot) {
        this.fs = fs
        this.collectionRoot = collectionRoot
    }

    /** @param {string} collection */
    root(collection) {
        return BrowserPath.join(this.collectionRoot(collection), '.metadata')
    }

    /** @param {string} collection @param {string} id */
    path(collection, id) {
        this.#checkId(id)
        return this.#shardPath(collection, id, shardOf(id))
    }

    /** @param {string} collection @param {string} id */
    async read(collection, id) {
        const target = await this.existingPath(collection, id)
        return await this.#readIfPresent(target, await this.fs.exists(target))
    }

    /** @param {string} collection @param {string} id @param {Record<string, any>} mutations */
    async mutate(collection, id, mutations) {
        MetadataRecord.validate(mutations)
        const current = await this.read(collection, id)
        const values = copySafeJson(current.values)
        this.#applyMutations(values, mutations)
        return await this.write(collection, id, values, Math.max(Date.now(), current.updatedAt + 1))
    }

    /** @param {string} collection @param {string} id @param {Record<string, any>} values @param {number} updatedAt */
    async write(collection, id, values, updatedAt) {
        const target = this.path(collection, id)
        await this.fs.mkdir(BrowserPath.join(this.root(collection), shardOf(id)), {
            recursive: true
        })
        await this.fs.writeText(target, JSON.stringify({ values, updatedAt }))
        const legacy = this.legacyPath(collection, id)
        await this.#removeExistingFile(legacy, await this.#legacyExists(legacy, target))
        return { values, updatedAt }
    }

    /** @param {string} collection @param {string} id */
    legacyPath(collection, id) {
        return this.#shardPath(collection, id, legacyShardOf(id))
    }

    /** @param {string} collection @param {string} id */
    async existingPath(collection, id) {
        const canonical = this.path(collection, id)
        return await this.#existingPath(collection, id, canonical, await this.fs.exists(canonical))
    }

    /** @param {string} id */
    #checkId(id) {
        if (!TTID.isTTID(id)) throw new Error(`Invalid document ID: ${id}`)
    }

    /** @param {string} collection @param {string} id @param {string} shard */
    #shardPath(collection, id, shard) {
        const root = this.root(collection)
        const target = BrowserPath.join(root, shard, `${id}.json`)
        BrowserPath.assertPathInside(root, target)
        return target
    }

    /** @param {string} target @param {boolean} exists */
    async #readIfPresent(target, exists) {
        if (!exists) return { values: safeRecord(), updatedAt: 0 }
        return await this.#readSidecar(target)
    }

    /** @param {string} target */
    async #readSidecar(target) {
        const parsed = JSON.parse(await this.fs.readText(target))
        return {
            values: this.#values(parsed?.values),
            updatedAt: this.#updatedAt(parsed?.updatedAt)
        }
    }

    /** @param {any} value */
    #values(value) {
        const object = this.#objectValues(value)
        return this.#copyRecord(object)
    }

    /** @param {any} value */
    #objectValues(value) {
        if (typeof value !== 'object') return safeRecord()
        return value
    }

    /** @param {any} value */
    #copyRecord(value) {
        if (Array.isArray(value)) return safeRecord()
        return copySafeJson(value)
    }

    /** @param {any} value */
    #updatedAt(value) {
        if (typeof value === 'number') return value
        return 0
    }

    /** @param {Record<string, any>} values @param {Record<string, any>} mutations */
    #applyMutations(values, mutations) {
        for (const [name, value] of Object.entries(mutations)) {
            this.#applyMutation(values, name, value)
        }
    }

    /** @param {Record<string, any>} values @param {string} name @param {any} value */
    #applyMutation(values, name, value) {
        if (value === null) delete values[name]
        else values[name] = copySafeJson(value)
    }

    /** @param {string} legacy @param {string} canonical */
    async #legacyExists(legacy, canonical) {
        if (legacy === canonical) return false
        return await this.fs.exists(legacy)
    }

    /** @param {string} path @param {boolean} exists */
    async #removeExistingFile(path, exists) {
        if (exists) await this.fs.remove(path)
    }

    /** @param {string} collection @param {string} id @param {string} canonical @param {boolean} exists */
    async #existingPath(collection, id, canonical, exists) {
        if (exists) return canonical
        return await this.#findLegacyPath(collection, id, canonical)
    }

    /** @param {string} collection @param {string} id @param {string} canonical */
    async #findLegacyPath(collection, id, canonical) {
        const legacy = this.legacyPath(collection, id)
        return this.#selectPath(canonical, legacy, await this.#legacyExists(legacy, canonical))
    }

    /** @param {string} canonical @param {string} legacy @param {boolean} exists */
    #selectPath(canonical, legacy, exists) {
        if (exists) return legacy
        return canonical
    }
}
