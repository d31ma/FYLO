// Drive `fylo-wasm` over real OPFS inside a dedicated Worker and publish the
// result for the Playwright spec to assert.

const status = document.querySelector('output')

const evidence = {
    supported: true,
    reason: '',
    status: 'running',
    frames: [],
    trace: [],
    bridgeError: '',
    surface: {},
    step: 'open-opfs-root'
}

try {
    // Report the storage surface before anything else: whether the sync access
    // handle and the async directory API disagree is the whole question.
    const directory = await navigator.storage.getDirectory()
    evidence.step = 'start-workers'
    evidence.surface = {
        syncAccessHandle: typeof FileSystemFileHandle?.prototype?.createSyncAccessHandle,
        getDirectoryHandleSync: typeof directory.getDirectoryHandleSync,
        getFileHandleSync: typeof directory.getFileHandleSync,
        removeEntrySync: typeof directory.removeEntrySync,
        keysSync: typeof directory.keysSync,
        move: typeof FileSystemFileHandle?.prototype?.move,
        webLocks: typeof navigator.locks?.request,
        crossOriginIsolated: globalThis.crossOriginIsolated === true
    }

    // Two Workers share one buffer: the bridge owns OPFS and runs promises,
    // the engine blocks on it. Neither can do both.
    const { createBridgeBuffer } = await import('/src/browser/opfs-bridge.mjs')
    const buffer = createBridgeBuffer()
    const root = `/fylo-${Date.now()}`
    const bridge = new Worker('/src/browser/opfs-bridge-worker.mjs', { type: 'module' })
    await new Promise((ready, failed) => {
        bridge.onmessage = (event) => {
            if (event.data?.bridgeError) {
                evidence.bridgeError = event.data.bridgeError
                failed(new Error(`bridge: ${event.data.bridgeError}`))
                return
            }
            ready(undefined)
        }
        bridge.onerror = (event) => failed(new Error(event.message ?? 'bridge worker failed'))
        bridge.postMessage({ buffer, root })
    })
    // Stays attached: a bridge that dies after readiness is why the engine hangs.
    bridge.addEventListener('message', (event) => {
        if (event.data?.bridgeError) evidence.bridgeError = event.data.bridgeError
    })

    const worker = new Worker('/src/browser/fylo-wasm-worker.mjs', { type: 'module' })
    const answer = await new Promise((resolve) => {
        const timer = setTimeout(() => resolve({ error: 'worker timed out after 30s' }), 30_000)
        worker.onmessage = (event) => {
            if (event.data?.trace) {
                evidence.trace.push(event.data.trace)
                return
            }
            clearTimeout(timer)
            resolve(event.data)
        }
        worker.onerror = (event) => {
            clearTimeout(timer)
            resolve({ error: event.message ?? 'worker failed to start' })
        }
        worker.postMessage({
            id: 1,
            buffer,
            moduleUrl: '/dist-web/fylo-browser.wasm',
            root,
            ndjson:
                [
                    { op: 'handshake' },
                    { op: 'createCollection', collection: 'notes', root },
                    { op: 'putData', collection: 'notes', data: { name: 'Ada' }, root },
                    { op: 'putData', collection: 'notes', data: { name: 'Grace' }, root },
                    { op: 'findDocs', collection: 'notes', query: {}, root },
                    { op: 'inspectCollection', collection: 'notes', root },
                    { op: 'createCollection', collection: 'equality', root },
                    ...[
                        { name: 'number', score: 1, profile: { score: 1 } },
                        { name: 'string', score: '1', profile: { score: '1' } },
                        { name: 'padded', score: '01', profile: { score: '01' } },
                        { name: 'boolean', score: true, profile: { score: true } },
                        { name: 'null', score: null },
                        { name: 'missing' },
                        { name: 'other', score: 2 }
                    ].map((data) => ({ op: 'putData', collection: 'equality', data, root })),
                    ...[
                        { score: { $eq: 1 } },
                        { score: { $eq: null } },
                        { 'profile.score': { $eq: 1 } },
                        { 'profile/score': { $eq: 1 } }
                    ].map((operation) => ({
                        op: 'findDocs',
                        collection: 'equality',
                        query: { $ops: [operation] },
                        root
                    }))
                ]
                    .map((request) => JSON.stringify(request))
                    .join('\n') + '\n'
        })
    })

    if (answer.error) {
        // A page that is not cross-origin isolated cannot build the
        // synchronous directory bridge OPFS requires. That is a browser
        // constraint, not a FYLO defect, so it is reported rather than failed.
        evidence.status = /directory bridge/.test(answer.error) ? 'unsupported' : 'failed'
        evidence.reason = answer.error
    } else {
        evidence.frames = (answer.ndjson ?? '')
            .split('\n')
            .filter(Boolean)
            .map((line) => JSON.parse(line))
        const failure = evidence.frames.find((frame) => !frame.ok)
        // A host failure carries its reason out of band; the frame only has an
        // errno, which would name a disk fault for a browser API gap. A batch
        // that succeeded has no reason: its last host error is an expected
        // probe, such as the NotFoundError of a stat on a missing path.
        evidence.reason = failure ? (answer.hostError ?? failure.error?.message ?? '') : ''
        if (!failure) evidence.status = 'passed'
        else evidence.status = /directory bridge/.test(evidence.reason) ? 'unsupported' : 'failed'
    }
} catch (error) {
    evidence.status =
        evidence.step === 'open-opfs-root' &&
        ['UnknownError', 'SecurityError', 'NotSupportedError'].includes(error?.name)
            ? 'unsupported'
            : 'failed'
    evidence.supported = evidence.status !== 'unsupported'
    evidence.reason = error instanceof Error ? `${error.message}` : String(error)
}

globalThis.__FYLO_ENGINE_EVIDENCE__ = evidence
status.dataset.status = evidence.status
status.textContent = evidence.status
