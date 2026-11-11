// `.vdbx` working copies kept in IndexedDB (ADR 0022), and the recovery and
// soak gates ADR 0008 left pending, run on both working-copy backends.
//
// - A page that is not a secure context has no OPFS and no Web Locks, as a
//   WKWebView page on `tachyon-app://` has none. The page is reached through a
//   proxy by a name that is not `localhost`, so every browser treats it as
//   insecure, and the `local` source must pick IndexedDB there.
// - A Worker killed at many points, inside the engine, inside a store and
//   between batches, must leave a copy that opens, verifies, keeps every
//   answered write, and holds the writes it kept in the order they were sent.
// - The soak test runs random writes against a model for a fixed time, with
//   clean reopens and kills between and inside batches, and checks the copy
//   against the model after each one.
//
// FYLO_BROWSER_SOAK_SECONDS sets the soak's length (45 seconds by default).

import { expect, test } from './browser-test.mjs'
import { expectOk, mintIds, nativeExec } from './engine-harness.mjs'
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { createServer, request as forward } from 'node:http'
import { join } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { gunzipSync } from 'node:zlib'

const PLAIN_PORT = Number(process.env.FYLO_E2E_PLAIN_PORT ?? 4174)
const PLAIN_ORIGIN = `http://127.0.0.1:${PLAIN_PORT}`
const INSECURE_HOST = 'fylo-insecure.test'
const INSECURE_ORIGIN = `http://${INSECURE_HOST}:${PLAIN_PORT}`
const HARNESS = '/tests/browser/fixtures/vdbx-harness.html'
const FIXTURES = 'target/browser-fixtures/vdbx-indexeddb'
const STORAGES = ['opfs', 'indexeddb']
const COLLECTION = 'items'

const KILL_DELAYS_MS = [0, 2, 6, 12, 25, 45, 80, 130, 220, 400, 700, 60_000]
const KILL_BATCHES = 40
const KILL_BATCH_SIZE = 5

const SOAK_MS = Number(process.env.FYLO_BROWSER_SOAK_SECONDS ?? 45) * 1000
const SOAK_EVENT_EVERY = 20
const SOAK_SAMPLE = 120

let fixture
let proxy

test.beforeAll(async ({}, workerInfo) => {
    test.setTimeout(300_000)
    const directory = join(FIXTURES, `${workerInfo.project.name}-${workerInfo.workerIndex}`)
    rmSync(directory, { recursive: true, force: true })
    mkdirSync(directory, { recursive: true })
    const empty = join(directory, 'empty.vdbx')
    expectOk(nativeExec(empty, [{ op: 'createCollection', collection: COLLECTION }]), 'fixture')
    fixture = { directory, url: `/${empty}` }
    proxy = await startProxy()
})

test.afterAll(async () => {
    if (fixture) rmSync(fixture.directory, { recursive: true, force: true })
    await new Promise((resolve) => (proxy ? proxy.close(resolve) : resolve()))
})

/** A proxy that sends `fylo-insecure.test:4174` to the plain test server. */
function startProxy() {
    const server = createServer((incoming, answer) => {
        let target
        try {
            target = new URL(incoming.url)
        } catch {
            answer.writeHead(400).end()
            return
        }
        if (target.hostname !== INSECURE_HOST) {
            answer.writeHead(403).end()
            return
        }
        const upstream = forward(
            {
                host: '127.0.0.1',
                port: PLAIN_PORT,
                method: incoming.method,
                path: `${target.pathname}${target.search}`,
                headers: { ...incoming.headers, host: `127.0.0.1:${PLAIN_PORT}` }
            },
            (reply) => {
                answer.writeHead(reply.statusCode ?? 502, reply.headers)
                reply.pipe(answer)
            }
        )
        upstream.on('error', () => answer.writeHead(502).end())
        incoming.pipe(upstream)
    })
    server.on('connect', (_request, socket) => socket.end('HTTP/1.1 502 Bad Gateway\r\n\r\n'))
    return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)))
}

/** The single-file harness page, driven from Node. */
class Harness {
    static async open(page, origin = PLAIN_ORIGIN) {
        await page.goto(`${origin}${HARNESS}`, { waitUntil: 'domcontentloaded' })
        await expect(page.locator('[data-status="ready"]')).toBeVisible()
        return new Harness(page)
    }

