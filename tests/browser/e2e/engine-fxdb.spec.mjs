// `.fxdb` round trips between fylo-browser.wasm and the native engine.
//
// The native engine writes a database with zstd frames and trained
// dictionaries; the browser must read every document of it exactly as native
// does, edit a working copy with its own LZ4 frames, and hand back a file that
// native reads, verifies and keeps writing, and so on around again. Damaged
// files and a Worker killed mid-batch must end the same way they end natively:
// reported, never a trap, a hang or a file native refuses.
//
// The page is served without COOP or COEP, as a static host serves it, so the
// engine runs through the single-file Worker (`dist-web/fxdb-worker.js`).

import { expect, test } from './browser-test.mjs'
import { comparable, expectOk, mintIds, nativeExec } from './engine-harness.mjs'
import { createHash } from 'node:crypto'
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { gunzipSync } from 'node:zlib'

const PLAIN_ORIGIN = 'http://127.0.0.1:4174'
const FIXTURES = 'target/browser-fixtures/engine-fxdb'

// A collection trains its zstd dictionary at its first checkpoint once its
// records reach 1 MiB (container_codec.rs, DICTIONARY_TRAINING_BYTES), and a
// checkpoint comes every 4,096 records (segment_checkpoint.rs). Each of these
// collections passes both, so its file holds frames written before and after
// its dictionary.
const DICTIONARY_RECORDS = 4400
const NOTES = 300
const TEAMS = ['red', 'blue', 'green', 'gold']
const CITIES = ['Lagos', 'Lima', 'Oslo', 'Pune', 'Kyiv', 'Quito', 'Hanoi', 'Perth', 'Turin']
const STATUSES = ['open', 'paid', 'shipped', 'returned']

// A `.fxdb` record frame: sync marker, length, CRC-32C, then the stored body,
// whose first byte holds the codec in its low four bits and 0x10 when a
// dictionary id follows (segments/container_frames.rs, container_record.rs).
const SYNC_MARKER = 0x015259f5
const CODECS = ['none', 'zstd', 'lz4']

/** Paths of this worker's fixtures, on disk and as the page fetches them. */
function fixturePaths(worker) {
    const directory = join(FIXTURES, worker)
    return {
        directory,
        disk: (name) => join(directory, name),
        url: (name) => `/${FIXTURES}/${worker}/${name}`
    }
}

function user(index) {
    return {
        name: `user-${index}`,
        team: TEAMS[index % TEAMS.length],
        age: 18 + ((index * 7) % 60),
        active: index % 3 === 0,
        address: {
            city: CITIES[index % CITIES.length],
            zip: String(10_000 + ((index * 13) % 89_999))
        },
        tags: [`t${index % 7}`, `k${index % 11}`],
        bio: `User ${index} joined the ${TEAMS[index % TEAMS.length]} team in ${CITIES[index % CITIES.length]}; ${'enjoys long reads and short queries. '.repeat(4)}`
    }
}

function order(index) {
    return {
        order: index,
        customer: `user-${(index * 31) % DICTIONARY_RECORDS}`,
        status: STATUSES[index % STATUSES.length],
        total: Math.round(((index * 37.5) % 1000) * 100) / 100,
        // Arrays of objects are refused (EARRAYOFOBJECTS), so lines are flat.
        skus: Array.from({ length: 1 + (index % 3) }, (_, line) => `SKU-${(index + line) % 97}`),
        shipping: { method: index % 2 ? 'courier' : 'post', days: 1 + (index % 6) },
        note: `Order ${index} for user-${(index * 31) % DICTIONARY_RECORDS}: ${'deliver to the side door please. '.repeat(3)}`
    }
}

function note(index) {
    return {
        title: `note ${index}`,
        body: `Note ${index}. ${'Remember the milk. '.repeat(1 + (index % 4))}`
    }
}

/** One collection's documents in batches the protocol accepts. */
function batches(collection, count, make, size = 400) {
    const requests = []
    for (let start = 0; start < count; start += size) {
        requests.push({
            op: 'batchPutData',
            collection,
            batch: Array.from({ length: Math.min(size, count - start) }, (_, offset) =>
                make(start + offset)
            )
        })
    }
    return requests
}

/** The identifiers native finds in each collection. */
function nativeIds(path, collections) {
    const frames = expectOk(
        nativeExec(
            path,
            collections.map((collection) => ({
                op: 'findDocs',
                collection,
                query: { $onlyIds: true }
            }))
        ),
        'native ids'
    )
    return Object.fromEntries(
        collections.map((collection, index) => [collection, frames[index].result])
    )
}

/**
 * The reads parity checks make: every document of every collection through
 * `findDocs`, and point reads of `points` documents per collection spread
 * evenly over its list, the newest writes included.
 *
 * Point reads are sampled because each machine request opens the database
 * afresh, about 20 ms in the browser over a picked file and 70 ms over ranged
 * requests, so reading 9,000 documents one by one would take minutes.
 */
