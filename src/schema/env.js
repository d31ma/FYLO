/**
 * @fileoverview Schema environment resolution.
 */

/** @returns {string | undefined} */
export function schemaEnv() {
    return process.env.FYLO_SCHEMA
}