    constructor(page) {
        this.page = page
    }

    call(name, ...args) {
        return this.page.evaluate(([name, args]) => globalThis.vdbx[name](...args), [name, args])
    }

    async open(source) {
        const opened = await this.call('open', source)
        expect(opened.error, `open ${JSON.stringify(source)}: ${opened.error}`).toBeUndefined()
        return opened
    }

    /** Open again, waiting while the last owner's Web Lock or lease is still held. */
    async reopen(source, waitMs = 15_000) {
        const started = Date.now()
        for (;;) {
            const opened = await this.call('open', source)
            if (opened.code !== 'EBUSY' || Date.now() - started > waitMs) {
                expect(opened.error, `reopen: ${opened.error}`).toBeUndefined()
                return { ...opened, waitedMs: Date.now() - started }
            }
            await this.page.waitForTimeout(100)
        }
    }

    async exec(id, requests) {
        const answer = await this.call('exec', id, requests, 300_000)
        expect(answer.error, `the Worker failed: ${answer.error} (${answer.code})`).toBeUndefined()
        expect(answer.timedOut, 'the engine did not answer in time').toBeUndefined()
        return answer.frames
    }

    /**
     * The ids the copy holds, after `verifyDatabase` passes. Firefox can let
     * a killed Worker's last OPFS write land after its lock is released; the
     * engine refuses a torn view with EENGINE_CONCURRENT_WRITE, so read again.
     */
    async verifiedIds(id) {
        let frames
        for (let attempt = 0; attempt < 10; attempt++) {
            frames = await this.exec(id, [
                { op: 'verifyDatabase' },
                { op: 'findDocs', collection: COLLECTION, query: { $onlyIds: true } }
            ])
            if (!frames.some((frame) => frame.error?.code === 'EENGINE_CONCURRENT_WRITE')) break
            await this.page.waitForTimeout(200)
        }
        const [verified, listed] = expectOk(frames, 'verify after reopen')
        expect(verified.result.ok, JSON.stringify(verified.result.problems)).toBe(true)
        return new Set(listed.result)
    }

    async snapshot(id) {
        const shot = await this.call('snapshot', id)
        return gunzipSync(Buffer.from(shot.gzipBase64, 'base64'))
    }
}

async function requireStorage(harness, storage) {
    if (storage !== 'opfs') return
    const reason = await harness.call('opfs')
    test.skip(reason !== '', reason)
}

function putOf(id, index) {
    return {
        op: 'putData',
        collection: COLLECTION,
        id,
        data: { index, text: `document ${index} `.repeat(16) }
    }
}

async function attach(testInfo, name, evidence) {
    await testInfo.attach(name, {
        body: `${JSON.stringify(evidence, null, 2)}\n`,
        contentType: 'application/json'
    })
}

test.describe('a page that is not a secure context', () => {
    test('picks IndexedDB, keeps one owner, and keeps every answered write', async ({
        browser
    }, testInfo) => {
        test.setTimeout(300_000)
        const context = await browser.newContext({
            proxy: { server: `http://127.0.0.1:${proxy.address().port}` }
        })
        const evidence = {}
        let name
        try {
            const page = await context.newPage()
            const harness = await Harness.open(page, INSECURE_ORIGIN)
            const surface = await harness.call('context')
            evidence.context = surface
            expect(surface.secure).toBe(false)
            expect(surface.storageManager).toBe('undefined')
            expect(surface.locks).toBe('undefined')

            const first = await harness.open({
                kind: 'copy',
                storage: 'local',
                label: 'insecure.vdbx',
                url: fixture.url
            })
            name = first.name
            expect(first.info.storage).toBe('indexeddb')
            const ids = mintIds(60)
            expectOk(await harness.exec(first.id, ids.slice(0, 50).map(putOf)))

            const second = await harness.call('open', { kind: 'opfs', storage: 'local', name })
            expect(second.code, 'a second owner is refused while the first is open').toBe('EBUSY')
            await harness.call('close', first.id)

            const reopened = await harness.open({ kind: 'opfs', storage: 'local', name })
            expect(reopened.info.storage).toBe('indexeddb')
            expect(reopened.waitedMs ?? 0).toBeLessThan(1000)
            expect(await harness.verifiedIds(reopened.id)).toEqual(new Set(ids.slice(0, 50)))
            expectOk(await harness.exec(reopened.id, ids.slice(50).map(putOf)))

            // A killed owner leaves its lease behind; with no Web Locks only
            // its expiry hands the copy over.
            await harness.call('kill', reopened.id)
            const afterKill = await harness.reopen({ kind: 'opfs', storage: 'local', name })
            evidence.waitedAfterKillMs = afterKill.waitedMs
            expect(afterKill.waitedMs).toBeLessThan(10_000)
            expect(await harness.verifiedIds(afterKill.id)).toEqual(new Set(ids))
            await harness.call('close', afterKill.id)

            const listed = await harness.call('listIndexed')
            expect(listed.map((copy) => copy.name)).toContain(name)
            await harness.call('removeCopy', name, 'indexeddb')
            name = undefined
            const after = await harness.call('listIndexed')
            expect(after.map((copy) => copy.name)).not.toContain(first.name)
        } finally {
            if (name) {
                const page = context.pages()[0]
                await page
                    ?.evaluate((name) => globalThis.vdbx.removeCopy(name, 'indexeddb'), name)
                    .catch(() => {})
            }
            await context.close()
            await attach(testInfo, 'vdbx-insecure', evidence)
        }
    })
})