function readsOf(ids, points = 60) {
    const requests = [{ op: 'inspectDatabase' }, { op: 'verifyDatabase' }]
    for (const collection of Object.keys(ids)) {
        requests.push({ op: 'findDocs', collection, query: {} })
    }
    for (const [collection, list] of Object.entries(ids)) {
        for (const id of spread(list, points)) requests.push({ op: 'getDoc', collection, id })
    }
    return requests
}

/** `count` entries of `list`, evenly spaced, the first and the last among them. */
function spread(list, count) {
    if (list.length <= count) return list
    return Array.from(
        { length: count },
        (_, index) => list[Math.round((index * (list.length - 1)) / (count - 1))]
    )
}

const QUERIES = [
    { op: 'findDocs', collection: 'users', query: { $ops: [{ team: { $eq: 'red' } }] } },
    {
        op: 'findDocs',
        collection: 'users',
        query: { $ops: [{ age: { $gte: 60 } }], $onlyIds: true }
    },
    { op: 'findDocs', collection: 'users', query: { $ops: [{ name: { $like: 'user-12%' } }] } },
    {
        op: 'findDocs',
        collection: 'users',
        query: { $ops: [{ team: { $eq: 'blue' } }, { active: { $eq: true } }], $limit: 25 }
    },
    {
        op: 'findDocs',
        collection: 'orders',
        query: { $ops: [{ status: { $ne: 'paid' } }], $onlyIds: true }
    },
    { op: 'findDocs', collection: 'orders', query: { $ops: [{ total: { $lt: 100 } }] } },
    { op: 'findDocs', collection: 'notes', query: { $ops: [{ title: { $eq: 'note 7' } }] } },
    { op: 'executeSQL', sql: "SELECT * FROM orders WHERE status = 'returned'" },
    { op: 'executeSQL', sql: 'SELECT * FROM users WHERE age > 70' },
    { op: 'inspectCollection', collection: 'users' },
    { op: 'inspectCollection', collection: 'notes' }
]

/** Assert two frame lists match frame for frame, naming the first that does not. */
function expectSameFrames(actual, expected, label, shape = comparable) {
    const got = actual.map(shape)
    const want = expected.map(shape)
    const first = got.findIndex((frame, index) => !isDeepStrictEqual(frame, want[index]))
    expect(
        first,
        first === -1
            ? ''
            : `${label}: frame ${first} differs\nbrowser: ${JSON.stringify(got[first])?.slice(0, 1200)}\nnative:  ${JSON.stringify(want[first])?.slice(0, 1200)}`
    ).toBe(-1)
    expect(got.length, `${label}: frame count`).toBe(want.length)
}

/** What a damaged file must agree on: success and its answer, or the error code. */
function outcome(frame) {
    return frame.ok
        ? { op: frame.op, ok: true, result: comparable(frame).result }
        : { op: frame.op, ok: false, code: frame.error?.code }
}

/**
 * Damage behind a valid checksum is past what the frame's CRC-32C promises to
 * catch, and a zstd body carries no checksum of its own. libzstd can decode
 * such a body to bytes of the right length that then fail as a corrupt commit
 * manifest (ENATIVE_METADATA), where ruzstd refuses the body itself
 * (ENATIVE_DOCUMENT). Both must refuse exactly the same requests.
 */
const DECODER_DAMAGE = new Set(['damaged zstd payload'])

function refusal(frame) {
    return frame.ok ? outcome(frame) : { op: frame.op, ok: false }
}

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex')

/** The record frames of every segment extent, with their codec. */
function recordFrames(bytes) {
    const extent = 2 ** bytes[24]
    const frames = []
    for (let at = extent; at + extent <= bytes.length; at += extent) {
        if (bytes.toString('latin1', at, at + 8) !== 'FXEXTNT\0' || bytes[at + 10] !== 2) continue
        const owner = bytes.readUInt32LE(at + 12)
        let offset = at + 4096 + 32
        while (offset + 13 <= at + extent && bytes.readUInt32LE(offset) === SYNC_MARKER) {
            const length = bytes.readUInt32LE(offset + 4)
            if (length === 0 || offset + 12 + length > at + extent) break
            const header = bytes[offset + 12]
            frames.push({
                owner,
                offset,
                length,
                codec: CODECS[header & 0x0f] ?? `unknown-${header & 0x0f}`,
                dictionary: (header & 0x10) !== 0
            })
            offset += 12 + length
        }
    }
    return frames
}

/** Frames per collection and codec, `dict` marking those with a dictionary. */
function census(bytes, collections) {
    const names = Object.fromEntries(collections.map(({ id, name }) => [id, name]))
    const counts = {}
    for (const frame of recordFrames(bytes)) {
        const name = names[frame.owner] ?? `#${frame.owner}`
        const key = `${frame.codec}${frame.dictionary ? '+dict' : ''}`
        counts[name] ??= {}
        counts[name][key] = (counts[name][key] ?? 0) + 1
    }
    return counts
}

const CRC32C = new Uint32Array(256).map((_, value) => {
    let crc = value
    for (let bit = 0; bit < 8; bit++) crc = crc & 1 ? 0x82f63b78 ^ (crc >>> 1) : crc >>> 1
    return crc >>> 0
})

