import { describe, expect, test } from 'bun:test'
import { FyloWorkerClient } from '../../src/browser/worker/client.js'
import { FyloWorkerRuntime, handleWorkerMessage } from '../../src/browser/worker/runtime.js'

/**
 * @param {FyloWorkerRuntime} runtime
 * @returns {FyloWorkerClient}
 */
function createClient(runtime) {
    const channel = new MessageChannel()
    channel.port2.onmessage = (message) => {
        void handleWorkerMessage(runtime, channel.port2, message.data)
    }
    channel.port1.start()
    channel.port2.start()
    return new FyloWorkerClient(channel.port1, {
        namespace: 'tests',
        storage: 'memory',
        root: '/'
    })
}

describe('FYLO worker client/runtime', () => {
    test('correlates request/response envelopes', async () => {
        const runtime = new FyloWorkerRuntime()
        const client = createClient(runtime)

        const response = await client.envelope({
            op: 'createCollection',
            collection: 'users'
        })

        expect(response.ok).toBe(true)
        expect(response.result).toEqual({ collection: 'users' })
    })

    test('fans collection events out to every subscribed port in the namespace', async () => {
        const runtime = new FyloWorkerRuntime()
        const a = createClient(runtime)
        const b = createClient(runtime)
        const eventsA = []
        const eventsB = []

        a.subscribe('users', (event) => eventsA.push(event))
        b.subscribe('users', (event) => eventsB.push(event))
        await Bun.sleep(1)

        await a.request({ op: 'createCollection', collection: 'users' })
        const id = await a.request({
            op: 'putData',
            collection: 'users',
            data: { name: 'Worker' }
        })
        await Bun.sleep(1)

        expect(eventsA).toContainEqual(expect.objectContaining({ action: 'insert', id }))
        expect(eventsB).toContainEqual(expect.objectContaining({ action: 'insert', id }))
    })
})