test('a lost lease fails the write instead of reporting it done', async ({ page }) => {
    test.setTimeout(180_000)
    const harness = await Harness.open(page)
    const opened = await harness.open({
        kind: 'copy',
        storage: 'indexeddb',
        label: 'fenced.vdbx',
        url: fixture.url
    })
    const name = opened.name
    try {
        const [kept, lost] = mintIds(2)
        expectOk(await harness.exec(opened.id, [putOf(kept, 0)]))
        // Another owner takes the lease, as a tab does once this one's has
        // expired. This Worker's next store must find out and refuse.
        await page.evaluate(
            (name) =>
                new Promise((resolve, reject) => {
                    const request = indexedDB.open('fylo-vdbx')
                    request.onsuccess = () => {
                        const transaction = request.result.transaction('owners', 'readwrite')
                        transaction.objectStore('owners').put({
                            name,
                            owner: 'another-tab',
                            expiresAt: Date.now() + 60_000
                        })
                        transaction.oncomplete = () => {
                            request.result.close()
                            resolve(undefined)
                        }
                        transaction.onabort = () => reject(transaction.error)
                    }
                    request.onerror = () => reject(request.error)
                }),
            name
        )
        const refused = await harness.call('exec', opened.id, [putOf(lost, 1)], 60_000)
        expect(refused.code).toBe('ELOCKLOST')
        const again = await harness.call('exec', opened.id, [{ op: 'handshake' }], 60_000)
        expect(again.code, 'the session stays refused until it is opened again').toBe('ELOCKLOST')
        await harness.call('close', opened.id)

        await page.evaluate(
            (name) =>
                new Promise((resolve) => {
                    const request = indexedDB.open('fylo-vdbx')
                    request.onsuccess = () => {
                        const transaction = request.result.transaction('owners', 'readwrite')
                        transaction.objectStore('owners').delete(name)
                        transaction.oncomplete = () => {
                            request.result.close()
                            resolve(undefined)
                        }
                    }
                }),
            name
        )
        const reopened = await harness.open({ kind: 'opfs', storage: 'indexeddb', name })
        expect(await harness.verifiedIds(reopened.id)).toEqual(new Set([kept]))
        await harness.call('close', reopened.id)
    } finally {
        await harness.call('removeCopy', name, 'indexeddb')
    }
})