function crc32c(bytes) {
    let crc = 0xffffffff
    for (const byte of bytes) crc = CRC32C[(crc ^ byte) & 0xff] ^ (crc >>> 8)
    return (crc ^ 0xffffffff) >>> 0
}

/**
 * Damage the compressed bytes of `frame` and give it a checksum that matches,
 * so the frame passes every structural check and only the decoder can tell.
 */
function corruptPayload(bytes, frame, at) {
    const damaged = Buffer.from(bytes)
    const body = frame.offset + 12
    const payload = body + 1 + (frame.dictionary ? 4 : 0)
    const start = payload + Math.floor((frame.length - (payload - body)) * at)
    for (let index = start; index < Math.min(start + 8, body + frame.length); index++) {
        damaged[index] ^= 0xa5
    }
    damaged.writeUInt32LE(crc32c(damaged.subarray(body, body + frame.length)), frame.offset + 8)
    return damaged
}

/** The single-file harness page, driven from Node. */
class FxdbPage {
    static async open(page) {
        await page.goto(`${PLAIN_ORIGIN}/tests/browser/fixtures/fxdb-harness.html`, {
            waitUntil: 'domcontentloaded'
        })
        await expect(page.locator('[data-status="ready"]')).toBeVisible()
        const harness = new FxdbPage(page)
        expect((await harness.surface()).crossOriginIsolated).toBe(false)
        return harness
    }

    constructor(page) {
        this.page = page
    }

    surface() {
        return this.page.evaluate(() => globalThis.fxdb.surface())
    }

    /** Why OPFS working copies cannot run here, or ''. */
    opfs() {
        return this.page.evaluate(() => globalThis.fxdb.opfs())
    }

    /** Open a database and fail the test if the Worker refuses it. */
    async open(source) {
        const opened = await this.tryOpen(source)
        expect(opened.error, `open ${JSON.stringify(source)}: ${opened.error}`).toBeUndefined()
        return opened
    }

    tryOpen(source) {
        return this.page.evaluate((source) => globalThis.fxdb.open(source), source)
    }

    /** The raw answer: `{ frames }`, `{ error, code }` or `{ timedOut }`. */
    run(id, requests, timeoutMs = 300_000) {
        return this.page.evaluate(
            ([id, requests, timeoutMs]) => globalThis.fxdb.exec(id, requests, timeoutMs),
            [id, requests, timeoutMs]
        )
    }

    /** Run a batch that must come back whole: no trap, no hang, every frame answered. */
    async exec(id, requests, timeoutMs) {
        const answer = await this.run(id, requests, timeoutMs)
        expect(answer.timedOut, 'the engine did not answer in time').toBeUndefined()
        expect(answer.error, `the Worker failed: ${answer.error} (${answer.code})`).toBeUndefined()
        expect(answer.frames.length, 'every request answered').toBe(requests.length)
        return answer.frames
    }

    start(id, requests) {
        return this.page.evaluate(
            ([id, requests]) => globalThis.fxdb.start(id, requests),
            [id, requests]
        )
    }

    kill(id) {
        return this.page.evaluate((id) => globalThis.fxdb.kill(id), id)
    }

    close(id) {
        return this.page.evaluate((id) => globalThis.fxdb.close(id), id)
    }

    removeCopy(name) {
        return this.page.evaluate((name) => globalThis.fxdb.removeCopy(name), name)
    }

    /** The whole file back in Node, checked against the digest the page took. */
    async snapshot(id) {
        const shot = await this.page.evaluate((id) => globalThis.fxdb.snapshot(id), id)
        const bytes = gunzipSync(Buffer.from(shot.gzipBase64, 'base64'))
        expect(bytes.length).toBe(shot.size)
        expect(sha256(bytes)).toBe(shot.sha256)
        return bytes
    }
}

async function attach(testInfo, name, evidence) {
    await testInfo.attach(name, {
        body: `${JSON.stringify(evidence, null, 2)}\n`,
        contentType: 'application/json'
    })
}

/** Skip where working copies cannot exist (Playwright's Linux WebKit has no OPFS). */
async function requireOpfs(harness) {
    const reason = await harness.opfs()
    test.skip(reason !== '', reason)
}

let fixture

