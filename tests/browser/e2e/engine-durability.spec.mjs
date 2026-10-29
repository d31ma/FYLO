// Durability of `fylo-browser.wasm` over OPFS: what survives a closed tab, a
// reload and a crash, how the root's Web Lock keeps a second writer out, and
// how the engine behaves at sizes a demo never reaches.
//
// The reference for every recovery is the native binary. A native writer
// killed mid-batch leaves a root the next `exec --loop` reads and writes with
// no repair step: the in-flight transaction is rolled back or forward on open.
// The browser is held to the same, and a crashed browser root copied to disk
// must recover to the same documents under both engines.
//
// OPFS charges per directory operation and the engine makes hundreds per
// write, so a browser write costs most of a second on this hardware. Browser
// written sets are therefore sized in the hundreds; the thousands come from a
// native root seeded into OPFS, which exercises the same storage layout.

import { createHash } from 'node:crypto'
import { rmSync } from 'node:fs'
import { writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import {
    browserRoot,
    expect,
    expectOk,
    nativeExec,
    nativeRoot,
    openTab,
    readTree,
    test as harnessTest,
    writeTree
} from './engine-harness.mjs'

// The module routes a Rust panic to the engine Worker's console before it
// traps, and the trap alone says only "unreachable"; keep the message.
const test = harnessTest.extend({
    panics: [
        async ({ context }, use, testInfo) => {
            const panics = []
            const watch = (page) =>
                page.on('worker', (worker) =>
                    worker.on('console', (message) => {
                        if (message.text().includes('FYLO panic')) panics.push(message.text())
                    })
                )
            context.pages().forEach(watch)
            context.on('page', watch)
            await use(panics)
            if (panics.length > 0) {
                await testInfo.attach('panics.txt', {
                    body: panics.join('\n'),
                    contentType: 'text/plain'
                })
            }
        },
        { auto: true }
    ]
})

const PAD = 'durable-'.repeat(8)
const SOURCE = 'incoming/source.bin'
const SOURCE_BYTES = 64 * 1024

/** The document written for index `i`; every read must return exactly this. */
function body(i) {
    return {
        i,
        label: `doc-${i}`,
        pad: `${PAD}${i}`,
        nested: { i, parity: i % 2 === 0 ? 'even' : 'odd', list: [i, i * 2, String(i)] }
    }
}

function bodies(from, count) {
    return Array.from({ length: count }, (_, offset) => body(from + offset))
}

/** The bytes `fylo.writeFile` generates for `size` and `seed`. */
function pattern(size, seed = 0) {
    const bytes = Buffer.alloc(size)
    for (let index = 0; index < size; index++) bytes[index] = (index * 31 + seed) & 255
    return bytes
}

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex')

async function recordEvidence(testInfo, name, evidence) {
    const path = testInfo.outputPath(name)
    await writeFile(path, `${JSON.stringify(evidence, null, 2)}\n`)
    await testInfo.attach(name, { path, contentType: 'application/json' })
}

async function harnessReady(page) {
    await expect(page.locator('[data-status="ready"]')).toBeVisible()
}

function writeHarnessFile(engine, root, path, options) {
    return engine.page.evaluate(
        ([root, path, options]) => globalThis.fylo.writeFile(root, path, options),
        [root, path, options]
    )
}

function digest(engine, root, path) {
    return engine.page.evaluate(([root, path]) => globalThis.fylo.digest(root, path), [root, path])
}

/**
 * Open a session, starting over when its bridge Worker never reports ready.
 *
 * Firefox occasionally parks a freshly started Worker's first OPFS call
 * forever when another Worker of the origin was terminated moments before; a
 * second Worker started right after is unaffected. That is start-up, not
 * durability, so it is retried here and recorded as an annotation. The stuck
 * Worker never created an engine Worker, so it holds no root lock.
 */
async function openSession(engine, root) {
    for (let attempt = 1; attempt <= 3; attempt++) {
        const session = await engine.page.evaluate(
            ([root, ms]) =>
                Promise.race([
                    globalThis.fylo.open(root),
                    new Promise((resolve) => setTimeout(() => resolve(null), ms))
                ]),
            [root, 20_000]
        )
        if (session !== null) return session
        test.info().annotations.push({
            type: 'bridge-start-retry',
            description: `${root}: bridge Worker not ready after 20 s (attempt ${attempt})`
        })
    }
    throw new Error(`the bridge Worker for ${root} never became ready`)
}

function locks(engine) {
    return engine.page.evaluate(() => globalThis.fylo.locks())
}

/** Every document and the inspection of `collection`, both of which must succeed. */
async function readCollection(engine, session, collection) {
    const [found, inspected] = expectOk(
        await engine.exec(session, [
            { op: 'findDocs', collection, query: {} },
            { op: 'inspectCollection', collection }
        ]),
        `read ${collection}`
    )
    return { docs: found.result, inspection: inspected.result }
}

/** No torn documents: each one read is exactly the one written for its index. */
function expectComplete(docs, label) {
    for (const [id, doc] of Object.entries(docs)) {
        expect(doc, `${label}: document ${id} is torn`).toEqual(body(doc.i))
    }
}

/**
 * Writes commit one document at a time, in order, so a crash leaves the
 * indexes `0..k-1` and nothing past a gap.
 */
function expectPrefix(docs, label) {
    const indexes = Object.values(docs)
        .map((doc) => doc.i)
        .sort((left, right) => left - right)
    expect(indexes, `${label}: visible documents are not a prefix`).toEqual(
        indexes.map((_, position) => position)
    )
    return indexes.length
}

/** Wait until `session` reports `step` among the trace entries after `from`. */
async function waitForStep(engine, session, step, from = 0, timeout = 60_000) {
    await expect
        .poll(async () => (await engine.trace(session)).slice(from), {
            timeout,
            intervals: [20]
        })
        .toContain(step)
}

/** `dump`, retried while the terminated Workers still hold their access handles. */
async function dumpAfterCrash(engine, root) {
    let lastError
    for (let attempt = 0; attempt < 50; attempt++) {
        try {
            return await engine.dump(root)
        } catch (error) {
            lastError = error
            await engine.page.waitForTimeout(200)
        }
    }
    throw lastError
}

/**
 * The write batch every crash interrupts: documents in chunks, with a raw file
 * stored after each chunk so the file collection's checksums are in flight too.
 */
function crashBatch(root, collection, from, chunks, chunk) {
    const frames = []
    for (let index = 0; index < chunks; index++) {
        frames.push({
            op: 'batchPutData',
            collection,
            batch: bodies(from + index * chunk, chunk)
        })
        frames.push({
            op: 'putData',
            collection: `${collection}files`,
            file: { path: `${root}/${SOURCE}` }
        })
    }
    return frames
}

/** A root with a document collection holding `seeded` documents and a file collection. */
async function prepareCrashRoot(engine, root, collection, seeded) {
    await writeHarnessFile(engine, root, SOURCE, { size: SOURCE_BYTES, seed: 7 })
    const session = await openSession(engine, root)
    expectOk(
        await engine.exec(session, [
            { op: 'createCollection', collection },
            { op: 'createCollection', collection: `${collection}files`, kind: 'file' },
            { op: 'batchPutData', collection, batch: bodies(0, seeded) }
        ]),
        'prepare'
    )
    await engine.close(session)
}

/**
 * What a crash must leave: a root the next session reads and writes without
 * a repair step, every visible document complete, and the count between the
 * counts before and after the interrupted batch.
 */
async function expectRecovered(engine, root, collection, before, after, label) {
    const session = await openSession(engine, root)
    try {
        const { docs, inspection } = await readCollection(engine, session, collection)
        expectComplete(docs, label)
        const visible = expectPrefix(docs, label)
        expect(visible, `${label}: fewer documents than before the batch`).toBeGreaterThanOrEqual(
            before
        )
        expect(visible, `${label}: more documents than the batch wrote`).toBeLessThanOrEqual(after)
        expect(inspection).toMatchObject({ docsStored: visible, indexedDocs: visible })

        const [verified, files] = expectOk(
            await engine.exec(session, [
                { op: 'verifyCollection', collection: `${collection}files` },
                { op: 'findDocs', collection: `${collection}files`, query: { $onlyIds: true } }
            ]),
            `${label}: verify`
        )
        expect(verified.result.corrupt, `${label}: corrupt files`).toEqual([])
        expect(verified.result.verified).toBe(verified.result.filesScanned)
        expect(verified.result.filesScanned).toBe(files.result.length)
        const expected = sha256(pattern(SOURCE_BYTES, 7))
        const contents = expectOk(
            await engine.exec(
                session,
                files.result.map((id) => ({
                    op: 'getFileData',
                    collection: `${collection}files`,
                    id
                }))
            ),
            `${label}: file contents`
        )
        for (const frame of contents) {
            expect(frame.result.checksumSHA256).toBe(expected)
            expect(sha256(Buffer.from(frame.result.data, 'base64'))).toBe(expected)
        }

        // Still writable, and the index agrees with a rebuild from the
        // documents. Soft, so a crash loop reports every iteration.
        const rewrite = await engine.exec(session, [
            { op: 'putData', collection, data: body(visible) },
            { op: 'rebuildCollection', collection },
            { op: 'findDocs', collection, query: {} }
        ])
        const failed = rewrite.findIndex((frame) => !frame.ok)
        expect
            .soft(
                failed,
                `${label}: not writable after the crash: ${JSON.stringify(rewrite[failed]?.error)}`
            )
            .toBe(-1)
        if (failed === -1) {
            const [put, rebuilt, again] = rewrite
            expect(typeof put.result).toBe('string')
            expect(rebuilt.result).toMatchObject({
                docsScanned: visible + 1,
                indexedDocs: visible + 1
            })
            expectComplete(again.result, `${label} after write`)
            expect(expectPrefix(again.result, `${label} after write`)).toBe(visible + 1)
        }
        return { visible, files: files.result.length, writable: failed === -1 }
    } finally {
        await engine.close(session)
    }
}

/** Run one iteration's checks, keeping a failure so the loop goes on to the next. */
async function attempt(iterations, meta, check) {
    try {
        iterations.push({ ...meta, ...(await check()) })
    } catch (error) {
        iterations.push({ ...meta, error: String(error?.message ?? error).split('\n')[0] })
    }
}

function expectNoFailures(iterations) {
    expect(
        iterations.filter((iteration) => iteration.error),
        JSON.stringify(iterations, null, 2)
    ).toEqual([])
}

test.describe('persistence', () => {
    async function writeEverything(engine, root) {
        await writeHarnessFile(engine, root, SOURCE, { size: SOURCE_BYTES, seed: 3 })
        const session = await openSession(engine, root)
        const frames = expectOk(
            await engine.exec(session, [
                { op: 'createCollection', collection: 'notes' },
                { op: 'createCollection', collection: 'blobs', kind: 'file' },
                { op: 'batchPutData', collection: 'notes', batch: bodies(0, 8) },
                { op: 'putData', collection: 'notes', data: body(8) },
                { op: 'putData', collection: 'blobs', file: { path: `${root}/${SOURCE}` } },
                { op: 'findDocs', collection: 'notes', query: {} }
            ]),
            'write'
        )
        await engine.close(session)
        return { docs: frames[5].result, file: frames[4].result }
    }

    async function expectEverything(engine, root, written) {
        const session = await openSession(engine, root)
        const { docs, inspection } = await readCollection(engine, session, 'notes')
        expect(docs).toEqual(written.docs)
        expect(Object.keys(docs)).toHaveLength(9)
        expectComplete(docs, 'notes')
        expect(inspection).toMatchObject({ docsStored: 9, indexedDocs: 9, deletedDocs: 0 })
        const [verified, file] = expectOk(
            await engine.exec(session, [
                { op: 'verifyCollection', collection: 'blobs' },
                { op: 'getFileData', collection: 'blobs', id: written.file }
            ]),
            'verify'
        )
        expect(verified.result).toMatchObject({ filesScanned: 1, verified: 1, corrupt: [] })
        expect(file.result.checksumSHA256).toBe(sha256(pattern(SOURCE_BYTES, 3)))
        await engine.close(session)
    }

    test('keeps every write across a closed session, a closed page and a new tab', async ({
        fylo,
        context
    }) => {
        test.setTimeout(240_000)
        const root = browserRoot('persist-tab')
        const written = await writeEverything(fylo, root)
        await fylo.page.close()
        const tab = await openTab(context)
        await expectEverything(tab, root, written)
    })

    test('keeps every write across a full page reload', async ({ fylo }) => {
        test.setTimeout(240_000)
        const root = browserRoot('persist-reload')
        const written = await writeEverything(fylo, root)
        await fylo.page.reload({ waitUntil: 'domcontentloaded' })
        await harnessReady(fylo.page)
        await expectEverything(fylo, root, written)
    })
})

test.describe('root lock', () => {
    // How long a waiting session must stay parked to count as blocked rather
    // than merely slow to start.
    const PARKED_MS = 3_000

    /** A session that has run a batch, and so holds the root's Web Lock. */
    async function holder(engine, root) {
        const session = await openSession(engine, root)
        expectOk(
            await engine.exec(session, [
                { op: 'createCollection', collection: 'notes' },
                { op: 'batchPutData', collection: 'notes', batch: bodies(0, 3) }
            ]),
            'holder'
        )
        return session
    }

    async function expectParked(engine, session, ticket, root) {
        await engine.page.waitForTimeout(PARKED_MS)
        expect(
            (await engine.settled(ticket)).done,
            'second session ran while the first held the root'
        ).toBe(false)
        const trace = await engine.trace(session)
        expect(trace).toContain('locking')
        expect(trace).not.toContain('locked')
        expect((await locks(engine)).pending).toContain(`fylo-root:${root}`)
    }

    async function expectProceeds(engine, ticket, label) {
        const answer = await engine.wait(ticket, 60_000)
        expect(
            answer.timedOut,
            `${label}: still waiting after the holder went away`
        ).toBeUndefined()
        expect(answer.error).toBeUndefined()
        const frames = expectOk(answer.frames, label)
        expectComplete(frames[0].result, label)
        return frames
    }

    test('a second worker on the same root waits for the first, then sees its writes', async ({
        fylo
    }) => {
        test.setTimeout(180_000)
        const root = browserRoot('lock-worker')
        const first = await holder(fylo, root)
        const second = await openSession(fylo, root)
        const ticket = await fylo.start(second, [
            { op: 'findDocs', collection: 'notes', query: {} },
            { op: 'putData', collection: 'notes', data: body(3) }
        ])
        await expectParked(fylo, second, ticket, root)
        // The first session can still write while the second waits.
        expectOk(await fylo.exec(first, [{ op: 'putData', collection: 'notes', data: body(4) }]))
        await fylo.close(first)
        const frames = await expectProceeds(fylo, ticket, 'second worker')
        expect(
            Object.values(frames[0].result)
                .map((doc) => doc.i)
                .sort()
        ).toEqual([0, 1, 2, 4])
        await fylo.close(second)
        await expect
            .poll(async () => (await locks(fylo)).held, { timeout: 10_000 })
            .not.toContain(`fylo-root:${root}`)
    })

    test('a second tab waits for the first tab, then proceeds once that tab closes', async ({
        fylo,
        context
    }) => {
        test.setTimeout(180_000)
        const root = browserRoot('lock-tab')
        const other = await openTab(context)
        await holder(other, root)
        const session = await openSession(fylo, root)
        const ticket = await fylo.start(session, [
            { op: 'findDocs', collection: 'notes', query: {} }
        ])
        await expectParked(fylo, session, ticket, root)
        await other.page.close()
        const frames = await expectProceeds(fylo, ticket, 'second tab')
        expect(Object.keys(frames[0].result)).toHaveLength(3)
        await fylo.close(session)
    })

    test('sessions on different roots run concurrently', async ({ fylo }) => {
        test.setTimeout(180_000)
        const busy = browserRoot('lock-busy')
        const free = browserRoot('lock-free')
        const first = await holder(fylo, busy)
        const second = await openSession(fylo, free)
        // A short batch on one root finishes while a long one on another root
        // is still running: neither waits for the other's lock.
        const mark = (await fylo.trace(first)).length
        const busyTicket = await fylo.start(first, [
            { op: 'batchPutData', collection: 'notes', batch: bodies(3, 12) }
        ])
        await waitForStep(fylo, first, 'executing', mark)
        const freeTicket = await fylo.start(second, [
            { op: 'createCollection', collection: 'notes' },
            { op: 'putData', collection: 'notes', data: body(0) }
        ])
        const freeAnswer = await fylo.wait(freeTicket, 60_000)
        expect(freeAnswer.timedOut, 'the free root waited').toBeUndefined()
        expectOk(freeAnswer.frames, 'free root')
        expect(
            (await fylo.settled(busyTicket)).done,
            'the busy batch finished first, so this proves nothing'
        ).toBe(false)
        const held = (await locks(fylo)).held
        expect(held).toContain(`fylo-root:${busy}`)
        expect(held).toContain(`fylo-root:${free}`)
        expectOk((await fylo.wait(busyTicket, 120_000)).frames, 'busy root')
        await fylo.close(first)
        await fylo.close(second)
    })

    test('killing the holder mid-batch releases the lock and leaves nothing stale', async ({
        fylo
    }) => {
        test.setTimeout(240_000)
        const root = browserRoot('lock-kill')
        const first = await holder(fylo, root)
        const mark = (await fylo.trace(first)).length
        await fylo.start(first, [{ op: 'batchPutData', collection: 'notes', batch: bodies(3, 40) }])
        await waitForStep(fylo, first, 'executing', mark)
        const second = await openSession(fylo, root)
        // Only collections the crash never touched: this is about the root's
        // lock; what the crash left in `notes` is the crash suite's subject.
        const ticket = await fylo.start(second, [
            { op: 'createCollection', collection: 'after' },
            { op: 'putData', collection: 'after', data: body(0) },
            { op: 'findDocs', collection: 'after', query: {} }
        ])
        await expectParked(fylo, second, ticket, root)
        await fylo.kill(first)
        const answer = await fylo.wait(ticket, 60_000)
        expect(answer.timedOut, 'the lock outlived the crashed holder').toBeUndefined()
        expect(answer.error, 'the engine Worker failed the batch').toBeUndefined()
        const frames = expectOk(answer.frames, 'after kill')
        expect(Object.values(frames[2].result)).toEqual([body(0)])
        await fylo.close(second)
        await expect
            .poll(async () => (await locks(fylo)).held, { timeout: 10_000 })
            .not.toContain(`fylo-root:${root}`)
        // A third session, after both are gone, takes the lock at once.
        const third = await openSession(fylo, root)
        const started = Date.now()
        const [found] = expectOk(
            await fylo.exec(third, [{ op: 'findDocs', collection: 'after', query: {} }])
        )
        expect(Object.values(found.result)).toEqual([body(0)])
        expect(Date.now() - started).toBeLessThan(30_000)
        await fylo.close(third)
    })
})

test.describe('crash recovery', () => {
    const CHUNKS = 3
    const CHUNK = 10
    const SEEDED = 2

    test('recovers from a worker killed mid-batch at ten different points', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(1_200_000)
        const root = browserRoot('crash-kill')
        // Milliseconds after the batch starts executing; the batch itself
        // takes tens of seconds, so these land early, mid-way and late.
        const delays = [0, 50, 200, 500, 1_000, 2_000, 3_500, 6_000, 10_000, 15_000]
        const iterations = []
        for (const [index, delay] of delays.entries()) {
            const collection = `crash${index}`
            await prepareCrashRoot(fylo, root, collection, SEEDED)
            const session = await openSession(fylo, root)
            // Take the lock and compile the module first, so the delay is
            // measured from the writes rather than from start-up.
            expectOk(await fylo.exec(session, [{ op: 'inspectCollection', collection }]))
            const mark = (await fylo.trace(session)).length
            const ticket = await fylo.start(
                session,
                crashBatch(root, collection, SEEDED, CHUNKS, CHUNK)
            )
            await waitForStep(fylo, session, 'executing', mark)
            await fylo.page.waitForTimeout(delay)
            const finished = (await fylo.settled(ticket)).done
            await fylo.kill(session)
            await attempt(iterations, { delay, finished }, () =>
                expectRecovered(
                    fylo,
                    root,
                    collection,
                    SEEDED,
                    SEEDED + CHUNKS * CHUNK,
                    `kill after ${delay} ms`
                )
            )
        }
        await recordEvidence(testInfo, 'crash-kill-evidence.json', { root, iterations })
        expectNoFailures(iterations)
        // At least one kill has to land inside the batch, or this proved nothing.
        expect(
            iterations.some(({ visible }) => visible > SEEDED && visible < SEEDED + CHUNKS * CHUNK),
            JSON.stringify(iterations)
        ).toBe(true)
    })

    test('recovers from a page reload mid-batch', async ({ fylo }, testInfo) => {
        test.setTimeout(600_000)
        const root = browserRoot('crash-reload')
        const iterations = []
        for (const [index, delay] of [300, 2_500, 7_000].entries()) {
            const collection = `reload${index}`
            await prepareCrashRoot(fylo, root, collection, SEEDED)
            const session = await openSession(fylo, root)
            expectOk(await fylo.exec(session, [{ op: 'inspectCollection', collection }]))
            const mark = (await fylo.trace(session)).length
            await fylo.start(session, crashBatch(root, collection, SEEDED, CHUNKS, CHUNK))
            await waitForStep(fylo, session, 'executing', mark)
            await fylo.page.waitForTimeout(delay)
            await fylo.page.reload({ waitUntil: 'domcontentloaded' })
            await harnessReady(fylo.page)
            await attempt(iterations, { delay }, () =>
                expectRecovered(
                    fylo,
                    root,
                    collection,
                    SEEDED,
                    SEEDED + CHUNKS * CHUNK,
                    `reload after ${delay} ms`
                )
            )
        }
        await recordEvidence(testInfo, 'crash-reload-evidence.json', { root, iterations })
        expectNoFailures(iterations)
    })

    test('recovers from a tab closed mid-batch', async ({ fylo, context }, testInfo) => {
        test.setTimeout(600_000)
        const root = browserRoot('crash-tab')
        const iterations = []
        for (const [index, delay] of [1_000, 5_000].entries()) {
            const collection = `tab${index}`
            await prepareCrashRoot(fylo, root, collection, SEEDED)
            const tab = await openTab(context)
            const session = await openSession(tab, root)
            expectOk(await tab.exec(session, [{ op: 'inspectCollection', collection }]))
            const mark = (await tab.trace(session)).length
            await tab.start(session, crashBatch(root, collection, SEEDED, CHUNKS, CHUNK))
            await waitForStep(tab, session, 'executing', mark)
            await tab.page.waitForTimeout(delay)
            await tab.page.close()
            await attempt(iterations, { delay }, () =>
                expectRecovered(
                    fylo,
                    root,
                    collection,
                    SEEDED,
                    SEEDED + CHUNKS * CHUNK,
                    `tab closed after ${delay} ms`
                )
            )
        }
        await recordEvidence(testInfo, 'crash-tab-evidence.json', { root, iterations })
        expectNoFailures(iterations)
    })

    test('a crashed browser root copied to disk recovers to the same documents natively', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(600_000)
        const root = browserRoot('crash-native')
        const collection = 'crashed'
        await prepareCrashRoot(fylo, root, collection, SEEDED)
        const session = await openSession(fylo, root)
        expectOk(await fylo.exec(session, [{ op: 'inspectCollection', collection }]))
        const mark = (await fylo.trace(session)).length
        await fylo.start(session, crashBatch(root, collection, SEEDED, CHUNKS, CHUNK))
        await waitForStep(fylo, session, 'executing', mark)
        await fylo.page.waitForTimeout(4_000)
        await fylo.kill(session)

        // The crashed bytes, before any browser session has recovered them.
        const crashed = await dumpAfterCrash(fylo, root)
        const directory = nativeRoot('crashed')
        try {
            const disk = join(directory, 'root')
            writeTree(disk, crashed)
            const [found, inspected, verified] = nativeExec(disk, [
                { op: 'findDocs', collection, query: {} },
                { op: 'inspectCollection', collection },
                { op: 'verifyCollection', collection: `${collection}files` }
            ])
            expectOk([found, inspected, verified], 'native recovery')
            expectComplete(found.result, 'native')
            const visible = expectPrefix(found.result, 'native')
            expect(visible).toBeGreaterThanOrEqual(SEEDED)
            expect(visible).toBeLessThanOrEqual(SEEDED + CHUNKS * CHUNK)
            expect(inspected.result).toMatchObject({ docsStored: visible, indexedDocs: visible })
            expect(verified.result.corrupt).toEqual([])

            const browser = await openSession(fylo, root)
            const recovered = await readCollection(fylo, browser, collection)
            await fylo.close(browser)
            // Same bytes, same recovery: the two engines must agree exactly.
            expect(recovered.docs).toEqual(found.result)
            expect(recovered.inspection).toEqual(inspected.result)

            await recordEvidence(testInfo, 'crash-native-evidence.json', {
                root,
                files: Object.keys(crashed).length,
                visible
            })
        } finally {
            rmSync(directory, { recursive: true, force: true })
        }
        await expectRecovered(fylo, root, collection, SEEDED, SEEDED + CHUNKS * CHUNK, 'browser')
    })
})

