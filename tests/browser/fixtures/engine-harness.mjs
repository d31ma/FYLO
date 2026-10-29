// A remote control for `fylo-browser.wasm` in a real browser.
//
// The other fixtures each run one scripted scenario. This page runs none: it
// exposes `globalThis.fylo`, and the specs drive it with `page.evaluate`, so a
// spec can open the same OPFS root in several Workers, kill one mid-batch,
// reload the page, or copy the root out to the native engine and back.
//
// A session is the pair the engine needs over OPFS: the bridge Worker that
// owns every handle, and the engine Worker that blocks on it. The engine
// Worker takes the root's Web Lock on its first batch, so a second session on
// the same root waits there until the first one goes away.

import { closeBridge, createBridgeBuffer } from '/src/browser/opfs-bridge.mjs'

const MODULE_URL = '/dist-web/fylo-browser.wasm'

/** @type {Map<number, Session>} */
const sessions = new Map()
/** @type {Map<number, { done: boolean, value?: unknown, promise: Promise<unknown> }>} */
const tickets = new Map()
let nextSession = 1
let nextTicket = 1
let nextRequest = 1

class Session {
    constructor(root, bridge, engine) {
        this.root = root
        this.bridge = bridge
        this.engine = engine
        this.trace = []
        this.bridgeError = ''
        /** @type {Map<number, (value: any) => void>} */
        this.waiting = new Map()
        this.killed = false
        engine.onmessage = (event) => {
            if (event.data?.trace) {
                this.trace.push(event.data.trace)
                return
            }
            const settle = this.waiting.get(event.data?.id)
            this.waiting.delete(event.data?.id)
            settle?.(event.data)
        }
        engine.onerror = (event) => {
            for (const settle of this.waiting.values()) {
                settle({ error: event.message ?? 'engine worker failed' })
            }
            this.waiting.clear()
        }
        bridge.addEventListener('message', (event) => {
            if (event.data?.bridgeError) this.bridgeError = event.data.bridgeError
        })
    }

    /** Post one batch; resolves with the parsed frames, or the Worker's error. */
    run(requests) {
        const id = nextRequest++
        const ndjson =
            requests.map((request) => JSON.stringify({ root: this.root, ...request })).join('\n') +
            '\n'
        return new Promise((resolve) => {
            if (this.killed) {
                resolve({ killed: true, frames: [] })
                return
            }
            this.waiting.set(id, (answer) => {
                if (answer.killed) {
                    resolve({ killed: true, frames: [] })
                    return
                }
                if (answer.error) {
                    resolve({ error: answer.error, frames: [], bridgeError: this.bridgeError })
                    return
                }
                resolve({
                    frames: (answer.ndjson ?? '')
                        .split('\n')
                        .filter(Boolean)
                        .map((line) => JSON.parse(line)),
                    hostError: answer.hostError,
                    bridgeError: this.bridgeError
                })
            })
            this.engine.postMessage({
                id,
                ndjson,
                buffer: this.buffer,
                moduleUrl: MODULE_URL,
                root: this.root
            })
        })
    }

    kill() {
        this.killed = true
        // Woken first, or WebKit keeps a Worker parked mid-call holding the root's lock.
        closeBridge(this.buffer)
        this.engine.terminate()
        this.bridge.terminate()
        for (const settle of this.waiting.values()) settle({ killed: true })
        this.waiting.clear()
    }
}

/** How long a bridge Worker may take to open its root before it is replaced. */
const BRIDGE_START_MS = 20_000
/** Bridge Workers that never became ready and were replaced, for the specs. */
let bridgeRestarts = 0

/**
 * A bridge Worker that has opened `root` over `buffer`.
 *
 * Firefox occasionally starts a Worker whose first OPFS call never settles,
 * moments after another Worker of the origin was terminated (about one start
 * in fifty under load). That is a browser start-up fault, not the engine's,
 * so a bridge that is not ready in time is terminated and started again.
 */
