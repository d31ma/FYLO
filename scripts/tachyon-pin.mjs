import { createHash } from 'node:crypto'
import { existsSync, readFileSync } from 'node:fs'

export class TachyonPin {
    static VERSION = '26.46.02'
    static SHA256 = {
        'linux-x64': '125423c88dac3e888cc442c807f1beffc10ff8e5db2ac4cd70b331ecda5ea1fd',
        'linux-arm64': '3006c7bcd1bd7e879055d60492b307c2903dde61ec72bb36950ace7ff3031b2e',
        'macos-arm64': 'a062ed5e7523739515b25117dc3426bd1b9f7e82a6eac110611df06adff462a6',
        'macos-x64': '28268aa54999298eb9663e1bd7ed8b52467804e9157e0771fe00f18d879d3ed8',
        'windows-x64': '5950bc91fd7eef88e7cd03b9ef4d26ecbdb248eca9a068cb2f45576e91fb6bab'
    }

    /** @returns {string} */
    static binary() {
        return TachyonPin.#chosen(process.env.TAC_BIN)
    }

    /**
     * @param {string | undefined} override
     * @returns {string}
     */
    static #chosen(override) {
        if (override) return override
        return TachyonPin.#verified(Bun.which('ty'))
    }

    /** @returns {string} */
    static #platform() {
        const system = { darwin: 'macos', linux: 'linux', win32: 'windows' }[process.platform]
        return `${system}-${process.arch}`
    }

    /**
     * @param {string | null} file
     * @returns {string}
     */
    static #verified(file) {
        TachyonPin.#require(Boolean(file) && existsSync(file), 'is not on PATH')
        const digest = createHash('sha256').update(readFileSync(file)).digest('hex')
        const expected = TachyonPin.SHA256[TachyonPin.#platform()]
        TachyonPin.#require(digest === expected, `at ${file} has SHA-256 ${digest}`)
        return file
    }

    /**
     * @param {boolean} holds
     * @param {string} problem
     */
    static #require(holds, problem) {
        if (holds) return
        throw new Error(
            `FYLO's site pins TACHYON ty ${TachyonPin.VERSION}; the ty ${problem}. ` +
                'Install it with `bun run vendor:install` (FYLO_VENDOR_BIN_DIR picks the folder), ' +
                'or set TAC_BIN to another ty build.'
        )
    }
}