test.describe('large data', () => {
    test('answers range and equality queries exactly over a browser-written collection', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(900_000)
        const root = browserRoot('large-queries')
        const COUNT = 120
        const CHUNK = 40
        const phases = {}
        const session = await openSession(fylo, root)
        let started = Date.now()
        expectOk(await fylo.exec(session, [{ op: 'createCollection', collection: 'rows' }]))
        const ids = []
        for (let from = 0; from < COUNT; from += CHUNK) {
            const batch = bodies(from, CHUNK).map((doc) => ({
                ...doc,
                n: doc.i,
                bucket: `b${doc.i % 7}`
            }))
            const [frame] = expectOk(
                await fylo.exec(session, [{ op: 'batchPutData', collection: 'rows', batch }]),
                `chunk ${from}`
            )
            ids.push(...frame.result)
        }
        phases.writeMs = Date.now() - started
        phases.writeMsPerDocument = phases.writeMs / COUNT
        expect(new Set(ids).size).toBe(COUNT)

        const expected = (predicate) => ids.filter((_, n) => predicate(n)).sort()
        const queries = [
            ['n == 77', { n: { $eq: 77 } }, (n) => n === 77],
            ['bucket == b3', { bucket: { $eq: 'b3' } }, (n) => n % 7 === 3],
            ['30 <= n < 75', { n: { $gte: 30, $lt: 75 } }, (n) => n >= 30 && n < 75],
            ['n > 100', { n: { $gt: 100 } }, (n) => n > 100],
            ['n <= 9', { n: { $lte: 9 } }, (n) => n <= 9],
            ['n != 5', { n: { $ne: 5 } }, (n) => n !== 5],
            [
                'b2 and n >= 60',
                { bucket: { $eq: 'b2' }, n: { $gte: 60 } },
                (n) => n % 7 === 2 && n >= 60
            ],
            ['nested parity even', { 'nested.parity': { $eq: 'even' } }, (n) => n % 2 === 0],
            ['n == 999 (none)', { n: { $eq: 999 } }, () => false]
        ]
        started = Date.now()
        const frames = expectOk(
            await fylo.exec(
                session,
                queries.map(([, operation]) => ({
                    op: 'findDocs',
                    collection: 'rows',
                    query: { $ops: [operation], $onlyIds: true }
                }))
            ),
            'queries'
        )
        phases.queryMs = Date.now() - started
        for (const [index, [label, , predicate]] of queries.entries()) {
            expect([...frames[index].result].sort(), label).toEqual(expected(predicate))
        }

        // Disjunction: either side of `$ops` matches.
        const [either] = expectOk(
            await fylo.exec(session, [
                {
                    op: 'findDocs',
                    collection: 'rows',
                    query: { $ops: [{ n: { $lt: 3 } }, { n: { $gte: 117 } }] }
                }
            ])
        )
        expect(Object.keys(either.result).sort()).toEqual(expected((n) => n < 3 || n >= 117))
        for (const [id, doc] of Object.entries(either.result)) {
            expect(doc).toEqual({ ...body(doc.i), n: doc.i, bucket: `b${doc.i % 7}` })
            expect(id).toBe(ids[doc.i])
        }

        started = Date.now()
        const { docs, inspection } = await readCollection(fylo, session, 'rows')
        phases.fullScanMs = Date.now() - started
        expect(Object.keys(docs).sort()).toEqual([...ids].sort())
        expect(inspection).toMatchObject({ docsStored: COUNT, indexedDocs: COUNT })
        await fylo.close(session)
        await recordEvidence(testInfo, 'large-queries-evidence.json', { count: COUNT, phases })
        // Sanity only: a write slower than this is a hang, not a slow disk.
        expect(phases.writeMsPerDocument).toBeLessThan(10_000)
    })

    test('queries a 5,000-document root written natively and keeps writing to it', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(900_000)
        const COUNT = 5_000
        const BUCKETS = 100
        const phases = {}
        const directory = nativeRoot('large')
        try {
            const disk = join(directory, 'root')
            let started = Date.now()
            const written = expectOk(
                nativeExec(disk, [
                    { op: 'createCollection', collection: 'rows' },
                    ...Array.from({ length: COUNT / 500 }, (_, chunk) => ({
                        op: 'batchPutData',
                        collection: 'rows',
                        batch: Array.from({ length: 500 }, (_, offset) => {
                            const n = chunk * 500 + offset
                            return { n, bucket: `b${n % BUCKETS}` }
                        })
                    }))
                ]),
                'native write'
            )
            const ids = written.slice(1).flatMap((frame) => frame.result)
            phases.nativeWriteMs = Date.now() - started

            const root = browserRoot('large-seeded')
            started = Date.now()
            const tree = readTree(disk)
            await fylo.seed(root, tree)
            phases.seedMs = Date.now() - started
            phases.files = Object.keys(tree).length

            const session = await openSession(fylo, root)
            started = Date.now()
            const [unique, bucket, empty] = expectOk(
                await fylo.exec(session, [
                    { op: 'findDocs', collection: 'rows', query: { $ops: [{ n: { $eq: 4321 } }] } },
                    {
                        op: 'findDocs',
                        collection: 'rows',
                        query: { $ops: [{ bucket: { $eq: 'b17' } }], $onlyIds: true }
                    },
                    {
                        op: 'findDocs',
                        collection: 'rows',
                        query: { $ops: [{ bucket: { $eq: 'b100' } }], $onlyIds: true }
                    }
                ]),
                'browser queries'
            )
            phases.queryMs = Date.now() - started
            expect(unique.result).toEqual({ [ids[4321]]: { n: 4321, bucket: 'b21' } })
            const b17 = ids.filter((_, n) => n % BUCKETS === 17)
            expect([...bucket.result].sort()).toEqual([...b17].sort())
            expect(empty.result).toEqual([])

            started = Date.now()
            const [added] = expectOk(
                await fylo.exec(session, [
                    {
                        op: 'batchPutData',
                        collection: 'rows',
                        batch: Array.from({ length: 20 }, (_, offset) => ({
                            n: COUNT + offset,
                            bucket: 'b17'
                        }))
                    }
                ]),
                'browser write'
            )
            phases.browserWriteMs = Date.now() - started
            const [after] = expectOk(
                await fylo.exec(session, [
                    {
                        op: 'findDocs',
                        collection: 'rows',
                        query: { $ops: [{ bucket: { $eq: 'b17' } }], $onlyIds: true }
                    }
                ])
            )
            const b17After = [...b17, ...added.result].sort()
            expect([...after.result].sort()).toEqual(b17After)
            await fylo.close(session)

            // Back to disk: native must count every document and answer alike.
            started = Date.now()
            const back = join(directory, 'back')
            writeTree(back, await fylo.dump(root))
            phases.dumpMs = Date.now() - started
            const [inspected, nativeBucket] = expectOk(
                nativeExec(back, [
                    { op: 'inspectCollection', collection: 'rows' },
                    {
                        op: 'findDocs',
                        collection: 'rows',
                        query: { $ops: [{ bucket: { $eq: 'b17' } }], $onlyIds: true }
                    }
                ]),
                'native read back'
            )
            expect(inspected.result).toMatchObject({
                docsStored: COUNT + 20,
                indexedDocs: COUNT + 20
            })
            expect([...nativeBucket.result].sort()).toEqual(b17After)
            await fylo.remove(root)
        } finally {
            rmSync(directory, { recursive: true, force: true })
            await recordEvidence(testInfo, 'large-seeded-evidence.json', { count: COUNT, phases })
        }
    })

    test('round-trips a ~1 MB document and a deeply nested one byte-exact', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(300_000)
        const root = browserRoot('large-document')
        // Multi-byte text, so a byte-level truncation or re-encoding shows.
        const unit = 'FYLO ✓ ünïcødé — 🚀 durable\n'
        const text = unit.repeat(Math.floor(960_000 / Buffer.byteLength(unit)))
        const big = { kind: 'big', text, tail: 'end' }
        let nested = { leaf: 'bottom', values: [1, 2.5, -3, true, null, 'x'] }
        for (let depth = 0; depth < 60; depth++) nested = { depth, child: nested, list: [depth] }
        const deep = { kind: 'deep', nested }
        const wide = {
            kind: 'wide',
            ...Object.fromEntries(Array.from({ length: 2_000 }, (_, k) => [`field${k}`, k]))
        }
        const request = JSON.stringify({ root, op: 'putData', collection: 'blobs', data: big })
        expect(Buffer.byteLength(request)).toBeLessThan(1024 * 1024)

        const phases = {}
        const session = await openSession(fylo, root)
        let started = Date.now()
        const [, bigId, deepId, wideId] = expectOk(
            await fylo.exec(session, [
                { op: 'createCollection', collection: 'blobs' },
                { op: 'putData', collection: 'blobs', data: big },
                { op: 'putData', collection: 'blobs', data: deep },
                { op: 'putData', collection: 'blobs', data: wide }
            ]),
            'write'
        )
        phases.writeMs = Date.now() - started
        await fylo.close(session)

        const reader = await openSession(fylo, root)
        started = Date.now()
        const [bigDoc, deepDoc, wideDoc, found] = expectOk(
            await fylo.exec(reader, [
                { op: 'getDoc', collection: 'blobs', id: bigId.result },
                { op: 'getDoc', collection: 'blobs', id: deepId.result },
                { op: 'getDoc', collection: 'blobs', id: wideId.result },
                {
                    op: 'findDocs',
                    collection: 'blobs',
                    query: { $ops: [{ kind: { $eq: 'big' } }] }
                }
            ]),
            'read'
        )
        phases.readMs = Date.now() - started
        await fylo.close(reader)
        const readBig = bigDoc.result[bigId.result]
        expect(readBig.text === text, 'the large string changed').toBe(true)
        expect(Buffer.byteLength(JSON.stringify(readBig))).toBe(
            Buffer.byteLength(JSON.stringify(big))
        )
        expect(readBig).toEqual(big)
        expect(found.result[bigId.result]).toEqual(big)
        expect(deepDoc.result[deepId.result]).toEqual(deep)
        expect(wideDoc.result[wideId.result]).toEqual(wide)
        await recordEvidence(testInfo, 'large-document-evidence.json', {
            bytes: Buffer.byteLength(JSON.stringify(big)),
            depth: 60,
            phases
        })
    })

    test('keeps many collections apart, and drops some without touching the rest', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(600_000)
        const root = browserRoot('large-collections')
        const names = Array.from({ length: 24 }, (_, k) => `shelf${String(k).padStart(2, '0')}`)
        let started = Date.now()
        const session = await openSession(fylo, root)
        expectOk(
            await fylo.exec(
                session,
                names.flatMap((collection, k) => [
                    { op: 'createCollection', collection },
                    { op: 'putData', collection, data: body(k) }
                ])
            ),
            'write'
        )
        const writeMs = Date.now() - started
        await fylo.close(session)

        const reader = await openSession(fylo, root)
        started = Date.now()
        const reads = expectOk(
            await fylo.exec(
                reader,
                names.map((collection) => ({ op: 'findDocs', collection, query: {} }))
            ),
            'read'
        )
        const readMs = Date.now() - started
        for (const [k, frame] of reads.entries()) {
            expect(Object.values(frame.result), names[k]).toEqual([body(k)])
        }
        const dropped = names.filter((_, k) => k % 3 === 0)
        expectOk(
            await fylo.exec(
                reader,
                dropped.map((collection) => ({ op: 'dropCollection', collection }))
            ),
            'drop'
        )
        const afterDrop = await fylo.exec(
            reader,
            names.map((collection) => ({ op: 'findDocs', collection, query: {} }))
        )
        for (const [k, frame] of afterDrop.entries()) {
            if (k % 3 === 0) {
                expect(frame.ok, `${names[k]} survived its drop`).toBe(false)
            } else {
                expect(frame.ok, JSON.stringify(frame.error)).toBe(true)
                expect(Object.values(frame.result)).toEqual([body(k)])
            }
        }
        await fylo.close(reader)
        await recordEvidence(testInfo, 'large-collections-evidence.json', {
            collections: names.length,
            writeMs,
            readMs
        })
    })

    test('pages through a query result inside one batch', async ({ fylo }) => {
        test.setTimeout(180_000)
        const root = browserRoot('large-page')
        const session = await openSession(fylo, root)
        expectOk(
            await fylo.exec(session, [
                { op: 'createCollection', collection: 'rows' },
                { op: 'batchPutData', collection: 'rows', batch: bodies(0, 7) }
            ])
        )
        // The handshake advertises `queryPagination`, so a first page must
        // come back, not trap the module.
        const answer = await fylo.page.evaluate(
            ([session]) =>
                globalThis.fylo.exec(session, [
                    { op: 'findDocs', collection: 'rows', query: {}, page: { limit: 3 } }
                ]),
            [session]
        )
        expect(answer.error, 'the engine Worker failed the batch').toBeUndefined()
        const [page] = expectOk(answer.frames, 'first page')
        expect(Object.keys(page.result.items)).toHaveLength(3)
        expect(page.result.page).toEqual({ count: 3, limit: 3 })
        expect(typeof page.result.nextCursor).toBe('string')
        // And the session is still usable afterwards.
        const [all] = expectOk(
            await fylo.exec(session, [{ op: 'findDocs', collection: 'rows', query: {} }])
        )
        expect(Object.keys(all.result)).toHaveLength(7)
        await fylo.close(session)
    })

    test('refuses an oversized frame cleanly and stores a file larger than the bridge area', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(600_000)
        const root = browserRoot('large-limits')
        const UNDER = 16 * 1024 * 1024
        const OVER = 66 * 1024 * 1024
        await writeHarnessFile(fylo, root, 'incoming/under.bin', { size: UNDER, seed: 11 })
        await writeHarnessFile(fylo, root, 'incoming/over.bin', { size: OVER, seed: 13 })
        await writeHarnessFile(fylo, root, SOURCE, { size: SOURCE_BYTES, seed: 7 })
        const session = await openSession(fylo, root)
        const [, , kept] = expectOk(
            await fylo.exec(session, [
                { op: 'createCollection', collection: 'notes' },
                { op: 'createCollection', collection: 'blobs', kind: 'file' },
                {
                    op: 'putData',
                    collection: 'blobs',
                    file: { path: `${root}/incoming/under.bin` }
                },
                { op: 'putData', collection: 'notes', data: body(0) }
            ]),
            'setup'
        )

        // Frames past the request limit, built in the page: 2 MiB and 65 MiB.
        // Each must fail alone, and the frame after it must still run.
        const frames = await fylo.page.evaluate(
            async ([session]) => {
                const answers = []
                for (const size of [2 * 1024 * 1024, 65 * 1024 * 1024]) {
                    const started = performance.now()
                    const answer = await globalThis.fylo.exec(session, [
                        { op: 'putData', collection: 'notes', data: { blob: 'x'.repeat(size) } },
                        { op: 'putData', collection: 'notes', data: { i: 1, after: size } }
                    ])
                    answers.push({
                        error: answer.error,
                        ms: performance.now() - started,
                        frames: answer.frames.map((frame) => ({
                            ok: frame.ok,
                            code: frame.error?.code
                        }))
                    })
                }
                return answers
            },
            [session]
        )
        for (const answer of frames) {
            expect(answer.error).toBeUndefined()
            expect(answer.frames).toEqual([
                { ok: false, code: 'EFRAME_REQUEST_TOO_LARGE' },
                { ok: true, code: undefined }
            ])
        }

        // A raw file past the bridge's 64 MiB shared area crosses in slices.
        let started = Date.now()
        const over = await fylo.page.evaluate(
            ([session, root]) =>
                globalThis.fylo.exec(session, [
                    {
                        op: 'putData',
                        collection: 'blobs',
                        file: { path: `${root}/incoming/over.bin` }
                    }
                ]),
            [session, root]
        )
        const overMs = Date.now() - started
        expect(over.error, 'the engine Worker died instead of answering').toBeUndefined()
        expect(over.frames).toHaveLength(1)
        const [stored] = expectOk(over.frames, 'a file past the bridge area')

        // Both files verify and read back exactly.
        started = Date.now()
        const [verified, ids, out, overOut, notes, more, reverified] = expectOk(
            await fylo.exec(session, [
                { op: 'verifyCollection', collection: 'blobs' },
                { op: 'findDocs', collection: 'blobs', query: { $onlyIds: true } },
                {
                    op: 'getFileData',
                    collection: 'blobs',
                    id: kept.result,
                    path: `${root}/outgoing/under.bin`
                },
                {
                    op: 'getFileData',
                    collection: 'blobs',
                    id: stored.result,
                    path: `${root}/outgoing/over.bin`
                },
                { op: 'findDocs', collection: 'notes', query: { $ops: [{ i: { $eq: 0 } }] } },
                { op: 'putData', collection: 'blobs', file: { path: `${root}/${SOURCE}` } },
                { op: 'verifyCollection', collection: 'blobs' }
            ]),
            'after the large file'
        )
        const underMs = Date.now() - started
        expect(verified.result).toMatchObject({ filesScanned: 2, verified: 2, corrupt: [] })
        expect([...ids.result].sort()).toEqual([kept.result, stored.result].sort())
        expect(typeof more.result).toBe('string')
        expect(reverified.result).toMatchObject({ filesScanned: 3, verified: 3, corrupt: [] })
        const expected = sha256(pattern(UNDER, 11))
        expect(out.result).toMatchObject({ contentLength: UNDER, checksumSHA256: expected })
        expect(await digest(fylo, root, 'outgoing/under.bin')).toEqual({
            size: UNDER,
            sha256: expected
        })
        const overExpected = sha256(pattern(OVER, 13))
        expect(overOut.result).toMatchObject({ contentLength: OVER, checksumSHA256: overExpected })
        expect(await digest(fylo, root, 'outgoing/over.bin')).toEqual({
            size: OVER,
            sha256: overExpected
        })
        expect(Object.values(notes.result)).toEqual([body(0)])
        await fylo.close(session)
        await recordEvidence(testInfo, 'large-limits-evidence.json', {
            oversizedFrames: frames,
            overFile: {
                bytes: OVER,
                ms: overMs,
                hostError: over.hostError
            },
            underFile: { bytes: UNDER, ms: underMs }
        })
    })
})

