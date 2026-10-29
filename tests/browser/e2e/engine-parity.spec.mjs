// The browser engine against the native one, op by op.
//
// Every scenario runs the same requests through `fylo-browser.wasm` over OPFS
// and through `fylo-rust exec --loop` over a temporary directory, side by
// side, one step at a time, and holds each browser frame to the native frame
// once `comparable` and the normalization below have removed what may
// legitimately differ. A step can depend on an earlier answer (a commit id, a
// cursor), which is why both engines run as live sessions rather than as one
// batch each.

import {
    browserRoot,
    comparable,
    expect,
    expectOk,
    mintIds,
    nativeBinary,
    nativeExec,
    nativeRoot,
    readTree,
    test,
    writeTree
} from './engine-harness.mjs'
import { spawn } from 'node:child_process'
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { createInterface } from 'node:readline'

/**
 * A native `exec --loop` process kept open, so a later step can use what an
 * earlier one answered, and so query cursors survive between steps.
 */
class NativeSession {
    constructor(root) {
        this.root = root
        this.waiting = []
        this.stderr = ''
        // No FYLO_* variables: the browser has no environment, so a native
        // answer shaped by a developer's shell would not be comparable.
        const env = Object.fromEntries(
            Object.entries(process.env).filter(([name]) => !name.startsWith('FYLO_'))
        )
        this.child = spawn(nativeBinary(), ['exec', '--loop', '--root', root], {
            env,
            stdio: ['pipe', 'pipe', 'pipe']
        })
        this.child.stderr.on('data', (chunk) => {
            this.stderr += chunk
        })
        createInterface({ input: this.child.stdout }).on('line', (line) => {
            this.waiting.shift()?.resolve(JSON.parse(line))
        })
        this.exited = new Promise((resolve) => {
            this.child.on('exit', (code) => {
                for (const waiter of this.waiting.splice(0)) {
                    waiter.reject(new Error(`native exited (${code}): ${this.stderr}`))
                }
                resolve(code)
            })
        })
    }

    exec(requests) {
        const answers = requests.map(
            () => new Promise((resolve, reject) => this.waiting.push({ resolve, reject }))
        )
        this.child.stdin.write(
            requests.map((request) => JSON.stringify({ root: this.root, ...request })).join('\n') +
                '\n'
        )
        return Promise.all(answers)
    }

    async close() {
        this.child.stdin.end()
        await this.exited
    }
}

/** Identifiers an engine allocated itself, which therefore differ per engine. */
function allocated(request, frame) {
    if (!frame.ok) return []
    if (request.op === 'putData' && request.id === undefined) return [frame.result]
    if (request.op === 'batchPutData') return frame.result
    if (request.op === 'executeSQL' && /^\s*insert/i.test(request.sql)) return [frame.result]
    if (request.op === 'commit' && frame.result) return [frame.result.id]
    return []
}

/**
 * One engine's side of a pair: its root, and the identifiers it allocated,
 * each aliased `<new:N>` in allocation order so both sides line up.
 */
class Side {
    constructor(name, root) {
        this.name = name
        this.root = root
        this.aliases = new Map()
    }

    track(requests, frames) {
        requests.forEach((request, index) => {
            for (const id of allocated(request, frames[index])) {
                if (typeof id === 'string' && !this.aliases.has(id)) {
                    this.aliases.set(id, `<new:${this.aliases.size}>`)
                }
            }
        })
    }

    /** A request with `<new:N>` replaced by this engine's own identifier. */
    resolve(value) {
        const reverse = new Map([...this.aliases].map(([id, alias]) => [alias, id]))
        return deepMap(value, (text) =>
            text.replace(/<new:\d+>/g, (alias) => reverse.get(alias) ?? alias)
        )
    }

    /** A frame with this engine's root and identifiers replaced by placeholders. */
    view(frame) {
        const aliases = [...this.aliases]
        const scrubbed = deepMap(frame, (text) => {
            let out = text.split(this.root).join('<root>')
            for (const [id, alias] of aliases) out = out.split(id).join(alias)
            return out
        })
        // A cursor names the process that issued it, so only its presence
        // can match.
        if (scrubbed.ok && typeof scrubbed.result?.nextCursor === 'string') {
            scrubbed.result.nextCursor = '<cursor>'
        }
        return comparable(dropKeys(scrubbed, MORE_VOLATILE))
    }
}

/** A frame from a one-off batch, with its root replaced by a placeholder. */
function rootView(frame, root) {
    return new Side('', root).view(frame)
}

// Wall-clock fields `comparable` does not know about: `getMeta` reports the
// record's `mtime` and a raw file its `lastModified`, which is when each
// engine happened to write it.
const MORE_VOLATILE = new Set(['mtime', 'lastModified'])

function dropKeys(value, keys) {
    if (Array.isArray(value)) return value.map((item) => dropKeys(item, keys))
    if (value && typeof value === 'object') {
        return Object.fromEntries(
            Object.entries(value)
                .filter(([key]) => !keys.has(key))
                .map(([key, inner]) => [key, dropKeys(inner, keys)])
        )
    }
    return value
}

function deepMap(value, mapString) {
    if (typeof value === 'string') return mapString(value)
    if (Array.isArray(value)) return value.map((item) => deepMap(item, mapString))
    if (value && typeof value === 'object') {
        return Object.fromEntries(
            Object.entries(value).map(([key, inner]) => [mapString(key), deepMap(inner, mapString)])
        )
    }
    return value
}

/** The same root, open in both engines at once. */
class Pair {
    static async open(fylo, label, { nativeDir, opfsRoot } = {}) {
        const pair = new Pair()
        pair.fylo = fylo
        pair.native = new Side('native', nativeDir ?? nativeRoot(label))
        pair.browser = new Side('browser', opfsRoot ?? browserRoot(label))
        pair.nativeSession = new NativeSession(pair.native.root)
        pair.browserSession = await fylo.open(pair.browser.root)
        return pair
    }

    /**
     * Run `requests` on both engines and expect every frame to match.
     * `differ(index, native, browser)` may claim a frame whose difference is
     * documented; it asserts that frame itself and returns true.
     */
    async step(requests, { differ } = {}) {
        const nativeRequests = requests.map((request) => this.native.resolve(request))
        const browserRequests = requests.map((request) => this.browser.resolve(request))
        const native = await this.nativeSession.exec(nativeRequests)
        const browser = await this.fylo.exec(this.browserSession, browserRequests)
        // A frame claimed as a documented difference may allocate on one side
        // only, so it is kept out of the aliasing that lines the sides up.
        const claimed = requests.map((_, index) =>
            Boolean(differ?.(index, native[index], browser[index]))
        )
        const kept = (frames) => frames.filter((_, index) => !claimed[index])
        this.native.track(kept(nativeRequests), kept(native))
        this.browser.track(kept(browserRequests), kept(browser))
        requests.forEach((request, index) => {
            if (claimed[index]) return
            expect
                .soft(
                    this.browser.view(browser[index]),
                    `frame ${index} ${JSON.stringify(request).slice(0, 160)}`
                )
                .toEqual(this.native.view(native[index]))
        })
        return { native, browser }
    }

    async close() {
        await this.fylo.close(this.browserSession)
        await this.nativeSession.close()
        await this.fylo.remove(this.browser.root)
        rmSync(this.native.root, { recursive: true, force: true })
    }
}

/** No soft failure so far: a later step built on a mismatch only adds noise. */
function expectNoMismatch() {
    expect(test.info().errors, 'browser frames differ from native').toEqual([])
}

test.describe.configure({ timeout: 600_000 })

