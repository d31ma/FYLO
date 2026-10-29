// Encryption and the size and startup budgets of fylo-browser.wasm, measured
// in the browser that runs it.
//
// The browser module is built with `RootConfig::default()` and takes no
// configuration (crates/fylo-wasm/src/boundary.rs), so it has no way to hold
// `fylo.encryption.key` or `fylo.cipher.salt`. Field encryption is declared by
// a schema's `$encrypted` list, natively, with keys. Whatever the browser is
// handed, an encrypted field must fail closed: no plaintext, no ciphertext
// passed off as data, and no plaintext written where ciphertext belongs.

import {
    browserRoot,
    comparable,
    expect,
    expectOk,
    nativeBinary,
    nativeExec,
    nativeRoot,
    readTree,
    test,
    writeTree
} from './engine-harness.mjs'
import { spawnSync } from 'node:child_process'
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { gunzipSync, gzipSync } from 'node:zlib'

const PLAIN_ORIGIN = 'http://127.0.0.1:4174'
const FIXTURES = 'target/browser-fixtures/engine-security'
const KEY = 'e2e-security-key-at-least-thirty-two-bytes'
const SALT = 'e2e-security-salt'
const CIPHERTEXT = /v2\.[A-Za-z0-9_-]{16,}/
// Every secret value the fixtures hold or the browser tries to write starts
// with one of these, so a leak shows up in an answer or a file as plain text.
const SECRET = /900-00-|4111-00|911-11-/

const PEOPLE = [
    { name: 'Ada', ssn: '900-00-0001', card: { number: '4111-0001', brand: 'visa' } },
    { name: 'Bo', ssn: '900-00-0002', card: { number: '4111-0002', brand: 'amex' } },
    { name: 'Cy', ssn: '900-00-0003', card: { number: '4111-0003', brand: 'visa' } }
]

/**
 * A root with an encrypted `pii` collection, written natively with keys,
 * and a plain `public` collection beside it.
 *
 * With `registry`, the schema is pushed into the root (`autoPush`), so a
 * process without the schema folder still knows which fields are encrypted.
 * Without it, only the folder knows, which is the default whenever the
 * collection is created after the folder's one-time import.
 */
function encryptedRoot(label, { registry }) {
    const work = nativeRoot(`security-${label}`)
    const schemas = join(work, 'schemas')
    mkdirSync(join(schemas, 'pii', 'history'), { recursive: true })
    writeFileSync(join(schemas, 'pii', 'manifest.json'), '{"current":"v1","versions":[{"v":"v1"}]}')
    writeFileSync(
        join(schemas, 'pii', 'history', 'v1.schema.json'),
        JSON.stringify({ $encrypted: ['ssn', 'card/number'] })
    )
    const schema = registry ? { path: './schemas', autoPush: true } : './schemas'
    writeConfig(join(work, 'application.json'), { schema })
    writeConfig(join(work, 'keys.json'), {})
    const root = join(work, 'root')
    const written = keyedExec(join(work, 'application.json'), root, [
        { op: 'createCollection', collection: 'pii' },
        { op: 'createCollection', collection: 'public' },
        ...PEOPLE.map((data) => ({ op: 'putData', collection: 'pii', data })),
        { op: 'putData', collection: 'public', data: { name: 'open', note: 'nothing secret' } }
    ])
    expectOk(written, `${label} fixture`)
    return {
        work,
        root,
        config: join(work, 'application.json'),
        keys: join(work, 'keys.json'),
        ids: written.slice(2, 2 + PEOPLE.length).map((frame) => frame.result),
        publicId: written.at(-1).result
    }
}

function writeConfig(path, extra) {
    writeFileSync(
        path,
        JSON.stringify({
            fylo: {
                ...extra,
                encryption: { key: '${FYLO_E2E_KEY}' },
                cipher: { salt: '${FYLO_E2E_SALT}' }
            }
        })
    )
}

/** `nativeExec` with a configuration file and the keys it references. */
function keyedExec(config, root, requests) {
    const outcome = spawnSync(
        nativeBinary(),
        ['--config', config, 'exec', '--loop', '--root', root],
        {
            input: `${requests.map((request) => JSON.stringify({ root, ...request })).join('\n')}\n`,
            encoding: 'utf8',
            maxBuffer: 64 * 1024 * 1024,
            env: { ...process.env, FYLO_E2E_KEY: KEY, FYLO_E2E_SALT: SALT }
        }
    )
    if (outcome.error) throw outcome.error
    const frames = outcome.stdout
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
    if (frames.length !== requests.length) {
        throw new Error(`native answered ${frames.length} of ${requests.length}: ${outcome.stderr}`)
    }
    return frames
}

