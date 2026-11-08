// Qualify the single-file hosts in a real browser, on a page served without
// COOP or COEP, and publish the result for the Playwright spec to assert.
//
// `?scenario=` picks one of: file, url, opfs, download, engine, inspect,
// documents.

import {
    SingleFileDatabase,
    hasVdbxMagic,
    listWorkingCopies,
    removeWorkingCopy,
    saveDatabase,
    workingCopyName
} from '/src/browser/vdbx-client.mjs'
import { STUB_SIZE, seededRandom, stubBytes } from './vdbx-stub.mjs'

const WORKER_URL = '/src/browser/fylo-single-file-worker.mjs'
const STUB_URL = '/target/browser-fixtures/stub.vdbx'

const output = document.querySelector('output')
const scenario = new URLSearchParams(location.search).get('scenario') ?? 'file'
const evidence = {
    scenario,
    status: 'running',
    reason: '',
    step: 'start',
    environment: {
        crossOriginIsolated: globalThis.crossOriginIsolated === true,
        sharedArrayBuffer: typeof SharedArrayBuffer,
        fileReaderSync: 'checked in the Worker',
        syncAccessHandle: typeof globalThis.FileSystemFileHandle?.prototype?.createSyncAccessHandle,
        createWritable: typeof globalThis.FileSystemFileHandle?.prototype?.createWritable,
        webLocks: typeof navigator.locks?.request
    }
}

const SCENARIOS = { file, url, opfs, download, engine, inspect, documents }

// Working copies need the origin-private file system. Every current browser
// ships it, but Playwright's Linux WebKit build does not, and that is a gap in
// the test browser rather than a pass.
const NEEDS_OPFS = new Set(['opfs', 'download', 'documents'])

try {
    if (!SCENARIOS[scenario]) throw new Error(`unknown scenario ${scenario}`)
    if (NEEDS_OPFS.has(scenario) && typeof navigator.storage?.getDirectory !== 'function') {
        evidence.status = 'unsupported'
        evidence.reason = 'this browser build has no origin-private file system'
    } else {
        await SCENARIOS[scenario]()
        evidence.status = 'passed'
    }
} catch (error) {
    evidence.status = 'failed'
    evidence.reason = `${evidence.step}: ${error?.message ?? error}`
}
globalThis.__FYLO_VDBX_EVIDENCE__ = evidence
output.dataset.status = evidence.status
output.textContent = evidence.status

async function file() {
    const expected = stubBytes()
    const picked = new File([expected], 'stub.vdbx', { lastModified: 1_700_000_000_000 })
    evidence.step = 'open'
    const database = await SingleFileDatabase.open({ kind: 'file', file: picked }, open())
    try {
        evidence.info = database.info
        evidence.stat = await database.stat()
        evidence.magic = hasVdbxMagic(await database.read(0, 8))
        evidence.step = 'random-reads'
        evidence.reads = await checkReads(database, expected, 200, 1)
        evidence.step = 'refusals'
        evidence.refusals = {
            write: await refusal(database.write(0, new Uint8Array([1]))),
            truncate: await refusal(database.truncate(0)),
            openWrite: await refusal(database.call({ op: 'host', call: 'openWrite' }))
        }
        evidence.step = 'snapshot'
        evidence.snapshotMatches =
            (await digest(await database.snapshot())) === (await digest(expected))
    } finally {
        await database.close()
    }
}

async function url() {
    const expected = stubBytes()
    evidence.step = 'open'
    const database = await SingleFileDatabase.open({ kind: 'url', url: STUB_URL }, open())
    try {
        evidence.info = database.info
        evidence.stat = await database.stat()
        evidence.magic = hasVdbxMagic(await database.read(0, 8))
        evidence.step = 'random-reads'
        evidence.reads = await checkReads(database, expected, 120, 2)
        evidence.refusals = { write: await refusal(database.write(0, new Uint8Array([1]))) }
    } finally {
        await database.close()
    }
    evidence.step = 'missing'
    evidence.missing = await refusal(
        SingleFileDatabase.open(
            { kind: 'url', url: '/target/browser-fixtures/absent.vdbx' },
            open()
        )
    )
}