async function startBridge(root, buffer) {
    for (let attempt = 1; ; attempt++) {
        const bridge = new Worker('/src/browser/opfs-bridge-worker.mjs', { type: 'module' })
        const ready = await new Promise((settle, failed) => {
            const timer = setTimeout(() => settle(false), BRIDGE_START_MS)
            bridge.onmessage = (event) => {
                clearTimeout(timer)
                if (event.data?.bridgeError) failed(new Error(`bridge: ${event.data.bridgeError}`))
                else settle(true)
            }
            bridge.onerror = (event) => {
                clearTimeout(timer)
                failed(new Error(event.message ?? 'bridge worker failed'))
            }
            bridge.postMessage({ buffer, root })
        })
        if (ready) return bridge
        bridge.terminate()
        bridgeRestarts++
        if (attempt === 3) throw new Error(`the bridge Worker for ${root} never became ready`)
    }
}

async function open(root) {
    if (!root.startsWith('/')) throw new Error(`a root is an absolute OPFS path: ${root}`)
    const buffer = createBridgeBuffer()
    const bridge = await startBridge(root, buffer)
    const engine = new Worker('/src/browser/fylo-wasm-worker.mjs', { type: 'module' })
    const session = new Session(root, bridge, engine)
    session.buffer = buffer
    const id = nextSession++
    sessions.set(id, session)
    return id
}

function session(id) {
    const found = sessions.get(id)
    if (!found) throw new Error(`no session ${id}`)
    return found
}

