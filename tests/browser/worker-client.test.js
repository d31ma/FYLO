import { describe, expect, test } from 'bun:test'
import { FyloWorkerClient } from '../../src/browser/worker/client.js'
import { FyloWorkerRuntime, handleWorkerMessage } from '../../src/browser/worker/runtime.js'

/**
 * @param {FyloWorkerRuntime} runtime
 * @param {Partial<ConstructorParameters<typeof FyloWorkerClient>[1]>} [options]
 * @returns {FyloWorkerClient}
 */
function createClient(runtime, options = {}) {
    const channel = new MessageChannel()
    channel.port2.onmessage = (message) => {
        void handleWorkerMessage(runtime, channel.port2, message.data)
    }
    channel.port1.start()
    channel.port2.start()
    return new FyloWorkerClient(channel.port1, {
        namespace: 'tests',
        storage: 'memory',
        root: '/',
        ...options
    })
}

describe('FYLO worker client/runtime', () => {
    test('propagates the browser build token to the worker module URL', async () => {
        const original = globalThis.SharedWorker
        let workerUrl
        globalThis.SharedWorker = class {
            constructor(url) {
                workerUrl = url
                this.port = {
                    onmessage: null,
                    start() {},
                    postMessage() {},
                    close() {}
                }
            }
        }
        try {
            const token = 'v=release-test'
            const module = await import(`../../src/browser/worker/client.js?${token}`)
            module.createWorkerClient({ namespace: 'tests', storage: 'memory' })
            expect(workerUrl.href).toEndWith(`/shared.js?${token}`)
        } finally {
            if (original === undefined) delete globalThis.SharedWorker
            else globalThis.SharedWorker = original
        }
    })

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

    test('waits for worker core initialization', async () => {
        const runtime = new FyloWorkerRuntime()
        const client = createClient(runtime)

        await expect(client.ready()).resolves.toBeUndefined()
        expect(runtime.cores.get('tests')?.index.accelerationStatus()).toEqual({
            mode: 'javascript',
            state: 'off'
        })
    })

    test('releases isolated File System Access worker cores on close', async () => {
        const runtime = new FyloWorkerRuntime()
        const client = createClient(runtime, { instanceId: 'fsa-test' })

        await client.ready()
        expect(runtime.cores.has('tests:fsa-test')).toBe(true)
        await client.close()
        expect(runtime.cores.has('tests:fsa-test')).toBe(false)
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
