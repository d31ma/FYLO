// Protected `.vdbx` databases in the browser (ADR 0021, decision P5-5).
//
// The native CLI makes a protected database with a `file:` admin and grants
// a `webcrypto:` identity, made in the page, read and write on one
// collection. The page opens a working copy as that identity: the engine's
// Worker signs records and opens the collection key through the crypto
// bridge Worker, whose HPKE and ECDSA run on WebCrypto. Native then reads
// what the browser wrote and verifies every signature, so a wrap made by
// Rust opens in WebCrypto, a wrap made by WebCrypto opens in Rust, and a
// signature made by WebCrypto checks in Rust.
//
// The bridge blocks in `Atomics.wait`, so the page is served cross-origin
// isolated (port 4173), unlike the plain single-file specs.

import { expect, test } from './browser-test.mjs'
import { expectOk, nativeBinary } from './engine-harness.mjs'
import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { gunzipSync } from 'node:zlib'

const ISOLATED_ORIGIN = 'http://127.0.0.1:4173'
const FIXTURES = 'target/browser-fixtures/engine-access'
const CANARY = 'WOMBAT-CANARY'

/** The fylo CLI in `directory`, with no identity from the environment. */
function fylo(directory, args) {
    const env = { ...process.env }
    delete env.FYLO_IDENTITY
    delete env.FYLO_TRUST_ANCHOR
    return execFileSync(nativeBinary(), args, { cwd: directory, env, encoding: 'utf8' }).trim()
}

/** `fylo` run as the database's admin. */
function asAdmin(fixture, args) {
    return fylo(fixture.directory, [
        ...args,
        '--identity',
        'file:admin.pem',
        '--trust-anchor',
        fixture.admin
    ])
}

/** A protected `app.vdbx` with one `users` document and an empty `notes` collection. */
function protectedDatabase(worker) {
    const directory = resolve(FIXTURES, worker)
    rmSync(directory, { recursive: true, force: true })
    mkdirSync(directory, { recursive: true })
    const admin = fylo(directory, ['identity', 'create', '--store', 'file:admin.pem'])
    fylo(directory, ['create', 'app.vdbx', '--admin', 'file:admin.pem'])
    const fixture = { directory, admin, url: `/${FIXTURES}/${worker}/app.vdbx` }
    for (const collection of ['users', 'notes']) {
        asAdmin(fixture, [
            'exec',
            '--request',
            JSON.stringify({ op: 'createCollection', collection }),
            '--root',
            'app.vdbx'
        ])
    }
    fixture.userId = JSON.parse(
        asAdmin(fixture, [
            'exec',
            '--request',
            JSON.stringify({ op: 'putData', collection: 'users', data: { name: CANARY } }),
            '--root',
            'app.vdbx'
        ])
    ).result
    return fixture
}

function grant(fixture, grantee, key, actions, resource = ['--collection', 'users']) {
    asAdmin(fixture, [
        'access',
        'grant',
        'app.vdbx',
        '--grantee',
        grantee,
        '--key',
        key,
        '--actions',
        actions,
        ...resource
    ])
}

/** The single-file harness on the isolated origin. */
class AccessPage {
    static async open(page) {
        await page.goto(`${ISOLATED_ORIGIN}/tests/browser/fixtures/vdbx-harness.html`, {
            waitUntil: 'domcontentloaded'
        })
        await expect(page.locator('[data-status="ready"]')).toBeVisible()
        const harness = new AccessPage(page)
        const surface = await page.evaluate(() => globalThis.vdbx.surface())
        expect(surface.crossOriginIsolated).toBe(true)
        return harness
    }

    constructor(page) {
        this.page = page
    }

    call(name, ...args) {
        return this.page.evaluate(([name, args]) => globalThis.vdbx[name](...args), [name, args])
    }

    async createIdentity(name) {
        const created = await this.call('createIdentity', name)
        expect(created.error, `create webcrypto:${name}: ${created.error}`).toBeUndefined()
        return created
    }

    async exec(id, requests) {
        const answer = await this.call('exec', id, requests, 120_000)
        expect(answer.error, `the Worker failed: ${answer.error}`).toBeUndefined()
        expect(answer.timedOut).toBeUndefined()
        return answer.frames
    }

    async snapshot(id) {
        const shot = await this.call('snapshot', id)
        return gunzipSync(Buffer.from(shot.gzipBase64, 'base64'))
    }
}

function identityName(testInfo, label) {
    return `${label}-${testInfo.project.name}-${Date.now().toString(36)}`
}

