import { createHash } from 'node:crypto'
import { existsSync, readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

export class TachyonPin {
    static COMMIT = 'd8332172321f9214519b7728d1dbddf390aa5598'
    static VERSION = '26.46.02'
    static SHA256 = {
        'macos-arm64': 'cc41c4dec04f282e6ed63a341b1204d9190a914ca9844099543ff218464b0cd2',
        'linux-arm64': '38e3b358f494c9a6f54d34b296f4b9e78b42d5bc6278e25f1d7507b38b689465'
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
        return TachyonPin.#verified(TachyonPin.#platform())
    }

    /** @returns {string} */
    static #platform() {
        const system = { darwin: 'macos', linux: 'linux' }[process.platform] ?? process.platform
        return `${system}-${process.arch}`
    }

    /**
     * @param {string} platform
     * @returns {string}
     */
    static #verified(platform) {
        const file = TachyonPin.#location(platform)
        TachyonPin.#require(existsSync(file), `is not at ${file}`)
        const digest = createHash('sha256').update(readFileSync(file)).digest('hex')
        TachyonPin.#require(
            digest === TachyonPin.SHA256[platform],
            `at ${file} has SHA-256 ${digest}`
        )
        return file
    }

    /**
     * @param {string} platform
     * @returns {string}
     */
    static #location(platform) {
        const root =
            process.env.TACHYON_DEVELOP_ROOT ??
            join(homedir(), 'Developer', 'AnvilTargets', 'tachyon-develop')
        return join(root, TachyonPin.COMMIT, 'ty', platform, 'ty')
    }

    /**
     * @param {boolean} holds
     * @param {string} problem
     */
    static #require(holds, problem) {
        if (holds) return
        throw new Error(
            `FYLO's site pins the TACHYON develop build ${TachyonPin.COMMIT} (ty ${TachyonPin.VERSION}); ` +
                `the build ${problem}. Set TACHYON_DEVELOP_ROOT or TAC_BIN to that build.`
        )
    }
}