/** What the browser is asked to read from an encrypted collection. */
function readsOf(fixture) {
    return [
        ...fixture.ids.map((id) => ({ op: 'getDoc', collection: 'pii', id })),
        { op: 'getLatest', collection: 'pii', id: fixture.ids[0] },
        { op: 'findDocs', collection: 'pii', query: {} },
        { op: 'findDocs', collection: 'pii', query: { $ops: [{ name: { $eq: 'Ada' } }] } },
        { op: 'findDocs', collection: 'pii', query: { $ops: [{ ssn: { $eq: '900-00-0001' } }] } },
        { op: 'executeSQL', sql: 'SELECT * FROM pii' }
    ]
}

/** What the browser tries to write into it, every secret starting `911-11-`. */
function writesOf(fixture) {
    return [
        { op: 'putData', collection: 'pii', data: { name: 'Eve', ssn: '911-11-0001' } },
        {
            op: 'batchPutData',
            collection: 'pii',
            batch: [
                { name: 'Fay', ssn: '911-11-0002', card: { number: '911-11-0003', brand: 'visa' } }
            ]
        },
        { op: 'patchDoc', collection: 'pii', id: fixture.ids[0], newDoc: { ssn: '911-11-0004' } },
        { op: 'patchDoc', collection: 'pii', id: fixture.ids[1], newDoc: { name: 'Bob' } },
        { op: 'executeSQL', sql: "INSERT INTO pii (name, ssn) VALUES ('Gus', '911-11-0005')" },
        { op: 'executeSQL', sql: "UPDATE pii SET ssn = '911-11-0006' WHERE name = 'Cy'" }
    ]
}

/** Assert an answer refused with `code` and carries neither secret nor ciphertext. */
function expectSealed(frame, code, label) {
    const text = JSON.stringify(frame)
    expect(frame.ok, `${label}: ${frame.op} must fail closed: ${text.slice(0, 300)}`).toBe(false)
    expect(frame.error?.code, `${label}: ${frame.op}`).toBe(code)
    expect(text, `${label}: ${frame.op} leaked a secret`).not.toMatch(SECRET)
    expect(text, `${label}: ${frame.op} leaked ciphertext`).not.toMatch(CIPHERTEXT)
}

function expectNoSecretInTree(files, label) {
    for (const [path, content] of Object.entries(files)) {
        const text = Buffer.from(content, 'base64').toString('latin1')
        expect(text, `${label}: ${path} holds a secret in plaintext`).not.toMatch(SECRET)
    }
}

/** Run the same frames through the harness page and through keyless native. */
async function browserAndNative(fylo, fixture, requests) {
    const root = browserRoot('security')
    const files = readTree(fixture.root)
    await fylo.seed(root, files)
    try {
        const browser = await fylo.once(root, requests)
        const dump = await fylo.dump(root)
        const copy = nativeRoot('security-keyless')
        cpSync(fixture.root, copy, { recursive: true })
        try {
            return { browser, native: nativeExec(copy, requests), seeded: files, dump }
        } finally {
            rmSync(copy, { recursive: true, force: true })
        }
    } finally {
        await fylo.remove(root)
    }
}

function expectSameOutcomes(browser, native, label) {
    const shape = (frame) =>
        frame.ok
            ? { op: frame.op, ok: true, result: comparable(frame).result }
            : { op: frame.op, ok: false, code: frame.error?.code }
    const first = browser.findIndex(
        (frame, index) => !isDeepStrictEqual(shape(frame), shape(native[index]))
    )
    expect(
        first,
        first === -1
            ? ''
            : `${label}: frame ${first} differs\nbrowser: ${JSON.stringify(browser[first])}\nnative:  ${JSON.stringify(native[first])}`
    ).toBe(-1)
}

let registryFixture
let folderFixture

test.beforeAll(() => {
    registryFixture = encryptedRoot('registry', { registry: true })
    folderFixture = encryptedRoot('folder', { registry: false })
})

test.afterAll(() => {
    for (const fixture of [registryFixture, folderFixture]) {
        if (fixture) rmSync(fixture.work, { recursive: true, force: true })
    }
})

