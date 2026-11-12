import { execFileSync } from 'node:child_process'
import { loadavg } from 'node:os'

/**
 * Stops a phone test run before it overloads the Mac (2026-11-11/12: iOS
 * simulator boots pushed the load to 518, 401 and 137). It starts before the
 * device boots and every 2 s reads the 1-minute load, how much it rose in
 * the last 10 s, how many more processes the Mac runs than just before the
 * boot, and macOS free memory. On a breach it calls `stop` once: the caller
 * stops the run's own process group by its saved id and shuts its own device
 * down by UDID.
 */
export class DeviceGuard {
    static POLL_MS = 2_000
    static LOAD_LIMIT = 60
    // A rise this steep within RISE_WINDOW_MS is a boot storm, even below the limit.
    static RISE_LIMIT = 40
    static RISE_WINDOW_MS = 10_000
    // More processes than this over the count taken just before the boot.
    static PROCESS_LIMIT = 260
    // Below this share of free memory macOS starts to swap hard (the dev
    // host's VM alone holds about 6 GiB).
    static FREE_MEMORY_LIMIT = 15

    /**
     * @param {{ loadLimit?: number, stop: (reason: string) => Promise<void> }} options
     */
    constructor({ loadLimit, stop }) {
        this.loadLimit = loadLimit ?? DeviceGuard.LOAD_LIMIT
        this.stop = stop
        this.timer = undefined
        this.breached = false
        this.baseline = 0
        this.samples = []
    }

    /** Take the process baseline and start polling; call it before the boot. */
    start() {
        this.baseline = DeviceGuard.processCount()
        console.error(`=== guard baseline: ${this.baseline} processes`)
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
        const rise = this.rise(load)
        const added = DeviceGuard.processCount() - this.baseline
        const free = DeviceGuard.freeMemory()
        const time = new Date().toISOString().slice(11, 19)
        console.error(
            `=== guard ${time} load ${load.toFixed(1)} rise ${rise.toFixed(1)} added processes ${added} free memory ${free}%`
        )
        return this.limitPassed(load, rise, added, free)
    }

    limitPassed(load, rise, added, free) {
        if (load > this.loadLimit) return `1-minute load ${load.toFixed(1)} over ${this.loadLimit}`
        if (rise > DeviceGuard.RISE_LIMIT) return `load rose ${rise.toFixed(1)} in 10 s`
        if (added > DeviceGuard.PROCESS_LIMIT) return `${added} more processes than before the boot`
        if (free < DeviceGuard.FREE_MEMORY_LIMIT)
            return `free memory ${free}% under ${DeviceGuard.FREE_MEMORY_LIMIT}%`
        return undefined
    }

    /** How much the load rose over the last RISE_WINDOW_MS. */
    rise(load) {
        const now = Date.now()
        this.samples.push({ time: now, load })
        this.samples = this.samples.filter(
            (sample) => now - sample.time <= DeviceGuard.RISE_WINDOW_MS
        )
        return load - this.samples[0].load
    }

    static processCount() {
        const listing = execFileSync('ps', ['-axo', 'pid='], { encoding: 'utf8' })
        return listing.split('\n').filter((line) => line.trim() !== '').length
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
