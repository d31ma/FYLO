import { describe, expect, test } from 'bun:test'
import { CoalescingScheduler } from '../../src/replication/coalescing-scheduler.js'

function deferred() {
    let resolve
    const promise = new Promise((done) => (resolve = done))
    return { promise, resolve }
}

describe('S3 reconcile scheduler', () => {
    test('coalesces an active run and any number of requests into one pending run', async () => {
        const gates = [deferred(), deferred()]
        let runs = 0
        const scheduler = new CoalescingScheduler(async () => {
            const gate = gates[runs++]
            await gate.promise
        })

        const first = scheduler.trigger()
        scheduler.trigger()
        scheduler.trigger()
        expect(runs).toBe(1)
        gates[0].resolve()
        await Bun.sleep(0)
        expect(runs).toBe(2)
        gates[1].resolve()
        await first
        expect(runs).toBe(2)
        await scheduler.close()
    })

    test('close cancels the pending pass, rejects new work, and drains the active pass', async () => {
        const gate = deferred()
        let runs = 0
        const scheduler = new CoalescingScheduler(async () => {
            runs++
            await gate.promise
        })
        const active = scheduler.trigger()
        scheduler.trigger()
        const closing = scheduler.close()
        expect(scheduler.state).toBe('closing')
        expect(runs).toBe(1)
        gate.resolve()
        await Promise.all([active, closing])
        expect(runs).toBe(1)
        expect(scheduler.state).toBe('closed')
        await expect(scheduler.trigger()).rejects.toThrow('closed')
    })

    test('validates the interval before creating a timer', () => {
        for (const intervalMs of [0, 999, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
            expect(() => new CoalescingScheduler(async () => {}, { intervalMs })).toThrow(
                'reconcileIntervalMs'
            )
        }
        expect(() => new CoalescingScheduler(async () => {}, { intervalMs: 1_000 })).not.toThrow()
    })

    test('recovers after a failed pass and does not wedge future work', async () => {
        let runs = 0
        const scheduler = new CoalescingScheduler(async () => {
            runs++
            if (runs === 1) throw new Error('transient pass failure')
        })

        await expect(scheduler.trigger()).rejects.toThrow('transient pass failure')
        await expect(scheduler.trigger()).resolves.toBeUndefined()
        expect(runs).toBe(2)
        await scheduler.close()
    })
})