for (const storage of STORAGES) {
    test(`a Worker killed at ${KILL_DELAYS_MS.length} points leaves a copy that opens, in order, with every answered write (${storage})`, async ({
        page
    }, testInfo) => {
        test.setTimeout(900_000)
        const harness = await Harness.open(page)
        await requireStorage(harness, storage)
        const perKill = KILL_BATCHES * KILL_BATCH_SIZE
        const ids = mintIds(KILL_DELAYS_MS.length * perKill)
        const evidence = { storage, kills: [] }
        let opened = await harness.open({
            kind: 'copy',
            storage,
            label: 'kills.vdbx',
            url: fixture.url
        })
        const name = opened.name
        const kept = new Set()
        try {
            for (const [round, delay] of KILL_DELAYS_MS.entries()) {
                const issued = ids.slice(round * perKill, (round + 1) * perKill)
                const batches = Array.from({ length: KILL_BATCHES }, (_, batch) =>
                    issued
                        .slice(batch * KILL_BATCH_SIZE, (batch + 1) * KILL_BATCH_SIZE)
                        .map((id, offset) => putOf(id, batch * KILL_BATCH_SIZE + offset))
                )
                const outcome = await harness.call('writeUntilKilled', opened.id, batches, delay)
                expect(
                    outcome.error,
                    `a batch failed before the kill: ${outcome.error}`
                ).toBeUndefined()

                opened = await harness.reopen({ kind: 'opfs', storage, name })
                const present = await harness.verifiedIds(opened.id)
                const survived = issued.filter((id) => present.has(id)).length
                evidence.kills.push({
                    delayMs: delay,
                    answered: outcome.answered,
                    inFlight: outcome.inFlight,
                    survived,
                    reopenWaitMs: opened.waitedMs
                })
                for (const id of kept)
                    expect(present.has(id), `${id} from an earlier round`).toBe(true)
                expect(issued.slice(0, survived).every((id) => present.has(id))).toBe(true)
                expect(issued.slice(survived).some((id) => present.has(id))).toBe(false)
                expect(survived).toBeGreaterThanOrEqual(outcome.answered * KILL_BATCH_SIZE)
                const limit = (outcome.answered + (outcome.inFlight ? 1 : 0)) * KILL_BATCH_SIZE
                expect(survived).toBeLessThanOrEqual(limit)
                // One IndexedDB transaction stores a whole batch, or none of it.
                if (storage === 'indexeddb') expect(survived % KILL_BATCH_SIZE).toBe(0)
                expect(present.size).toBe(kept.size + survived)
                for (const id of issued.slice(0, survived)) kept.add(id)
            }
            expect(
                evidence.kills.some((kill) => kill.survived < perKill),
                'at least one kill landed before the end of its round'
            ).toBe(true)

            const bytes = await harness.snapshot(opened.id)
            const copy = join(fixture.directory, `killed-${storage}.vdbx`)
            writeFileSync(copy, bytes)
            const [verified, listed] = expectOk(
                nativeExec(copy, [
                    { op: 'verifyDatabase' },
                    { op: 'findDocs', collection: COLLECTION, query: { $onlyIds: true } }
                ]),
                'native reading the killed copy'
            )
            expect(verified.result.ok).toBe(true)
            expect(new Set(listed.result)).toEqual(kept)
        } finally {
            await harness.call('close', opened.id).catch(() => {})
            await harness.call('removeCopy', name, storage)
            await attach(testInfo, `vdbx-kills-${storage}`, evidence)
        }
    })
}

/** A small seeded generator, so a failing soak can be run again exactly. */
function random(seed) {
    let state = seed >>> 0
    return (limit) => {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0
        return state % limit
    }
}

