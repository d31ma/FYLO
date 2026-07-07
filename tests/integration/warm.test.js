import { describe, expect, test } from 'bun:test'
import { warm } from '../../src/vendor/warm.js'

// warm() ref/unref a shim subprocess around in-flight requests. A fake `_proc`
// lets us observe the ref state deterministically without a real child.
describe('warm() shim lifecycle', () => {
    function harness(request) {
        let refd = null
        const proc = { ref: () => (refd = true), unref: () => (refd = false) }
        const client = warm({ _proc: proc, request })
        return { client, isRefd: () => refd }
    }

    test('releases the ref when request throws synchronously (#54)', () => {
        const { client, isRefd } = harness(() => {
            throw new Error('sync boom')
        })
        expect(() => client.request({})).toThrow('sync boom')
        // A synchronous throw must not leave the process pinned ref'd.
        expect(isRefd()).toBe(false)
    })

    test('refs while a request is in flight and unrefs after it settles', async () => {
        const { client, isRefd } = harness(async () => 'ok')
        const pending = client.request({})
        expect(isRefd()).toBe(true)
        expect(await pending).toBe('ok')
        expect(isRefd()).toBe(false)
    })
})
