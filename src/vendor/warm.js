// Lifecycle management for a warm shim subprocess (chex/ttid NDJSON client).
//
// A long-lived child with piped stdio keeps the parent's event loop alive, so a
// one-shot CLI/library process would hang on exit. We unref the child (and its
// pipes) while idle so the process can exit, and ref it again while any request
// is in flight so the response isn't dropped. An explicit in-flight counter
// drives the ref/unref transitions — refing on 0→1 and unrefing on 1→0 — so
// concurrent requests and out-of-order settles can't leave it stuck ref'd.
//
/**
 * @template {{ _proc: any, request: (op: any) => Promise<any> }} C
 * @param {C} client
 * @returns {C}
 */
export function warm(client) {
    const proc = client._proc
    const setRef = (/** @type {boolean} */ on) => {
        const fn = on ? 'ref' : 'unref'
        proc[fn]?.()
        proc.stdin?.[fn]?.()
        proc.stdout?.[fn]?.()
    }
    let inflight = 0
    setRef(false)
    const request = client.request.bind(client)
    client.request = (/** @type {any} */ op) => {
        if (inflight++ === 0) setRef(true)
        const done = () => {
            if (--inflight === 0) setRef(false)
        }
        return request(op).then(
            (/** @type {any} */ r) => {
                done()
                return r
            },
            (/** @type {any} */ e) => {
                done()
                throw e
            }
        )
    }
    return client
}