test.describe('encrypted directory roots', () => {
    test('a registry-declared encrypted collection fails closed for every read and write', async ({
        fylo
    }, testInfo) => {
        const fixture = registryFixture
        const reads = readsOf(fixture)
        const writes = writesOf(fixture)
        const publicReads = [
            { op: 'getDoc', collection: 'public', id: fixture.publicId },
            { op: 'findDocs', collection: 'public', query: {} }
        ]
        const requests = [...reads, ...writes, ...publicReads]
        const { browser, native, seeded, dump } = await browserAndNative(fylo, fixture, requests)
        await testInfo.attach('encrypted-registry-root', {
            body: JSON.stringify(browser, null, 2),
            contentType: 'application/json'
        })

        // Native without keys answers the same: the browser is no weaker.
        expectSameOutcomes(browser, native, 'browser and keyless native')
        browser
            .slice(0, reads.length)
            .forEach((frame) => expectSealed(frame, 'EENGINE_ENCRYPTION', 'read'))
        browser
            .slice(reads.length, reads.length + writes.length)
            .forEach((frame) => expectSealed(frame, 'EENGINE_ENCRYPTION', 'write'))
        // Encryption is per collection: the plain one beside it still reads.
        expectOk(browser.slice(reads.length + writes.length), 'public collection')

        // Nothing the browser was asked to write reached the root.
        expectNoSecretInTree(dump, 'browser root')
        const documents = (files) =>
            Object.fromEntries(
                Object.entries(files).filter(([path]) => path.startsWith('.collections/pii/docs/'))
            )
        expect(documents(dump)).toEqual(documents(seeded))
        const back = nativeRoot('security-back')
        try {
            writeTree(back, dump)
            const [all] = expectOk(
                keyedExec(fixture.keys, back, [{ op: 'findDocs', collection: 'pii', query: {} }])
            )
            expect(Object.values(all.result).map(({ _v, ...person }) => person)).toEqual(PEOPLE)
        } finally {
            rmSync(back, { recursive: true, force: true })
        }
    })

    test('a folder-declared encrypted collection refuses reads without leaking', async ({
        fylo
    }) => {
        const fixture = folderFixture
        const reads = readsOf(fixture)
        const { browser, native } = await browserAndNative(fylo, fixture, reads)
        expectSameOutcomes(browser, native, 'browser and keyless native')
        // Without the folder the browser cannot know the fields, but the
        // ciphertext it finds makes it refuse rather than hand it out. A query
        // on an encrypted value matches nothing, since only ciphertext is
        // indexed, and may say so.
        for (const frame of browser) {
            if (frame.ok && frame.op === 'findDocs') {
                expect(frame.result, 'an empty answer, not data').toEqual({})
                continue
            }
            expectSealed(frame, 'EENGINE_ENCRYPTION', 'read')
        }
    })

    test('a folder-declared encrypted collection gets no plaintext secrets from the browser', async ({
        fylo
    }, testInfo) => {
        // Known engine bug, native too: without `autoPush` a collection made
        // after the folder's one-time import never gets its `$encrypted` list
        // into the registry, so a keyless writer cannot tell it holds secrets
        // and stores new ones in the clear. Remove this once that is fixed.
        test.fail(true, 'keyless writes store plaintext in a folder-declared encrypted collection')
        const fixture = folderFixture
        const writes = writesOf(fixture)
        const { browser, dump } = await browserAndNative(fylo, fixture, writes)
        await testInfo.attach('encrypted-folder-writes', {
            body: JSON.stringify(browser, null, 2),
            contentType: 'application/json'
        })
        // A writer that cannot tell which fields are encrypted, in a
        // collection that holds ciphertext, has to refuse the write: the
        // alternative is a secret stored in the clear beside encrypted ones.
        for (const index of [0, 1, 2, 4, 5]) {
            const frame = browser[index]
            expect(
                frame.ok,
                `${frame.op} stored a secret in plaintext: ${JSON.stringify(frame)}`
            ).toBe(false)
        }
        expectNoSecretInTree(dump, 'browser root')
    })
})

