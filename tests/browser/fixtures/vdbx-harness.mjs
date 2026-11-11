// A remote control for `fylo-browser.wasm` over a single `.vdbx` file, on a
// page served without cross-origin isolation, as a static host serves one.
//
// `engine-harness.mjs` drives the engine over an OPFS folder; this page drives
// it over one database file through the shipped single-file Worker
// (`dist-web/vdbx-worker.js`). It runs no scenario of its own: it exposes
// `globalThis.vdbx`, and the specs open databases from bytes they hand over,
// run batches, kill the Worker mid-batch, and take the file back out to the
// native engine.
//
// Bytes cross the page boundary as base64, gzipped when asked: a `.vdbx`
// preallocates whole extents, so a 64 MiB file is mostly zeros and shrinks to
// a few hundred kilobytes. A source can also name a URL on this origin, which
// is how the specs hand over large native fixtures.

import {
    SingleFileDatabase,
    listWorkingCopies,
    removeWorkingCopy,
    workingCopyName
} from '/src/browser/vdbx-client.mjs'
import {
    createWebCryptoIdentity,
    describeIdentity,
    hpkeUnwrap,
    hpkeWrap,
    loadWebCryptoIdentity
} from '/src/browser/identity-bridge.mjs'

const WORKER_URL = '/dist-web/vdbx-worker.js'
const IDENTITY_WORKER_URL = '/dist-web/identity-worker.js'
const MODULE_URL = '/dist-web/fylo-browser.wasm'

/** @type {Map<number, { database: SingleFileDatabase, name?: string, kind: string }>} */
const sessions = new Map()
let nextSession = 1

function session(id) {
    const found = sessions.get(id)
    if (!found) throw new Error(`no session ${id}`)
    return found
}

function failure(error) {
    return { error: String(error?.message ?? error), code: error?.code ?? error?.name ?? null }
}

/** The bytes a source describes: `{ url }` on this origin, or `{ base64, gzip? }`. */
async function blobOf(source) {
    if (source.url) {
        const response = await fetch(source.url, { cache: 'no-store' })
        if (!response.ok) throw new Error(`${source.url}: HTTP ${response.status}`)
        return await response.blob()
    }
    const bytes = fromBase64(source.base64 ?? '')
    return new Blob([source.gzip ? await gunzip(bytes) : bytes])
}

/**
 * Open a database. Refusals come back as `{ error, code }`, not as a throw.
 * A working copy (`copy`, `opfs`) is kept in `source.storage`: `opfs` (the
 * default), `indexeddb`, or `local` to let the Worker choose.
 */
async function open(source) {
    const started = performance.now()
    let name
    let opened
    try {
        const storage = source.storage ?? 'opfs'
        if (source.kind === 'copy') {
            name = source.name ?? workingCopyName(source.label ?? 'harness.vdbx')
            opened = { kind: storage, name, copyFrom: await blobOf(source) }
        } else if (source.kind === 'opfs') {
            name = source.name
            opened = { kind: storage, name }
        } else if (source.kind === 'file') {
            const blob = await blobOf(source)
            opened = { kind: 'file', file: new File([blob], source.label ?? 'picked.vdbx') }
        } else if (source.kind === 'url') {
            opened = { kind: 'url', url: source.url }
        } else {
            throw new Error(`unknown source kind ${source.kind}`)
        }
        const database = await SingleFileDatabase.open(opened, {
            workerUrl: WORKER_URL,
            moduleUrl: MODULE_URL,
            identity: source.identity
                ? { ...source.identity, workerUrl: IDENTITY_WORKER_URL }
                : undefined
        })
        const id = nextSession++
        sessions.set(id, { database, name, kind: source.kind })
        return { id, name, info: database.info, openMs: performance.now() - started }
    } catch (error) {
        return { ...failure(error), name }
    }
}

/** A batch as NDJSON; each request gets the database's path as its root. */
function ndjsonOf(database, requests) {
    return (
        requests
            .map((request) => JSON.stringify({ root: database.info.path, ...request }))
            .join('\n') + '\n'
    )
}

function framesOf(text) {
    return text
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
}