async function opfs() {
    const original = stubBytes()
    const picked = new File([original], 'stub.vdbx')
    const name = workingCopyName('stub (copy).vdbx')
    evidence.workingCopy = name
    const progress = []
    evidence.step = 'edit-a-copy'
    const database = await SingleFileDatabase.open(
        { kind: 'opfs', name, copyFrom: picked },
        open({ onProgress: (event) => progress.push(event) })
    )
    let mirror = original.slice()
    try {
        evidence.info = database.info
        evidence.progress = { events: progress.length, last: progress.at(-1) }
        evidence.copyMatches =
            (await digest(await database.snapshot())) === (await digest(original))
        evidence.step = 'reads-before-writes'
        evidence.readsBefore = await checkReads(database, mirror, 80, 3)

        evidence.step = 'second-tab'
        evidence.secondOpen = await refusal(SingleFileDatabase.open({ kind: 'opfs', name }, open()))

        evidence.step = 'writes'
        const random = seededRandom(4)
        let writes = 0
        for (let index = 0; index < 48; index++) {
            // A few writes run past the end, so the file grows as an engine
            // appending an extent would make it.
            const pastEnd = index % 12 === 11
            const offset = pastEnd
                ? mirror.length - Math.floor(random() * 512)
                : Math.floor(random() * mirror.length)
            const bytes = new Uint8Array(1 + Math.floor(random() * 65_536))
            for (let at = 0; at < bytes.length; at++) bytes[at] = (index * 31 + at) & 0xff
            const { written } = await database.write(offset, bytes)
            if (written !== bytes.length) throw new Error(`wrote ${written} of ${bytes.length}`)
            mirror = writeInto(mirror, offset, bytes)
            writes++
        }
        evidence.writes = writes
        evidence.step = 'truncate'
        await database.truncate(mirror.length - 12_345)
        mirror = mirror.slice(0, mirror.length - 12_345)
        await database.truncate(mirror.length + 777)
        mirror = resized(mirror, mirror.length + 777)
        await database.flush()
        evidence.statAfterWrites = await database.stat()
        evidence.expectedSize = mirror.length
        evidence.step = 'reads-after-writes'
        evidence.readsAfter = await checkReads(database, mirror, 120, 5)
        evidence.editedMatches =
            (await digest(await database.snapshot())) === (await digest(mirror))
    } finally {
        await database.close()
    }

    evidence.step = 'reopen'
    const reopened = await SingleFileDatabase.open({ kind: 'opfs', name }, open())
    let saved
    try {
        evidence.reopenedSize = (await reopened.stat()).len
        saved = await reopened.snapshot()
        evidence.reopenedMatches = (await digest(saved)) === (await digest(mirror))
    } finally {
        await reopened.close()
    }

    evidence.step = 'save'
    const root = await navigator.storage.getDirectory()
    const target = await root.getFileHandle(`saved-${name}`, { create: true })
    if (typeof target.createWritable === 'function') {
        evidence.saved = await saveDatabase(saved, { handle: target })
        evidence.savedMatches = (await digest(await target.getFile())) === (await digest(mirror))
    } else {
        // Safari has no `createWritable`; Save falls back to a download,
        // which the download scenario covers.
        evidence.saved = 'unsupported'
    }
    await root.removeEntry(`saved-${name}`)

    evidence.step = 'working-copies'
    evidence.listed = (await listWorkingCopies()).some((copy) => copy.name === name)
    await removeWorkingCopy(name)
    evidence.removed = !(await listWorkingCopies()).some((copy) => copy.name === name)
}