test.describe('encrypted .fxdb databases', () => {
    let packed
    let forged

    test.beforeAll(() => {
        mkdirSync(FIXTURES, { recursive: true })
        // `fylo pack` carries the schema registry and the ciphertext into a
        // database file as they are.
        packed = join(FIXTURES, 'encrypted-fields.fxdb')
        rmSync(packed, { force: true })
        const pack = spawnSync(nativeBinary(), ['pack', registryFixture.root, '--out', packed], {
            encoding: 'utf8'
        })
        expect(pack.status, pack.stderr).toBe(0)
        // A whole-file encrypted database, sealed by `fylo pack --encrypt`
        // from a plain root: every record frame is sealed with the key.
        const plain = join(nativeRoot('security-sealed'), 'root')
        expectOk(
            nativeExec(plain, [
                { op: 'createCollection', collection: 'pii' },
                ...PEOPLE.map((data) => ({ op: 'putData', collection: 'pii', data }))
            ])
        )
        forged = join(FIXTURES, 'encrypted-file.fxdb')
        rmSync(forged, { force: true })
        const sealed = spawnSync(
            nativeBinary(),
            ['--config', registryFixture.keys, 'pack', plain, '--out', forged, '--encrypt'],
            {
                encoding: 'utf8',
                env: { ...process.env, FYLO_E2E_KEY: KEY, FYLO_E2E_SALT: SALT }
            }
        )
        expect(sealed.status, sealed.stderr).toBe(0)
        const bytes = readFileSync(forged)
        expect(sealedFrames(bytes), 'pack --encrypt seals the record frames').toBeGreaterThan(0)
        expect(bytes.toString('latin1')).not.toMatch(SECRET)
        expectOk(
            keyedExec(registryFixture.keys, forged, [
                { op: 'findDocs', collection: 'pii', query: {} }
            ]),
            'the key opens the sealed database'
        )
    })

    test('field-encrypted documents in a database file refuse every read', async ({
        page
    }, testInfo) => {
        const harness = await openFxdb(page)
        const reads = readsOf(registryFixture)
        const native = nativeExec(join(FIXTURES, copyOf(packed, 'keyless-reads')), reads)
        const file = await harness.open({ kind: 'file', url: `/${packed}` })
        try {
            const frames = await harness.exec(file.id, reads)
            await testInfo.attach('encrypted-fields-fxdb-reads', {
                body: JSON.stringify(frames, null, 2),
                contentType: 'application/json'
            })
            expectSameOutcomes(frames, native, 'picked file and keyless native')
            frames.forEach((frame) => expectSealed(frame, 'EENGINE_ENCRYPTION', 'picked file'))
        } finally {
            await harness.close(file.id)
        }
    })

    test('a working copy of a field-encrypted database takes no plaintext secrets', async ({
        page
    }, testInfo) => {
        const harness = await openFxdb(page)
        const reason = await harness.opfs()
        test.skip(reason !== '', reason)
        // Known engine bug, native too: on a .fxdb a keyless SQL UPDATE writes
        // plaintext into a registry-declared encrypted field, which the same
        // statement on a directory root refuses. Remove this once fixed.
        test.fail(true, 'keyless SQL UPDATE writes plaintext into an encrypted .fxdb field')
        const fixture = registryFixture
        const before = expectOk(
            keyedExec(fixture.keys, packed, [{ op: 'findDocs', collection: 'pii', query: {} }]),
            'native with keys'
        )
        const writes = writesOf(fixture)
        const native = nativeExec(join(FIXTURES, copyOf(packed, 'keyless-writes')), writes)
        const copy = await harness.open({
            kind: 'copy',
            label: 'encrypted.fxdb',
            url: `/${packed}`
        })
        try {
            const frames = await harness.exec(copy.id, writes)
            await testInfo.attach('encrypted-fields-fxdb-writes', {
                body: JSON.stringify(frames, null, 2),
                contentType: 'application/json'
            })
            // The browser does what keyless native does, then must refuse
            // every write: none can be encrypted without the key.
            expectSameOutcomes(frames, native, 'working copy and keyless native')
            const saved = join(FIXTURES, 'encrypted-fields.after.fxdb')
            writeFileSync(saved, await harness.snapshot(copy.id))
            const after = keyedExec(fixture.keys, saved, [
                { op: 'findDocs', collection: 'pii', query: {} }
            ])
            frames.forEach((frame) => expectSealed(frame, 'EENGINE_ENCRYPTION', 'working copy'))
            expect(after.map(comparable), 'nothing the browser tried was stored').toEqual(
                before.map(comparable)
            )
        } finally {
            await harness.close(copy.id)
            await harness.removeCopy(copy.name)
        }
    })

    test('a whole-file encrypted database is refused as native refuses it', async ({
        page
    }, testInfo) => {
        const harness = await openFxdb(page)
        const requests = [
            { op: 'inspectDatabase' },
            { op: 'verifyDatabase' },
            { op: 'findDocs', collection: 'pii', query: {} },
            { op: 'putData', collection: 'pii', data: { name: 'Eve', ssn: '911-11-0007' } }
        ]
        const native = nativeExec(join(FIXTURES, copyOf(forged, 'native')), requests)
        expect(native[0].result).toMatchObject({ encrypted: true, locked: true })
        const original = readFileSync(forged)
        const file = await harness.open({ kind: 'file', url: `/${forged}` })
        try {
            const frames = await harness.exec(file.id, requests)
            await testInfo.attach('encrypted-file-fxdb', {
                body: JSON.stringify(frames, null, 2),
                contentType: 'application/json'
            })
            expectSameOutcomes(frames, native, 'browser and native')
            expect(frames[0].result).toMatchObject({ encrypted: true, locked: true })
            for (const frame of frames.slice(2)) {
                expect(frame.ok).toBe(false)
                expect(JSON.stringify(frame)).not.toMatch(SECRET)
            }
            expect(Buffer.compare(await harness.snapshot(file.id), original)).toBe(0)
        } finally {
            await harness.close(file.id)
        }
    })
})