test.beforeAll(async ({}, workerInfo) => {
    test.setTimeout(600_000)
    const paths = fixturePaths(`${workerInfo.project.name}-${workerInfo.workerIndex}`)
    rmSync(paths.directory, { recursive: true, force: true })
    mkdirSync(paths.directory, { recursive: true })

    // The main database: two collections with trained dictionaries, one
    // without, every frame written by native zstd.
    const database = paths.disk('native.fxdb')
    expectOk(
        nativeExec(database, [
            { op: 'createCollection', collection: 'users' },
            { op: 'createCollection', collection: 'orders' },
            { op: 'createCollection', collection: 'notes' },
            ...batches('users', DICTIONARY_RECORDS, user),
            ...batches('orders', DICTIONARY_RECORDS, order),
            ...batches('notes', NOTES, note)
        ]),
        'native fixture'
    )
    const [inspected] = expectOk(nativeExec(database, [{ op: 'inspectDatabase' }]))
    const dictionaries = Object.fromEntries(
        inspected.result.collections.map((collection) => [collection.name, collection.dictionary])
    )
    // Without these the dictionary paths would go untested and the spec
    // would still pass.
    expect(dictionaries.users, 'users trained a dictionary').toBeTruthy()
    expect(dictionaries.orders, 'orders trained a dictionary').toBeTruthy()
    expect(dictionaries.notes, 'notes stays below the training threshold').toBeNull()
    expect(inspected.result.codecs).toEqual(['zstd'])

    // A small database for damage, whose frames are easy to find.
    const small = paths.disk('small.fxdb')
    const smallIds = mintIds(40)
    expectOk(
        nativeExec(small, [
            { op: 'createCollection', collection: 'notes' },
            ...smallIds.map((id, index) => ({
                op: 'putData',
                collection: 'notes',
                id,
                data: note(index)
            }))
        ]),
        'small fixture'
    )

    fixture = {
        paths,
        database,
        small,
        smallIds,
        ids: nativeIds(database, ['users', 'orders', 'notes']),
        collections: inspected.result.collections
    }
})

test('reads a native database, dictionaries and all, exactly as native does', async ({
    page
}, testInfo) => {
    // Firefox takes minutes for these reads on a loaded machine.
    test.setTimeout(1_200_000)
    const harness = await FxdbPage.open(page)
    // Ranged requests are slower per open, so they get fewer point reads.
    const reads = {
        file: [...readsOf(fixture.ids, 150), ...QUERIES],
        url: [...readsOf(fixture.ids, 20), ...QUERIES]
    }
    const native = {
        file: expectOk(nativeExec(fixture.database, reads.file), 'native reads'),
        url: expectOk(nativeExec(fixture.database, reads.url), 'native reads')
    }
    const before = sha256(readFileSync(fixture.database))
    const frames = recordFrames(readFileSync(fixture.database))
    const evidence = { census: census(readFileSync(fixture.database), fixture.collections) }
    // Records written before the checkpoint carry no dictionary, those after
    // carry one: the browser must decode both.
    expect(evidence.census.users['zstd+dict']).toBeGreaterThan(0)
    expect(evidence.census.users.zstd).toBeGreaterThan(0)
    expect(frames.some((frame) => frame.codec === 'lz4')).toBe(false)

    // A picked file, then the same file by ranged requests.
    for (const kind of ['file', 'url']) {
        const opened = await harness.open({ kind, url: fixture.paths.url('native.fxdb') })
        try {
            expect(opened.info.writable).toBe(false)
            const started = Date.now()
            const browser = await harness.exec(opened.id, reads[kind], 540_000)
            evidence[kind] = { ms: Date.now() - started, frames: browser.length }
            console.log(`fxdb reads over ${kind}: ${evidence[kind].ms} ms`)
            expectOk(browser, `browser reads (${kind})`)
            expectSameFrames(browser, native[kind], `reads over ${kind}`)
        } finally {
            await harness.close(opened.id)
        }
    }
    const documents = Object.values(fixture.ids).reduce((sum, list) => sum + list.length, 0)
    expect(documents).toBe(2 * DICTIONARY_RECORDS + NOTES)
    expect(sha256(readFileSync(fixture.database)), 'reading changed nothing').toBe(before)
    await attach(testInfo, 'fxdb-native-reads', evidence)
})

