// Singleton adapter over the vendored `ttid` binary shim.
//
// FYLO consumes TTID as the compiled `ttid` binary (installed from GitHub
// Releases) driven through its NDJSON loop client — no npm package. This module
// keeps ONE warm subprocess for the whole process and re-exposes the handful of
// operations FYLO uses under the original static-method names and return
// shapes, so call sites only change from sync to `await`.
//
//   isTTID(id)     → Date | null   (creation date, matching the old package)
//   isUUID(id)     → boolean
//   generate(id?, del?) → string
//   decodeTime(id) → { createdAt, updatedAt?, deletedAt? }
import { TTID as TTIDClient } from './ttid.mjs'
import { warm } from './warm.js'

/** @type {TTIDClient | undefined} */
let client

function driver() {
    if (!client) client = warm(new TTIDClient())
    return /** @type {TTIDClient} */ (client)
}

export default {
    /**
     * @param {string} [id]
     * @param {boolean} [del]
     * @returns {Promise<string>}
     */
    generate(id, del) {
        return driver().generate(id, del)
    },

    /**
     * @param {string} id
     * @returns {Promise<Date | null>}
     */
    async isTTID(id) {
        const r = await driver().isTTID(id)
        return r.valid ? new Date(r.createdAt) : null
    },

    /**
     * @param {string} id
     * @returns {Promise<boolean>}
     */
    async isUUID(id) {
        const r = await driver().isUUID(id)
        return r.valid
    },

    /**
     * @param {string} id
     * @returns {Promise<{ createdAt: number, updatedAt?: number, deletedAt?: number }>}
     */
    decodeTime(id) {
        return driver().decodeTime(id)
    }
}
