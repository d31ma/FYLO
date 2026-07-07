// Singleton adapter over the vendored `chex` binary shim.
//
// FYLO consumes CHEX as the compiled `chex` binary (installed from GitHub
// Releases) driven through its NDJSON loop client — no npm package. One warm
// subprocess for the whole process; `validate` returns the validated data and
// rejects on a schema mismatch.
import { CHEX as CHEXClient } from './chex.mjs'
import { warm } from './warm.js'

/** @type {CHEXClient | undefined} */
let client

function driver() {
    if (!client) client = warm(new CHEXClient())
    return /** @type {CHEXClient} */ (client)
}

export default {
    /**
     * @param {string} schema  A `.schema.json` path, or a name resolved against `schemaDir`.
     * @param {Record<string, unknown>} data
     * @param {string} [schemaDir]
     * @returns {Promise<Record<string, unknown>>}
     */
    validate(schema, data, schemaDir) {
        return /** @type {Promise<Record<string, unknown>>} */ (
            driver().validate(schema, data, schemaDir)
        )
    }
}
