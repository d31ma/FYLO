import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../../src/index.js'
import { createFyloHttpHandler, serveFyloHttp } from '../../../src/server/http.js'
import { VersionRepository } from '../../../src/versioning/repository.js'
import { createTestRoot } from '../../helpers/root.js'

const roots = []

async function createRoot(prefix) {
    const root = await createTestRoot(prefix)
    roots.push(root)
    return root
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

/**
 * @param {ReturnType<typeof createFyloHttpHandler>} handler
 * @param {string} pathname
 * @param {RequestInit=} init
 * @returns {Promise<Response>}
 */
function request(handler, pathname, init = {}) {
    const headers = new Headers(init.headers)
    if (!headers.has('authorization')) headers.set('authorization', 'Bearer test-token')
    if (init.body && !headers.has('content-type')) headers.set('content-type', 'application/json')
    return handler(
        new Request(`http://fylo.test${pathname}`, {
            ...init,
            headers
        })
    )
}

/**
 * @param {Response} response
 * @returns {Promise<Record<string, any>>}
 */
async function json(response) {
    return /** @type {Record<string, any>} */ (await response.json())
}

describe('FYLO HTTP gateway', () => {
    test('protects authenticated routes and exposes health/openapi metadata', async () => {
        const root = await createRoot('fylo-http-auth-')
        const handler = createFyloHttpHandler({
            root,
            token: 'test-token',
            corsOrigin: 'https://app.example'
        })

        const unauthorized = await handler(new Request('http://fylo.test/v1/health'))
        expect(unauthorized.status).toBe(401)

        const options = await handler(
            new Request('http://fylo.test/v1/users', { method: 'OPTIONS' })
        )
        expect(options.status).toBe(204)
        expect(options.headers.get('access-control-allow-origin')).toBe('https://app.example')

        const health = await request(handler, '/v1/health')
        expect(health.status).toBe(200)
        expect((await json(health)).protocolVersion).toBe(1)

        const openapi = await request(handler, '/v1/openapi.json')
        expect(openapi.status).toBe(200)
        expect((await json(openapi)).openapi).toBe('3.1.0')
    })

    test('echoes an allow-listed CORS origin and omits unlisted ones', async () => {
        const root = await createRoot('fylo-http-cors-')
        const handler = createFyloHttpHandler({
            root,
            token: 'test-token',
            corsOrigin: ['https://a.example', 'https://b.example']
        })

        const allowed = await handler(
            new Request('http://fylo.test/v1/users', {
                method: 'OPTIONS',
                headers: { origin: 'https://b.example' }
            })
        )
        expect(allowed.headers.get('access-control-allow-origin')).toBe('https://b.example')

        const blocked = await handler(
            new Request('http://fylo.test/v1/users', {
                method: 'OPTIONS',
                headers: { origin: 'https://evil.example' }
            })
        )
        expect(blocked.headers.get('access-control-allow-origin')).toBeNull()
    })

    test('maps a malformed document id to 400 instead of a generic 500', async () => {
        const root = await createRoot('fylo-http-badid-')
        const handler = createFyloHttpHandler({ root, token: 'test-token' })
        const response = await request(handler, '/v1/http-users/not-a-ttid')
        expect(response.status).toBe(400)
        expect((await json(response)).ok).toBe(false)
    })

    test('performs collection CRUD and URL-filtered queries', async () => {
        const root = await createRoot('fylo-http-crud-')
        const handler = createFyloHttpHandler({ root, token: 'test-token' })
        const collectionName = 'http-users'

        const missingCollection = await request(handler, `/v1/${collectionName}`, {
            method: 'POST',
            body: JSON.stringify({ name: 'Should fail' })
        })
        expect(missingCollection.status).toBe(404)
        expect((await json(missingCollection)).error.name).toBe('CollectionNotFoundError')

        const collection = await request(handler, '/v1/sql', {
            method: 'POST',
            body: JSON.stringify({ sql: `CREATE TABLE ${collectionName}` })
        })
        expect(collection.status).toBe(200)

        const create = await request(handler, `/v1/${collectionName}`, {
            method: 'POST',
            body: JSON.stringify({
                name: 'Ada Lovelace',
                email: 'ada.lovelace@example.com',
                active: true,
                age: 36
            })
        })
        expect(create.status).toBe(201)
        const id = (await json(create)).result.id
        expect(typeof id).toBe('string')

        const byId = await request(handler, `/v1/${collectionName}/${id}`)
        expect(byId.status).toBe(200)
        expect((await json(byId)).result[id].name).toBe('Ada Lovelace')

        const filtered = await request(
            handler,
            `/v1/${collectionName}?active=eq.true&age=gte.30&limit=5`
        )
        expect(filtered.status).toBe(200)
        expect((await json(filtered)).result[id].name).toBe('Ada Lovelace')

        const dottedValue = await request(
            handler,
            `/v1/${collectionName}?email=ada.lovelace%40example.com`
        )
        expect(dottedValue.status).toBe(200)
        expect((await json(dottedValue)).result[id].name).toBe('Ada Lovelace')

        const patch = await request(handler, `/v1/${collectionName}/${id}`, {
            method: 'PATCH',
            body: JSON.stringify({ role: 'admin' })
        })
        expect(patch.status).toBe(200)

        const patched = await request(handler, `/v1/${collectionName}/${id}`)
        expect((await json(patched)).result[id].role).toBe('admin')

        const deleted = await request(handler, `/v1/${collectionName}/${id}`, { method: 'DELETE' })
        expect(deleted.status).toBe(200)

        const missing = await request(handler, `/v1/${collectionName}/${id}`)
        expect(missing.status).toBe(404)
    })

    test('executes SQL and machine JSON over the same remote boundary', async () => {
        const root = await createRoot('fylo-http-exec-')
        const handler = createFyloHttpHandler({ root, token: 'test-token' })

        const create = await request(handler, '/v1/sql', {
            method: 'POST',
            body: JSON.stringify({ sql: 'CREATE TABLE remote-posts' })
        })
        expect(create.status).toBe(200)

        const put = await request(handler, '/v1/exec', {
            method: 'POST',
            body: JSON.stringify({
                requestId: 'put-1',
                op: 'putData',
                collection: 'remote-posts',
                data: { title: 'Network-native', published: true }
            })
        })
        expect(put.status).toBe(200)
        const putPayload = await json(put)
        expect(putPayload.ok).toBe(true)
        expect(putPayload.requestId).toBe('put-1')

        const query = await request(handler, '/v1/exec', {
            method: 'POST',
            body: JSON.stringify({
                op: 'findDocs',
                collection: 'remote-posts',
                query: { published: true }
            })
        })
        expect(query.status).toBe(200)
        const docs = (await json(query)).result
        expect(Object.values(docs)[0].title).toBe('Network-native')
    })

    test('streams raw file uploads and downloads without exposing server paths', async () => {
        const root = await createRoot('fylo-http-files-')
        const db = new Fylo(root, { versioning: { autoCommit: false } })
        await db.assets.create({ kind: 'file' })
        const handler = createFyloHttpHandler({
            root,
            token: 'test-token',
            maxBodyBytes: 1024
        })

        const upload = await request(handler, '/v1/assets', {
            method: 'POST',
            headers: {
                'content-type': 'text/plain',
                'x-fylo-filename': 'remote.txt',
                'x-fylo-key': '/remote/uploads/remote.txt'
            },
            body: 'remote raw bytes'
        })
        expect(upload.status).toBe(201)
        const id = (await json(upload)).result.id

        const metadata = await request(handler, `/v1/assets/${id}`)
        expect((await json(metadata)).result[id]).toMatchObject({
            key: '/remote/uploads/remote.txt',
            extension: '.txt',
            contentType: 'text/plain',
            contentLength: 16
        })

        const download = await request(handler, `/v1/assets/${id}/raw`)
        expect(download.status).toBe(200)
        expect(download.headers.get('content-type')).toBe('text/plain')
        expect(await download.text()).toBe('remote raw bytes')

        const blockedPath = await request(handler, '/v1/exec', {
            method: 'POST',
            body: JSON.stringify({
                op: 'putData',
                collection: 'assets',
                file: { path: `${root}/secret.txt` }
            })
        })
        expect(blockedPath.status).toBe(400)
        expect((await json(blockedPath)).error.message).toContain(
            'Local file paths are not allowed'
        )
    })

    test('pins branch profiles instead of following the active checkout', async () => {
        const root = await createRoot('fylo-http-branch-')
        const repo = new VersionRepository(root)
        const main = new Fylo(root)
        await main['profiles'].create()
        const mainId = await main['profiles'].put({ name: 'main-only' })
        await repo.commit('main snapshot')
        await repo.checkout('feature', { create: true })
        const feature = new Fylo(root)
        const featureId = await feature['profiles'].put({ name: 'feature-only' })
        await repo.commit('feature snapshot')

        const handler = createFyloHttpHandler({ root, token: 'test-token' })
        const active = await request(handler, '/v1/profiles')
        expect(Object.keys((await json(active)).result)).toContain(featureId)

        const mainProfile = await request(handler, '/v1/profiles', {
            headers: { 'accept-profile': 'main' }
        })
        const mainDocs = (await json(mainProfile)).result
        expect(Object.keys(mainDocs)).toContain(mainId)
        expect(Object.keys(mainDocs)).not.toContain(featureId)

        const featureProfile = await request(handler, '/v1/profiles', {
            headers: { 'accept-profile': 'feature' }
        })
        const featureDocs = (await json(featureProfile)).result
        expect(Object.keys(featureDocs)).toContain(mainId)
        expect(Object.keys(featureDocs)).toContain(featureId)
    })

    test('rejects unsafe public binding without authentication', async () => {
        const root = await createRoot('fylo-http-bind-')
        expect(() => serveFyloHttp({ root, host: '0.0.0.0', port: 0 })).toThrow(/requires --token/)
    })
})