test('CRUD and metadata answer exactly as native', async ({ fylo }) => {
    const id = mintIds(6)
    const pair = await Pair.open(fylo, 'parity-crud')
    try {
        const setup = await pair.step([
            { op: 'createCollection', collection: 'people' },
            { op: 'putData', collection: 'people', id: id[0], data: ada() },
            {
                op: 'putData',
                collection: 'people',
                id: id[1],
                data: { name: 'Grace', age: 85, tags: [], profile: { city: 'Arlington' } },
                meta: { source: 'import', rank: 3, nested: { list: [1, 'two', null] } }
            },
            { op: 'putData', collection: 'people', id: id[2], data: { name: 'Linus', age: 54 } },
            // An identifier the engine allocates, aliased on each side.
            { op: 'putData', collection: 'people', data: { name: 'Allocated', age: 1 } },
            { op: 'getDoc', collection: 'people', id: id[0] },
            { op: 'getDoc', collection: 'people', id: id[1] },
            { op: 'getMeta', collection: 'people', id: id[0] },
            { op: 'getMeta', collection: 'people', id: id[1] },
            { op: 'setMeta', collection: 'people', id: id[0], meta: { reviewed: true } },
            { op: 'getMeta', collection: 'people', id: id[0] },
            { op: 'setMeta', collection: 'people', id: id[1], meta: { rank: 4 } },
            { op: 'patchDoc', collection: 'people', id: id[0], newDoc: patch() },
            { op: 'getDoc', collection: 'people', id: id[0] },
            { op: 'getLatest', collection: 'people', id: id[0] },
            { op: 'getLatest', collection: 'people', id: id[0], onlyId: true },
            { op: 'getLatest', collection: 'people', id: id[5] },
            { op: 'getLatest', collection: 'people', id: id[5], onlyId: true },
            {
                op: 'batchPutData',
                collection: 'people',
                batch: [
                    { name: 'Batch A', age: 20 },
                    { name: 'Batch B', age: 30, profile: { city: 'Zürich' } },
                    { name: 'Batch C', age: 40 }
                ]
            },
            { op: 'findDocs', collection: 'people', query: {} }
        ])
        expectOk(setup.browser, 'browser setup')
        expect(setup.native[5].result[id[0]].name).toBe('Ada')
        expectNoMismatch()

        const bulk = await pair.step([
            { op: 'getDoc', collection: 'people', id: '<new:0>' },
            { op: 'getDoc', collection: 'people', id: '<new:2>' },
            {
                op: 'patchDocs',
                collection: 'people',
                update: { $set: { senior: true }, $where: { $ops: [{ age: { $gte: 50 } }] } }
            },
            // Without `$where` the update itself is the filter, so this
            // touches every document.
            { op: 'patchDocs', collection: 'people', update: { $set: { touched: 1 } } },
            { op: 'findDocs', collection: 'people', query: {} },
            { op: 'delDoc', collection: 'people', id: id[2] },
            { op: 'getDoc', collection: 'people', id: id[2] },
            { op: 'getLatest', collection: 'people', id: id[2] },
            { op: 'findDeletedDocs', collection: 'people', query: {} },
            {
                op: 'delDocs',
                collection: 'people',
                delete: { $ops: [{ name: { $like: 'Batch%' } }] }
            },
            {
                op: 'delDocs',
                collection: 'people',
                delete: { $where: { $ops: [{ name: { $eq: 'nobody' } }] } }
            },
            { op: 'findDocs', collection: 'people', query: {} },
            { op: 'findDeletedDocs', collection: 'people', query: {} },
            {
                op: 'findDeletedDocs',
                collection: 'people',
                query: { $ops: [{ name: { $eq: 'Linus' } }] }
            },
            { op: 'findDeletedDocs', collection: 'people', query: { $onlyIds: true } },
            { op: 'restoreDoc', collection: 'people', id: id[2] },
            { op: 'getDoc', collection: 'people', id: id[2] },
            { op: 'restoreDoc', collection: 'people', id: '<new:1>' },
            { op: 'findDeletedDocs', collection: 'people', query: {} },
            { op: 'findDocs', collection: 'people', query: {} },
            { op: 'getMeta', collection: 'people', id: id[1] },
            { op: 'inspectCollection', collection: 'people' }
        ])
        expectOk(bulk.browser, 'browser bulk')
        expect(Object.values(bulk.native[0].result)[0]?.name).toBe('Allocated')
        expect(bulk.native[2].result).toBe(2)
        expect(bulk.native[9].result).toBe(3)
        expect(Object.keys(bulk.native[12].result)).toHaveLength(4)
        expectNoMismatch()
    } finally {
        await pair.close()
    }
})

test('error frames carry the native code and message', async ({ fylo }) => {
    const id = mintIds(4)
    const pair = await Pair.open(fylo, 'parity-errors')
    try {
        await pair.step([
            { op: 'createCollection', collection: 'people' },
            { op: 'putData', collection: 'people', id: id[0], data: ada() }
        ])
        const { browser } = await pair.step([
            // Absent but well formed: an empty answer, not an error.
            { op: 'getDoc', collection: 'people', id: id[1] },
            { op: 'getDoc', collection: 'people', id: 'not-an-id' },
            { op: 'getDoc', collection: 'people' },
            { op: 'getDoc', collection: 'ghosts', id: id[0] },
            { op: 'getMeta', collection: 'people', id: id[1] },
            { op: 'patchDoc', collection: 'people', id: id[1], newDoc: { a: 1 } },
            { op: 'patchDoc', collection: 'people', id: id[0] },
            { op: 'patchDoc', collection: 'people', id: id[0], newDoc: [1] },
            { op: 'delDoc', collection: 'people', id: id[1] },
            { op: 'delDoc', collection: 'people', id: 'nope' },
            { op: 'restoreDoc', collection: 'people', id: id[0] },
            { op: 'restoreDoc', collection: 'people', id: id[1] },
            { op: 'setMeta', collection: 'people', id: id[0] },
            { op: 'setMeta', collection: 'people', id: id[1], meta: { a: 1 } },
            { op: 'putData', collection: 'people' },
            { op: 'putData', collection: 'people', data: [1, 2] },
            { op: 'putData', collection: 'ghosts', id: id[2], data: { a: 1 } },
            { op: 'putData', collection: 'people', id: 'bad id', data: { a: 1 } },
            { op: 'putData', data: { a: 1 } },
            { op: 'putData', collection: 'people', id: id[3], data: { list: [{ a: 1 }] } },
            { op: 'batchPutData', collection: 'people', batch: { a: 1 } },
            { op: 'batchPutData', collection: 'people', batch: [1] },
            { op: 'patchDocs', collection: 'people', update: { name: 'x' } },
            { op: 'patchDocs', collection: 'people' },
            { op: 'delDocs', collection: 'people' },
            { op: 'findDocs', collection: 'people', query: { $ops: 5 } },
            { op: 'findDocs', collection: 'people', query: { $ops: [1] } },
            { op: 'findDocs', collection: 'people', query: { $limit: -1 } },
            { op: 'findDocs', collection: 'people', query: { $created: { $gt: 'x' } } },
            { op: 'findDocs', collection: 'people', query: [] },
            { op: 'findDocs', collection: 'ghosts', query: {} },
            { op: 'findDeletedDocs', collection: 'ghosts', query: {} },
            { op: 'inspectCollection', collection: 'ghosts' },
            // Only a file collection has raw objects to verify.
            { op: 'verifyCollection', collection: 'people' },
            { op: 'createCollection', collection: 'people' },
            { op: 'createCollection', collection: 'u' },
            { op: 'createCollection', collection: '../escape' },
            { op: 'createCollection', collection: 'files', kind: 'folder' },
            { op: 'executeSQL', sql: 'SELECT FROM people' },
            { op: 'executeSQL' },
            { op: 'joinDocs' },
            { op: 'bogus' },
            { op: 'backup' },
            { op: 42 },
            {}
        ])
        // The answers must be errors, not accidental successes on both sides;
        // creating an existing collection is idempotent.
        expect(browser.filter((frame) => frame.ok).map((frame) => frame.op)).toEqual([
            'getDoc',
            'createCollection'
        ])
        expectNoMismatch()
    } finally {
        await pair.close()
    }
})