test('encryption is off unless configured: native roots and databases carry no markers', () => {
    const requests = [
        { op: 'createCollection', collection: 'people' },
        ...PEOPLE.map((data) => ({ op: 'putData', collection: 'people', data }))
    ]
    const directory = nativeRoot('security-plain')
    const holder = nativeRoot('security-plain-db')
    const database = join(holder, 'plain.fxdb')
    try {
        expectOk(nativeExec(directory, requests))
        expectOk(nativeExec(database, requests))
        expectPlainTree(readTree(directory), 'native root')
        const bytes = readFileSync(database)
        expectPlainSuperblocks(bytes)
        const [inspected] = expectOk(nativeExec(database, [{ op: 'inspectDatabase' }]))
        expect(inspected.result.encrypted).toBe(false)
        expect(sealedFrames(bytes)).toBe(0)
    } finally {
        rmSync(directory, { recursive: true, force: true })
        rmSync(holder, { recursive: true, force: true })
    }
})

test('a root the browser writes is plain', async ({ fylo }) => {
    const root = browserRoot('plain')
    try {
        expectOk(
            await fylo.once(root, [
                { op: 'createCollection', collection: 'people' },
                ...PEOPLE.map((data) => ({ op: 'putData', collection: 'people', data }))
            ]),
            'browser writes'
        )
        expectPlainTree(await fylo.dump(root), 'browser root')
    } finally {
        await fylo.remove(root)
    }
})

test('a database the browser writes is plain', async ({ page }) => {
    const harness = await openFxdb(page)
    const reason = await harness.opfs()
    test.skip(reason !== '', reason)
    mkdirSync(FIXTURES, { recursive: true })
    const database = join(FIXTURES, 'plain.fxdb')
    rmSync(database, { force: true })
    expectOk(nativeExec(database, [{ op: 'createCollection', collection: 'people' }]))
    const opened = await harness.open({ kind: 'copy', label: 'plain.fxdb', url: `/${database}` })
    try {
        expectOk(
            await harness.exec(
                opened.id,
                PEOPLE.map((data) => ({ op: 'putData', collection: 'people', data }))
            ),
            'browser writes'
        )
        const bytes = await harness.snapshot(opened.id)
        expectPlainSuperblocks(bytes)
        expect(sealedFrames(bytes)).toBe(0)
        const saved = join(FIXTURES, 'plain.after.fxdb')
        writeFileSync(saved, bytes)
        const [inspected, all] = expectOk(
            nativeExec(saved, [
                { op: 'inspectDatabase' },
                { op: 'findDocs', collection: 'people', query: {} }
            ])
        )
        expect(inspected.result).toMatchObject({ encrypted: false, locked: false })
        expect(Object.values(all.result)).toEqual(PEOPLE)
    } finally {
        await harness.close(opened.id)
        await harness.removeCopy(opened.name)
    }
})