/** The OPFS directory a root path names, created on the way down when asked. */
async function directoryFor(root, create) {
    let handle = await navigator.storage.getDirectory()
    for (const part of root.split('/').filter(Boolean)) {
        handle = await handle.getDirectoryHandle(part, { create })
    }
    return handle
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

// A reload or navigation ends every session; wake their engines first, as
// `kill` does, so WebKit releases their locks.
addEventListener('pagehide', () => {
    for (const session of sessions.values()) closeBridge(session.buffer)
})

globalThis.fylo = {
    surface() {
        return {
            crossOriginIsolated: globalThis.crossOriginIsolated === true,
            sharedArrayBuffer: typeof SharedArrayBuffer,
            opfs: typeof navigator.storage?.getDirectory,
            webLocks: typeof navigator.locks?.request,
            userAgent: navigator.userAgent
        }
    },

    /** Whether this browser can run the engine over OPFS at all. */
    async supported() {
        if (globalThis.crossOriginIsolated !== true) return 'page is not cross-origin isolated'
        if (typeof navigator.storage?.getDirectory !== 'function') return 'no OPFS'
        try {
            await navigator.storage.getDirectory()
        } catch (error) {
            return `OPFS refused: ${error?.name ?? error}`
        }
        // `createSyncAccessHandle` exists only inside a Worker, so it cannot be
        // probed here; a browser without it fails the first batch loudly.
        return ''
    },

    open,

    /** Bridge Workers replaced because they never became ready. */
    bridgeRestarts() {
        return bridgeRestarts
    },

    /** Run one batch and wait for every frame. */
    exec(id, requests) {
        return session(id).run(requests)
    },

    /** Start a batch without waiting; `settled` and `wait` look at it later. */
    start(id, requests) {
        const ticket = nextTicket++
        const entry = { done: false, value: undefined, promise: undefined }
        entry.promise = session(id)
            .run(requests)
            .then((value) => {
                entry.done = true
                entry.value = value
                return value
            })
        tickets.set(ticket, entry)
        return ticket
    },

    settled(ticket) {
        const entry = tickets.get(ticket)
        return entry ? { done: entry.done, value: entry.value } : { done: false, missing: true }
    },

    /** Wait up to `ms` for a started batch. */
    async wait(ticket, ms = 60_000) {
        const entry = tickets.get(ticket)
        if (!entry) throw new Error(`no ticket ${ticket}`)
        const timeout = new Promise((resolve) => setTimeout(() => resolve({ timedOut: true }), ms))
        return Promise.race([entry.promise, timeout])
    },

    trace(id) {
        return session(id).trace.slice()
    },

    /** End a session the way a crash does: no close, no flush. */
    kill(id) {
        session(id).kill()
        sessions.delete(id)
    },

    /** End a session cleanly; Worker termination is the only release there is. */
    close(id) {
        const found = sessions.get(id)
        if (!found) return
        found.kill()
        sessions.delete(id)
    },

    /** Every file under `root`, base64 encoded, keyed by its relative path. */
    async dump(root) {
        const files = {}
        const walk = async (handle, prefix) => {
            for await (const [name, child] of handle.entries()) {
                const path = prefix ? `${prefix}/${name}` : name
                if (child.kind === 'directory') {
                    files[`${path}/`] = ''
                    await walk(child, path)
                } else {
                    const file = await child.getFile()
                    files[path] = toBase64(new Uint8Array(await file.arrayBuffer()))
                }
            }
        }
        await walk(await directoryFor(root, false), '')
        return files
    },

    /** Write `files` (as `dump` returns them) under `root`, replacing it. */
    async seed(root, files) {
        await this.remove(root)
        const base = await directoryFor(root, true)
        for (const [path, content] of Object.entries(files)) {
            const parts = path.split('/').filter(Boolean)
            let directory = base
            const leaf = path.endsWith('/') ? undefined : parts.pop()
            for (const part of parts) {
                directory = await directory.getDirectoryHandle(part, { create: true })
            }
            if (leaf === undefined) continue
            const handle = await directory.getFileHandle(leaf, { create: true })
            const writable = await handle.createWritable()
            await writable.write(fromBase64(content))
            await writable.close()
        }
    },

    async remove(root) {
        const parts = root.split('/').filter(Boolean)
        const leaf = parts.pop()
        let parent = await navigator.storage.getDirectory()
        try {
            for (const part of parts) parent = await parent.getDirectoryHandle(part)
            await parent.removeEntry(leaf, { recursive: true })
        } catch (error) {
            if (error?.name !== 'NotFoundError') throw error
        }
    },

    /**
     * Write one file under `root`, creating its directories: `base64` bytes,
     * or `size` bytes of the pattern `(index * 31 + seed) & 255`, generated
     * here so a large input never crosses the automation channel.
     */
    async writeFile(root, path, { base64, size = 0, seed = 0 } = {}) {
        const parts = path.split('/').filter(Boolean)
        const leaf = parts.pop()
        let directory = await directoryFor(root, true)
        for (const part of parts) {
            directory = await directory.getDirectoryHandle(part, { create: true })
        }
        let bytes
        if (base64 !== undefined) {
            bytes = fromBase64(base64)
        } else {
            bytes = new Uint8Array(size)
            for (let index = 0; index < size; index++) bytes[index] = (index * 31 + seed) & 255
        }
        const writable = await (
            await directory.getFileHandle(leaf, { create: true })
        ).createWritable()
        await writable.write(bytes)
        await writable.close()
        return bytes.length
    },

    /** SHA-256 (hex) and size of one file under `root`, or `null` when it is missing. */
    async digest(root, path) {
        const parts = path.split('/').filter(Boolean)
        const leaf = parts.pop()
        try {
            let directory = await directoryFor(root, false)
            for (const part of parts) directory = await directory.getDirectoryHandle(part)
            const file = await (await directory.getFileHandle(leaf)).getFile()
            const hash = await crypto.subtle.digest('SHA-256', await file.arrayBuffer())
            return {
                size: file.size,
                sha256: [...new Uint8Array(hash)]
                    .map((byte) => byte.toString(16).padStart(2, '0'))
                    .join('')
            }
        } catch (error) {
            if (error?.name === 'NotFoundError') return null
            throw error
        }
    },

    /** The origin's Web Locks, held and pending, by name. */
    async locks() {
        const { held = [], pending = [] } = await navigator.locks.query()
        return {
            held: held.map((lock) => lock.name),
            pending: pending.map((lock) => lock.name)
        }
    },

    /** Bytes OPFS holds for this origin, as the browser estimates them. */
    async estimate() {
        const { usage, quota } = await navigator.storage.estimate()
        return { usage, quota }
    },

    /** What it costs to fetch and compile the engine module in this browser. */
    async moduleCost() {
        const fetchStarted = performance.now()
        const response = await fetch(MODULE_URL, { cache: 'no-store' })
        const bytes = new Uint8Array(await response.arrayBuffer())
        const fetchMs = performance.now() - fetchStarted
        const compileStarted = performance.now()
        const module = await WebAssembly.compile(bytes)
        const compileMs = performance.now() - compileStarted
        return {
            bytes: bytes.length,
            contentType: response.headers.get('content-type'),
            fetchMs,
            compileMs,
            imports: WebAssembly.Module.imports(module).map(
                (entry) => `${entry.module}.${entry.name}`
            ),
            exports: WebAssembly.Module.exports(module).map((entry) => entry.name)
        }
    }
}

const output = document.querySelector('output')
output.dataset.status = 'ready'
output.textContent = 'ready'