test('edits a working copy in LZ4 that native reads, verifies and keeps writing, round after round', async ({
    page
}, testInfo) => {
    test.setTimeout(900_000)
    const harness = await FxdbPage.open(page)
    await requireOpfs(harness)
    const { paths } = fixture
    const evidence = {}
    const ids = structuredClone(fixture.ids)
    const fresh = mintIds(1300)
    const take = (count) => fresh.splice(0, count)
    let copy

    // Round 1, browser: writes, patches and deletes in collections with and
    // without a dictionary, and in one it creates.
    const browserUsers = take(500)
    const browserNotes = take(150)
    const browserEvents = take(250)
    const patchedUsers = ids.users.filter((_, index) => index % 13 === 0)
    const deletedUsers = ids.users.filter((_, index) => index % 17 === 1)
    const patchedOrders = ids.orders.filter((_, index) => index % 29 === 0)
    const deletedOrders = ids.orders.filter((_, index) => index % 31 === 2)
    const round1 = [
        { op: 'createCollection', collection: 'events' },
        ...browserUsers.map((id, index) => ({
            op: 'putData',
            collection: 'users',
            id,
            data: { ...user(100_000 + index), writer: 'browser' }
        })),
        ...browserNotes.map((id, index) => ({
            op: 'putData',
            collection: 'notes',
            id,
            data: note(5000 + index)
        })),
        ...browserEvents.map((id, index) => ({
            op: 'putData',
            collection: 'events',
            id,
            data: {
                kind: index % 2 ? 'click' : 'view',
                at: index,
                detail: `event ${index} `.repeat(12)
            }
        })),
        ...patchedUsers.map((id) => ({
            op: 'patchDoc',
            collection: 'users',
            id,
            newDoc: { team: 'purple', patchedBy: 'browser' }
        })),
        ...deletedUsers.map((id) => ({ op: 'delDoc', collection: 'users', id })),
        ...patchedOrders.map((id) => ({
            op: 'patchDoc',
            collection: 'orders',
            id,
            newDoc: { status: 'cancelled' }
        })),
        ...deletedOrders.map((id) => ({ op: 'delDoc', collection: 'orders', id })),
        ...browserUsers.slice(0, 40).map((id) => ({
            op: 'patchDoc',
            collection: 'users',
            id,
            newDoc: { age: 99 }
        })),
        ...browserNotes.slice(0, 20).map((id) => ({ op: 'delDoc', collection: 'notes', id }))
    ]
    ids.users.push(...browserUsers)
    ids.notes.push(...browserNotes)
    ids.events = [...browserEvents]
    try {
        const opened = await harness.open({
            kind: 'copy',
            label: 'round-trip.fxdb',
            url: paths.url('native.fxdb')
        })
        copy = opened.name
        expect(opened.info.writable).toBe(true)
        let started = Date.now()
        expectOk(await harness.exec(opened.id, round1), 'browser round 1 writes')
        evidence.round1 = { writes: round1.length, ms: Date.now() - started }
        const [inspected] = expectOk(await harness.exec(opened.id, [{ op: 'inspectDatabase' }]))
        expect(inspected.result.codecs.sort()).toEqual(['lz4', 'zstd'])
        const reads = [...readsOf(ids), ...QUERIES]
        started = Date.now()
        const browserView = expectOk(await harness.exec(opened.id, reads), 'browser round 1 reads')
        evidence.round1.readMs = Date.now() - started
        const bytes = await harness.snapshot(opened.id)
        await harness.close(opened.id)
        writeFileSync(paths.disk('round1.fxdb'), bytes)

        // The browser's frames are LZ4, with the collection's dictionary
        // where it has one; what native wrote stays zstd.
        const collections = inspected.result.collections
        evidence.round1.census = census(bytes, collections)
        expect(evidence.round1.census.users['lz4+dict']).toBeGreaterThan(0)
        expect(evidence.round1.census.orders['lz4+dict']).toBeGreaterThan(0)
        expect(evidence.round1.census.events.lz4).toBeGreaterThan(0)
        expect(evidence.round1.census.events['lz4+dict']).toBeUndefined()
        expect(evidence.round1.census.users['zstd+dict']).toBeGreaterThan(0)

        // Round 2, native: the same answers, a clean verify, then more writes.
        const native = expectOk(
            nativeExec(paths.disk('round1.fxdb'), reads),
            'native round 2 reads'
        )
        expectSameFrames(native, browserView, 'native reading the browser file')
        const [verified, nativeInspect] = expectOk(
            nativeExec(paths.disk('round1.fxdb'), [
                { op: 'verifyDatabase' },
                { op: 'inspectDatabase' }
            ])
        )
        expect(verified.result).toMatchObject({ ok: true, problems: [] })
        expect(nativeInspect.result.codecs.sort()).toEqual(['lz4', 'zstd'])

        const nativeUsers = take(250)
        const nativeEvents = take(100)
        const round2 = [
            ...nativeUsers.map((id, index) => ({
                op: 'putData',
                collection: 'users',
                id,
                data: { ...user(200_000 + index), writer: 'native' }
            })),
            ...nativeEvents.map((id, index) => ({
                op: 'putData',
                collection: 'events',
                id,
                data: { kind: 'native', at: index, detail: `native event ${index} `.repeat(10) }
            })),
            ...browserUsers.slice(40, 140).map((id) => ({
                op: 'patchDoc',
                collection: 'users',
                id,
                newDoc: { patchedBy: 'native' }
            })),
            ...browserUsers
                .slice(140, 190)
                .map((id) => ({ op: 'delDoc', collection: 'users', id })),
            ...browserEvents.slice(0, 30).map((id) => ({ op: 'delDoc', collection: 'events', id }))
        ]
        expectOk(nativeExec(paths.disk('round1.fxdb'), round2), 'native round 2 writes')
        ids.users.push(...nativeUsers)
        ids.events.push(...nativeEvents)
        const reads3 = [...readsOf(ids), ...QUERIES]
        const nativeView = expectOk(
            nativeExec(paths.disk('round1.fxdb'), reads3),
            'native round 2 view'
        )
        evidence.round2 = {
            census: census(readFileSync(paths.disk('round1.fxdb')), collections)
        }
        // Native writes zstd again, with the collection's dictionary.
        expect(evidence.round2.census.users['zstd+dict']).toBeGreaterThan(
            evidence.round1.census.users['zstd+dict']
        )

        // Round 3, browser: native's writes are there, and the browser writes on.
        await harness.removeCopy(copy)
        const again = await harness.open({
            kind: 'copy',
            label: 'round-trip.fxdb',
            url: paths.url('round1.fxdb')
        })
        copy = again.name
        expectSameFrames(
            expectOk(await harness.exec(again.id, reads3), 'browser round 3 reads'),
            nativeView,
            'browser reading the file native wrote back'
        )
        const lastNotes = take(60)
        expectOk(
            await harness.exec(again.id, [
                ...lastNotes.map((id, index) => ({
                    op: 'putData',
                    collection: 'notes',
                    id,
                    data: note(9000 + index)
                })),
                ...nativeUsers.slice(0, 25).map((id) => ({
                    op: 'patchDoc',
                    collection: 'users',
                    id,
                    newDoc: { patchedBy: 'browser again' }
                }))
            ]),
            'browser round 3 writes'
        )
        ids.notes.push(...lastNotes)
        const reads4 = [...readsOf(ids), ...QUERIES]
        const browserFinal = expectOk(await harness.exec(again.id, reads4), 'browser round 3 view')
        const finalBytes = await harness.snapshot(again.id)
        await harness.close(again.id)
        writeFileSync(paths.disk('round3.fxdb'), finalBytes)
        evidence.round3 = { census: census(finalBytes, collections) }
        expect(evidence.round3.census.notes.lz4 ?? 0).toBeGreaterThan(
            evidence.round1.census.notes.lz4 ?? 0
        )
        expectSameFrames(
            expectOk(nativeExec(paths.disk('round3.fxdb'), reads4), 'native final reads'),
            browserFinal,
            'native reading the third round'
        )
        const [finalVerify] = expectOk(
            nativeExec(paths.disk('round3.fxdb'), [{ op: 'verifyDatabase' }])
        )
        expect(finalVerify.result).toMatchObject({ ok: true, problems: [] })

        // An LZ4 body damaged behind a valid checksum: only the decoder can
        // catch it, and both engines must catch it the same way.
        const lz4 = recordFrames(bytes).filter(
            (frame) => frame.codec === 'lz4' && frame.dictionary && frame.length > 64
        )
        expect(lz4.length).toBeGreaterThan(0)
        const damaged = corruptPayload(bytes, lz4[Math.floor(lz4.length / 2)], 0.5)
        writeFileSync(paths.disk('lz4-damaged.fxdb'), damaged)
        copyFileSync(paths.disk('lz4-damaged.fxdb'), paths.disk('lz4-damaged.native.fxdb'))
        const damageReads = [
            { op: 'verifyDatabase' },
            ...['users', 'orders', 'notes', 'events'].map((collection) => ({
                op: 'findDocs',
                collection,
                query: {}
            })),
            ...browserUsers.map((id) => ({ op: 'getDoc', collection: 'users', id }))
        ]
        const reader = await harness.open({ kind: 'file', url: paths.url('lz4-damaged.fxdb') })
        try {
            const browserDamage = await harness.exec(reader.id, damageReads)
            const nativeDamage = nativeExec(paths.disk('lz4-damaged.native.fxdb'), damageReads)
            evidence.lz4Damage = browserDamage
                .filter((frame) => !frame.ok)
                .slice(0, 3)
                .map((frame) => frame.error)
            expectSameFrames(browserDamage, nativeDamage, 'damaged LZ4 frame', outcome)
        } finally {
            await harness.close(reader.id)
        }
    } finally {
        if (copy) await harness.removeCopy(copy)
        await attach(testInfo, 'fxdb-round-trips', evidence)
    }
})

