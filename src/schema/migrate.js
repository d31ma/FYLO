/**
 * @fileoverview Read-side materialization. Upgrades a stored doc to the
 * head schema version using the registered upgrader chain. In-memory
 * only — does not persist the upgraded shape (lazy persistence is an
 * opt-in concern handled at the engine level if/when added).
 */
import {
    stripVersion,
    attachVersion,
    currentVersion,
    upgradeDoc,
    isVersioned,
    loadManifest
} from './versioning.js'
import { schemaEnv } from './env.js'

/**
 * @param {string} collection
 * @param {Record<string, any> | null | undefined} doc
 * @param {{ schemaDir?: string|null }} [options]
 * @returns {Promise<Record<string, any> | null | undefined>}
 */
export async function materializeDoc(collection, doc, options = {}) {
    if (doc === null || doc === undefined || typeof doc !== 'object') return doc
    const schemaDir = options.schemaDir ?? schemaEnv()
    if (!(await isVersioned(collection, schemaDir))) return doc
    const manifest = await loadManifest(collection, schemaDir)
    if (!manifest) return doc
    const head = /** @type {string} */ (await currentVersion(collection, schemaDir))
    const { version, rest } = stripVersion(doc)
    // Pre-versioning docs (no _v field) are treated as the oldest known
    // version in the manifest — that's how MongoDB's Schema Versioning
    // Pattern handles legacy data not yet stamped with `_v`.
    const fromVersion = version ?? manifest.versions[0].v
    if (fromVersion === head) return doc
    const upgraded = await upgradeDoc(
        collection,
        /** @type {Record<string, any>} */ (rest),
        fromVersion,
        head,
        /** @type {string} */ (schemaDir)
    )
    return attachVersion(upgraded, head)
}

/**
 * Materialize a `{ [_id]: data }` envelope. Returns the same envelope shape
 * with `data` upgraded. `null`/empty inputs pass through.
 * @param {string} collection
 * @param {Record<string, Record<string, any>> | null | undefined} envelope
 * @param {{ schemaDir?: string|null }} [options]
 */
export async function materializeEnvelope(collection, envelope, options = {}) {
    if (!envelope || typeof envelope !== 'object') return envelope
    const entries = Object.entries(envelope)
    if (entries.length === 0) return envelope
    /** @type {Record<string, Record<string, any>>} */
    const out = {}
    for (const [id, data] of entries) {
        const upgraded = await materializeDoc(collection, data, options)
        out[id] = /** @type {Record<string, any>} */ (upgraded)
    }
    return out
}
