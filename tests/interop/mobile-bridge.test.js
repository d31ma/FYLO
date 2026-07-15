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

describe('mobile WebView bridge', () => {
    test('getDoc resolves the browser collection get facade to document data', async () => {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-mobile-bridge-'))
        workspaces.push(workspace)
        await writeFile(
            path.join(workspace, 'fylo.mjs'),
            `export function createBrowserClient() {
    return {
        async ready() {},
        users: {
            get(id) {
                return {
                    async once() { return { [id]: { name: 'Ada', score: 51 } } },
                    async metadata() { return {} }
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
            result: { 'doc-1': { name: 'Ada', score: 51 } }
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

        expect(messages.find((message) => message.id === 2)?.error.message).toContain(
            'unknown method'
        )
        expect(messages.find((message) => message.id === 3)?.error.message).toContain(
            'unsupported characters'
        )
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
            error: { message: 'FYLO bridge request exceeds the 8 MiB limit' }
        })
        expect(messages.find((message) => message.id === 10)?.ok).toBe(false)
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
        users: {
            get(id) {
                return { async once() { return { [id]: { value: '\u00e9'.repeat(3 * 1024 * 1024 + 1) } } } }
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
            error: { message: 'FYLO bridge response exceeds the 6 MiB limit' }
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
})
