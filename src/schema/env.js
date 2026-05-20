/**
 * @fileoverview Schema environment resolution.
 */

/** @returns {string | undefined} */
export function schemaEnv() {
    return process.env.FYLO_SCHEMA
}

/**
 * Keep CHEX aligned with FYLO's schema root for callers that use CHEX
 * directly after constructing FYLO.
 */
export function syncChexSchemaEnv() {
    if (process.env.FYLO_SCHEMA && !process.env.CHEX_SCHEMA_DIR) {
        process.env.CHEX_SCHEMA_DIR = process.env.FYLO_SCHEMA
    }
}
