// fylo link conformance for @tursodatabase/database: a stock SDK, with the
// FYLO-built native library linked, opens a FYLO root as its database.
// Usage: node conformance.mjs <fylo-root> <plain-db-path>
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { connect } from '@tursodatabase/database'

const [root, plain] = process.argv.slice(2)
const results = []
const check = async (name, body) => {
    try {
        await body()
        results.push(`PASS ${name}`)
    } catch (error) {
        results.push(`FAIL ${name}: ${error?.stack ?? error}`)
    }
}
const failsWith = async (code, body) => {
    await assert.rejects(body, (error) => {
        assert.match(String(error?.message ?? error), new RegExp(code))
        return true
    })
}

const db = await connect(root)

await check('collections are tables', async () => {
    const rows = await db.all('SELECT id, name, price FROM products ORDER BY name LIMIT 3')
    assert.equal(rows.length, 3)
    assert.equal(rows[0].name, 'item-0000')
    assert.match(rows[0].id, /^[0-9A-Z]+$/)
    const [{ count }] = await db.all('SELECT count(*) AS count FROM products')
    assert.equal(count, 60)
})

await check('pushdown returns what a full scan returns', async () => {
    for (const [pushed, scanned, value] of [
        ['price = ?', 'price + 0 = ?', 7],
        ['price > ?', 'price + 0 > ?', 15],
        ['name = ?', "name || '' = ?", 'item-0042'],
        ['"group" = ?', '"group" || \'\' = ?', 'g1']
    ]) {
        const a = await db.all(`SELECT id FROM products WHERE ${pushed} ORDER BY id`, value)
        const b = await db.all(`SELECT id FROM products WHERE ${scanned} ORDER BY id`, value)
        assert.deepEqual(a, b, pushed)
        assert.ok(a.length > 0, pushed)
    }
})

let inserted
await check('insert, update and delete commit through FYLO', async () => {
    const run = await db.run('INSERT INTO notes (doc) VALUES (?)', JSON.stringify({ text: 'hello', n: 1 }))
    assert.equal(run.changes, 1)
    const [row] = await db.all('SELECT id, doc FROM notes')
    inserted = row.id
    assert.deepEqual(JSON.parse(row.doc), { text: 'hello', n: 1 })
    const updated = await db.run("UPDATE notes SET doc = json_set(doc, '$.n', 2) WHERE id = ?", inserted)
    assert.equal(updated.changes, 1)
    const [{ n }] = await db.all("SELECT json_extract(doc, '$.n') AS n FROM notes WHERE id = ?", inserted)
    assert.equal(n, 2)
    const priced = await db.run('UPDATE products SET price = ? WHERE name = ?', 99, 'item-0001')
    assert.equal(priced.changes, 1)
    const [{ price }] = await db.all('SELECT price FROM products WHERE name = ?', 'item-0001')
    assert.equal(price, 99)
})

await check('a transaction commits whole and rolls back whole', async () => {
    await db.exec('BEGIN')
    await db.run('INSERT INTO notes (doc) VALUES (?), (?)', '{"t":1}', '{"t":2}')
    const inside = await db.all("SELECT id FROM notes WHERE json_extract(doc, '$.t') IS NOT NULL")
    assert.equal(inside.length, 2)
    await db.exec('COMMIT')
    const after = await db.all("SELECT id FROM notes WHERE json_extract(doc, '$.t') IS NOT NULL")
    assert.equal(after.length, 2)
    await db.exec('BEGIN')
    await db.run('DELETE FROM notes')
    await db.exec('ROLLBACK')
    const kept = await db.all('SELECT id FROM notes')
    assert.equal(kept.length, 3)
})

await check('a second collection in one transaction is EFYLO_TXN_SCOPE', async () => {
    await db.exec('BEGIN')
    await db.run('INSERT INTO notes (doc) VALUES (?)', '{"scope":1}')
    await failsWith('EFYLO_TXN_SCOPE', () => db.run("INSERT INTO products (doc) VALUES ('{\"name\":\"x\"}')"))
    await db.exec('ROLLBACK').catch(() => {})
    const leaked = await db.all("SELECT id FROM notes WHERE json_extract(doc, '$.scope') = 1")
    assert.equal(leaked.length, 0)
})

await check('FYLO errors keep their codes', async () => {
    await failsWith('EARRAYOFOBJECTS', () => db.run('INSERT INTO notes (doc) VALUES (?)', '{"tags":[{"a":1}]}'))
    await failsWith('EFYLO_SQL_ID', () => db.run("UPDATE notes SET id = 'X' WHERE id = ?", inserted))
})

await check('the queue is tables', async () => {
    await db.run("INSERT INTO fylo_queue_publish (topic, payload) VALUES ('jobs', ?)", '{"job":1}')
    const claimed = await db.all(
        "SELECT id, receipt, attempt, payload FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'"
    )
    assert.equal(claimed.length, 1)
    assert.deepEqual(JSON.parse(claimed[0].payload), { job: 1 })
    await db.run(
        "INSERT INTO fylo_queue_ack (topic, group_name, id, receipt) VALUES ('jobs', 'workers', ?, ?)",
        claimed[0].id,
        claimed[0].receipt
    )
    const again = await db.all("SELECT id FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'")
    assert.equal(again.length, 0)
    const [{ value }] = await db.all("SELECT value FROM fylo_info WHERE name = 'turso_core'")
    assert.equal(value, '0.8.3')
})

await db.close()

await check('a plain database path opens as stock Turso', async () => {
    const file = await connect(plain)
    await file.exec('CREATE TABLE t (x INTEGER)')
    const run = await file.run('INSERT INTO t VALUES (?), (?)', 1, 2)
    assert.equal(run.changes, 2)
    const [{ total }] = await file.all('SELECT sum(x) AS total FROM t')
    assert.equal(total, 3)
    await file.close()
    assert.ok(existsSync(plain), 'the plain path is a database file')
})

console.log(results.join('\n'))
process.exit(results.some((line) => line.startsWith('FAIL')) ? 1 : 0)