// Every bridge call costs the browser about a millisecond, and a document a
// query has to read costs some sixty of them, so the operator matrix runs over
// a small collection and the volume test below over a native-built one.
test('findDocs agrees on every operator and edge value', async ({ fylo }) => {
    test.setTimeout(1_200_000)
    // The loose comparisons over one field of every awkward value, and the
    // remaining operators over ordinary rows, each in its own collection so a
    // query only reads the documents it is about.
    const collections = {
        edges: EDGE_VALUES.map((value, index) =>
            value === MISSING ? { edge: index } : { edge: index, v: value }
        ),
        rows: [
            ...Array.from({ length: 24 }, (_, index) => row(index)),
            { clé: 'x', 'a.b': 'dotted key', a: { b: 'nested' } }
        ]
    }
    const queries = {
        edges: QUERIES.filter(
            (query) => /"v"/.test(JSON.stringify(query)) || query.$ops === undefined
        ),
        rows: QUERIES.filter((query) => !/"v"/.test(JSON.stringify(query)))
    }
    const id = mintIds(collections.edges.length + collections.rows.length)
    const ids = {
        edges: id.slice(0, collections.edges.length),
        rows: id.slice(collections.edges.length)
    }
    const pair = await Pair.open(fylo, 'parity-query')
    try {
        const writes = Object.entries(collections).flatMap(([collection, documents]) => [
            { op: 'createCollection', collection },
            ...documents.map((data, index) => ({
                op: 'putData',
                collection,
                id: ids[collection][index],
                data
            }))
        ])
        const loaded = await pair.step(writes)
        expectOk(loaded.browser, 'browser load')

        const run = async (collection, list, label) => {
            const { native } = await pair.step(
                list.map((query) => ({ op: 'findDocs', collection, query }))
            )
            const sizes = native.map((frame) => (frame.ok ? Object.keys(frame.result).length : -1))
            // Guards against a suite that passes because nothing matches.
            expect(sizes.filter((size) => size > 0).length, label).toBeGreaterThan(
                list.length * 0.7
            )
            return native
        }
        const edges = await run('edges', queries.edges, 'edge values')
        expect(count(edges, queries.edges, { v: { $eq: 1 } })).toBeGreaterThan(3)
        const rows = await run('rows', queries.rows, 'rows')
        expect(Object.keys(rows[0].result)).toHaveLength(collections.rows.length)
        expect(count(rows, queries.rows, { group: { $eq: 'red' } })).toBe(8)
        expect(count(rows, queries.rows, { s: { $eq: 7 } })).toBe(1)
        expectNoMismatch()

        // The index must follow writes the same way in both engines.
        await pair.step([
            ...ids.rows.slice(0, 6).map((target, index) => ({
                op: 'patchDoc',
                collection: 'rows',
                id: target,
                newDoc: { group: 'patched', n: 1000 + index, nested: { a: { b: -1 } }, s: '01' }
            })),
            ...ids.rows
                .slice(6, 10)
                .map((target) => ({ op: 'delDoc', collection: 'rows', id: target })),
            { op: 'restoreDoc', collection: 'rows', id: ids.rows[7] }
        ])
        const subset = queries.rows.filter((query) =>
            /group|"n"|nested|"s"|\$limit|\$onlyIds/.test(JSON.stringify(query))
        )
        await run('rows', subset, 'after writes')
        expectNoMismatch()

        await pair.step([{ op: 'rebuildCollection', collection: 'rows' }])
        await run('rows', subset.slice(0, 12), 'after rebuild')
        expectNoMismatch()
    } finally {
        await pair.close()
    }
})

test('findDocs agrees over a few hundred documents', async ({ fylo }) => {
    test.setTimeout(900_000)
    const id = mintIds(320)
    // Built natively and copied into both roots: three hundred browser writes
    // would take five minutes, and the browser's writes are covered above.
    const seedDir = nativeRoot('parity-volume-seed')
    const nativeDir = nativeRoot('parity-volume')
    const opfsRoot = browserRoot('parity-volume')
    try {
        expectOk(
            nativeExec(seedDir, [
                { op: 'createCollection', collection: 'rows' },
                ...id.slice(0, 300).map((target, index) => ({
                    op: 'putData',
                    collection: 'rows',
                    id: target,
                    data: row(index)
                }))
            ]),
            'native seed'
        )
        cpSync(seedDir, nativeDir, { recursive: true })
        await fylo.seed(opfsRoot, readTree(seedDir))
        const pair = await Pair.open(fylo, 'parity-volume', { nativeDir, opfsRoot })
        try {
            const queries = [
                {},
                { $ops: [{ group: { $eq: 'red' } }] },
                { $ops: [{ n: { $eq: 150 } }] },
                { $ops: [{ s: { $eq: '042' } }, { s: { $eq: 43 } }] },
                { $ops: [{ 'nested/a/b': { $eq: 3 } }] },
                { $ops: [{ group: { $eq: 'blue' } }], $limit: 7 },
                { $onlyIds: true, $ops: [{ group: { $eq: 'green' } }] }
            ].map((query) => ({ op: 'findDocs', collection: 'rows', query }))
            const before = await pair.step(queries)
            expect(Object.keys(before.native[0].result)).toHaveLength(300)
            expect(Object.keys(before.native[1].result)).toHaveLength(100)
            expectNoMismatch()
            await pair.step([
                ...id.slice(300, 310).map((target, index) => ({
                    op: 'putData',
                    collection: 'rows',
                    id: target,
                    data: row(300 + index)
                })),
                ...id.slice(0, 10).map((target, index) => ({
                    op: 'patchDoc',
                    collection: 'rows',
                    id: target,
                    newDoc: { group: 'red', n: 150 + index }
                })),
                ...id
                    .slice(100, 110)
                    .map((target) => ({ op: 'delDoc', collection: 'rows', id: target }))
            ])
            const after = await pair.step(queries)
            expect(Object.keys(after.native[0].result)).toHaveLength(300)
            expectNoMismatch()
        } finally {
            await pair.close()
        }
    } finally {
        await fylo.remove(opfsRoot)
        for (const dir of [seedDir, nativeDir]) rmSync(dir, { recursive: true, force: true })
    }
})