test('a webcrypto: identity keeps non-extractable keys and opens its own HPKE wraps', async ({
    page
}, testInfo) => {
    const harness = await AccessPage.open(page)
    const name = identityName(testInfo, 'keys')
    const created = await harness.createIdentity(name)
    expect(created.publicKey).toMatch(/^fpk1:p256:[A-Za-z0-9_-]{87}$/)
    expect(created.id).toMatch(/^fid1:p256:[A-Z2-7]{52}$/)
    expect(await harness.call('identityKeys', name)).toEqual({
        sign: { type: 'private', extractable: false },
        derive: { type: 'private', extractable: false }
    })
    const again = await harness.call('createIdentity', name)
    expect(again.code).toBe('EIDENTITY_EXISTS')
    const secret = Buffer.alloc(32, 7).toString('base64')
    const info = Buffer.from('fylo test info').toString('base64')
    expect(await harness.call('hpkeRoundTrip', name, secret, info)).toBe(secret)
})

test('a granted webcrypto: identity reads and writes a protected database native then verifies', async ({
    page
}, testInfo) => {
    const fixture = protectedDatabase(`readwrite-${testInfo.project.name}`)
    const harness = await AccessPage.open(page)
    const name = identityName(testInfo, 'writer')
    const identity = await harness.createIdentity(name)
    grant(fixture, 'browser', identity.publicKey, 'read,write')

    const opened = await harness.call('open', {
        kind: 'copy',
        url: fixture.url,
        label: `access-${testInfo.project.name}.vdbx`,
        identity: { name, trustAnchor: fixture.admin }
    })
    expect(opened.error, `open: ${opened.error} (${opened.code})`).toBeUndefined()
    expect(opened.info.identity).toBe(identity.id)

    const [read, written, refused] = await harness.exec(opened.id, [
        { op: 'getDoc', collection: 'users', id: fixture.userId },
        { op: 'putData', collection: 'users', data: { name: 'from the browser' } },
        { op: 'putData', collection: 'notes', data: { title: 'not granted' } }
    ])
    expectOk([read, written])
    expect(JSON.stringify(read.result)).toContain(CANARY)
    expect(refused.ok).toBe(false)
    expect(refused.error.code).toBe('EACCES')

    const bytes = await harness.snapshot(opened.id)
    await harness.call('close', opened.id)
    await harness.call('removeCopy', opened.name)
    expect(bytes.includes(Buffer.from(CANARY))).toBe(false)
    expect(bytes.includes(Buffer.from('from the browser'))).toBe(false)

    writeFileSync(join(fixture.directory, 'app.vdbx'), bytes)
    const verified = JSON.parse(asAdmin(fixture, ['verify', 'app.vdbx']))
    expect(verified.ok, JSON.stringify(verified)).toBe(true)
    const document = JSON.parse(
        asAdmin(fixture, ['get', 'users', written.result, '--root', 'app.vdbx', '--json'])
    )
    expect(JSON.stringify(document)).toContain('from the browser')
})

test('a collection a webcrypto: admin creates opens natively with the key WebCrypto wrapped', async ({
    page
}, testInfo) => {
    const fixture = protectedDatabase(`admin-${testInfo.project.name}`)
    const harness = await AccessPage.open(page)
    const name = identityName(testInfo, 'admin')
    const identity = await harness.createIdentity(name)
    grant(fixture, 'browser-admin', identity.publicKey, 'admin', ['--database'])

    const opened = await harness.call('open', {
        kind: 'copy',
        url: fixture.url,
        label: `access-admin-${testInfo.project.name}.vdbx`,
        identity: { name, trustAnchor: fixture.admin }
    })
    expect(opened.error, `open: ${opened.error} (${opened.code})`).toBeUndefined()
    const [created, written] = expectOk(
        await harness.exec(opened.id, [
            { op: 'createCollection', collection: 'drafts' },
            { op: 'putData', collection: 'drafts', data: { title: 'wrapped in the browser' } }
        ])
    )
    expect(created.ok).toBe(true)
    const bytes = await harness.snapshot(opened.id)
    await harness.call('close', opened.id)
    await harness.call('removeCopy', opened.name)

    writeFileSync(join(fixture.directory, 'app.vdbx'), bytes)
    const document = JSON.parse(
        asAdmin(fixture, ['get', 'drafts', written.result, '--root', 'app.vdbx', '--json'])
    )
    expect(JSON.stringify(document)).toContain('wrapped in the browser')
    const verified = JSON.parse(asAdmin(fixture, ['verify', 'app.vdbx']))
    expect(verified.ok, JSON.stringify(verified)).toBe(true)
})