for (const storage of STORAGES) {
    test(`soaks random writes, reopens and kills against a model (${storage})`, async ({
        page
    }, testInfo) => {
        test.setTimeout(SOAK_MS + 600_000)
        const harness = await Harness.open(page)
        await requireStorage(harness, storage)
        const seed = Number(process.env.FYLO_BROWSER_SOAK_SEED ?? Date.now() % 1_000_000)
        const next = random(seed)
        const fresh = mintIds(8000)
        const model = new Map()
        const evidence = {
            storage,
            seed,
            batches: 0,
            operations: 0,
            reopens: 0,
            kills: 0,
            midBatchKills: 0,
            checks: 0
        }
        let opened = await harness.open({
            kind: 'copy',
            storage,
            label: 'soak.vdbx',
            url: fixture.url
        })
        const name = opened.name
        const source = { kind: 'opfs', storage, name }

        const planBatch = () => {
            const ops = []
            const touched = new Map()
            const live = [...model.keys()]
            for (let count = 1 + next(8); count > 0; count--) {
                const choice = next(10)
                if ((choice < 5 || live.length < 10) && fresh.length > 0) {
                    const id = fresh.shift()
                    const data = {
                        n: evidence.operations + ops.length,
                        text: `soak ${next(1_000_000)} `.repeat(1 + next(20))
                    }
                    ops.push({ op: 'putData', collection: COLLECTION, id, data })
                    touched.set(id, { before: undefined, after: data })
                    live.push(id)
                    continue
                }
                if (live.length === 0) break
                // Each document is touched once per batch, so a batch cut short
                // leaves every document at its value before or after it.
                const id = live[next(live.length)]
                const before = model.get(id)
                if (touched.has(id) || before === undefined) continue
                if (choice < 8) {
                    const change = { touched: next(1_000_000) }
                    ops.push({ op: 'patchDoc', collection: COLLECTION, id, newDoc: change })
                    touched.set(id, { before, after: { ...before, ...change } })
                } else {
                    ops.push({ op: 'delDoc', collection: COLLECTION, id })
                    touched.set(id, { before, after: undefined })
                }
            }
            return { ops, touched }
        }

        const apply = (touched, state) => {
            for (const [id, change] of touched) {
                const value = change[state]
                if (value === undefined) model.delete(id)
                else model.set(id, value)
            }
        }

        const readDocs = async (ids) => {
            if (ids.length === 0) return new Map()
            const frames = expectOk(
                await harness.exec(
                    opened.id,
                    ids.map((id) => ({ op: 'getDoc', collection: COLLECTION, id }))
                ),
                'read back'
            )
            return new Map(ids.map((id, index) => [id, frames[index].result[id]]))
        }

        const check = async () => {
            const present = await harness.verifiedIds(opened.id)
            expect(present).toEqual(new Set(model.keys()))
            const sample = [...model.keys()].filter(() => next(model.size) < SOAK_SAMPLE)
            const read = await readDocs(sample)
            for (const id of sample) expect(read.get(id), id).toEqual(model.get(id))
            evidence.checks++
        }

        const reopen = async () => {
            opened = await harness.reopen(source)
            evidence.reopens++
        }

        try {
            const deadline = Date.now() + SOAK_MS
            while (Date.now() < deadline) {
                const { ops, touched } = planBatch()
                if (ops.length === 0) continue
                evidence.batches++
                const event =
                    evidence.batches % SOAK_EVENT_EVERY === 0
                        ? ['reopen', 'kill', 'mid'][next(3)]
                        : 'none'
                if (event === 'mid') {
                    const outcome = await harness.call(
                        'writeUntilKilled',
                        opened.id,
                        [ops],
                        next(15)
                    )
                    expect(outcome.error, outcome.error).toBeUndefined()
                    evidence.midBatchKills++
                    await reopen()
                    const read = await readDocs([...touched.keys()])
                    const states = new Set()
                    for (const [id, change] of touched) {
                        const actual = read.get(id)
                        const state = isDeepStrictEqual(actual, change.after) ? 'after' : 'before'
                        expect(
                            actual,
                            `${id} is its value before or after the killed batch`
                        ).toEqual(change[state])
                        states.add(state)
                    }
                    if (outcome.answered === 1) expect([...states]).toEqual(['after'])
                    if (storage === 'indexeddb')
                        expect(states.size, 'a killed batch is stored whole or not at all').toBe(1)
                    for (const [id, change] of touched) {
                        const actual = read.get(id)
                        apply(
                            new Map([[id, change]]),
                            isDeepStrictEqual(actual, change.after) ? 'after' : 'before'
                        )
                    }
                    evidence.operations += ops.length
                    await check()
                    continue
                }
                expectOk(await harness.exec(opened.id, ops), `soak batch ${evidence.batches}`)
                apply(touched, 'after')
                evidence.operations += ops.length
                if (event === 'reopen') {
                    await harness.call('close', opened.id)
                    await reopen()
                    await check()
                } else if (event === 'kill') {
                    await harness.call('kill', opened.id)
                    evidence.kills++
                    await reopen()
                    await check()
                }
            }
            await check()
            evidence.documents = model.size
            expect(evidence.reopens).toBeGreaterThan(0)
        } finally {
            await harness.call('close', opened.id).catch(() => {})
            await harness.call('removeCopy', name, storage)
            await attach(testInfo, `vdbx-soak-${storage}`, evidence)
        }
    })
}