/** Run one batch. */
async function run(id, requests, timeoutMs) {
    const { database } = session(id)
    const started = performance.now()
    const answer = database.exec(ndjsonOf(database, requests)).then(
        (text) => ({ frames: framesOf(text), ms: performance.now() - started }),
        (error) => failure(error)
    )
    // A wasm trap ends up as an error above; a hang is the one outcome that
    // would otherwise never come back.
    const timeout = new Promise((resolve) =>
        setTimeout(() => resolve({ timedOut: true, ms: timeoutMs }), timeoutMs)
    )
    return Promise.race([answer, timeout])
}

globalThis.vdbx = {
    surface() {
        return {
            crossOriginIsolated: globalThis.crossOriginIsolated === true,
            sharedArrayBuffer: typeof SharedArrayBuffer,
            opfs: typeof navigator.storage?.getDirectory,
            compressionStream: typeof CompressionStream,
            userAgent: navigator.userAgent
        }
    },

    /** Why this browser cannot keep an OPFS working copy, or '' when it can. */
    /** Whether this page is a secure context, and the APIs that follow from it. */
    context() {
        return {
            secure: globalThis.isSecureContext === true,
            storageManager: typeof navigator.storage,
            locks: typeof navigator.locks,
            indexedDB: typeof indexedDB
        }
    },

    /** The IndexedDB working copies this origin holds. */
    listIndexed() {
        return listWorkingCopies({ storage: 'indexeddb' })
    },

    async opfs() {
        if (typeof navigator.storage?.getDirectory !== 'function') {
            return 'this browser build has no origin-private file system'
        }
        try {
            await navigator.storage.getDirectory()
        } catch (error) {
            return `OPFS refused: ${error?.name ?? error}`
        }
        return ''
    },

    open,

    /** Make the origin's `webcrypto:<name>` identity; its text forms come back. */
    async createIdentity(name) {
        try {
            return await describeIdentity(await createWebCryptoIdentity(name))
        } catch (error) {
            return failure(error)
        }
    },

    /** Whether the origin's `webcrypto:<name>` keys are non-extractable CryptoKeys. */
    async identityKeys(name) {
        const identity = await loadWebCryptoIdentity(name)
        return {
            sign: { type: identity.sign.type, extractable: identity.sign.extractable },
            derive: { type: identity.derive.type, extractable: identity.derive.extractable }
        }
    },

    /** Wrap and unwrap a secret with the page's own HPKE, for `name`'s key. */
    async hpkeRoundTrip(name, secretBase64, infoBase64) {
        const identity = await loadWebCryptoIdentity(name)
        const secret = fromBase64(secretBase64)
        const info = fromBase64(infoBase64)
        const wrapped = await hpkeWrap(identity.publicKey, info, secret)
        const opened = await hpkeUnwrap(identity.derive, identity.publicKey, info, wrapped)
        return toBase64(opened)
    },

    exec(id, requests, timeoutMs = 300_000) {
        return run(id, requests, timeoutMs)
    },

    /**
     * Queue `chunks` as one batch of execs and kill the Worker the way a
     * crash does (no close, no flush) the moment the answer to chunk
     * `killAfter` arrives, with every later chunk still queued behind it. The Worker answers chunks one at a time, in order, so
     * the kill lands after exactly `killAfter` answered chunks and while the
     * rest of the batch is still running. It happens here, in the task that
     * delivers that answer, so no round trip to the test gives the Worker
     * time to run on to the end. Comes back with the frames answered before
     * the kill, or `{ error, code }` when a chunk failed first.
     */
    killMidBatch(id, chunks, killAfter) {
        const { database } = session(id)
        const answered = []
        return new Promise((resolve) => {
            chunks.forEach((requests, index) => {
                database.exec(ndjsonOf(database, requests)).then(
                    (text) => {
                        answered.push(...framesOf(text))
                        if (index + 1 !== killAfter) return
                        database.worker.terminate()
                        sessions.delete(id)
                        resolve({ frames: answered, queued: chunks.length - killAfter })
                    },
                    (error) => resolve(failure(error))
                )
            })
        })
    },

    /**
     * Run `batches` one after another, each waiting for the answer before the
     * next is sent, and kill the Worker `killAfterMs` after the first one
     * starts, wherever it is: inside the engine, inside a store, or between
     * two batches. Comes back with how many batches were answered, and
     * whether one was still running when the Worker went.
     */
    writeUntilKilled(id, batches, killAfterMs) {
        const { database } = session(id)
        let answered = 0
        let running = false
        let killed = false
        return new Promise((resolve) => {
            const kill = () => {
                killed = true
                database.worker.terminate()
                sessions.delete(id)
                resolve({ answered, inFlight: running, finished: answered === batches.length })
            }
            const timer = setTimeout(kill, killAfterMs)
            ;(async () => {
                for (const requests of batches) {
                    running = true
                    let frames
                    try {
                        frames = framesOf(await database.exec(ndjsonOf(database, requests)))
                    } catch (error) {
                        if (killed) return
                        clearTimeout(timer)
                        resolve({ ...failure(error), answered })
                        return
                    }
                    if (killed) return
                    running = false
                    const failed = frames.find((frame) => !frame.ok)
                    if (failed) {
                        clearTimeout(timer)
                        resolve({ error: JSON.stringify(failed.error), answered })
                        return
                    }
                    answered++
                }
                clearTimeout(timer)
                if (!killed) kill()
            })()
        })
    },

    /** Kill the Worker the way a crash does, with no close and no store. */
    kill(id) {
        const { database } = session(id)
        database.worker.terminate()
        sessions.delete(id)
    },

    async close(id) {
        const found = sessions.get(id)
        if (!found) return
        sessions.delete(id)
        await found.database.close()
    },

    /** The whole file as it stands: its size, SHA-256, and gzipped base64. */
    async snapshot(id) {
        const blob = await session(id).database.snapshot()
        const bytes = new Uint8Array(await blob.arrayBuffer())
        return {
            size: bytes.length,
            sha256: await sha256(bytes),
            gzipBase64: toBase64(new Uint8Array(await gzip(bytes)))
        }
    },

    /** The bytes a source would hand the Worker, digested here. */
    async digest(source) {
        return sha256(new Uint8Array(await (await blobOf(source)).arrayBuffer()))
    },

    /**
     * Remove a working copy. A Worker terminated mid-batch can hold the
     * copy's sync access handle for a moment after it is gone (Firefox
     * refuses the removal with NoModificationAllowedError meanwhile), so a
     * held copy is retried for up to ten seconds.
     */
    async removeCopy(name, storage = 'opfs') {
        for (let waited = 0; ; waited += 250) {
            try {
                await removeWorkingCopy(name, { storage })
                return
            } catch (error) {
                if (error?.name === 'NotFoundError') return
                const held = error?.name === 'NoModificationAllowedError' || error?.code === 'EBUSY'
                if (!held || waited >= 10_000) throw error
            }
            await new Promise((resolve) => setTimeout(resolve, 250))
        }
    },

    /**
     * What a first open costs: the Worker started, the module fetched and
     * instantiated over a picked file, then the first and a second handshake.
     */
    async startup(source) {
        const opened = await open({ ...source, kind: 'file' })
        if (opened.error) return opened
        try {
            const first = await run(opened.id, [{ op: 'handshake' }], 60_000)
            const second = await run(opened.id, [{ op: 'handshake' }], 60_000)
            return {
                openMs: opened.openMs,
                firstHandshakeMs: first.ms,
                secondHandshakeMs: second.ms,
                handshake: first.frames?.[0] ?? first
            }
        } finally {
            await globalThis.vdbx.close(opened.id)
        }
    }
}

async function gzip(bytes) {
    const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream('gzip'))
    return new Response(stream).arrayBuffer()
}

async function gunzip(bytes) {
    const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'))
    return new Uint8Array(await new Response(stream).arrayBuffer())
}

async function sha256(bytes) {
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))
    return [...hash].map((byte) => byte.toString(16).padStart(2, '0')).join('')
}

function toBase64(bytes) {
    let binary = ''
    for (let offset = 0; offset < bytes.length; offset += 0x8000) {
        binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000))
    }
    return btoa(binary)
}

function fromBase64(text) {
    const binary = atob(text)
    const bytes = new Uint8Array(binary.length)
    for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index)
    return bytes
}

const output = document.querySelector('output')
output.dataset.status = 'ready'
output.textContent = 'ready'