test('without an identity, or with one that has no grant, a protected database stays locked', async ({
    page
}, testInfo) => {
    const fixture = protectedDatabase(`locked-${testInfo.project.name}`)
    const harness = await AccessPage.open(page)

    const plain = await harness.call('open', { kind: 'url', url: fixture.url })
    expect(plain.error).toBeUndefined()
    const [locked] = await harness.exec(plain.id, [
        { op: 'getDoc', collection: 'users', id: fixture.userId }
    ])
    expect(locked.ok).toBe(false)
    expect(locked.error.code).toBe('ENOIDENTITY')
    await harness.call('close', plain.id)

    const name = identityName(testInfo, 'stranger')
    await harness.createIdentity(name)
    const stranger = await harness.call('open', {
        kind: 'url',
        url: fixture.url,
        identity: { name, trustAnchor: fixture.admin }
    })
    expect(stranger.error).toBeUndefined()
    const [refused] = await harness.exec(stranger.id, [
        { op: 'getDoc', collection: 'users', id: fixture.userId }
    ])
    expect(refused.ok).toBe(false)
    expect(JSON.stringify(refused.error)).not.toContain(CANARY)
    await harness.call('close', stranger.id)

    const missing = await harness.call('open', {
        kind: 'url',
        url: fixture.url,
        identity: { name: identityName(testInfo, 'missing'), trustAnchor: fixture.admin }
    })
    expect(missing.code).toBe('ENOIDENTITY')
})

const ACCESS_SLOT_MAGIC = Buffer.from('VDXACCS\0', 'latin1')
const ACCESS_SLOT_HEADER_BYTES = 24

/**
 * Make the newest access slot copy of `file` fail its checksum, as a power
 * cut tearing the last slot write of a revocation leaves it: the older copy
 * still holds the previous catalog key, so the rotation stays pending.
 */
function tearNewestAccessSlot(file) {
    const bytes = readFileSync(file)
    const first = bytes.indexOf(ACCESS_SLOT_MAGIC)
    const second = bytes.indexOf(ACCESS_SLOT_MAGIC, first + 1)
    expect(second).toBeGreaterThan(first)
    const newest =
        bytes.readBigUInt64LE(second + 8) > bytes.readBigUInt64LE(first + 8) ? second : first
    bytes[newest + ACCESS_SLOT_HEADER_BYTES] ^= 0xff
    writeFileSync(file, bytes)
}

/** Open `url` as the webcrypto: identity `name`, which the admin anchors. */
async function openAs(harness, fixture, url, name, label) {
    const opened = await harness.call('open', {
        kind: 'copy',
        url,
        label,
        identity: { name, trustAnchor: fixture.admin }
    })
    expect(opened.error, `open: ${opened.error} (${opened.code})`).toBeUndefined()
    return opened
}

/**
 * Read the canary document and verify the file, then close the copy and
 * remove it. The browser's session has no root of its own, so its
 * handshake names no database; `verifyDatabase` is what reports a pending
 * rotation there.
 */
async function readCanary(harness, opened, fixture) {
    const [read, verified] = await harness.exec(opened.id, [
        { op: 'getDoc', collection: 'users', id: fixture.userId },
        { op: 'verifyDatabase' }
    ])
    await harness.call('close', opened.id)
    await harness.call('removeCopy', opened.name)
    expectOk([read, verified])
    expect(JSON.stringify(read.result)).toContain(CANARY)
    return verified.result.problems.map((problem) => problem.code)
}

/**
 * A protected database where the browser identity `name` holds
 * `browserGrant` (reading `users` by default) and a second identity was
 * granted and then revoked, which rotates the catalog key.
 */
async function revokedFixture(harness, testInfo, worker, browserGrant = ['read']) {
    const fixture = protectedDatabase(`${worker}-${testInfo.project.name}`)
    const name = identityName(testInfo, worker)
    const identity = await harness.createIdentity(name)
    grant(fixture, 'browser', identity.publicKey, ...browserGrant)
    const revokedName = identityName(testInfo, `${worker}-revoked`)
    const revoked = await harness.createIdentity(revokedName)
    asAdmin(fixture, [
        'access',
        'grant',
        'app.vdbx',
        '--grantee',
        'revoked',
        '--key',
        revoked.publicKey,
        '--actions',
        'read',
        '--collection',
        'users',
        '--rule',
        'revoked-rule'
    ])
    asAdmin(fixture, ['access', 'revoke', 'app.vdbx', '--rule', 'revoked-rule'])
    return { fixture, name, revokedName }
}