test('the engine module stays within its size and startup budgets', async ({
    page,
    browserName
}, testInfo) => {
    const budgets = readBudgets()
    const onDisk = readFileSync('dist-web/fylo-browser.wasm')
    const gzipBytes = gzipSync(onDisk, { level: 9 }).byteLength
    expect(onDisk.byteLength).toBeLessThanOrEqual(budgets.MAX_ENGINE_WASM_BYTES)
    expect(gzipBytes).toBeLessThanOrEqual(budgets.MAX_ENGINE_WASM_GZIP_BYTES)

    // Fetched and compiled by the browser on the isolated origin, as the
    // folder engine loads it.
    await page.goto('/tests/browser/fixtures/engine-harness.html', {
        waitUntil: 'domcontentloaded'
    })
    await expect(page.locator('[data-status="ready"]')).toBeVisible()
    const cost = await page.evaluate(() => globalThis.fylo.moduleCost())
    expect(cost.bytes).toBe(onDisk.byteLength)
    expect(cost.contentType).toBe('application/wasm')

    // Its imports are exactly the `fylo_host` table: no WASI, no `env`, no
    // binding-generator glue an embedder would have to ship.
    const table = hostTable()
    expect(cost.imports.filter((name) => !name.startsWith('fylo_host.'))).toEqual([])
    expect([...cost.imports].sort()).toEqual(table.map((name) => `fylo_host.${name}`).sort())
    expect(cost.exports).toEqual(
        expect.arrayContaining([
            'memory',
            'fylo_abi_version',
            'fylo_alloc',
            'fylo_free',
            'fylo_exec'
        ])
    )

    // On the plain origin, the single-file Worker: start, instantiate, then
    // the first answer.
    const harness = await openFxdb(page)
    mkdirSync(FIXTURES, { recursive: true })
    const database = join(FIXTURES, 'startup.fxdb')
    rmSync(database, { force: true })
    expectOk(nativeExec(database, [{ op: 'createCollection', collection: 'people' }]))
    const startup = await page.evaluate((url) => globalThis.fxdb.startup({ url }), `/${database}`)
    expect(startup.error).toBeUndefined()
    expect(startup.handshake.ok).toBe(true)
    expect(startup.handshake.result.buildTarget).toBe('browser-wasm32')
    const plainType = await page.evaluate(async () => {
        const response = await fetch('/dist-web/fylo-browser.wasm', {
            method: 'HEAD',
            cache: 'no-store'
        })
        return response.headers.get('content-type')
    })
    expect(plainType).toBe('application/wasm')

    const evidence = {
        browser: browserName,
        budgets,
        bytes: onDisk.byteLength,
        gzipBytes,
        fetchMs: cost.fetchMs,
        compileMs: cost.compileMs,
        openMs: startup.openMs,
        firstHandshakeMs: startup.firstHandshakeMs,
        secondHandshakeMs: startup.secondHandshakeMs,
        imports: cost.imports.length
    }
    await testInfo.attach('engine-module-budget', {
        body: `${JSON.stringify(evidence, null, 2)}\n`,
        contentType: 'application/json'
    })
    // Ceilings, not targets: generous enough for a loaded CI machine, tight
    // enough to catch a module that stopped compiling lazily or a start that
    // began doing real work.
    const ceiling = CEILINGS[browserName]
    expect(cost.compileMs).toBeLessThan(ceiling.compileMs)
    expect(startup.openMs).toBeLessThan(ceiling.openMs)
    expect(startup.firstHandshakeMs).toBeLessThan(ceiling.handshakeMs)
})

const CEILINGS = {
    chromium: { compileMs: 10_000, openMs: 15_000, handshakeMs: 5_000 },
    firefox: { compileMs: 10_000, openMs: 15_000, handshakeMs: 5_000 },
    webkit: { compileMs: 20_000, openMs: 30_000, handshakeMs: 10_000 }
}

/** The budget constants of scripts/verify-browser-wasm-budgets.mjs, read from its source. */
function readBudgets() {
    const source = readFileSync('scripts/verify-browser-wasm-budgets.mjs', 'utf8')
    const budgets = {}
    for (const name of ['MAX_ENGINE_WASM_BYTES', 'MAX_ENGINE_WASM_GZIP_BYTES']) {
        const match = source.match(new RegExp(`const ${name} = ([\\d\\s*]+)\\n`))
        expect(match, `${name} in the budget script`).not.toBeNull()
        budgets[name] = match[1]
            .split('*')
            .map((factor) => Number(factor.trim()))
            .reduce((product, factor) => product * factor, 1)
    }
    return budgets
}

