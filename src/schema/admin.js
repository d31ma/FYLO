import path from 'node:path'
import { loadManifest, loadHeadSchema, currentVersion } from './versioning.js'
import { materializeDoc } from './migrate.js'
import { validateAgainstHead } from './validation.js'
import { schemaEnv } from './env.js'

/**
 * @typedef {import('./versioning.js').ManifestRecord} ManifestRecord
 */

/**
 * @typedef {object} SchemaVersionStatus
 * @property {string} version
 * @property {boolean} current
 * @property {string | undefined} addedAt
 * @property {string | undefined} sha256
 * @property {string} path
 * @property {boolean} exists
 * @property {string | undefined} actualSha256
 * @property {boolean | undefined} sha256Ok
 * @property {string | undefined} nextVersion
 * @property {string | undefined} upgraderPath
 * @property {boolean | undefined} upgraderExists
 */

/**
 * @typedef {object} SchemaInspectResult
 * @property {string} collection
 * @property {string} schemaDir
 * @property {boolean} versioned
 * @property {string | null} current
 * @property {string} manifestPath
 * @property {ManifestRecord | null} manifest
 * @property {SchemaVersionStatus[]} versions
 */

/**
 * @typedef {object} SchemaDoctorResult
 * @property {string} collection
 * @property {string} schemaDir
 * @property {boolean} ok
 * @property {string[]} issues
 * @property {string[]} warnings
 * @property {SchemaInspectResult | null} inspect
 */

/**
 * @param {string | undefined | null} schemaDir
 * @returns {string}
 */
export function resolveSchemaDir(schemaDir = schemaEnv()) {
    if (!schemaDir) {
        throw new Error('Schema commands require --schema-dir <path> or FYLO_SCHEMA')
    }
    return path.resolve(schemaDir)
}

/**
 * @param {string} collection
 * @param {string} schemaDir
 * @returns {string}
 */
function collectionDir(collection, schemaDir) {
    return path.join(schemaDir, collection)
}

/**
 * @param {string} collection
 * @param {string} schemaDir
 * @returns {string}
 */
function manifestPath(collection, schemaDir) {
    return path.join(collectionDir(collection, schemaDir), 'manifest.json')
}

/**
 * @param {string} collection
 * @param {string} schemaDir
 * @param {string} version
 * @returns {string}
 */
function schemaVersionPath(collection, schemaDir, version) {
    return path.join(collectionDir(collection, schemaDir), 'history', `${version}.schema.json`)
}

/**
 * @param {string} collection
 * @param {string} schemaDir
 * @param {string} fromVersion
 * @param {string} toVersion
 * @returns {string}
 */
function upgraderPath(collection, schemaDir, fromVersion, toVersion) {
    return path.join(
        collectionDir(collection, schemaDir),
        'upgraders',
        `${fromVersion}-to-${toVersion}.js`
    )
}

/**
 * @param {string} target
 * @returns {Promise<boolean>}
 */
async function exists(target) {
    return await Bun.file(target).exists()
}

/**
 * @param {string} target
 * @returns {Promise<string>}
 */
async function fileSha256(target) {
    const bytes = await Bun.file(target).bytes()
    return new Bun.CryptoHasher('sha256').update(bytes).digest('hex')
}

/**
 * @param {string} collection
 * @param {string | undefined | null} [schemaDir]
 * @returns {Promise<SchemaInspectResult>}
 */