test('paged queries page through the same snapshot as native', async ({ fylo }) => {
    const id = mintIds(40)
    const pair = await Pair.open(fylo, 'parity-pages')
    try {
        await pair.step(
            [
                { op: 'createCollection', collection: 'rows' },
                ...id.map((target, index) => ({
                    op: 'putData',
                    collection: 'rows',
                    id: target,
                    data: row(index)
                })),
                ...id
                    .slice(0, 10)
                    .map((target) => ({ op: 'delDoc', collection: 'rows', id: target }))
            ],
            { differ: () => true }
        )
        // Refused before any cursor exists.
        await pair.step([
            { op: 'findDocs', collection: 'rows', query: {}, page: { limit: 0 } },
            { op: 'findDocs', collection: 'rows', query: {}, page: { limit: 100_000 } },
            { op: 'findDocs', collection: 'rows', query: {}, page: { cursor: 'fqc1.1.1' } },
            { op: 'findDocs', collection: 'rows', query: {}, page: 3 }
        ])
        expectNoMismatch()

        // Each paged query in a batch of its own: a trap in the module loses
        // every frame of the batch it happens in.
        const firstPage = async (request) => {
            try {
                return (await pair.step([request])).browser[0]
            } catch (error) {
                expect
                    .soft(`${error.message}`, `browser trapped on ${JSON.stringify(request)}`)
                    .toBe('a frame')
                // A trap leaves the session's OPFS handles behind, and every
                // later batch on it fails with EIO: start a fresh one.
                await fylo.close(pair.browserSession)
                pair.browserSession = await fylo.open(pair.browser.root)
                return undefined
            }
        }
        await firstPage({
            op: 'findDocs',
            collection: 'rows',
            query: { $onlyIds: true, $ops: [{ group: { $eq: 'red' } }] },
            page: { limit: 4 }
        })
        await firstPage({ op: 'findDocs', collection: 'rows', query: {}, page: {} })
        await firstPage({
            op: 'findDeletedDocs',
            collection: 'rows',
            query: {},
            page: { limit: 3 }
        })
        const request = { op: 'findDocs', collection: 'rows', query: {}, page: { limit: 12 } }
        const browserFirst = await firstPage(request)
        expectNoMismatch()

        // Each engine follows its own cursor; the pages must line up. The
        // handshake scopes cursors to the process, which in a browser is the
        // Worker that holds the module, across batches.
        if (!browserFirst) return
        let [native] = await pair.nativeSession.exec([request])
        let browser = browserFirst
        let pages = 1
        while (native.result.nextCursor) {
            const next = (cursor) => ({ ...request, page: { ...request.page, cursor } })
            ;[native] = await pair.nativeSession.exec([next(native.result.nextCursor)])
            ;[browser] = await fylo.exec(pair.browserSession, [next(browser.result.nextCursor)])
            pages += 1
            expect(pair.browser.view(browser), `page ${pages}`).toEqual(pair.native.view(native))
        }
        expect(pages).toBe(3)
    } finally {
        await pair.close()
    }
})

test('executeSQL reads and writes like native', async ({ fylo }) => {
    const id = mintIds(12)
    const pair = await Pair.open(fylo, 'parity-sql')
    try {
        const people = [
            { name: 'Ada', score: 91, team: 'red', active: true, profile: { city: 'London' } },
            { name: 'Grace', score: 85, team: 'blue', active: false },
            { name: "O'Brien", score: 42, team: 'red', active: true },
            { name: 'Linus', score: '54', team: 'green', active: true },
            { name: 'Zoë', score: 7, team: 'blue', active: false, tags: ['x'] },
            { name: '', score: null, team: 'green' },
            { name: 'Ken', score: 100, team: 'red', active: true }
        ]
        await pair.step([
            { op: 'createCollection', collection: 'sql-users' },
            ...people.map((data, index) => ({
                op: 'putData',
                collection: 'sql-users',
                id: id[index],
                data
            }))
        ])
        const select = (sql) => ({ op: 'executeSQL', sql })
        const { native } = await pair.step([
            select('SELECT * FROM sql-users'),
            select('SELECT name, score FROM sql-users WHERE score >= 50'),
            select("SELECT name FROM sql-users WHERE team = 'red' AND score > 50"),
            select("SELECT name FROM sql-users WHERE team = 'blue' OR score < 10"),
            select("SELECT * FROM sql-users WHERE name != 'Ada'"),
            select("SELECT name FROM sql-users WHERE name LIKE '%a%'"),
            select("SELECT name FROM sql-users WHERE name LIKE 'Zo_'"),
            select("SELECT name FROM sql-users WHERE name = 'O''Brien'"),
            select("SELECT name FROM sql-users WHERE name = ''"),
            select('SELECT name FROM sql-users WHERE active = true'),
            select("SELECT name FROM sql-users WHERE score = '54'"),
            select('SELECT name FROM sql-users WHERE score = 54'),
            select("SELECT name FROM sql-users WHERE profile.city = 'London'"),
            select('SELECT name FROM sql-users ORDER BY score DESC'),
            select('SELECT name FROM sql-users LIMIT 2'),
            select('SELECT name FROM sql-users WHERE score > 0 ORDER BY name LIMIT 3'),
            select('SELECT * FROM sql-users GROUP BY team'),
            select('SELECT _id FROM sql-users WHERE score >= 90'),
            select('SELECT COUNT(*) FROM sql-users'),
            select('SELECT name FROM sql-users INNER JOIN teams ON team = id'),
            select('SELECT * FROM ghosts'),
            select('SELECT'),
            select('DROP TABLE sql-users'),
            select("EXPLAIN SELECT * FROM sql-users WHERE team = 'red'")
        ])
        expect(Object.keys(native[0].result)).toHaveLength(people.length)
        expectNoMismatch()

        const writes = await pair.step([
            select("INSERT INTO sql-users (name, score, team) VALUES ('Cy', 12, 'green')"),
            select("INSERT INTO sql-users (name, active) VALUES ('Di', false)"),
            select("UPDATE sql-users SET score = 50, active = false WHERE team = 'red'"),
            select("UPDATE sql-users SET team = 'violet' WHERE name = 'nobody'"),
            select("DELETE FROM sql-users WHERE team = 'blue'"),
            select('SELECT * FROM sql-users'),
            { op: 'findDeletedDocs', collection: 'sql-users', query: {} },
            select("UPDATE sql-users SET reviewed = 'yes'"),
            select('SELECT name, reviewed FROM sql-users'),
            select('DELETE FROM sql-users'),
            select('SELECT * FROM sql-users'),
            select("INSERT INTO ghosts (name) VALUES ('x')"),
            select("UPDATE sql-users SET WHERE name = 'x'")
        ])
        expect(writes.native.slice(2, 5).map((frame) => frame.result)).toEqual([3, 0, 2])
        expectNoMismatch()
    } finally {
        await pair.close()
    }
})

test('joinDocs matches native in every mode', async ({ fylo }) => {
    const id = mintIds(10)
    const pair = await Pair.open(fylo, 'parity-join')
    try {
        const users = [
            { name: 'Ada', score: 5, team: 'red' },
            { name: 'Bob', score: 7, team: 'blue' },
            { name: 'Cy', score: 3, team: 'red' },
            { name: 'Dee', score: '9', team: 'blue' }
        ]
        const orders = [
            { user: 'Ada', total: 3, team: 'red' },
            { user: 'Cy', total: 9, team: 'red' },
            { user: 'Cy', total: 1, team: 'blue' },
            { user: 'Eve', total: 4, team: 'green' }
        ]
        await pair.step([
            { op: 'createCollection', collection: 'users' },
            { op: 'createCollection', collection: 'orders' },
            ...users.map((data, index) => ({
                op: 'putData',
                collection: 'users',
                id: id[index],
                data
            })),
            ...orders.map((data, index) => ({
                op: 'putData',
                collection: 'orders',
                id: id[5 + index],
                data
            }))
        ])
        const join = (spec) => ({
            op: 'joinDocs',
            join: { $leftCollection: 'users', $rightCollection: 'orders', ...spec }
        })
        const on = { name: { $eq: 'user' } }
        const { native } = await pair.step([
            join({ $mode: 'inner', $on: on }),
            join({ $mode: 'left', $on: on }),
            join({ $mode: 'right', $on: on }),
            join({ $mode: 'outer', $on: on }),
            join({ $mode: 'inner', $on: { name: { $eq: 'user' }, team: { $eq: 'team' } } }),
            join({ $mode: 'inner', $on: { score: { $gt: 'total' } } }),
            join({ $mode: 'inner', $on: { score: { $lte: 'total' } } }),
            join({ $mode: 'inner', $on: { name: { $ne: 'user' } }, $limit: 3 }),
            join({ $mode: 'inner', $on: on, $select: ['name', 'total'] }),
            join({ $mode: 'inner', $on: on, $rename: { total: 'amount' } }),
            join({ $mode: 'inner', $on: on, $onlyIds: true }),
            join({ $mode: 'left', $on: on, $groupby: 'team' }),
            join({ $mode: 'sideways', $on: on }),
            join({ $mode: 'inner', $on: { name: { $like: 'user' } } }),
            join({ $mode: 'inner', $on: {} }),
            {
                op: 'joinDocs',
                join: {
                    $leftCollection: 'users',
                    $rightCollection: 'ghosts',
                    $mode: 'inner',
                    $on: on
                }
            }
        ])
        expect(Object.keys(native[0].result)).toHaveLength(3)
        expectNoMismatch()
    } finally {
        await pair.close()
    }
})

