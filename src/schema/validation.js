/**
 * @fileoverview Validation wrapper around the vendored `chex` binary.
 *
 * FYLO consumes CHEX as a standalone binary (installed from GitHub Releases),
 * driven through the vendored shim in `src/vendor/`. CHEX validates against a
 * schema file on disk, so FYLO points it at the head-version schema file
 * (`history/<current>.schema.json`) for versioned collections, or lets CHEX
 * resolve a flat `<schemaDir>/<collection>.schema.json` by name otherwise.
 *
 * CHEX is strict-key (rejects data fields not declared in the schema), so
 * FYLO-reserved metadata like `_v` is stripped before delegation and the head
 * version is re-attached on success so callers can persist `_v`.
 */
import chex from '../vendor/chex.js'
import {
    stripVersion,
    attachVersion,
    currentVersion,
    isVersioned,
    loadHeadSchema,
    headSchemaFilePath
} from './versioning.js'
import { schemaEnv } from './env.js'

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
    const schemaDir = options.schemaDir ?? schemaEnv()
    const versioned = await isVersioned(collection, schemaDir)
    const rest = /** @type {Record<string, unknown>} */ (stripVersion(doc).rest ?? {})

    let validated
    if (versioned) {
        // Existence + FYLO-support guard (cached), then validate against the head file.
        await loadHeadSchema(collection, schemaDir)
        const headPath = /** @type {string} */ (await headSchemaFilePath(collection, schemaDir))
        validated = await chex.validate(headPath, rest)
    } else {
        // Unversioned: let CHEX resolve a flat <schemaDir>/<collection>.schema.json by name.
        validated = await chex.validate(collection, rest, schemaDir ?? undefined)
    }

    if (!versioned) return validated
    const head = /** @type {string} */ (await currentVersion(collection, schemaDir))
    return attachVersion(validated, head)
}
