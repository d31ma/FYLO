import { afterEach, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'

const workspaces = []

afterEach(async () => {
    delete globalThis.__fyloDispatch
    delete globalThis.__fyloNative
    await Promise.all(
        workspaces.splice(0).map((workspace) => rm(workspace, { recursive: true, force: true }))
    )
})

async function loadRealBridge() {
    const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-engine-'))
    workspaces.push(workspace)
    const clientUrl = pathToFileURL(path.resolve('src/browser/client.js')).href
    await writeFile(
        path.join(workspace, 'fylo.mjs'),
        `import { createBrowserClient as createEngineClient } from ${JSON.stringify(clientUrl)}
export const requestedOptions = []
export const clients = []
export function createBrowserClient(options) {
    requestedOptions.push(options)
    const client = createEngineClient({ ...options, storage: 'memory' })
    clients.push(client)
    return client
}
`
    )
    await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))
    const replies = new Map()
    globalThis.__fyloNative = {
        onMessage(text) {
            const reply = JSON.parse(text)
            replies.set(reply.id, reply)
        }
    }
    const engine = await import(pathToFileURL(path.join(workspace, 'fylo.mjs')).href)
    await import(pathToFileURL(path.join(workspace, 'bridge.js')).href)
    let nextId = 1
    const call = async (method, args = {}) => {
        const id = nextId++
        const encoded = Buffer.from(JSON.stringify({ id, method, args })).toString('base64')
        await globalThis.__fyloDispatch(encoded)
        return replies.get(id)
    }
    return { call, engine, replies }
}

async function openWithUsers() {
    const bridge = await loadRealBridge()
    await bridge.call('open', { config: {} })
    await bridge.call('createCollection', { collection: 'users' })
    const ada = await bridge.call('putData', {
        collection: 'users',
        data: { name: 'Ada', role: 'admin', age: 36, tags: ['engineering', 'math'] }
    })
    const bob = await bridge.call('putData', {
        collection: 'users',
        data: { name: 'Bob', role: 'user', age: 17, tags: ['sales'] }
    })
    return { ...bridge, ada: ada.result, bob: bob.result }
}

async function loadChangeFeedBridge() {
    const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-changes-'))
    workspaces.push(workspace)
    await writeFile(
        path.join(workspace, 'fylo.mjs'),
        `export const calls = []
export function createBrowserClient() {
    return {
        async ready() {},
        collection(name) {
            return {
                async changes(after, limit) {
                    calls.push([name, after, limit])
                    return { changes: [{ id: 'doc-1', kind: 'put' }], cursor: 'cursor-2', more: false }
                }
            }
        }
    }
}
`
    )
    await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))
    const messages = []
    globalThis.__fyloNative = { onMessage: (text) => messages.push(JSON.parse(text)) }
    const engine = await import(pathToFileURL(path.join(workspace, 'fylo.mjs')).href)
    await import(`${pathToFileURL(path.join(workspace, 'bridge.js')).href}?test=${Date.now()}`)
    const send = async (id, args) => {
        const encoded = Buffer.from(JSON.stringify({ id, method: 'changes', args })).toString(
            'base64'
        )
        await globalThis.__fyloDispatch(encoded)
        return messages.find((message) => message.id === id)
    }
    return { calls: engine.calls, send }
}

