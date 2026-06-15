import { runBrowserRequest } from '../../../src/browser/core/protocol.js'

/**
 * @param {unknown} condition
 * @param {string} message
 * @returns {asserts condition}
 */
function assert(condition, message) {
    if (!condition) throw new Error(message)
}

/**
 * @param {any} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @returns {Promise<Record<string, any>>}
 */
async function collectFind(fylo, collection, query = {}) {
    /** @type {Record<string, any>} */
    const docs = {}
    for await (const value of fylo[collection].find(query).collect()) {
        if (value && typeof value === 'object' && !Array.isArray(value)) Object.assign(docs, value)
    }
    return docs
}

/**
 * @param {any} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @returns {Promise<Record<string, any>>}
 */
async function collectDeleted(fylo, collection, query = {}) {
    /** @type {Record<string, any>} */
    const docs = {}
    for await (const value of fylo[collection].find.deleted(query).collect()) {
        if (value && typeof value === 'object' && !Array.isArray(value)) Object.assign(docs, value)
    }
    return docs
}

/**
 * Runs the browser runtime contract against any FYLO-compatible runtime.
 *
 * The suite intentionally verifies public behavior only. It does not assume a
 * specific storage layout, index strategy, or concrete browser implementation.
 *
 * @param {() => Promise<any> | any} createRuntime
 * @returns {Promise<{ userId: string, orderId: string }>}
 */
export async function runBrowserConformance(createRuntime) {
    const fylo = await createRuntime()
    const users = 'browser-users'
    const orders = 'browser-orders'
    try {
        await fylo[users].create()
        await fylo[orders].create()

        const userId = await fylo[users].put({
            name: 'Alice',
            role: 'admin',
            score: 42,
            tags: ['browser', 'fylo']
        })
        const orderId = await fylo[orders].put({
            userId,
            total: 125,
            status: 'open'
        })

        assert((await fylo[users].latest(userId))[userId].name === 'Alice', 'getLatest failed')

        const exact = await collectFind(fylo, users, {
            $ops: [{ role: { $eq: 'admin' } }]
        })
        assert(Object.keys(exact).length === 1, 'exact query returned the wrong count')
        assert(exact[userId].score === 42, 'exact query returned the wrong document')

        const like = await collectFind(fylo, users, {
            $ops: [{ name: { $like: 'Ali%' } }]
        })
        assert(Object.hasOwn(like, userId), 'LIKE query failed')

        const range = await collectFind(fylo, orders, {
            $ops: [{ total: { $gte: 100 } }]
        })
        assert(Object.hasOwn(range, orderId), 'range query failed')

        const patchedId = await fylo[users].patch(userId, { role: 'owner', score: 50 })
        assert(patchedId === userId, 'patch changed a stable document id')
        assert((await fylo[users].latest(userId))[userId].role === 'owner', 'patch failed')

        const patchedCount = await fylo[orders].patch.many({
            $where: { $ops: [{ status: { $eq: 'open' } }] },
            $set: { status: 'paid' }
        })
        assert(patchedCount === 1, 'patchDocs returned the wrong count')
        assert((await fylo[orders].latest(orderId))[orderId].status === 'paid', 'patchDocs failed')

        const joined = await fylo.join({
            $leftCollection: orders,
            $rightCollection: users,
            $mode: 'inner',
            $on: { userId: { $eq: 'userId' } }
        })
        assert(Object.keys(joined).length === 0, 'join should not match without matching right key')

        const sqlId = await fylo._sql(
            "INSERT INTO browser-users (name, role, score) VALUES ('Bob', 'member', 12)"
        )
        const sqlRows = await fylo._sql("SELECT * FROM browser-users WHERE role = 'member'")
        assert(Object.hasOwn(sqlRows, sqlId), 'SQL insert/select failed')

        const response = await runBrowserRequest(fylo, {
            op: 'findDocs',
            collection: users,
            query: { $ops: [{ role: { $eq: 'owner' } }] }
        })
        assert(response.ok === true, 'protocol request failed')
        assert(
            Object.hasOwn(/** @type {Record<string, any>} */ (response.result), userId),
            'protocol request returned wrong result'
        )

        const deletedFloor = Date.now()
        await fylo[users].delete(userId)
        assert(
            Object.keys(await fylo[users].latest(userId)).length === 0,
            'delete did not hide doc'
        )

        const deleted = await collectDeleted(fylo, users, {
            $ops: [{ role: { $eq: 'owner' } }],
            $deleted: { $gte: deletedFloor }
        })
        assert(Object.hasOwn(deleted, userId), 'deleted query failed')

        const restoredId = await fylo[users].restore(userId)
        assert(restoredId === userId, 'restore returned wrong id')
        assert((await fylo[users].latest(userId))[userId].role === 'owner', 'restore failed')

        const inspect = await fylo[users].inspect()
        assert(inspect.exists === true, 'inspect did not report existing collection')
        assert(inspect.docsStored >= 2, 'inspect docsStored is wrong')

        return { userId, orderId }
    } finally {
        await fylo.close?.()
    }
}
