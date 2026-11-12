import { spawn } from 'node:child_process'

/**
 * The iOS simulator device FYLO's tests use (ADR 0026). It is made once and
 * kept: a new device's first boot starts about 240 iOS processes and pushed
 * the Mac's load past 400, while a later boot of the same device is light.
 * `bun scripts/test-mobile.mjs ios --warm-up` does that first boot alone;
 * `--remove-device` deletes it.
 */
export class IosSimulator {
    static DEVICE_NAME = 'fylo-tests'
    static DEVICE_TYPE = 'com.apple.CoreSimulator.SimDeviceType.iPhone-16'

    constructor() {
        this.udid = ''
    }

    /** Boot the kept device, making it first if it does not exist. */
    async start() {
        this.udid = await IosSimulator.keptDevice()
        console.error(`=== simulator ${this.udid}`)
        await IosSimulator.simctl(['bootstatus', this.udid, '-b'])
    }

    /** Shut the device down and keep it. */
    async stop() {
        console.error(`=== simulator shutdown ${this.udid}`)
        await IosSimulator.simctl(['shutdown', this.udid]).catch(() => undefined)
    }

    /** Shut the device down and delete it. */
    async remove() {
        this.udid = await IosSimulator.existingDevice()
        if (this.udid === '') return
        await this.stop()
        console.error(`=== simulator delete ${this.udid}`)
        await IosSimulator.simctl(['delete', this.udid])
    }

    /** @returns {Promise<string>} */
    static async keptDevice() {
        const existing = await IosSimulator.existingDevice()
        if (existing !== '') return existing
        const runtime = await IosSimulator.newestRuntime()
        console.error(`=== simulator create ${IosSimulator.DEVICE_NAME} on ${runtime}`)
        const created = await IosSimulator.simctl([
            'create',
            IosSimulator.DEVICE_NAME,
            IosSimulator.DEVICE_TYPE,
            runtime
        ])
        return created.trim()
    }

    /** @returns {Promise<string>} The kept device's UDID, or '' when there is none. */
    static async existingDevice() {
        const listing = JSON.parse(await IosSimulator.simctl(['list', 'devices', '--json']))
        const devices = Object.values(listing.devices).flat()
        const kept = devices.find(
            (device) => device.name === IosSimulator.DEVICE_NAME && device.isAvailable
        )
        return kept?.udid ?? ''
    }

    /** @returns {Promise<string>} */
    static async newestRuntime() {
        const listing = JSON.parse(
            await IosSimulator.simctl(['list', 'runtimes', 'iOS', 'available', '--json'])
        )
        // Device-specific runtimes (an identifier ending in -iPhone<model>)
        // support only their own model, so pick one that runs DEVICE_TYPE.
        const runtime = listing.runtimes.findLast((candidate) =>
            candidate.supportedDeviceTypes.some(
                (type) => type.identifier === IosSimulator.DEVICE_TYPE
            )
        )
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