export async function inspectSchema(collection, schemaDir) {
    const resolvedSchemaDir = resolveSchemaDir(schemaDir)
    const manifest = await loadManifest(collection, resolvedSchemaDir)
    const current = manifest?.current ?? null
    /** @type {SchemaVersionStatus[]} */
    const versions = []
    if (manifest) {
        for (let i = 0; i < manifest.versions.length; i++) {
            const entry = manifest.versions[i]
            const versionPath = schemaVersionPath(collection, resolvedSchemaDir, entry.v)
            const versionExists = await exists(versionPath)
            const actualSha256 = versionExists ? await fileSha256(versionPath) : undefined
            const nextVersion = manifest.versions[i + 1]?.v
            const nextUpgraderPath = nextVersion
                ? upgraderPath(collection, resolvedSchemaDir, entry.v, nextVersion)
                : undefined
            versions.push({
                version: entry.v,
                current: entry.v === manifest.current,
                addedAt: entry.addedAt,
                sha256: entry.sha256,
                path: versionPath,
                exists: versionExists,
                actualSha256,
                sha256Ok: entry.sha256 ? entry.sha256 === actualSha256 : undefined,
                nextVersion,
                upgraderPath: nextUpgraderPath,
                upgraderExists: nextUpgraderPath ? await exists(nextUpgraderPath) : undefined
            })
        }
    }
    return {
        collection,
        schemaDir: resolvedSchemaDir,
        versioned: manifest !== null,
        current,
        manifestPath: manifestPath(collection, resolvedSchemaDir),
        manifest,
        versions
    }
}

/**
 * @param {string} collection
 * @param {string | undefined | null} [schemaDir]
 * @returns {Promise<SchemaDoctorResult>}
 */
export async function doctorSchema(collection, schemaDir) {
    const resolvedSchemaDir = resolveSchemaDir(schemaDir)
    /** @type {string[]} */
    const issues = []
    /** @type {string[]} */
    const warnings = []
    /** @type {SchemaInspectResult | null} */
    let inspect = null
    try {
        inspect = await inspectSchema(collection, resolvedSchemaDir)
        if (!inspect.versioned) {
            issues.push(`Missing manifest: ${inspect.manifestPath}`)
        }
        const manifest = inspect.manifest
        if (manifest) {
            const seen = new Set()
            for (const entry of manifest.versions) {
                if (seen.has(entry.v)) issues.push(`Duplicate version label: ${entry.v}`)
                seen.add(entry.v)
            }
            if (!seen.has(manifest.current)) {
                issues.push(
                    `Current version is not declared in manifest.versions: ${manifest.current}`
                )
            }
            for (const version of inspect.versions) {
                if (!version.exists) issues.push(`Missing schema version file: ${version.path}`)
                if (version.sha256 && version.sha256Ok === false) {
                    issues.push(`SHA-256 mismatch for ${version.version}: ${version.path}`)
                }
                if (version.nextVersion && !version.upgraderExists) {
                    issues.push(
                        `Missing upgrader ${version.version}->${version.nextVersion}: ${version.upgraderPath}`
                    )
                }
            }
            try {
                await loadHeadSchema(collection, resolvedSchemaDir)
            } catch (error) {
                issues.push(/** @type {Error} */ (error).message)
            }
        }
    } catch (error) {
        issues.push(/** @type {Error} */ (error).message)
    }
    return {
        collection,
        schemaDir: resolvedSchemaDir,
        ok: issues.length === 0,
        issues,
        warnings,
        inspect
    }
}

/**
 * @param {string} collection
 * @param {Record<string, any>} document
 * @param {string | undefined | null} [schemaDir]
 * @returns {Promise<{ collection: string, schemaDir: string, current: string | null, valid: true, document: Record<string, any> }>}
 */
export async function validateSchemaDocument(collection, document, schemaDir) {
    const resolvedSchemaDir = resolveSchemaDir(schemaDir)
    const validated = await validateAgainstHead(collection, document, {
        schemaDir: resolvedSchemaDir
    })
    return {
        collection,
        schemaDir: resolvedSchemaDir,
        current: await currentVersion(collection, resolvedSchemaDir),
        valid: true,
        document: validated
    }
}

/**
 * @param {string} collection
 * @param {Record<string, any>} document
 * @param {string | undefined | null} [schemaDir]
 * @returns {Promise<{ collection: string, schemaDir: string, current: string | null, document: Record<string, any> }>}
 */
export async function materializeSchemaDocument(collection, document, schemaDir) {
    const resolvedSchemaDir = resolveSchemaDir(schemaDir)
    const materialized = /** @type {Record<string, any>} */ (
        await materializeDoc(collection, document, { schemaDir: resolvedSchemaDir })
    )
    return {
        collection,
        schemaDir: resolvedSchemaDir,
        current: await currentVersion(collection, resolvedSchemaDir),
        document: materialized
    }
}
