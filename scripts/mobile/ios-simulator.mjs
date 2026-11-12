import { SimctlCall } from './simctl-call.mjs'

/**
 * The iOS simulator device FYLO's tests use (ADR 0026). It is made once and
 * kept: a new device's first boot starts about 240 iOS processes and pushed
 * the Mac's load past 400, while a later boot of the same device is light.
 * `bun scripts/test-mobile.mjs ios --warm-up` does that first boot alone;
 * `--remove-device` deletes it. FYLO_IOS_DEVICE_TYPE names another device
 * type, for an Xcode whose runtimes do not run DEVICE_TYPE (the release job).
 */
export class IosSimulator {
    static DEVICE_NAME = 'fylo-tests'
    static DEVICE_TYPE = 'com.apple.CoreSimulator.SimDeviceType.iPhone-16'
    // A first boot on a GitHub runner can take minutes; never wait forever.
    static BOOT_LIMIT_MS = 15 * 60_000

    constructor() {
        this.udid = ''
        /** @type {SimctlCall | undefined} */
        this.boot = undefined
    }

    /** Find the kept device, making it first if it does not exist; no boot. */
    async prepare() {
        this.udid = await IosSimulator.keptDevice()
        console.error(`=== simulator ${this.udid}`)
    }

    /** Boot the prepared device and wait until it is ready. */
    async start() {
        console.error(`=== simulator boot ${this.udid}`)
        this.boot = new SimctlCall(['bootstatus', this.udid, '-b'], IosSimulator.BOOT_LIMIT_MS)
        await this.boot.run()
    }

    /** End a boot still waiting, shut the device down, and keep it. */
    async stop() {
        this.boot?.cancel('was ended: the device is shutting down')
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
            IosSimulator.deviceType(),
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
        // support only their own model, so pick one that runs the device type.
        const runtime = listing.runtimes.findLast((candidate) =>
            candidate.supportedDeviceTypes.some(
                (type) => type.identifier === IosSimulator.deviceType()
            )
        )
        if (runtime === undefined) {
            throw new Error(
                'No iOS simulator runtime is installed: run xcodebuild -downloadPlatform iOS'
            )
        }
        return runtime.identifier
    }

    /** @returns {string} */
    static deviceType() {
        return process.env.FYLO_IOS_DEVICE_TYPE ?? IosSimulator.DEVICE_TYPE
    }

    /**
     * @param {string[]} args
     * @returns {Promise<string>}
     */
    static simctl(args) {
        return new SimctlCall(args).run()
    }
}