describe('mobile WebView bridge', () => {
    test('getDoc resolves the browser collection get facade to document data', async () => {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-'))
        workspaces.push(workspace)
        await writeFile(
            path.join(workspace, 'fylo.mjs'),
            `export function createBrowserClient() {
    return {
        async ready() {},
        collection(name) {
            return {
                get(id) {
                    return {
                        async once() { return { [id]: { name: 'Ada', score: 51, collection: name } } },
                        async metadata() { return {} }
                    }
                }
            }
        }
    }
}
`
        )
        await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))

        const messages = []
        globalThis.__fyloNative = {
            onMessage(text) {
                messages.push(JSON.parse(text))
            }
        }
        await import(`${pathToFileURL(path.join(workspace, 'bridge.js')).href}?test=${Date.now()}`)

        const request = Buffer.from(
            JSON.stringify({
                id: 1,
                method: 'getDoc',
                args: { collection: 'users', id: 'doc-1' }
            })
        ).toString('base64')
        await globalThis.__fyloDispatch(request)

        expect(messages.find((message) => message.id === 1)).toEqual({
            id: 1,
            ok: true,
            result: { 'doc-1': { name: 'Ada', score: 51, collection: 'users' } }
        })
    })

    test('rejects unknown operations and invalid collection input', async () => {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-'))
        workspaces.push(workspace)
        await writeFile(
            path.join(workspace, 'fylo.mjs'),
            `export function createBrowserClient() { return { async ready() {} } }`
        )
        await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))
        const messages = []
        globalThis.__fyloNative = { onMessage: (text) => messages.push(JSON.parse(text)) }
        await import(`${pathToFileURL(path.join(workspace, 'bridge.js')).href}?test=${Date.now()}`)

        for (const payload of [
            { id: 2, method: 'fetch', args: {} },
            { id: 3, method: 'getDoc', args: { collection: '../users', id: 'doc-1' } }
        ]) {
            await globalThis.__fyloDispatch(Buffer.from(JSON.stringify(payload)).toString('base64'))
        }

        expect(messages.find((message) => message.id === 2)?.error).toEqual({
            message: 'unknown method: fetch',
            code: 'EUNSUPPORTEDOP'
        })
        expect(messages.find((message) => message.id === 3)?.error).toEqual({
            message: 'collection contains unsupported characters',
            code: 'EBADREQUEST'
        })
    })

    test('changes routes one change feed page to the browser collection', async () => {
        const { calls, send } = await loadChangeFeedBridge()

        const first = await send(1, { collection: 'users' })
        const next = await send(2, { collection: 'users', after: 'cursor-1', limit: 5 })

        expect(first).toEqual({
            id: 1,
            ok: true,
            result: { changes: [{ id: 'doc-1', kind: 'put' }], cursor: 'cursor-2', more: false }
        })
        expect(next.ok).toBe(true)
        expect(calls).toEqual([
            ['users', null, undefined],
            ['users', 'cursor-1', 5]
        ])
    })

    test('changes accepts null after and null limit from native clients', async () => {
        const { calls, send } = await loadChangeFeedBridge()

        const reply = await send(1, { collection: 'users', after: null, limit: null })

        expect(reply.ok).toBe(true)
        expect(calls).toEqual([['users', null, undefined]])
    })

    test('changes rejects a bad after type and an out-of-range limit', async () => {
        const { calls, send } = await loadChangeFeedBridge()

        const badAfter = await send(1, { collection: 'users', after: 42 })
        const zeroLimit = await send(2, { collection: 'users', limit: 0 })
        const largeLimit = await send(3, { collection: 'users', limit: 1001 })
        const fractionLimit = await send(4, { collection: 'users', limit: 2.5 })

        expect(badAfter.error).toEqual({
            message: 'after must be a change cursor string or null',
            code: 'EBADREQUEST'
        })
        const limitError = {
            message: 'limit must be an integer from 1 to 1000',
            code: 'EBADREQUEST'
        }
        expect(zeroLimit.error).toEqual(limitError)
        expect(largeLimit.error).toEqual(limitError)
        expect(fractionLimit.error).toEqual(limitError)
        expect(calls).toEqual([])
    })

    test('returns correlated errors for oversized and malformed requests', async () => {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-'))
        workspaces.push(workspace)
        await writeFile(
            path.join(workspace, 'fylo.mjs'),
            `export function createBrowserClient() { return { async ready() {} } }`
        )
        await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))
        const messages = []
        globalThis.__fyloNative = { onMessage: (text) => messages.push(JSON.parse(text)) }
        await import(`${pathToFileURL(path.join(workspace, 'bridge.js')).href}?test=${Date.now()}`)

        const oversized = `{"id":9,"method":"open","args":{"padding":"${'x'.repeat(6 * 1024 * 1024)}"}}`
        await globalThis.__fyloDispatch(Buffer.from(oversized).toString('base64'))
        await globalThis.__fyloDispatch(
            Buffer.from('{"id":10,"method":"open","args":').toString('base64')
        )

        expect(messages.find((message) => message.id === 9)).toEqual({
            id: 9,
            ok: false,
            error: {
                message: 'FYLO bridge request exceeds the 8 MiB limit',
                code: 'EFRAME_REQUEST_TOO_LARGE'
            }
        })
        expect(messages.find((message) => message.id === 10)?.ok).toBe(false)
        expect(messages.find((message) => message.id === 10)?.error.code).toBe('EBADREQUEST')
        expect(messages.find((message) => message.id === 10)?.error.message.length).toBeGreaterThan(
            0
        )
    })

    test('replaces oversized UTF-8 responses with a small correlated error', async () => {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-'))
        workspaces.push(workspace)
        await writeFile(
            path.join(workspace, 'fylo.mjs'),
            `export function createBrowserClient() {
    return {
        async ready() {},
        collection() {
            return {
                get(id) {
                    return { async once() { return { [id]: { value: '\u00e9'.repeat(3 * 1024 * 1024 + 1) } } } }
                }
            }
        }
    }
}`
        )
        await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))
        const rawMessages = []
        globalThis.__fyloNative = { onMessage: (text) => rawMessages.push(text) }
        await import(`${pathToFileURL(path.join(workspace, 'bridge.js')).href}?test=${Date.now()}`)

        const request = Buffer.from(
            JSON.stringify({ id: 11, method: 'getDoc', args: { collection: 'users', id: 'doc-1' } })
        ).toString('base64')
        await globalThis.__fyloDispatch(request)

        const rawReply = rawMessages.find((text) => JSON.parse(text).id === 11)
        expect(Buffer.byteLength(rawReply, 'utf8')).toBeLessThan(1024)
        expect(JSON.parse(rawReply)).toEqual({
            id: 11,
            ok: false,
            error: {
                message: 'FYLO bridge response exceeds the 6 MiB limit',
                code: 'EFRAME_RESPONSE_TOO_LARGE'
            }
        })
    })

    test('native mobile clients cap requests and pending RPCs before dispatch', async () => {
        const [kotlin, flutter, swift] = await Promise.all([
            readFile('clients/kotlin/Fylo.kt', 'utf8'),
            readFile('clients/flutter/fylo.dart', 'utf8'),
            readFile('clients/swift/Fylo.swift', 'utf8')
        ])

        expect(kotlin).toContain('MAX_BRIDGE_REQUEST_BYTES = 6 * 1024 * 1024')
        expect(kotlin).toContain('MAX_PENDING_REQUESTS = 256')
        expect(kotlin).toContain(
            'ALLOWED_ASSET_PATHS = setOf("/host.html", "/bridge.js", "/fylo.mjs")'
        )
        expect(kotlin).toContain('val allowed = isAllowedUrl(req.url, documentOnly = true)')
        expect(kotlin).toContain('return deniedResponse()')
        expect(kotlin).toContain('recoverReplyId(text)')
        expect(kotlin.indexOf('text.toByteArray(Charsets.UTF_8).size')).toBeLessThan(
            kotlin.indexOf('JSONObject(text)')
        )
        expect(kotlin.indexOf('payloadBytes.size > MAX_BRIDGE_REQUEST_BYTES')).toBeLessThan(
            kotlin.indexOf('pending[id] = deferred')
        )
        expect(kotlin.indexOf('pending.size >= MAX_PENDING_REQUESTS')).toBeLessThan(
            kotlin.indexOf('webView.evaluateJavascript')
        )
        expect(kotlin).toContain('requests.forEach { it.completeExceptionally(error) }')

        expect(flutter).toContain('_maxBridgeRequestBytes = 6 * 1024 * 1024')
        expect(flutter).toContain('_maxPendingRequests = 256')
        expect(flutter).toContain('await controller.getUrl()')
        expect(flutter).toContain('_isAllowedUrl(current, port, documentOnly: true)')
        expect(flutter).toContain('_isAllowedUrl(uri, port, documentOnly: true)')
        expect(flutter).toContain('shouldInterceptRequest:')
        expect(flutter).toContain('_recoverReplyId(text)')
        expect(flutter.indexOf('utf8.encode(text).length')).toBeLessThan(
            flutter.indexOf('jsonDecode(text)')
        )
        expect(flutter.indexOf('payloadBytes.length > _maxBridgeRequestBytes')).toBeLessThan(
            flutter.indexOf('_pending[id] = request')
        )
        expect(flutter.indexOf('_pending.length >= _maxPendingRequests')).toBeLessThan(
            flutter.indexOf('controller.evaluateJavascript')
        )
        expect(flutter).toContain('_pending.clear();')

        expect(swift).toContain('private static let maxBridgeBytes = 6 * 1024 * 1024')
        expect(swift).toContain('func close()')
        expect(swift).toContain('pending.removeAll()')
        expect(swift).toContain('failCorrelated(id: id, message:')
        expect(swift.indexOf('text.utf8.count <= Self.maxBridgeBytes')).toBeLessThan(
            swift.indexOf('JSONSerialization.jsonObject(with: data)')
        )
    })

    test('native mobile clients expire missing replies and drain renderer failures exactly once', async () => {
        const [kotlin, flutter, swift] = await Promise.all([
            readFile('clients/kotlin/Fylo.kt', 'utf8'),
            readFile('clients/flutter/fylo.dart', 'utf8'),
            readFile('clients/swift/Fylo.swift', 'utf8')
        ])

        expect(swift).toContain('rpcTimeout: TimeInterval = 30')
        expect(swift).toContain('rpcTimeout <= Self.maxRPCTimeout')
        expect(swift).toContain('withTaskCancellationHandler')
        expect(swift.match(/withTaskCancellationHandler/g)?.length).toBe(2)
        expect(swift).toContain('self?.failBridge(CancellationError())')
        expect(swift).toContain('var initializationSucceeded = false')
        expect(swift).toContain('if !initializationSucceeded { failBridge(initializationFailure) }')
        expect(swift).toContain('request.timeout.cancel()')
        expect(swift).toContain('func webViewWebContentProcessDidTerminate')
        expect(swift).toContain('didFailProvisionalNavigation')
        expect(swift).toContain('guard !isClosed else { return }')

        expect(kotlin).toContain('rpcTimeoutMillis: Long = DEFAULT_RPC_TIMEOUT_MILLIS')
        expect(kotlin).toContain('val completed = withTimeoutOrNull(rpcTimeoutMillis)')
        expect(kotlin).toContain('catch (error: CancellationException)')
        expect(kotlin).toContain('candidate?.failBridge(')
        expect(kotlin).toContain('candidate?.disposeWebView()')
        expect(kotlin).toContain('var initializationSucceeded = false')
        expect(kotlin).toContain('if (!initializationSucceeded)')
        expect(kotlin).toContain('throw error // Preserve structured coroutine cancellation.')
        expect(kotlin.indexOf('candidate = created')).toBeLessThan(
            kotlin.indexOf('created.webView.settings.javaScriptEnabled = true')
        )
        expect(kotlin.indexOf('candidate = created')).toBeLessThan(
            kotlin.indexOf('created.webView.settings.domStorageEnabled = true')
        )
        expect(kotlin).toContain('removePending(id, deferred)')
        expect(kotlin).toContain('override fun onRenderProcessGone(')
        expect(kotlin).toContain('override fun onReceivedError(')
        expect(kotlin).toContain('if (!closed.compareAndSet(false, true)) return')

        expect(flutter).toContain('Duration rpcTimeout = _defaultRpcTimeout')
        expect(flutter).toContain('final timeout = Timer(_rpcTimeout, () {')
        expect(flutter).toContain('timeout.cancel();')
        expect(flutter).toContain('androidOnRenderProcessGone:')
        expect(flutter).toContain('iosOnWebContentProcessDidTerminate:')
        expect(flutter).toContain('onLoadError:')
        expect(flutter).toContain('if (_closed) return;')

        class PendingLifecycle {
            pending = new Map()
            closed = false
            add(id, timeoutMs) {
                let resolve
                let reject
                const promise = new Promise((ok, fail) => {
                    resolve = ok
                    reject = fail
                })
                const timer = setTimeout(() => {
                    const entry = this.pending.get(id)
                    if (!entry) return
                    this.pending.delete(id)
                    entry.reject(new Error('timed out; verify the WebView is responsive'))
                }, timeoutMs)
                this.pending.set(id, { resolve, reject, timer })
                return promise
            }
            reply(id, value) {
                const entry = this.pending.get(id)
                if (!entry) return
                this.pending.delete(id)
                clearTimeout(entry.timer)
                entry.resolve(value)
            }
            terminate(message) {
                if (this.closed) return
                this.closed = true
                const entries = [...this.pending.values()]
                this.pending.clear()
                for (const entry of entries) {
                    clearTimeout(entry.timer)
                    entry.reject(new Error(message))
                }
            }
        }

        const lifecycle = new PendingLifecycle()
        const missingReply = lifecycle.add(1, 5)
        await expect(missingReply).rejects.toThrow('timed out')
        expect(lifecycle.pending.size).toBe(0)
        lifecycle.reply(1, 'late')

        const terminated = lifecycle.add(2, 1_000)
        lifecycle.terminate('render process terminated')
        lifecycle.terminate('second close must be ignored')
        await expect(terminated).rejects.toThrow('render process terminated')
        expect(lifecycle.pending.size).toBe(0)
    })

    test('Swift initialization closes on cancellation, readiness timeout, and failed open', async () => {
        if (!Bun.which('swiftc')) return
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-swift-cancel-'))
        workspaces.push(workspace)
        const bundle = path.join(workspace, 'FyloTest.bundle')
        const failingBundle = path.join(workspace, 'FyloFailingTest.bundle')
        await mkdir(bundle)
        await mkdir(failingBundle)
        await Promise.all([
            writeFile(
                path.join(bundle, 'Info.plist'),
                '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.fylo</string></dict></plist>'
            ),
            writeFile(
                path.join(bundle, 'host.html'),
                '<!doctype html><script type="module" src="bridge.js"></script>'
            ),
            writeFile(path.join(bundle, 'bridge.js'), "import './fylo.mjs'\n"),
            writeFile(path.join(bundle, 'fylo.mjs'), 'export const pending = true\n'),
            writeFile(
                path.join(failingBundle, 'Info.plist'),
                '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.fylo.failure</string></dict></plist>'
            ),
            writeFile(
                path.join(failingBundle, 'host.html'),
                '<!doctype html><script type="module" src="bridge.js"></script>'
            ),
            writeFile(
                path.join(failingBundle, 'bridge.js'),
                `window.__fyloDispatch = () => window.webkit.messageHandlers.fylo.postMessage(JSON.stringify({ id: 1, ok: false, error: { message: 'initial open failed' } }));
window.webkit.messageHandlers.fylo.postMessage(JSON.stringify({ id: 0, ok: true }));
`
            ),
            writeFile(path.join(failingBundle, 'fylo.mjs'), 'export const unused = true\n')
        ])
        const source = path.join(workspace, 'Runtime.swift')
        const executable = path.join(workspace, 'runtime')
        await writeFile(
            source,
            `import Foundation

@main
struct Runtime {
    @MainActor
    static func main() async throws {
        guard let bundle = Bundle(path: ${JSON.stringify(bundle)}) else { fatalError("bundle") }
        let opening = Task { @MainActor in try await Fylo(bundle: bundle, rpcTimeout: 300) }
        try await Task.sleep(nanoseconds: 20_000_000)
        let started = Date()
        opening.cancel()
        do {
            _ = try await opening.value
            fatalError("cancelled initialization unexpectedly succeeded")
        } catch is CancellationError {
            precondition(Date().timeIntervalSince(started) < 1)
        }
        do {
            _ = try await Fylo(bundle: bundle, rpcTimeout: 0.05)
            fatalError("readiness timeout unexpectedly succeeded")
        } catch let error as FyloError {
            precondition(error.description.contains("did not become ready"))
        }
        guard let failingBundle = Bundle(path: ${JSON.stringify(failingBundle)}) else { fatalError("failing bundle") }
        do {
            _ = try await Fylo(bundle: failingBundle, rpcTimeout: 5)
            fatalError("failed initial open unexpectedly succeeded")
        } catch let error as FyloError {
            precondition(error.description.contains("initial open failed"))
        }
        print("swift initialization cleanup ok")
    }
}
`
        )
        const compile = Bun.spawn(
            [
                'swiftc',
                '-swift-version',
                '5',
                '-parse-as-library',
                path.resolve('clients/swift/Fylo.swift'),
                source,
                '-o',
                executable
            ],
            { stdout: 'pipe', stderr: 'pipe' }
        )
        const [compileExit, compileError] = await Promise.all([
            compile.exited,
            new Response(compile.stderr).text()
        ])
        if (compileExit !== 0) throw new Error(compileError)

        const runtime = Bun.spawn([executable], { stdout: 'pipe', stderr: 'pipe' })
        const [runtimeExit, output, runtimeError] = await Promise.all([
            runtime.exited,
            new Response(runtime.stdout).text(),
            new Response(runtime.stderr).text()
        ])
        if (runtimeExit !== 0) throw new Error(runtimeError)
        expect(output).toContain('swift initialization cleanup ok')
    })

    test('native mobile clients dispatch without awaiting, expose executeSQL and error codes, and send kind', async () => {
        const [kotlin, flutter, swift] = await Promise.all([
            readFile('clients/kotlin/Fylo.kt', 'utf8'),
            readFile('clients/flutter/fylo.dart', 'utf8'),
            readFile('clients/swift/Fylo.swift', 'utf8')
        ])

        expect(swift).toContain('evaluateJavaScript("void window.__fyloDispatch(')
        expect(kotlin).toContain('evaluateJavascript("void window.__fyloDispatch(')
        expect(flutter).toContain("source: 'void window.__fyloDispatch(")
        expect(swift).toContain('func executeSQL(_ statement: String)')
        expect(kotlin).toContain('suspend fun executeSQL(sql: String)')
        expect(flutter).toContain('Future<dynamic> executeSQL(String sql)')
        expect(swift).toContain('var code: String?')
        expect(swift).toContain('code: error?["code"] as? String')
        expect(kotlin).toContain('class FyloException(message: String, val code: String? = null)')
        expect(flutter).toContain('final String? code;')
        expect(swift).toContain('["collection": collection, "kind": kind]')
        expect(kotlin).toContain('mapOf("collection" to collection, "kind" to kind)')
        expect(flutter).toContain("{'collection': collection, 'kind': kind}")
    })
})