test('history: commit, log, status, diff, branch, checkout, merge, restoreCommit', async ({
    fylo
}) => {
    const id = mintIds(4)
    const pair = await Pair.open(fylo, 'parity-history')
    try {
        const steps = [
            [
                { op: 'log' },
                { op: 'status' },
                { op: 'branch' },
                // Nothing to commit yet: null, not a commit.
                { op: 'commit', message: 'empty' },
                { op: 'createCollection', collection: 'notes' },
                { op: 'putData', collection: 'notes', id: id[0], data: { text: 'first' } },
                { op: 'putData', collection: 'notes', id: id[1], data: { text: 'second' } },
                { op: 'status' },
                { op: 'diff' },
                { op: 'commit', message: 'first' },
                { op: 'status' },
                { op: 'log' },
                { op: 'log', limit: 1 },
                { op: 'commit', message: 'nothing changed' },
                { op: 'commit' }
            ],
            [
                { op: 'checkout', branch: 'feature', create: true },
                { op: 'branch' },
                { op: 'putData', collection: 'notes', id: id[2], data: { text: 'feature' } },
                { op: 'patchDoc', collection: 'notes', id: id[0], newDoc: { text: 'edited' } },
                { op: 'delDoc', collection: 'notes', id: id[1] },
                { op: 'status' },
                { op: 'diff' },
                { op: 'commit', message: 'feature work' },
                { op: 'diff', from: '<new:0>', to: '<new:1>' },
                { op: 'diff', from: '<new:1>', to: 'WORKTREE' },
                { op: 'log' },
                { op: 'checkout', branch: 'main' },
                { op: 'findDocs', collection: 'notes', query: {} },
                { op: 'checkout', branch: 'feature', create: true },
                { op: 'checkout', branch: 'nope' },
                { op: 'checkout' }
            ],
            [
                { op: 'merge', source: 'feature' },
                { op: 'findDocs', collection: 'notes', query: {} },
                { op: 'status' },
                { op: 'log' },
                { op: 'branch' },
                { op: 'merge', source: 'feature' },
                { op: 'merge', source: 'nope' },
                { op: 'merge' }
            ],
            [
                { op: 'putData', collection: 'notes', id: id[3], data: { text: 'uncommitted' } },
                // A dirty worktree: refused without force.
                { op: 'restoreCommit', id: '<new:0>' },
                { op: 'restoreCommit', id: '<new:0>', force: true },
                { op: 'findDocs', collection: 'notes', query: {} },
                { op: 'status' },
                { op: 'log' },
                { op: 'restoreCommit', id: id[3] },
                { op: 'restoreCommit' }
            ]
        ]
        for (const requests of steps) await pair.step(requests)
        expect(pair.native.aliases.size).toBe(2)
        expectNoMismatch()
    } finally {
        await pair.close()
    }
})

test('history a native engine wrote reads the same in the browser', async ({ fylo }) => {
    const id = mintIds(3)
    const nativeDir = nativeRoot('parity-history-seed')
    const opfsRoot = browserRoot('parity-history-seed')
    try {
        expectOk(
            nativeExec(nativeDir, [
                { op: 'createCollection', collection: 'notes' },
                { op: 'putData', collection: 'notes', id: id[0], data: { text: 'first' } },
                { op: 'commit', message: 'first' },
                { op: 'checkout', branch: 'feature', create: true },
                { op: 'putData', collection: 'notes', id: id[1], data: { text: 'feature' } },
                { op: 'commit', message: 'feature work' },
                { op: 'checkout', branch: 'main' },
                { op: 'putData', collection: 'notes', id: id[2], data: { text: 'uncommitted' } }
            ]),
            'native history'
        )
        const reads = [
            { op: 'log' },
            { op: 'status' },
            { op: 'branch' },
            { op: 'diff' },
            { op: 'findDocs', collection: 'notes', query: {} }
        ]
        const expected = nativeExec(nativeDir, reads)
        await fylo.seed(opfsRoot, readTree(nativeDir))
        const answered = await fylo.once(opfsRoot, reads)
        reads.forEach((request, index) => {
            expect
                .soft(rootView(answered[index], opfsRoot), JSON.stringify(request))
                .toEqual(rootView(expected[index], nativeDir))
        })
        // `log` follows the checked-out branch.
        expect(expected[0].result.map((commit) => commit.message)).toEqual(['first'])
        expect(expected[2].result.branches.map((branch) => branch.name)).toEqual([
            'feature',
            'main'
        ])
        expectNoMismatch()
    } finally {
        await fylo.remove(opfsRoot)
        rmSync(nativeDir, { recursive: true, force: true })
    }
})