/** The functions the module imports from `fylo_host`, as boundary.rs declares them. */
function hostTable() {
    const source = readFileSync('crates/fylo-wasm/src/boundary.rs', 'utf8')
    const block = source.match(
        /#\[link\(wasm_import_module = "fylo_host"\)\]\s*unsafe extern "C" \{([\s\S]*?)\n\}/
    )
    expect(block, 'the fylo_host block in boundary.rs').not.toBeNull()
    const names = [...block[1].matchAll(/\bfn (\w+)\(/g)].map((match) => match[1])
    expect(names.length).toBeGreaterThan(10)
    return names
}

const SUPERBLOCK_BYTES = 4096
const INCOMPAT_ENCRYPTED = 1

/** Both superblock copies: the encrypted bit clear, no salt, no key check. */
function expectPlainSuperblocks(bytes) {
    for (const base of [0, SUPERBLOCK_BYTES]) {
        expect(bytes.readUInt32LE(base + 20) & INCOMPAT_ENCRYPTED, `superblock at ${base}`).toBe(0)
        expect(bytes.subarray(base + 80, base + 128).every((byte) => byte === 0)).toBe(true)
    }
}

/** Record frames whose stored body says it is sealed (header flag 0x20). */
function sealedFrames(bytes) {
    const extent = 2 ** bytes[24]
    let sealed = 0
    for (let at = extent; at + extent <= bytes.length; at += extent) {
        if (bytes.toString('latin1', at, at + 8) !== 'FXEXTNT\0' || bytes[at + 10] !== 2) continue
        let offset = at + 4096 + 32
        while (offset + 13 <= at + extent && bytes.readUInt32LE(offset) === 0x015259f5) {
            const length = bytes.readUInt32LE(offset + 4)
            if (length === 0 || offset + 12 + length > at + extent) break
            if (bytes[offset + 12] & 0x20) sealed++
            offset += 12 + length
        }
    }
    return sealed
}

/** No ciphertext envelope anywhere in a root, and no `$encrypted` list in its registry. */
function expectPlainTree(files, label) {
    for (const [path, content] of Object.entries(files)) {
        const text = Buffer.from(content, 'base64').toString('latin1')
        expect(text, `${label}: ${path}`).not.toMatch(CIPHERTEXT)
        expect(text, `${label}: ${path}`).not.toContain('$encrypted')
    }
    const documents = Object.keys(files).filter((path) =>
        path.startsWith('.collections/people/docs/')
    )
    expect(documents.filter((path) => !path.endsWith('/')).length).toBe(PEOPLE.length)
}

/** A copy for native next to `path`, so nothing native does reaches the browser's file. */
function copyOf(path, label) {
    const name = `${path.split('/').pop()}.${label}.fxdb`
    writeFileSync(join(FIXTURES, name), readFileSync(path))
    return name
}

/** The single-file harness on the plain origin. */
async function openFxdb(page) {
    await page.goto(`${PLAIN_ORIGIN}/tests/browser/fixtures/fxdb-harness.html`, {
        waitUntil: 'domcontentloaded'
    })
    await expect(page.locator('[data-status="ready"]')).toBeVisible()
    return {
        opfs: () => page.evaluate(() => globalThis.fxdb.opfs()),
        async open(source) {
            const opened = await page.evaluate((source) => globalThis.fxdb.open(source), source)
            expect(opened.error, `open: ${opened.error}`).toBeUndefined()
            return opened
        },
        async exec(id, requests) {
            const answer = await page.evaluate(
                ([id, requests]) => globalThis.fxdb.exec(id, requests, 120_000),
                [id, requests]
            )
            expect(answer.timedOut).toBeUndefined()
            expect(answer.error, `the Worker failed: ${answer.error}`).toBeUndefined()
            expect(answer.frames.length).toBe(requests.length)
            return answer.frames
        },
        async snapshot(id) {
            const shot = await page.evaluate((id) => globalThis.fxdb.snapshot(id), id)
            return gunzipSync(Buffer.from(shot.gzipBase64, 'base64'))
        },
        close: (id) => page.evaluate((id) => globalThis.fxdb.close(id), id),
        removeCopy: (name) => page.evaluate((name) => globalThis.fxdb.removeCopy(name), name)
    }
}
