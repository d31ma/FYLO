import { spawn } from 'node:child_process'

export class ClippyWasmTarget {
    static BROWSER_TARGET = 'wasm32-unknown-unknown'
    static WASI_TARGET = 'wasm32-wasip1'
    static WASI_PACKAGES = ['fylo-cli', 'fylo-identity']
    static BROWSER_PACKAGES = [
        'fylo-wasm',
        'fylo-browser-index',
        'fylo-machine',
        'fylo-engine',
        'fylo-storage-native',
        'fylo-vfs',
        'fylo-query',
        'fylo-format',
        'fylo-identity',
        'fylo-chex'
    ]
    static TARGET_RUSTFLAGS = '--cfg getrandom_backend="custom"'

    /** @returns {Promise<void>} */
    static async run() {
        await ClippyWasmTarget.lint(
            ClippyWasmTarget.BROWSER_TARGET,
            ClippyWasmTarget.BROWSER_PACKAGES
        )
        await ClippyWasmTarget.lint(ClippyWasmTarget.WASI_TARGET, ClippyWasmTarget.WASI_PACKAGES)
    }

    /**
     * @param {string} target
     * @param {string[]} packages
     * @returns {Promise<void>}
     */
    static async lint(target, packages) {
        await ClippyWasmTarget.spawnClippy(
            target,
            ClippyWasmTarget.clippyArguments(target, packages)
        )
        console.log(`Clippy passed for ${target}`)
    }

    /**
     * @param {string} target
     * @param {string[]} packageNames
     * @returns {string[]}
     */
    static clippyArguments(target, packageNames) {
        const packages = packageNames.flatMap((name) => ['-p', name])
        return [
            './scripts/run-rust.mjs',
            'cargo',
            'clippy',
            '--locked',
            '--target',
            target,
            ...packages,
            '--all-features',
            '--',
            '-D',
            'warnings'
        ]
    }

    /**
     * @param {string} target
     * @param {string[]} args
     * @returns {Promise<void>}
     */
    static spawnClippy(target, args) {
        const environment = {
            ...process.env,
            CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS: ClippyWasmTarget.TARGET_RUSTFLAGS
        }
        return new Promise((resolve, reject) => {
            const child = spawn(process.execPath, args, { stdio: 'inherit', env: environment })
            child.once('error', reject)
            child.once('exit', (code) => ClippyWasmTarget.settle(target, code, resolve, reject))
        })
    }

    /**
     * @param {string} target
     * @param {number | null} code
     * @param {(value: void) => void} resolve
     * @param {(error: Error) => void} reject
     * @returns {void}
     */
    static settle(target, code, resolve, reject) {
        if (code !== 0) {
            reject(new Error(`clippy --target ${target} exited with ${code}`))
            return
        }
        resolve()
    }
}

await ClippyWasmTarget.run()