test.describe('storage', () => {
    test('the storage estimate grows with writes and shrinks after dropCollection', async ({
        fylo
    }, testInfo) => {
        test.setTimeout(300_000)
        const root = browserRoot('storage')
        const BYTES = 8 * 1024 * 1024
        const MEANINGFUL = BYTES / 2
        const usage = async () => (await fylo.estimate()).usage
        // Estimates are refreshed lazily; read until the number settles.
        const settledUsage = async (predicate) => {
            let value = await usage()
            for (let attempt = 0; attempt < 25 && !predicate(value); attempt++) {
                await fylo.page.waitForTimeout(200)
                value = await usage()
            }
            return value
        }

        await writeHarnessFile(fylo, root, 'incoming/payload.bin', { size: BYTES, seed: 5 })
        const session = await openSession(fylo, root)
        expectOk(
            await fylo.exec(session, [
                { op: 'createCollection', collection: 'blobs', kind: 'file' }
            ])
        )
        const before = await usage()
        expectOk(
            await fylo.exec(session, [
                {
                    op: 'putData',
                    collection: 'blobs',
                    file: { path: `${root}/incoming/payload.bin` }
                }
            ])
        )
        const written = await settledUsage((value) => value - before >= MEANINGFUL)
        expectOk(await fylo.exec(session, [{ op: 'dropCollection', collection: 'blobs' }]))
        await fylo.close(session)
        const dropped = await settledUsage((value) => written - value >= MEANINGFUL)
        const evidence = { before, written, dropped, bytes: BYTES }
        await recordEvidence(testInfo, 'storage-evidence.json', evidence)
        if (written - before < MEANINGFUL) {
            // This browser's estimate is too coarse to see an 8 MiB write.
            testInfo.annotations.push({
                type: 'coarse-estimate',
                description: JSON.stringify(evidence)
            })
            return
        }
        expect(written - dropped, JSON.stringify(evidence)).toBeGreaterThanOrEqual(MEANINGFUL)
        await fylo.remove(root)
    })
})
