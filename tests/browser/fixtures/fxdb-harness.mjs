// A remote control for `fylo-browser.wasm` over a single `.fxdb` file, on a
// page served without cross-origin isolation, as a static host serves one.
//
// `engine-harness.mjs` drives the engine over an OPFS folder; this page drives
// it over one database file through the shipped single-file Worker
// (`dist-web/fxdb-worker.js`). It runs no scenario of its own: it exposes
// `globalThis.fxdb`, and the specs open databases from bytes they hand over,
// run batches, kill the Worker mid-batch, and take the file back out to the
// native engine.
//
// Bytes cross the page boundary as base64, gzipped when asked: a `.fxdb`
// preallocates whole extents, so a 64 MiB file is mostly zeros and shrinks to
// a few hundred kilobytes. A source can also name a URL on this origin, which
// is how the specs hand over large native fixtures.

import {
    SingleFileDatabase,
    removeWorkingCopy,
    workingCopyName
} from '/src/browser/fxdb-client.mjs'

const WORKER_URL = '/dist-web/fxdb-worker.js'
const MODULE_URL = '/dist-web/fylo-browser.wasm'

/** @type {Map<number, { database: SingleFileDatabase, name?: string, kind: string }>} */
const sessions = new Map()
/** @type {Map<number, { done: boolean, value?: unknown, promise: Promise<unknown> }>} */
const tickets = new Map()
let nextSession = 1
let nextTicket = 1

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

/** Open a database. Refusals come back as `{ error, code }`, not as a throw. */
async function open(source) {
    const started = performance.now()
    let name
    let opened
    try {
        if (source.kind === 'copy') {
            name = source.name ?? workingCopyName(source.label ?? 'harness.fxdb')
            opened = { kind: 'opfs', name, copyFrom: await blobOf(source) }
        } else if (source.kind === 'opfs') {
            name = source.name
            opened = { kind: 'opfs', name }
        } else if (source.kind === 'file') {
            const blob = await blobOf(source)
            opened = { kind: 'file', file: new File([blob], source.label ?? 'picked.fxdb') }
        } else if (source.kind === 'url') {
            opened = { kind: 'url', url: source.url }
        } else {
            throw new Error(`unknown source kind ${source.kind}`)
        }
        const database = await SingleFileDatabase.open(opened, {
            workerUrl: WORKER_URL,
            moduleUrl: MODULE_URL
        })
        const id = nextSession++
        sessions.set(id, { database, name, kind: source.kind })
        return { id, name, info: database.info, openMs: performance.now() - started }
    } catch (error) {
        return { ...failure(error), name }
    }
}

/** Run one batch; each request gets the database's path as its root. */
async function run(id, requests, timeoutMs) {
    const { database } = session(id)
    const ndjson =
        requests
            .map((request) => JSON.stringify({ root: database.info.path, ...request }))
            .join('\n') + '\n'
    const started = performance.now()
    const answer = database.exec(ndjson).then(
        (text) => ({
            frames: text
                .split('\n')
                .filter(Boolean)
                .map((line) => JSON.parse(line)),
            ms: performance.now() - started
        }),
        (error) => failure(error)
    )
    // A wasm trap ends up as an error above; a hang is the one outcome that
    // would otherwise never come back.
    const timeout = new Promise((resolve) =>
        setTimeout(() => resolve({ timedOut: true, ms: timeoutMs }), timeoutMs)
    )
    return Promise.race([answer, timeout])
}

globalThis.fxdb = {
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

    exec(id, requests, timeoutMs = 300_000) {
        return run(id, requests, timeoutMs)
    },

    /** Start a batch without waiting for it; `wait` looks at it later. */
    start(id, requests, timeoutMs = 300_000) {
        const ticket = nextTicket++
        const entry = { done: false, value: undefined, promise: undefined }
        entry.promise = run(id, requests, timeoutMs).then((value) => {
            entry.done = true
            entry.value = value
            return value
        })
        tickets.set(ticket, entry)
        return ticket
    },

    async wait(ticket, ms = 60_000) {
        const entry = tickets.get(ticket)
        if (!entry) throw new Error(`no ticket ${ticket}`)
        const timeout = new Promise((resolve) => setTimeout(() => resolve({ timedOut: true }), ms))
        return Promise.race([entry.promise, timeout])
    },

    /** End a session the way a crash does: no close, no flush, the Worker gone. */
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
    async removeCopy(name) {
        for (let waited = 0; ; waited += 250) {
            try {
                await removeWorkingCopy(name)
                return
            } catch (error) {
                if (error?.name === 'NotFoundError') return
                if (error?.name !== 'NoModificationAllowedError' || waited >= 10_000) throw error
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
            await globalThis.fxdb.close(opened.id)
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
