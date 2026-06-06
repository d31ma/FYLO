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

    test('performs collection CRUD and URL-filtered queries', async () => {
        const root = await createRoot('fylo-http-crud-')
        const handler = createFyloHttpHandler({ root, token: 'test-token' })

        const create = await request(handler, '/v1/users', {
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

        const byId = await request(handler, `/v1/users/${id}`)
        expect(byId.status).toBe(200)
        expect((await json(byId)).result[id].name).toBe('Ada Lovelace')

        const filtered = await request(handler, '/v1/users?active=eq.true&age=gte.30&limit=5')
        expect(filtered.status).toBe(200)
        expect((await json(filtered)).result[id].name).toBe('Ada Lovelace')

        const dottedValue = await request(handler, '/v1/users?email=ada.lovelace%40example.com')
        expect(dottedValue.status).toBe(200)
        expect((await json(dottedValue)).result[id].name).toBe('Ada Lovelace')

        const patch = await request(handler, `/v1/users/${id}`, {
            method: 'PATCH',
            body: JSON.stringify({ role: 'admin' })
        })
        expect(patch.status).toBe(200)

        const patched = await request(handler, `/v1/users/${id}`)
        expect((await json(patched)).result[id].role).toBe('admin')

        const deleted = await request(handler, `/v1/users/${id}`, { method: 'DELETE' })
        expect(deleted.status).toBe(200)

        const missing = await request(handler, `/v1/users/${id}`)
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

    test('pins branch profiles instead of following the active checkout', async () => {
        const root = await createRoot('fylo-http-branch-')
        const repo = new VersionRepository(root)
        const main = new Fylo(root)
        await main.createCollection('profiles')
        const mainId = await main.putData('profiles', { name: 'main-only' })
        await repo.commit('main snapshot')
        await repo.checkout('feature', { create: true })
        const feature = new Fylo(root)
        const featureId = await feature.putData('profiles', { name: 'feature-only' })
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