test('schema validation in the browser refuses exactly what native refuses', async ({ fylo }) => {
    // The browser has no regex compiler (ADR 0017 section 4): native pushes
    // the schemas, both engines then get the same root and the same writes.
    const seedDir = nativeRoot('parity-schema-seed')
    const schemaDir = nativeRoot('parity-schema-folder')
    const author = (collection, chex) => {
        mkdirSync(join(schemaDir, collection, 'history'), { recursive: true })
        writeFileSync(
            join(schemaDir, collection, 'manifest.json'),
            JSON.stringify({ current: 'v1', versions: [{ v: 'v1' }] })
        )
        writeFileSync(
            join(schemaDir, collection, 'history', 'v1.schema.json'),
            JSON.stringify(chex)
        )
    }
    author('people', { name: '^[A-Z]\\w+$', 'age?': '^[0-9]+$', city: '(?i)^[a-zé ]+$' })
    // A Unicode word boundary has no DFA form, so this schema is not portable.
    author('words', { word: '\\bword\\b' })
    // Never pushed: it exists only in the folder, which the browser cannot read.
    author('legacy', { name: '^[a-z]+$' })
    const setup = nativeExec(seedDir, [
        { op: 'createCollection', collection: 'people' },
        { op: 'createCollection', collection: 'words' },
        { op: 'schemaPush', collection: 'people', schemaDir },
        { op: 'schemaPush', collection: 'words', schemaDir },
        { op: 'createCollection', collection: 'legacy' },
        ...['people', 'words', 'legacy'].map((collection) => ({
            op: 'schemaMode',
            collection,
            mode: 'strict'
        }))
    ])
    expectOk(setup, 'native schema setup')
    for (const [collection, portable] of [
        ['people', true],
        ['words', false]
    ]) {
        const record = JSON.parse(
            readFileSync(join(seedDir, '.fylo-schemas', collection, '1.json'), 'utf8')
        )
        expect(record.portable, collection).toBe(portable)
    }

    const nativeDir = nativeRoot('parity-schema')
    cpSync(seedDir, nativeDir, { recursive: true })
    const opfsRoot = browserRoot('parity-schema')
    await fylo.seed(opfsRoot, readTree(seedDir))
    const pair = await Pair.open(fylo, 'parity-schema', { nativeDir, opfsRoot })
    const invalid = { name: 'ada', city: 'Zürich' }
    try {
        // `words` and `legacy` are where the engines part: native compiles the
        // non-portable regex (and has no folder for `legacy` either in this
        // session), the browser must fail closed instead of guessing.
        const unavailable = new Set()
        const differ = (index, native, browser) => {
            if (!unavailable.has(index)) return false
            expect(browser.ok, `frame ${index} must fail closed`).toBe(false)
            expect(browser.error.code).toBe('ESCHEMAUNAVAILABLE')
            return true
        }
        const requests = [
            { op: 'schemaMode', collection: 'people' },
            { op: 'schemaCurrent', collection: 'people' },
            {
                op: 'putData',
                collection: 'people',
                data: { name: 'Ada', age: 36, city: 'Montréal' }
            },
            { op: 'putData', collection: 'people', data: invalid },
            { op: 'putData', collection: 'people', data: { name: 'Ada', extra: 1, city: 'x' } },
            { op: 'putData', collection: 'people', data: { city: 'x' } },
            { op: 'putData', collection: 'people', data: { name: 'Ada', age: 'old', city: 'x' } },
            { op: 'putData', collection: 'people', data: { name: 'Éa', city: 'x' } },
            {
                op: 'executeSQL',
                sql: "INSERT INTO people (name, city) VALUES ('grace', 'Paris')"
            },
            {
                op: 'executeSQL',
                sql: "INSERT INTO people (name, city) VALUES ('Grace', 'Paris')"
            },
            {
                op: 'schemaValidate',
                collection: 'people',
                document: { name: 'Grace', city: 'Été' }
            },
            { op: 'schemaValidate', collection: 'people', document: invalid },
            { op: 'patchDoc', collection: 'people', id: '<new:0>', newDoc: { name: 'lower' } },
            { op: 'patchDoc', collection: 'people', id: '<new:0>', newDoc: { age: 37 } },
            {
                op: 'executeSQL',
                sql: "UPDATE people SET city = '123' WHERE name = 'Ada'"
            },
            {
                op: 'batchPutData',
                collection: 'people',
                // Refused at its first entry: a batch is not atomic, and the
                // identifiers of a partial batch never reach the caller.
                batch: [invalid, { name: 'Bea', city: 'x' }]
            },
            { op: 'schemaMode', collection: 'people', mode: 'warn' },
            { op: 'putData', collection: 'people', data: invalid },
            { op: 'schemaMode', collection: 'people' },
            { op: 'schemaMode', collection: 'people', mode: 'off' },
            { op: 'putData', collection: 'people', data: { anything: true } },
            { op: 'schemaMode', collection: 'people', mode: 'loud' },
            { op: 'schemaMode', collection: 'people', mode: 'strict' },
            { op: 'schemaCheck', collection: 'people' },
            { op: 'findDocs', collection: 'people', query: {} },
            { op: 'inspectCollection', collection: 'people' },
            { op: 'putData', collection: 'words', data: { word: 'a word' } },
            { op: 'schemaMode', collection: 'words', mode: 'warn' },
            { op: 'putData', collection: 'words', data: { word: 'a word' } },
            { op: 'schemaMode', collection: 'words', mode: 'off' },
            { op: 'putData', collection: 'words', data: { word: 'a word' } },
            { op: 'putData', collection: 'legacy', data: { name: 'ok' } }
        ]
        for (const [index, request] of requests.entries()) {
            if (request.op === 'putData' && request.collection === 'words') {
                const mode = requests
                    .slice(0, index)
                    .findLast(
                        (earlier) => earlier.op === 'schemaMode' && earlier.collection === 'words'
                    )?.mode
                if (mode !== 'off') unavailable.add(index)
            }
            if (request.collection === 'legacy') unavailable.add(index)
        }
        const { native, browser } = await pair.step(requests, { differ })
        expect(native[3].error?.code).toBe('ESCHEMA')
        expect(browser[3].error).toEqual(native[3].error)
        expect(browser[2].ok, JSON.stringify(browser[2].error)).toBe(true)
        expect(browser[17].ok, 'warn mode must store the document').toBe(true)
        expect(browser[20].ok, 'off mode must store the document').toBe(true)
        expect(browser.at(-2).ok, 'off mode must not consult the schema').toBe(true)
        expectNoMismatch()
    } finally {
        await pair.close()
        for (const dir of [seedDir, schemaDir, nativeDir]) {
            rmSync(dir, { recursive: true, force: true })
        }
    }
})

test('a browser-written root reads the same natively, and comes back', async ({ fylo }) => {
    const id = mintIds(12)
    const opfsRoot = browserRoot('parity-interop')
    const write = [
        { op: 'createCollection', collection: 'people' },
        { op: 'createCollection', collection: 'orders' },
        ...[0, 1, 2, 3, 4].map((index) => ({
            op: 'putData',
            collection: 'people',
            id: id[index],
            data: { ...row(index), name: `person-${index}` },
            ...(index % 2 === 0 ? { meta: { source: 'browser', index } } : {})
        })),
        { op: 'putData', collection: 'orders', id: id[5], data: { user: 'person-1', total: 9 } },
        { op: 'patchDoc', collection: 'people', id: id[1], newDoc: { group: 'patched' } },
        { op: 'setMeta', collection: 'people', id: id[3], meta: { flagged: true } },
        { op: 'delDoc', collection: 'people', id: id[4] }
    ]
    const reads = [
        { op: 'findDocs', collection: 'people', query: {} },
        { op: 'findDocs', collection: 'people', query: { $ops: [{ n: { $gte: 2 } }] } },
        { op: 'findDocs', collection: 'people', query: { $ops: [{ 'nested.a.b': { $eq: 1 } }] } },
        { op: 'findDeletedDocs', collection: 'people', query: {} },
        { op: 'findDocs', collection: 'orders', query: {} },
        { op: 'executeSQL', sql: 'SELECT name, n FROM people WHERE n >= 1 AND flag = false' },
        {
            op: 'joinDocs',
            join: {
                $leftCollection: 'people',
                $rightCollection: 'orders',
                $mode: 'inner',
                $on: { name: { $eq: 'user' } }
            }
        },
        { op: 'inspectCollection', collection: 'people' },
        { op: 'inspectCollection', collection: 'orders' }
    ]
    const nativeDir = nativeRoot('parity-interop')
    try {
        const written = await fylo.once(opfsRoot, write)
        expectOk(written, 'browser writes')
        const browserReads = await fylo.once(opfsRoot, reads)
        expectOk(browserReads, 'browser reads')

        const files = await fylo.dump(opfsRoot)
        // Developer metadata has no xattrs to live in here: the bridge keeps
        // one manifest at the root, and the engine never lists it.
        const attributes = files['.fylo-attributes.json']
        test.info().annotations.push({
            type: 'browser attributes',
            description:
                attributes === undefined
                    ? 'no .fylo-attributes.json for a document-only root'
                    : Buffer.from(attributes, 'base64').toString('utf8').slice(0, 400)
        })
        writeTree(nativeDir, files)
        const nativeReads = nativeExec(nativeDir, reads)
        reads.forEach((request, index) => {
            expect
                .soft(
                    rootView(nativeReads[index], nativeDir),
                    `native reading the browser root: ${JSON.stringify(request)}`
                )
                .toEqual(rootView(browserReads[index], opfsRoot))
        })
        expectNoMismatch()

        // Native writes on top, and the root goes back into OPFS.
        const more = nativeExec(nativeDir, [
            { op: 'putData', collection: 'people', id: id[6], data: { name: 'native', n: 6 } },
            { op: 'patchDoc', collection: 'people', id: id[0], newDoc: { name: 'renamed' } },
            { op: 'setMeta', collection: 'people', id: id[2], meta: { source: 'native' } },
            { op: 'restoreDoc', collection: 'people', id: id[4] },
            { op: 'delDoc', collection: 'people', id: id[3] },
            { op: 'executeSQL', sql: "INSERT INTO orders (user, total) VALUES ('native', 1)" }
        ])
        expectOk(more, 'native writes')
        const finalReads = reads
        const expected = nativeExec(nativeDir, finalReads)
        const backRoot = browserRoot('parity-interop-back')
        await fylo.seed(backRoot, readTree(nativeDir))
        const back = await fylo.once(backRoot, finalReads)
        finalReads.forEach((request, index) => {
            expect
                .soft(
                    rootView(back[index], backRoot),
                    `browser reading it back: ${JSON.stringify(request)}`
                )
                .toEqual(rootView(expected[index], nativeDir))
        })
        expect(Object.keys(expected[0].result)).toHaveLength(5)
        expectNoMismatch()
        await fylo.remove(backRoot)
    } finally {
        await fylo.remove(opfsRoot)
        rmSync(nativeDir, { recursive: true, force: true })
    }
})