test('a sealed pack of a protected database opens for a granted webcrypto: identity', async ({
    page
}, testInfo) => {
    const fixture = protectedDatabase(`sealed-${testInfo.project.name}`)
    const harness = await AccessPage.open(page)
    const name = identityName(testInfo, 'sealed')
    const identity = await harness.createIdentity(name)
    grant(fixture, 'browser', identity.publicKey, 'read')
    const packed = JSON.parse(
        asAdmin(fixture, ['pack', 'app.vdbx', '--out', 'sealed.vdbx', '--sealed'])
    )
    expect(packed.sealed).toBeTruthy()
    const url = `/${FIXTURES}/sealed-${testInfo.project.name}/sealed.vdbx`

    const opened = await openAs(harness, fixture, url, name, `sealed-${testInfo.project.name}.vdbx`)
    const [read, refused] = await harness.exec(opened.id, [
        { op: 'getDoc', collection: 'users', id: fixture.userId },
        { op: 'putData', collection: 'users', data: { name: 'not on a sealed copy' } }
    ])
    expectOk([read])
    expect(JSON.stringify(read.result)).toContain(CANARY)
    expect(refused.ok).toBe(false)
    await harness.call('close', opened.id)
    await harness.call('removeCopy', opened.name)

    const stranger = identityName(testInfo, 'sealed-stranger')
    await harness.createIdentity(stranger)
    const locked = await harness.call('open', {
        kind: 'url',
        url,
        identity: { name: stranger, trustAnchor: fixture.admin }
    })
    if (locked.error === undefined) {
        const [denied] = await harness.exec(locked.id, [
            { op: 'getDoc', collection: 'users', id: fixture.userId }
        ])
        expect(denied.ok).toBe(false)
        expect(JSON.stringify(denied.error)).not.toContain(CANARY)
        await harness.call('close', locked.id)
    }
})

test('after a revocation rotated the catalog key a granted identity reads and the revoked one does not', async ({
    page
}, testInfo) => {
    const harness = await AccessPage.open(page)
    const { fixture, name, revokedName } = await revokedFixture(harness, testInfo, 'rotated')
    const audit = JSON.parse(asAdmin(fixture, ['access', 'audit', 'app.vdbx']))
    expect(audit.entries.map((entry) => entry.kind)).toEqual(['create', 'grant', 'grant', 'revoke'])

    const opened = await openAs(
        harness,
        fixture,
        fixture.url,
        name,
        `rotated-${testInfo.project.name}.vdbx`
    )
    expect(await readCanary(harness, opened, fixture)).toEqual([])

    const revoked = await harness.call('open', {
        kind: 'url',
        url: fixture.url,
        identity: { name: revokedName, trustAnchor: fixture.admin }
    })
    if (revoked.error === undefined) {
        const [denied] = await harness.exec(revoked.id, [
            { op: 'getDoc', collection: 'users', id: fixture.userId }
        ])
        expect(denied.ok).toBe(false)
        expect(JSON.stringify(denied.error)).not.toContain(CANARY)
        await harness.call('close', revoked.id)
    } else {
        expect(revoked.code).toBe('EACCES')
    }
})

test('a rotation left pending by a torn slot write reads in the browser and reports pending', async ({
    page
}, testInfo) => {
    const harness = await AccessPage.open(page)
    const { fixture, name } = await revokedFixture(harness, testInfo, 'pending')
    tearNewestAccessSlot(join(fixture.directory, 'app.vdbx'))
    const report = JSON.parse(asAdmin(fixture, ['access', 'report', 'app.vdbx']))
    expect(report.catalogRotation.pendingSince).toBe(4)

    const opened = await openAs(
        harness,
        fixture,
        fixture.url,
        name,
        `pending-${testInfo.project.name}.vdbx`
    )
    expect(await readCanary(harness, opened, fixture)).toEqual(['catalogRotationPending'])
})

test('a webcrypto: admin writes under a pending rotation and leaves it for a native admin to settle', async ({
    page
}, testInfo) => {
    const harness = await AccessPage.open(page)
    const { fixture, name } = await revokedFixture(harness, testInfo, 'pending-admin', [
        'admin',
        ['--database']
    ])
    tearNewestAccessSlot(join(fixture.directory, 'app.vdbx'))

    const opened = await openAs(
        harness,
        fixture,
        fixture.url,
        name,
        `pending-admin-${testInfo.project.name}.vdbx`
    )
    const [written] = await harness.exec(opened.id, [
        { op: 'putData', collection: 'users', data: { name: 'written while pending' } }
    ])
    expectOk([written])
    const bytes = await harness.snapshot(opened.id)
    await harness.call('close', opened.id)
    await harness.call('removeCopy', opened.name)
    writeFileSync(join(fixture.directory, 'app.vdbx'), bytes)

    const report = JSON.parse(asAdmin(fixture, ['access', 'report', 'app.vdbx']))
    expect(report.catalogRotation.pendingSince).toBe(4)
    const settled = JSON.parse(asAdmin(fixture, ['access', 'settle', 'app.vdbx']))
    expect(settled.settled).toBe(true)
    expect(settled.pendingSince).toBe(4)
    const verified = JSON.parse(asAdmin(fixture, ['verify', 'app.vdbx']))
    expect(verified.ok, JSON.stringify(verified)).toBe(true)
    const document = JSON.parse(
        asAdmin(fixture, ['get', 'users', written.result, '--root', 'app.vdbx', '--json'])
    )
    expect(JSON.stringify(document)).toContain('written while pending')
})
