import { spawn } from 'node:child_process'

export class ClippyWasmTarget {
    static TARGET = 'wasm32-unknown-unknown'
    static PACKAGES = [
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
        await ClippyWasmTarget.spawnClippy(ClippyWasmTarget.clippyArguments())
        console.log(`Clippy passed for ${ClippyWasmTarget.TARGET}`)
    }

    /** @returns {string[]} */
    static clippyArguments() {
        const packages = ClippyWasmTarget.PACKAGES.flatMap((name) => ['-p', name])
        return [
            './scripts/run-rust.mjs',
            'cargo',
            'clippy',
            '--locked',
            '--target',
            ClippyWasmTarget.TARGET,
            ...packages,
            '--all-features',
            '--',
            '-D',
            'warnings'
        ]
    }

    /**
     * @param {string[]} args
     * @returns {Promise<void>}
     */
    static spawnClippy(args) {
        const environment = {
            ...process.env,
            CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS: ClippyWasmTarget.TARGET_RUSTFLAGS
        }
        return new Promise((resolve, reject) => {
            const child = spawn(process.execPath, args, { stdio: 'inherit', env: environment })
            child.once('error', reject)
            child.once('exit', (code) => ClippyWasmTarget.settle(code, resolve, reject))
        })
    }

    /**
     * @param {number | null} code
     * @param {(value: void) => void} resolve
     * @param {(error: Error) => void} reject
     * @returns {void}
     */
    static settle(code, resolve, reject) {
        if (code !== 0) {
            reject(new Error(`clippy --target ${ClippyWasmTarget.TARGET} exited with ${code}`))
            return
        }
        resolve()
    }
}

await ClippyWasmTarget.run()