test('a native-built root seeded into OPFS reads the same in the browser', async ({ fylo }) => {
    const id = mintIds(50)
    const nativeDir = nativeRoot('parity-seeded')
    const opfsRoot = browserRoot('parity-seeded')
    try {
        const built = nativeExec(nativeDir, [
            { op: 'createCollection', collection: 'rows' },
            { op: 'createCollection', collection: 'tags' },
            ...id.slice(0, 40).map((target, index) => ({
                op: 'putData',
                collection: 'rows',
                id: target,
                data: row(index)
            })),
            ...id.slice(40).map((target, index) => ({
                op: 'putData',
                collection: 'tags',
                id: target,
                data: { tag: `t${index % 4}`, weight: index }
            })),
            { op: 'commit', message: 'base' },
            ...id.slice(0, 10).map((target) => ({ op: 'delDoc', collection: 'rows', id: target })),
            { op: 'patchDoc', collection: 'rows', id: id[20], newDoc: { group: 'edited' } }
        ])
        expectOk(built, 'native build')
        const reads = [
            { op: 'findDocs', collection: 'rows', query: {} },
            ...[
                { $ops: [{ group: { $eq: 'red' } }] },
                { $ops: [{ n: { $gte: 30 } }] },
                { $ops: [{ 'nested.a.b': { $eq: 3 } }] },
                { $ops: [{ word: { $like: '%é%' } }] },
                { $ops: [{ tags: { $contains: 't1' } }], $limit: 4 },
                { $onlyIds: true, $ops: [{ maybe: { $eq: null } }] }
            ].map((query) => ({ op: 'findDocs', collection: 'rows', query })),
            { op: 'findDeletedDocs', collection: 'rows', query: {} },
            { op: 'findDocs', collection: 'tags', query: { $ops: [{ tag: { $eq: 't1' } }] } },
            ...id.slice(0, 20).map((target) => ({ op: 'getDoc', collection: 'rows', id: target })),
            { op: 'executeSQL', sql: "SELECT n, s FROM rows WHERE word = 'gamma' OR n < 3" },
            { op: 'inspectCollection', collection: 'rows' },
            { op: 'inspectCollection', collection: 'tags' }
        ]
        const expected = nativeExec(nativeDir, reads)
        await fylo.seed(opfsRoot, readTree(nativeDir))
        const answered = await fylo.once(opfsRoot, reads)
        reads.forEach((request, index) => {
            expect
                .soft(rootView(answered[index], opfsRoot), JSON.stringify(request).slice(0, 160))
                .toEqual(rootView(expected[index], nativeDir))
        })
        expect(Object.keys(expected[0].result)).toHaveLength(30)
        expectNoMismatch()
    } finally {
        await fylo.remove(opfsRoot)
        rmSync(nativeDir, { recursive: true, force: true })
    }
})

test('a plain copy between the engines drops attributes, and file reads fail closed', async ({
    fylo
}) => {
    // Developer metadata and a raw file's logical key, checksum and access
    // are attributes. Natively they are xattrs (or, on EFS, a sidecar bound to
    // the record's identity); the OPFS bridge keeps one `.fylo-attributes.json`
    // at the root. Neither survives a plain file copy to the other engine, as
    // xattrs do not survive `cp` without `-a`: document data moves, attributes
    // do not. What must hold is that nothing reads back wrong. A file record
    // without its attributes is refused, never served with a guessed key or
    // checksum, and documents read exactly as before minus their metadata.
    const id = mintIds(4)
    const opfsRoot = browserRoot('parity-attrs')
    const nativeDir = nativeRoot('parity-attrs')
    const sourceDir = nativeRoot('parity-attrs-source')
    const reads = (fileId, docId) => [
        { op: 'getDoc', collection: 'files', id: fileId },
        { op: 'getFileData', collection: 'files', id: fileId },
        { op: 'findDocs', collection: 'files', query: {} },
        { op: 'getMeta', collection: 'people', id: docId },
        { op: 'getDoc', collection: 'people', id: docId }
    ]
    const expectMoved = (label, moved, before, docId, source) => {
        for (const [index, frame] of moved.slice(0, 3).entries()) {
            expect(frame.ok, `${label}: file read ${index} must be refused`).toBe(false)
            expect(frame.error.code, `${label}: file read ${index}`).toBe('ENATIVE_METADATA')
        }
        expect(moved[3].ok, `${label}: getMeta`).toBe(true)
        expect(moved[3].result?.source, `${label}: metadata does not travel`).toBeUndefined()
        expect(moved[4].ok, `${label}: getDoc`).toBe(true)
        expect(moved[4].result, `${label}: document data travels`).toEqual(before[4].result)
        expect(before[3].result.source, `${label}: metadata before the move`).toBe(source)
    }
    try {
        // Browser to native.
        await fylo.seed(opfsRoot, {
            'source.bin': Buffer.from('hello bucket').toString('base64')
        })
        const writes = [
            { op: 'createCollection', collection: 'files', kind: 'file' },
            {
                op: 'putData',
                collection: 'files',
                id: id[0],
                file: { path: `${opfsRoot}/source.bin` },
                meta: { reviewed: true }
            },
            { op: 'createCollection', collection: 'people' },
            {
                op: 'putData',
                collection: 'people',
                id: id[1],
                data: { name: 'Ada' },
                meta: { source: 'browser' }
            }
        ]
        const written = await fylo.once(opfsRoot, [...writes, ...reads(id[0], id[1])])
        expectOk(written, 'browser writes')
        const browserAnswers = written.slice(writes.length)
        expect(browserAnswers[0].result[id[0]]).toMatchObject({
            key: '/source.bin',
            meta: { reviewed: true }
        })
        const files = await fylo.dump(opfsRoot)
        const manifest = JSON.parse(
            Buffer.from(files['.fylo-attributes.json'] ?? '', 'base64').toString('utf8') || '{}'
        )
        expect(Object.keys(manifest).length, 'one manifest entry per attributed record').toBe(2)
        writeTree(nativeDir, files)
        expectMoved(
            'native reading the browser root',
            nativeExec(nativeDir, reads(id[0], id[1])),
            browserAnswers,
            id[1],
            'browser'
        )

        // Native to browser.
        rmSync(nativeDir, { recursive: true, force: true })
        mkdirSync(nativeDir, { recursive: true })
        writeFileSync(join(sourceDir, 'source.bin'), 'hello bucket')
        const nativeAnswers = nativeExec(nativeDir, [
            { op: 'createCollection', collection: 'files', kind: 'file' },
            {
                op: 'putData',
                collection: 'files',
                id: id[2],
                file: { path: join(sourceDir, 'source.bin') },
                meta: { reviewed: true }
            },
            { op: 'createCollection', collection: 'people' },
            {
                op: 'putData',
                collection: 'people',
                id: id[3],
                data: { name: 'Ada' },
                meta: { source: 'native' }
            },
            ...reads(id[2], id[3])
        ]).slice(4)
        expectOk(nativeAnswers, 'native attributes')
        const backRoot = browserRoot('parity-attrs-back')
        await fylo.seed(backRoot, readTree(nativeDir))
        expectMoved(
            'browser reading the native root',
            await fylo.once(backRoot, reads(id[2], id[3])),
            nativeAnswers,
            id[3],
            'native'
        )
        await fylo.remove(backRoot)
    } finally {
        await fylo.remove(opfsRoot)
        for (const dir of [nativeDir, sourceDir]) rmSync(dir, { recursive: true, force: true })
    }
})