async function download() {
    const expected = stubBytes(256 * 1024 + 3, 9)
    const name = workingCopyName('download.vdbx')
    evidence.step = 'edit-a-copy'
    const database = await SingleFileDatabase.open(
        { kind: 'opfs', name, copyFrom: new Blob([expected]) },
        open()
    )
    try {
        await database.write(10, new Uint8Array([1, 2, 3, 4]))
        expected.set([1, 2, 3, 4], 10)
        evidence.step = 'save'
        const blob = await database.snapshot()
        evidence.digest = await digest(expected)
        evidence.size = expected.length
        // No handle and, in a test, no picker: Save has to download.
        const picker = globalThis.showSaveFilePicker
        globalThis.showSaveFilePicker = undefined
        try {
            evidence.saved = await saveDatabase(blob, { suggestedName: 'download.vdbx' })
        } finally {
            globalThis.showSaveFilePicker = picker
        }
    } finally {
        await database.close()
        await removeWorkingCopy(name)
    }
}

/**
 * Inspect a real `.vdbx`, written by the `vdbx_fixture` example, through every
 * host: picked file, ranged requests, and an OPFS working copy where there is
 * one. Each must hand the engine the same bytes, so each report must match.
 */
async function inspect() {
    const FIXTURE = '/target/browser-fixtures/fixture.vdbx'
    evidence.step = 'fetch-fixture'
    evidence.expected = await (await fetch('/target/browser-fixtures/fixture.json')).json()
    const picked = new File([await (await fetch(FIXTURE)).arrayBuffer()], 'fixture.vdbx')
    const sources = { file: { kind: 'file', file: picked }, url: { kind: 'url', url: FIXTURE } }
    const copy = workingCopyName('fixture.vdbx')
    if (typeof navigator.storage?.getDirectory === 'function') {
        sources.opfs = { kind: 'opfs', name: copy, copyFrom: picked }
    }
    evidence.reports = {}
    for (const [label, source] of Object.entries(sources)) {
        evidence.step = `inspect-${label}`
        const database = await SingleFileDatabase.open(
            source,
            open({ moduleUrl: '/dist-web/fylo-browser.wasm' })
        )
        try {
            const request = { op: 'inspectDatabase', root: database.info.path }
            const [frame] = (await database.exec(`${JSON.stringify(request)}\n`))
                .split('\n')
                .filter(Boolean)
                .map((line) => JSON.parse(line))
            evidence.reports[label] = frame.ok ? frame.result : { error: frame.error }
        } finally {
            await database.close()
        }
    }
    if (sources.opfs) await removeWorkingCopy(copy)

    evidence.step = 'inspect-stub'
    const stub = await SingleFileDatabase.open(
        { kind: 'file', file: new File([stubBytes(64 * 1024)], 'stub.vdbx') },
        open({ moduleUrl: '/dist-web/fylo-browser.wasm' })
    )
    try {
        const request = { op: 'inspectDatabase', root: stub.info.path }
        evidence.stubFrame = JSON.parse((await stub.exec(`${JSON.stringify(request)}\n`)).trim())
    } finally {
        await stub.close()
    }
}

/** Run machine requests against the database and return their results. */
async function requests(database, frames) {
    const ndjson = frames
        .map((frame) => JSON.stringify({ ...frame, root: database.info.path }))
        .join('\n')
    const replies = (await database.exec(`${ndjson}\n`))
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
    for (const reply of replies) {
        if (!reply.ok) throw new Error(`${reply.op}: ${reply.error?.code} ${reply.error?.message}`)
    }
    return replies.map((reply) => reply.result)
}

/** Read the documents of a database the native engine wrote, then edit an
 * OPFS working copy of it and read the edit back after reopening. */