test('a picked file and a hosted file refuse writes and keep their bytes', async ({
    page
}, testInfo) => {
    test.setTimeout(300_000)
    const harness = await FxdbPage.open(page)
    const { paths, ids } = fixture
    const [fresh] = mintIds(1)
    const attempts = [
        { op: 'putData', collection: 'notes', id: fresh, data: note(1) },
        { op: 'batchPutData', collection: 'notes', batch: [note(2), note(3)] },
        { op: 'patchDoc', collection: 'users', id: ids.users[0], newDoc: { team: 'none' } },
        { op: 'delDoc', collection: 'orders', id: ids.orders[0] },
        { op: 'createCollection', collection: 'refused' },
        { op: 'executeSQL', sql: "UPDATE notes SET title = 'x' WHERE title = 'note 1'" }
    ]
    const after = [
        { op: 'getDoc', collection: 'notes', id: fresh },
        { op: 'getDoc', collection: 'users', id: ids.users[0] },
        { op: 'getDoc', collection: 'orders', id: ids.orders[0] },
        { op: 'findDocs', collection: 'notes', query: {} },
        { op: 'verifyDatabase' }
    ]
    const native = expectOk(nativeExec(fixture.database, after), 'native reads')
    const expected = sha256(readFileSync(fixture.database))
    const evidence = {}
    for (const kind of ['file', 'url']) {
        const opened = await harness.open({ kind, url: paths.url('native.fxdb') })
        try {
            expect(opened.info.writable).toBe(false)
            const refused = await harness.exec(opened.id, attempts)
            evidence[kind] = refused.map((frame) => ({
                op: frame.op,
                ok: frame.ok,
                error: frame.error
            }))
            for (const frame of refused) {
                expect(frame.ok, `${kind}: ${frame.op} must be refused`).toBe(false)
                expect(frame.error?.code, `${kind}: ${frame.op} names why`).toMatch(/^E[A-Z_]+$/)
            }
            // The refusals leave the engine usable and the database as it was.
            expectSameFrames(
                await harness.exec(opened.id, after),
                native,
                `reads after refusals (${kind})`
            )
            if (kind === 'file') {
                const bytes = await harness.snapshot(opened.id)
                expect(sha256(bytes), 'the picked file is byte for byte unchanged').toBe(expected)
            }
        } finally {
            await harness.close(opened.id)
        }
    }
    // Both read-only hosts refuse the same way.
    expect(evidence.url.map((frame) => frame.error?.code)).toEqual(
        evidence.file.map((frame) => frame.error?.code)
    )
    expect(sha256(readFileSync(fixture.database))).toBe(expected)
    await attach(testInfo, 'fxdb-read-only-refusals', evidence)
})

