/**
 * @fileoverview Validation wrapper around `@d31ma/chex`.
 *
 * chex resolves name-based schemas from `<schemaDir>/<collection>.schema.json`,
 * but FYLO uses a versioned per-collection directory layout. We bypass chex's
 * loader by pre-populating the cache it consults; chex's own `validateData`
 * will see a hit and skip its file read entirely.
 *
 * chex is also strict-key (rejects data fields not declared in the schema),
 * so FYLO-reserved metadata like `_v` is stripped before delegation. The
 * head version is re-attached on success so callers can persist `_v`.
 */
import { validateData as chexValidateData } from '@d31ma/chex'
import {
    stripVersion,
    attachVersion,
    currentVersion,
    isVersioned,
    loadHeadSchema
} from './versioning.js'
import { schemaEnv } from './env.js'

/**
 * @param {string} collection
 * @param {string | null | undefined} schemaDir
 * @returns {string}
 */
function cacheKey(collection, schemaDir) {
    if (!schemaDir) return collection
    const digest = new Bun.CryptoHasher('sha256').update(schemaDir).digest('hex').slice(0, 16)
    return `${collection}__${digest}`
}

/**
 * CHEX 26.21 namespaces caller-provided caches by source type. FYLO still
 * supplies a synthetic schema ref so CHEX never reads its flat schema layout.
 * @param {string} schemaRef
 * @param {string | null | undefined} schemaDir
 * @returns {string}
 */
function chexCacheKey(schemaRef, schemaDir) {
    return schemaDir ? `dir:${schemaDir}:${schemaRef}` : `path:${schemaRef}`
}

/**
 * Runtime schema validator that adapts FYLO's versioned schema layout to
 * CHEX's name-based validation API.
 */
export class SchemaValidator {
    /** Cache passed into chex; we own it so chex never needs to read from disk. */
    chexCache = new Map()

    /**
     * @param {string} collection
     * @param {Record<string, any>} doc
     * @param {{ schemaDir?: string|null }} [options]
     * @returns {Promise<Record<string, any>>}
     */
    async validateAgainstHead(collection, doc, options = {}) {
        const schemaDir = options.schemaDir ?? schemaEnv()
        const key = cacheKey(collection, schemaDir)
        const chexKey = chexCacheKey(key, schemaDir)
        if (!this.chexCache.has(chexKey)) {
            const head = await loadHeadSchema(collection, schemaDir)
            if (head) {
                this.chexCache.set(key, head)
                this.chexCache.set(chexKey, head)
            }
        }
        const { rest } = stripVersion(doc)
        const validated = /** @type {Record<string, any>} */ (
            await chexValidateData(key, rest, { schemaDir, cache: this.chexCache })
        )
        if (!(await isVersioned(collection, schemaDir))) return validated
        const head = /** @type {string} */ (await currentVersion(collection, schemaDir))
        return attachVersion(validated, head)
    }

    /** Clear the CHEX cache owned by this validator. */
    clearCache() {
        this.chexCache.clear()
    }
}

/** Shared process-level schema validator instance. */
export const schemaValidator = new SchemaValidator()

/**
 * Validate `doc` against the head schema for `collection`. Returns a doc
 * with `_v=head` attached when the collection is versioned.
 *
 * @param {string} collection
 * @param {Record<string, any>} doc
 * @param {{ schemaDir?: string|null }} [options]
 * @returns {Promise<Record<string, any>>}
 */
export async function validateAgainstHead(collection, doc, options = {}) {
    return await schemaValidator.validateAgainstHead(collection, doc, options)
}

/** Test/dev hook to clear the chex schema cache owned by this module. */
export function _resetValidationCache() {
    schemaValidator.clearCache()
}