describe('mobile WebView bridge against the browser engine', () => {
    test('signals ready and opens the engine on OPFS without a worker by default', async () => {
        const { call, engine, replies } = await loadRealBridge()

        const opened = await call('open', { config: {} })

        expect(replies.get(0)).toEqual({ id: 0, ok: true, result: { bridge: 'ready' } })
        expect(opened).toEqual({ id: 1, ok: true, result: { ready: true } })
        expect(engine.requestedOptions).toEqual([{ worker: false, storage: 'opfs' }])
    })

    test('passes an explicit worker setting from the native config', async () => {
        const { call, engine } = await loadRealBridge()

        await call('open', { config: { worker: true, namespace: 'app' } })

        expect(engine.requestedOptions).toEqual([
            { namespace: 'app', worker: true, storage: 'opfs' }
        ])
    })

    test('opens with defaults when the native side sends no config', async () => {
        const { call, engine } = await loadRealBridge()

        await call('open')

        expect(engine.requestedOptions).toEqual([{ worker: false, storage: 'opfs' }])
    })

    test('creates, inspects, rebuilds, and drops a collection', async () => {
        const { call } = await openWithUsers()

        const inspected = await call('inspectCollection', { collection: 'users' })
        const rebuilt = await call('rebuildCollection', { collection: 'users' })
        const dropped = await call('dropCollection', { collection: 'users' })
        const afterDrop = await call('inspectCollection', { collection: 'users' })

        expect(inspected.result).toMatchObject({ collection: 'users', exists: true, docsStored: 2 })
        expect(rebuilt.result).toMatchObject({
            collection: 'users',
            docsScanned: 2,
            indexedDocs: 2
        })
        expect(dropped).toMatchObject({ ok: true, result: null })
        expect(afterDrop.result).toMatchObject({ collection: 'users', exists: false })
    })

    test('accepts the document kind that the Flutter client sends', async () => {
        const { call } = await loadRealBridge()

        const created = await call('createCollection', { collection: 'notes', kind: 'document' })
        const inspected = await call('inspectCollection', { collection: 'notes' })

        expect(created).toMatchObject({ ok: true, result: null })
        expect(inspected.result.exists).toBe(true)
    })

    test('reads, patches, deletes, and restores a document', async () => {
        const { call, ada } = await openWithUsers()

        const read = await call('getDoc', { collection: 'users', id: ada })
        const patched = await call('patchDoc', {
            collection: 'users',
            id: ada,
            newDoc: { role: 'owner' }
        })
        const latest = await call('getLatest', { collection: 'users', id: ada })
        const deleted = await call('delDoc', { collection: 'users', id: ada })
        const afterDelete = await call('getDoc', { collection: 'users', id: ada })
        const restored = await call('restoreDoc', { collection: 'users', id: ada })
        const afterRestore = await call('getLatest', { collection: 'users', id: ada })

        expect(read.result[ada]).toMatchObject({ name: 'Ada', role: 'admin' })
        expect(patched.result).toBe(ada)
        expect(latest.result[ada]).toMatchObject({ name: 'Ada', role: 'owner' })
        expect(deleted).toMatchObject({ ok: true, result: null })
        expect(afterDelete.result ?? {}).toEqual({})
        expect(restored.result).toBe(ada)
        expect(afterRestore.result[ada]).toMatchObject({ name: 'Ada', role: 'owner' })
    })

    test('rejects a direct write onto a deleted id', async () => {
        const { call, bob } = await openWithUsers()
        await call('delDoc', { collection: 'users', id: bob })

        const written = await call('putData', {
            collection: 'users',
            data: { [bob]: { name: 'x' } }
        })

        expect(written.ok).toBe(false)
        expect(written.error.code).toBeString()
    })

    test('sets metadata and removes a key set to null', async () => {
        const { call, ada } = await openWithUsers()

        const fresh = await call('getMeta', { collection: 'users', id: ada })
        const set = await call('setMeta', {
            collection: 'users',
            id: ada,
            meta: { source: 'import', reviewed: false }
        })
        const removed = await call('setMeta', {
            collection: 'users',
            id: ada,
            meta: { source: null }
        })
        const after = await call('getMeta', { collection: 'users', id: ada })

        expect(fresh.result).toMatchObject({ id: ada })
        expect(fresh.result.createdAt).toBeNumber()
        expect(set.result).toMatchObject({ source: 'import', reviewed: false })
        expect(Object.hasOwn(removed.result, 'source')).toBe(false)
        expect(Object.hasOwn(after.result, 'source')).toBe(false)
        expect(after.result.reviewed).toBe(false)
    })

    test('findDocs applies $eq, $gte, $contains, and OR operators', async () => {
        const { call, ada, bob } = await openWithUsers()

        const equal = await call('findDocs', {
            collection: 'users',
            query: { $ops: [{ role: { $eq: 'admin' } }] }
        })
        const adults = await call('findDocs', {
            collection: 'users',
            query: { $ops: [{ age: { $gte: 18 } }] }
        })
        const tagged = await call('findDocs', {
            collection: 'users',
            query: { $ops: [{ tags: { $contains: 'sales' } }] }
        })
        const either = await call('findDocs', {
            collection: 'users',
            query: { $ops: [{ role: { $eq: 'admin' } }, { role: { $eq: 'user' } }] }
        })

        expect(Object.keys(equal.result)).toEqual([ada])
        expect(Object.keys(adults.result)).toEqual([ada])
        expect(Object.keys(tagged.result)).toEqual([bob])
        expect(Object.keys(either.result).sort()).toEqual([ada, bob].sort())
    })

    test('findDocs with $onlyIds returns the same id list as the browser engine', async () => {
        const { call, engine, ada } = await openWithUsers()
        const query = { $ops: [{ role: { $eq: 'admin' } }], $onlyIds: true }

        const reply = await call('findDocs', { collection: 'users', query })
        const direct = await engine.clients[0].request({
            op: 'findDocs',
            collection: 'users',
            query
        })

        expect(reply.result).toEqual([ada])
        expect(reply.result).toEqual(direct.result)
    })

    test('executeSQL creates, writes, reads, updates, and deletes rows', async () => {
        const { call } = await loadRealBridge()

        const created = await call('executeSQL', { sql: 'CREATE TABLE posts' })
        const inserted = await call('executeSQL', {
            sql: "INSERT INTO posts (title, published) VALUES ('Hello', true)"
        })
        const selected = await call('executeSQL', {
            sql: 'SELECT title FROM posts WHERE published = true'
        })
        const updated = await call('executeSQL', {
            sql: "UPDATE posts SET title = 'Bye' WHERE title = 'Hello'"
        })
        const deleted = await call('executeSQL', { sql: "DELETE FROM posts WHERE title = 'Bye'" })
        const empty = await call('executeSQL', { sql: 'SELECT * FROM posts' })

        expect(created.ok).toBe(true)
        expect(inserted.ok).toBe(true)
        expect(Object.values(selected.result)).toEqual([{ title: 'Hello' }])
        expect(updated.result).toBe(1)
        expect(deleted.result).toBe(1)
        expect(Object.keys(empty.result)).toEqual([])
    })

    test('engine errors keep their stable code', async () => {
        const { call } = await loadRealBridge()

        const missing = await call('getDoc', { collection: 'ghosts', id: '4WNMX8R4TRG' })

        expect(missing).toEqual({
            id: 1,
            ok: false,
            error: { message: 'Collection not found: ghosts', code: 'FYLO_COLLECTION_NOT_FOUND' }
        })
    })

    test.each(['options', 'collections', 'collection', 'browser', 'request', 'create', 'inspect'])(
        'a collection named %s is a normal collection',
        async (name) => {
            const { call } = await loadRealBridge()

            const created = await call('createCollection', { collection: name })
            const id = await call('putData', { collection: name, data: { value: name } })
            const read = await call('getDoc', { collection: name, id: id.result })

            expect(created.ok).toBe(true)
            expect(read.result[id.result]).toEqual({ value: name })
        }
    )

    test.each(['then', 'close', 'ready', 'sql'])(
        'a collection named %s gets the engine reserved-name error',
        async (name) => {
            const { call } = await loadRealBridge()

            const created = await call('createCollection', { collection: name })

            expect(created.ok).toBe(false)
            expect(created.error.message).toBe(
                `'${name}' is a reserved name and cannot be used as a collection name. Choose a different name.`
            )
            expect(created.error.code).toBe('EUNKNOWN')
        }
    )

    test.each([
        ['a file bucket', { kind: 'file' }],
        ['a schema', { options: { schema: { name: '^.+$' } } }],
        ['$encrypted fields', { options: { $encrypted: ['ssn'] } }],
        ['an encryption key', { options: { encryption: { key: 'secret' } } }]
    ])('createCollection with %s fails closed', async (_label, args) => {
        const { call } = await loadRealBridge()

        const created = await call('createCollection', { collection: 'vault', ...args })
        const inspected = await call('inspectCollection', { collection: 'vault' })

        expect(created.ok).toBe(false)
        expect(created.error.code).toBe('EBROWSER_UNSUPPORTED')
        expect(inspected.result.exists).toBe(false)
    })

    test('a failed open can be retried', async () => {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-'))
        workspaces.push(workspace)
        await writeFile(
            path.join(workspace, 'fylo.mjs'),
            `let attempts = 0
export function createBrowserClient() {
    attempts += 1
    const attempt = attempts
    return {
        async ready() {
            if (attempt === 1) throw Object.assign(new Error('storage busy'), { code: 'EBUSY' })
        }
    }
}
`
        )
        await Bun.write(path.join(workspace, 'bridge.js'), Bun.file('clients/mobile/bridge.js'))
        const messages = []
        globalThis.__fyloNative = { onMessage: (text) => messages.push(JSON.parse(text)) }
        await import(`${pathToFileURL(path.join(workspace, 'bridge.js')).href}?test=${Date.now()}`)
        const open = (id) =>
            globalThis.__fyloDispatch(
                Buffer.from(JSON.stringify({ id, method: 'open', args: {} })).toString('base64')
            )

        await open(1)
        await open(2)

        expect(messages.find((message) => message.id === 1)).toEqual({
            id: 1,
            ok: false,
            error: { message: 'storage busy', code: 'EBUSY' }
        })
        expect(messages.find((message) => message.id === 2)).toEqual({
            id: 2,
            ok: true,
            result: { ready: true }
        })
    })
})