test('reports damaged and truncated files the way native does, without a trap or a hang', async ({
    page
}, testInfo) => {
    test.setTimeout(600_000)
    const harness = await FxdbPage.open(page)
    const { paths, smallIds } = fixture
    const small = readFileSync(fixture.small)
    const extent = 2 ** small[24]
    const frames = recordFrames(small)
    expect(frames.length).toBeGreaterThan(smallIds.length)
    const middle = frames[Math.floor(frames.length / 2)]
    const last = frames.at(-1)
    const segmentExtent = Math.floor(middle.offset / extent) * extent
    const variants = {
        // A flipped bit inside a record body: the frame's CRC no longer matches.
        'flipped record byte': flip(small, middle.offset + 12 + Math.floor(middle.length / 2)),
        'flipped last record byte': flip(small, last.offset + 12 + Math.floor(last.length / 2)),
        // A torn write: the last frame's second half never reached the disk.
        'torn last frame': zeroed(
            small,
            last.offset + 12 + Math.floor(last.length / 2),
            last.offset + 12 + last.length
        ),
        // A frame length that claims far more than the extent holds.
        'oversized frame length': patched(small, (bytes) =>
            bytes.writeUInt32LE(0xfffffff0, middle.offset + 4)
        ),
        // A zstd body damaged behind a valid checksum.
        'damaged zstd payload': corruptPayload(small, middle, 0.5),
        'damaged zstd header': corruptPayload(small, middle, 0),
        'damaged directory extent': flip(small, extent + 4096 + 3),
        'both superblocks damaged': flip(flip(small, 40), 4096 + 40),
        'file cut short by 3 MiB': small.subarray(0, small.length - 3 * 1024 * 1024),
        'file cut inside the segment extent': small.subarray(
            0,
            segmentExtent + 4096 + 32 + Math.floor((middle.offset - segmentExtent) / 2)
        ),
        'file cut inside the superblock': small.subarray(0, 6000),
        'empty file': Buffer.alloc(0)
    }
    const requests = [
        { op: 'inspectDatabase' },
        { op: 'verifyDatabase' },
        { op: 'findDocs', collection: 'notes', query: { $onlyIds: true } },
        { op: 'findDocs', collection: 'notes', query: {} },
        ...[0, 10, 20, 30, 39].map((index) => ({
            op: 'getDoc',
            collection: 'notes',
            id: smallIds[index]
        }))
    ]
    const evidence = {}
    for (const [label, bytes] of Object.entries(variants)) {
        const name = `damaged-${label.replaceAll(' ', '-')}.fxdb`
        writeFileSync(paths.disk(name), bytes)
        // Native gets its own copy, so nothing it might repair reaches the browser.
        writeFileSync(paths.disk(`${name}.native.fxdb`), bytes)
        const native = nativeExec(paths.disk(`${name}.native.fxdb`), requests)
        const opened = await harness.open({ kind: 'file', url: paths.url(name) })
        try {
            const answer = await harness.run(opened.id, requests, 120_000)
            evidence[label] = {
                answer: answer.frames
                    ? answer.frames.map((frame) =>
                          frame.ok ? 'ok' : `${frame.error.code}: ${frame.error.message}`
                      )
                    : answer,
                native: native.map((frame) =>
                    frame.ok ? 'ok' : `${frame.error.code}: ${frame.error.message}`
                )
            }
            expect(answer.timedOut, `${label}: the engine hung`).toBeUndefined()
            expect(
                answer.error,
                `${label}: the Worker failed (${answer.code}): ${answer.error}`
            ).toBeUndefined()
            expect(answer.frames.length, `${label}: every request answered`).toBe(requests.length)
            if (DECODER_DAMAGE.has(label)) {
                expectSameFrames(answer.frames, native, label, refusal)
                for (const frame of answer.frames.filter((frame) => !frame.ok)) {
                    expect(frame.error.code, label).toMatch(
                        /^(ENATIVE_DOCUMENT|ENATIVE_METADATA|EFXDB_CORRUPT)$/
                    )
                }
            } else {
                expectSameFrames(answer.frames, native, label, outcome)
            }
            // A refusal is a protocol error, never a panic surfacing as one.
            for (const frame of answer.frames.filter((frame) => !frame.ok)) {
                expect(frame.error.message, label).not.toMatch(/panicked|unreachable|RuntimeError/)
            }
            // The module survived: it still answers on the same instance.
            const [handshake] = await harness.exec(opened.id, [{ op: 'handshake' }], 60_000)
            expect(handshake.ok, `${label}: engine still alive`).toBe(true)
        } finally {
            await harness.close(opened.id)
        }
    }
    await attach(testInfo, 'fxdb-damage', evidence)
})

