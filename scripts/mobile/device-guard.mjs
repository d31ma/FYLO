import { execFileSync } from 'node:child_process'
import { loadavg } from 'node:os'

/**
 * Stops a phone test run before it overloads the Mac (2026-11-11/12: an iOS
 * simulator's first boot pushed the load past 400). Every few seconds it
 * reads the 1-minute load and, for a simulator, counts the processes of that
 * one device (by UDID). On a breach it calls `stop` once: the caller kills
 * the run's own process group by its saved id and shuts its own device down.
 */
export class DeviceGuard {
    static POLL_MS = 5_000
    static LOAD_LIMIT = 60
    static PROCESS_LIMIT = 260
    // Below this share of free memory macOS starts to swap hard (the dev
    // host's VM alone holds about 6 GiB).
    static FREE_MEMORY_LIMIT = 15

    /**
     * @param {{ udid?: string, loadLimit?: number, stop: (reason: string) => Promise<void> }} options
     */
    constructor({ udid, loadLimit, stop }) {
        this.udid = udid ?? ''
        this.loadLimit = loadLimit ?? DeviceGuard.LOAD_LIMIT
        this.stop = stop
        this.timer = undefined
        this.breached = false
    }

    start() {
        this.timer = setInterval(() => this.check(), DeviceGuard.POLL_MS)
    }

    end() {
        clearInterval(this.timer)
    }

    check() {
        const reason = this.breach()
        if (reason === undefined || this.breached) return
        this.breached = true
        console.error(`=== guard: ${reason}; stopping the run`)
        this.end()
        this.stop(reason)
    }

    /** Read one sample, log it, and name the limit it passes, if any. */
    breach() {
        const load = loadavg()[0]
        const processes = this.deviceProcesses()
        const free = DeviceGuard.freeMemory()
        const time = new Date().toISOString().slice(11, 19)
        console.error(
            `=== guard ${time} load ${load.toFixed(1)} device processes ${processes} free memory ${free}%`
        )
        if (load > this.loadLimit) return `1-minute load ${load.toFixed(1)} over ${this.loadLimit}`
        if (processes > DeviceGuard.PROCESS_LIMIT) return `${processes} processes on the device`
        if (free < DeviceGuard.FREE_MEMORY_LIMIT)
            return `free memory ${free}% under ${DeviceGuard.FREE_MEMORY_LIMIT}%`
        return undefined
    }

    deviceProcesses() {
        if (this.udid === '') return 0
        const listing = execFileSync('ps', ['-axo', 'command'], { encoding: 'utf8' })
        return listing.split('\n').filter((line) => line.includes(this.udid)).length
    }

    /** macOS's free-memory percentage (`memory_pressure`), 100 elsewhere. */
    static freeMemory() {
        if (process.platform !== 'darwin') return 100
        const summary = execFileSync('memory_pressure', { encoding: 'utf8' })
        const match = summary.match(/free percentage: (\d+)%/)
        return match === null ? 100 : Number(match[1])
    }

    /** Wait until the 1-minute load is under `limit`, at most `timeoutMs`. */
    static async quiet(limit, timeoutMs) {
        const deadline = Date.now() + timeoutMs
        while (loadavg()[0] >= limit) {
            if (Date.now() > deadline) throw new Error(`the load stayed at or above ${limit}`)
            await new Promise((resolve) => setTimeout(resolve, 15_000))
        }
    }
}
