import { spawn } from 'node:child_process'
import { join } from 'node:path'
import { homedir } from 'node:os'
import { setTimeout as sleep } from 'node:timers/promises'

/** One headless Android emulator, started for a test run and stopped after it (ADR 0026). */
export class AndroidEmulator {
    static DEFAULT_AVD = 'tachyon-gate'
    static PORT = 5584
    static BOOT_TIMEOUT_MS = 300_000
    static POLL_MS = 2_000

    /** @param {NodeJS.ProcessEnv} environment */
    constructor(environment) {
        this.sdk = environment.ANDROID_HOME ?? join(homedir(), 'Library', 'Android', 'sdk')
        this.avd = environment.FYLO_ANDROID_AVD ?? AndroidEmulator.DEFAULT_AVD
        this.serial = `emulator-${AndroidEmulator.PORT}`
        this.process = null
    }

    /** @returns {string} */
    adb() {
        return join(this.sdk, 'platform-tools', 'adb')
    }

    /**
     * Start the emulator read-only, so the shared AVD is never changed, and
     * wait until Android has booted.
     *
     * @returns {Promise<void>}
     */
    async start() {
        const emulator = join(this.sdk, 'emulator', 'emulator')
        const args = ['-avd', this.avd, '-port', String(AndroidEmulator.PORT), '-read-only']
        const quiet = [
            '-no-window',
            '-no-audio',
            '-no-boot-anim',
            '-no-snapshot',
            '-gpu',
            'swiftshader_indirect'
        ]
        console.error(`=== emulator ${this.avd} as ${this.serial}`)
        this.process = spawn(emulator, [...args, ...quiet], {
            stdio: ['ignore', 'inherit', 'inherit']
        })
        await this.waitForBoot()
    }

    /** @returns {Promise<void>} */
    async waitForBoot() {
        const deadline = Date.now() + AndroidEmulator.BOOT_TIMEOUT_MS
        while (Date.now() < deadline) {
            if ((await this.property('sys.boot_completed')) === '1') return
            await sleep(AndroidEmulator.POLL_MS)
        }
        throw new Error(`${this.avd} did not boot within ${AndroidEmulator.BOOT_TIMEOUT_MS} ms`)
    }

    /**
     * @param {string} name
     * @returns {Promise<string>}
     */
    property(name) {
        return new Promise((resolve) => {
            const child = spawn(this.adb(), ['-s', this.serial, 'shell', 'getprop', name], {
                stdio: ['ignore', 'pipe', 'ignore']
            })
            let output = ''
            child.stdout.on('data', (chunk) => (output += chunk))
            child.once('error', () => resolve(''))
            child.once('close', () => resolve(output.trim()))
        })
    }

    /** @returns {Promise<void>} */
    async stop() {
        console.error(`=== emulator stop ${this.serial}`)
        await new Promise((resolve) => {
            const child = spawn(this.adb(), ['-s', this.serial, 'emu', 'kill'], {
                stdio: 'inherit'
            })
            child.once('error', resolve)
            child.once('close', resolve)
        })
        await this.waitForExit()
    }

    /** @returns {Promise<void>} */
    async waitForExit() {
        if (this.process === null || this.process.exitCode !== null) return
        await new Promise((resolve) => this.process.once('exit', resolve))
    }
}