test('a Worker killed mid-batch leaves a working copy native verifies', async ({
    page
}, testInfo) => {
    test.setTimeout(600_000)
    const harness = await FxdbPage.open(page)
    await requireOpfs(harness)
    const { paths } = fixture
    const evidence = {}
    const warm = mintIds(40)
    const batch = mintIds(3000)
    const writes = batch.map((id, index) => ({
        op: 'putData',
        collection: 'users',
        id,
        data: { ...user(300_000 + index), writer: 'crash' }
    }))
    const opened = await harness.open({
        kind: 'copy',
        label: 'crash.fxdb',
        url: paths.url('native.fxdb')
    })
    const copy = opened.name
    let reopened
    try {
        // Time a few writes, then kill the Worker about a third of the way
        // through a batch that would take several times as long.
        const started = Date.now()
        expectOk(
            await harness.exec(
                opened.id,
                warm.map((id, index) => ({
                    op: 'putData',
                    collection: 'users',
                    id,
                    data: user(index)
                }))
            )
        )
        const perWrite = (Date.now() - started) / warm.length
        const delay = Math.max(50, Math.round((perWrite * batch.length) / 3))
        await harness.start(opened.id, writes)
        await page.waitForTimeout(delay)
        await harness.kill(opened.id)
        evidence.perWriteMs = perWrite
        evidence.killedAfterMs = delay

        // The Web Lock goes with the Worker; reopening may race its release.
        for (let attempt = 0; attempt < 50; attempt++) {
            reopened = await harness.tryOpen({ kind: 'opfs', name: copy })
            if (reopened.code !== 'EBUSY') break
            await page.waitForTimeout(100)
        }
        expect(reopened.error, `reopen: ${reopened.error}`).toBeUndefined()
        const [verified, listed] = expectOk(
            await harness.exec(reopened.id, [
                { op: 'verifyDatabase' },
                { op: 'findDocs', collection: 'users', query: { $onlyIds: true } }
            ]),
            'browser after the crash'
        )
        expect(verified.result.ok).toBe(true)
        const present = new Set(listed.result)
        const survived = batch.filter((id) => present.has(id)).length
        evidence.survived = survived
        // Each putData commits on its own, in order: what survived is exactly
        // the first writes of the batch, and the kill came before the end.
        expect(batch.slice(0, survived).every((id) => present.has(id))).toBe(true)
        expect(batch.slice(survived).some((id) => present.has(id))).toBe(false)
        expect(survived, 'the Worker was killed mid-batch').toBeLessThan(batch.length)
        expect(warm.every((id) => present.has(id))).toBe(true)

        // The copy keeps working, and native agrees with what it holds.
        const [after] = mintIds(1)
        expectOk(
            await harness.exec(reopened.id, [
                { op: 'putData', collection: 'users', id: after, data: user(1) }
            ])
        )
        const reads = [
            { op: 'findDocs', collection: 'users', query: { $onlyIds: true } },
            ...[...warm, ...batch.slice(0, survived), after].map((id) => ({
                op: 'getDoc',
                collection: 'users',
                id
            }))
        ]
        const browserView = expectOk(await harness.exec(reopened.id, reads))
        const bytes = await harness.snapshot(reopened.id)
        await harness.close(reopened.id)
        writeFileSync(paths.disk('crashed.fxdb'), bytes)
        const [nativeVerify] = expectOk(
            nativeExec(paths.disk('crashed.fxdb'), [{ op: 'verifyDatabase' }])
        )
        evidence.nativeVerify = nativeVerify.result
        expect(nativeVerify.result.ok).toBe(true)
        expect(
            nativeVerify.result.problems.filter((problem) => problem.severity !== 'warning')
        ).toEqual([])
        expectSameFrames(
            expectOk(nativeExec(paths.disk('crashed.fxdb'), reads)),
            browserView,
            'native reading the crashed copy'
        )
        const [more] = mintIds(1)
        expectOk(
            nativeExec(paths.disk('crashed.fxdb'), [
                { op: 'putData', collection: 'users', id: more, data: user(2) }
            ])
        )
    } finally {
        // A failure above can leave the reopened session holding the copy;
        // close it so the cleanup does not hide that failure behind its own.
        if (reopened?.id !== undefined) await harness.close(reopened.id).catch(() => {})
        await harness.removeCopy(copy)
        await attach(testInfo, 'fxdb-crash', evidence)
    }
})

function flip(bytes, at) {
    const out = Buffer.from(bytes)
    out[at] ^= 0x40
    return out
}

function zeroed(bytes, from, to) {
    const out = Buffer.from(bytes)
    out.fill(0, from, to)
    return out
}

function patched(bytes, change) {
    const out = Buffer.from(bytes)
    change(out)
    return out
}