async function documents() {
    evidence.step = 'fetch-database'
    const bytes = await (await fetch('/target/browser-fixtures/documents.vdbx')).arrayBuffer()
    const picked = new File([bytes], 'documents.vdbx')
    evidence.step = 'read-picked'
    const reader = await SingleFileDatabase.open(
        { kind: 'file', file: picked },
        open({ moduleUrl: '/dist-web/fylo-browser.wasm' })
    )
    try {
        const [red] = await requests(reader, [
            { op: 'findDocs', collection: 'users', query: { $ops: [{ team: { $eq: 'red' } }] } }
        ])
        evidence.red = Object.values(red)
            .map((document) => document.name)
            .sort()
    } finally {
        await reader.close()
    }
    const copy = workingCopyName('documents.vdbx')
    try {
        evidence.step = 'edit-opfs'
        const writer = await SingleFileDatabase.open(
            { kind: 'opfs', name: copy, copyFrom: picked },
            open({ moduleUrl: '/dist-web/fylo-browser.wasm' })
        )
        try {
            const [identifier] = await requests(writer, [
                { op: 'putData', collection: 'users', data: { name: 'browser', team: 'red' } }
            ])
            evidence.written = identifier
        } finally {
            await writer.close()
        }
        evidence.step = 'reopen-opfs'
        const reopened = await SingleFileDatabase.open(
            { kind: 'opfs', name: copy },
            open({ moduleUrl: '/dist-web/fylo-browser.wasm' })
        )
        try {
            const [found, all] = await requests(reopened, [
                { op: 'getDoc', collection: 'users', id: evidence.written },
                { op: 'findDocs', collection: 'users', query: {} }
            ])
            evidence.reread = found[evidence.written]?.name
            evidence.total = Object.keys(all).length
        } finally {
            await reopened.close()
        }
    } finally {
        await removeWorkingCopy(copy)
    }
}

async function engine() {
    const picked = new File([stubBytes(64 * 1024)], 'stub.vdbx')
    evidence.step = 'instantiate'
    const database = await SingleFileDatabase.open(
        { kind: 'file', file: picked },
        open({ moduleUrl: '/dist-web/fylo-browser.wasm' })
    )
    try {
        evidence.info = database.info
        evidence.step = 'handshake'
        const answer = await database.exec(`${JSON.stringify({ op: 'handshake' })}\n`)
        evidence.frames = answer
            .split('\n')
            .filter(Boolean)
            .map((line) => JSON.parse(line))
    } finally {
        await database.close()
    }
}

function open(options = {}) {
    return { workerUrl: WORKER_URL, ...options }
}

/** Read at seeded random offsets, plus the edges, and compare every byte. */
async function checkReads(database, expected, count, seed) {
    const random = seededRandom(seed)
    const size = expected.length
    const cases = [
        [0, 0],
        [0, 8],
        [size - 1, 1],
        [size - 5, 64],
        [size, 16],
        [size + 4096, 16],
        [0, Math.min(size, 1024 * 1024)]
    ]
    for (let index = 0; index < count; index++) {
        cases.push([
            Math.floor(random() * (size + 1024)),
            Math.floor(random() * (random() < 0.1 ? 1024 * 1024 : 4096)) + 1
        ])
    }
    const mismatches = []
    let bytes = 0
    for (const [offset, length] of cases) {
        const got = await database.read(offset, length)
        const want = expected.subarray(Math.min(offset, size), Math.min(offset + length, size))
        bytes += got.length
        if (!sameBytes(got, want)) {
            mismatches.push({ offset, length, got: got.length, want: want.length })
        }
    }
    return { reads: cases.length, bytes, mismatches: mismatches.slice(0, 5) }
}

async function refusal(promise) {
    try {
        await promise
        return { refused: false }
    } catch (error) {
        return {
            refused: true,
            code: error?.code ?? null,
            message: String(error?.message ?? error)
        }
    }
}

function writeInto(target, offset, bytes) {
    const grown = resized(target, Math.max(target.length, offset + bytes.length))
    grown.set(bytes, offset)
    return grown
}

function resized(bytes, length) {
    if (length === bytes.length) return bytes
    const next = new Uint8Array(length)
    next.set(bytes.subarray(0, Math.min(length, bytes.length)))
    return next
}

function sameBytes(a, b) {
    if (a.length !== b.length) return false
    for (let index = 0; index < a.length; index++) if (a[index] !== b[index]) return false
    return true
}

async function digest(value) {
    const buffer = value instanceof Blob ? await value.arrayBuffer() : value
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', buffer))
    return [...hash].map((byte) => byte.toString(16).padStart(2, '0')).join('')
}

export { STUB_SIZE }