function ada() {
    return {
        name: 'Ada',
        age: 36,
        tags: ['math', 'poetry'],
        profile: { city: 'London', langs: ['en', 'fr'] },
        note: 'déjà vu — 日本語 🚀',
        empty: '',
        nothing: null,
        big: 9007199254740991,
        float: 0.1
    }
}

function patch() {
    return { age: 37, profile: { city: 'Paris' }, added: [1, 'two'], nothing: 'now' }
}

const GROUPS = ['red', 'green', 'blue']
const WORDS = [
    'alpha',
    'Beta',
    'gamma',
    'déjà vu',
    'naïve',
    '日本語',
    'emoji 🚀',
    '',
    "O'Brien",
    '100%',
    'under_score',
    'ALPHA'
]

/** A deterministic document with a bit of every shape a query meets. */
function row(index) {
    return {
        n: index,
        s: String(index).padStart(3, '0'),
        group: GROUPS[index % 3],
        word: WORDS[index % WORDS.length],
        nested: { a: { b: index % 7 }, label: `L${index % 5}` },
        tags: [GROUPS[index % 3], `t${index % 4}`],
        flag: index % 2 === 0,
        score: index % 10 === 0 ? null : ((index * 37) % 101) / 4,
        big: index % 50 === 0 ? 2 ** 53 + index : index * 1e12,
        ...(index % 11 === 0 ? {} : { maybe: index % 3 === 0 ? null : index })
    }
}

const MISSING = Symbol('missing')
// Values the loose comparisons have to agree on: numeric strings, booleans,
// null against missing, single-element arrays, and numbers at the edges.
const EDGE_VALUES = [
    '01',
    1,
    '1',
    1.5,
    '1.5',
    true,
    false,
    0,
    '0',
    '',
    ' ',
    null,
    MISSING,
    ' 1 ',
    '1e0',
    '0x1',
    '0b1',
    [1],
    [],
    [1, 2],
    ['1'],
    { x: 1 },
    9007199254740991,
    -9007199254740991,
    1e21,
    1e-7,
    -1,
    '-1',
    'Infinity',
    'NaN',
    'abc',
    'ÄBC',
    'äbc',
    '🚀',
    'true',
    'null'
]

const QUERIES = [
    {},
    ...[1, '1', '01', 1.5, '1.5', true, false, 0, '0', '', null, 'abc', 'äbc', '🚀', -1, 1e21].map(
        (value) => ({ $ops: [{ v: { $eq: value } }] })
    ),
    ...[1, null, '', true].map((value) => ({ $ops: [{ v: { $ne: value } }] })),
    { $ops: [{ v: { $gt: 0 } }] },
    { $ops: [{ v: { $gte: 1 } }] },
    { $ops: [{ v: { $lt: 1 } }] },
    { $ops: [{ v: { $lte: 0 } }] },
    { $ops: [{ v: { $gt: '0' } }] },
    { $ops: [{ v: { $lt: 'abc' } }] },
    { $ops: [{ v: { $gt: null } }] },
    { $ops: [{ v: { $gte: -9007199254740991, $lte: 9007199254740991 } }] },
    { $ops: [{ v: { $like: '%1%' } }] },
    { $ops: [{ v: { $like: '_' } }] },
    { $ops: [{ v: { $like: '' } }] },
    { $ops: [{ v: { $like: '%' } }] },
    { $ops: [{ v: { $contains: 1 } }] },
    { $ops: [{ v: { $contains: '1' } }] },
    { $ops: [{ group: { $eq: 'red' } }] },
    { $ops: [{ group: { $ne: 'red' } }] },
    { $ops: [{ group: { $eq: 'RED' } }] },
    { $ops: [{ n: { $gte: 10, $lt: 20 } }] },
    { $ops: [{ n: { $gt: 20 } }] },
    { $ops: [{ n: { $lte: '5' } }] },
    { $ops: [{ s: { $eq: 7 } }] },
    { $ops: [{ s: { $eq: '007' } }] },
    { $ops: [{ s: { $eq: '7' } }] },
    { $ops: [{ s: { $gt: '019' } }] },
    { $ops: [{ s: { $like: '00_' } }] },
    { $ops: [{ word: { $like: 'déjà%' } }] },
    { $ops: [{ word: { $like: '%🚀' } }] },
    { $ops: [{ word: { $like: '%語' } }] },
    { $ops: [{ word: { $like: '100%' } }] },
    { $ops: [{ word: { $like: 'under_score' } }] },
    { $ops: [{ word: { $like: 'alpha' } }] },
    { $ops: [{ word: { $eq: '' } }] },
    { $ops: [{ word: { $eq: "O'Brien" } }] },
    { $ops: [{ word: { $eq: 'naïve' } }] },
    { $ops: [{ tags: { $contains: 'red' } }] },
    { $ops: [{ tags: { $contains: 't1' } }] },
    { $ops: [{ tags: { $contains: 'nope' } }] },
    { $ops: [{ tags: { $eq: 'red' } }] },
    { $ops: [{ 'nested.a.b': { $eq: 3 } }] },
    { $ops: [{ 'nested/a/b': { $eq: 3 } }] },
    { $ops: [{ 'nested.a.b': { $gte: 5 } }] },
    { $ops: [{ 'nested/a/b': { $lt: '2' } }] },
    { $ops: [{ 'nested.label': { $like: 'L_' } }] },
    { $ops: [{ 'nested.label': { $eq: 'L3' } }] },
    { $ops: [{ 'nested.zz.b': { $eq: null } }] },
    { $ops: [{ 'nested.a': { $eq: null } }] },
    { $ops: [{ 'a.b': { $eq: 'nested' } }] },
    { $ops: [{ 'a.b': { $eq: 'dotted key' } }] },
    { $ops: [{ clé: { $eq: 'x' } }] },
    { $ops: [{ maybe: { $eq: null } }] },
    { $ops: [{ maybe: { $ne: null } }] },
    { $ops: [{ score: { $eq: null } }] },
    { $ops: [{ score: { $gt: 20.5 } }] },
    { $ops: [{ flag: { $eq: true } }] },
    { $ops: [{ flag: { $eq: 1 } }] },
    { $ops: [{ flag: { $eq: 'true' } }] },
    { $ops: [{ big: { $gt: 1e15 } }] },
    { $ops: [{ big: { $eq: 2 ** 53 } }] },
    { $ops: [{ big: { $gte: 2 ** 53 } }] },
    { $ops: [{ group: { $eq: 'red' } }, { group: { $eq: 'blue' } }] },
    { $ops: [{ group: { $eq: 'red' }, flag: { $eq: true } }] },
    { $ops: [{ group: { $eq: 'red' }, n: { $lt: 30 } }, { 'nested.a.b': { $eq: 6 } }] },
    { $ops: [{ group: { $foo: 1 } }] },
    { $ops: [{ group: 'red' }] },
    { $ops: [] },
    { $ops: [{ group: { $eq: 'green' } }], $limit: 5 },
    { $ops: [{ group: { $eq: 'green' } }], $limit: 0 },
    { $onlyIds: true, $ops: [{ n: { $lt: 10 } }] },
    { $onlyIds: true, $limit: 3 },
    { $created: { $gt: 0 } },
    { $updated: { $gte: 0 }, $ops: [{ n: { $lt: 3 } }] },
    { $created: { $lt: 1 } }
]

function count(frames, queries, operation) {
    const index = queries.findIndex(
        (query) => JSON.stringify(query) === JSON.stringify({ $ops: [operation] })
    )
    return Object.keys(frames[index].result).length
}
