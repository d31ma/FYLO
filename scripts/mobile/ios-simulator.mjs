import { spawn } from 'node:child_process'

/** One iOS simulator device, made and booted for a test run and deleted after it (ADR 0026). */
export class IosSimulator {
    static DEVICE_NAME = 'fylo-tests'
    static DEVICE_TYPE = 'com.apple.CoreSimulator.SimDeviceType.iPhone-16'

    constructor() {
        this.udid = ''
    }

    /** @returns {Promise<void>} */
    async start() {
        const runtime = await IosSimulator.newestRuntime()
        const created = await IosSimulator.simctl([
            'create',
            IosSimulator.DEVICE_NAME,
            IosSimulator.DEVICE_TYPE,
            runtime
        ])
        this.udid = created.trim()
        console.error(`=== simulator ${this.udid} on ${runtime}`)
        await IosSimulator.simctl(['bootstatus', this.udid, '-b'])
    }

    /** @returns {Promise<void>} */
    async stop() {
        console.error(`=== simulator stop ${this.udid}`)
        await IosSimulator.simctl(['shutdown', this.udid])
        await IosSimulator.simctl(['delete', this.udid])
    }

    /** @returns {Promise<string>} */
    static async newestRuntime() {
        const listing = JSON.parse(
            await IosSimulator.simctl(['list', 'runtimes', 'iOS', 'available', '--json'])
        )
        const runtime = listing.runtimes.at(-1)
        if (runtime === undefined) {
            throw new Error(
                'No iOS simulator runtime is installed: run xcodebuild -downloadPlatform iOS'
            )
        }
        return runtime.identifier
    }

    /**
     * @param {string[]} args
     * @returns {Promise<string>}
     */
    static simctl(args) {
        return new Promise((resolve, reject) => {
            const child = spawn('xcrun', ['simctl', ...args], {
                stdio: ['ignore', 'pipe', 'inherit']
            })
            let output = ''
            child.stdout.on('data', (chunk) => (output += chunk))
            child.once('error', reject)
            child.once('close', (code) =>
                code === 0
                    ? resolve(output)
                    : reject(new Error(`simctl ${args[0]} exited with ${code}`))
            )
        })
    }
}
