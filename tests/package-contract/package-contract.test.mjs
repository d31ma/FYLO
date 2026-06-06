import { afterAll, beforeAll, test } from 'bun:test'
import { createFyloConsumer, uniqueName } from './helpers.mjs'

let consumer

beforeAll(async () => {
    consumer = await createFyloConsumer()
})

afterAll(async () => {
    await consumer?.cleanup()
})

test('packed package supports a document lifecycle from a clean consumer project', async () => {
    await consumer.runModule(`
    import { mkdtemp, rm } from 'node:fs/promises'
    import os from 'node:os'
    import path from 'node:path'
    import Fylo from '@d31ma/fylo'

    const root = await mkdtemp(path.join(os.tmpdir(), '${uniqueName('fylo-root')}-'))
    try {
      const fylo = new Fylo(root)
      const { db, sql } = fylo
      const collection = 'blackbox-posts'
      await fylo.createCollection(collection)

      const id = await fylo.putData(collection, {
        title: 'Blackbox package smoke',
        tags: ['fylo', 'package-contract'],
        meta: { score: 9 },
      })

      const found = await fylo.getDoc(collection, id).once()
      if (!found[id]) throw new Error('getDoc did not return the created document')
      if (found[id].title !== 'Blackbox package smoke') throw new Error('getDoc did not round-trip')

      const queryResults = {}
      for await (const doc of fylo.findDocs(collection, {
        $ops: [{ 'meta.score': { $gte: 9 } }],
      }).collect()) {
        Object.assign(queryResults, doc)
      }
      if (!queryResults[id]) throw new Error('findDocs did not find indexed document')

      await sql\`CREATE TABLE blackbox-facade\`
      const taggedId = await sql\`INSERT INTO blackbox-facade (title, author) VALUES (\${'Tagged SQL'}, \${"O'Brien"})\`
      const taggedRows = await sql\`SELECT * FROM blackbox-facade WHERE author = \${"O'Brien"}\`
      if (!taggedRows[taggedId]) throw new Error('sql tag did not parameterize and query data')
      const facadeRows = await db['blackbox-facade'].getDoc(taggedId).once()
      if (facadeRows[taggedId].title !== 'Tagged SQL') throw new Error('db facade did not read by collection')
      try {
        db.getDoc
        throw new Error('reserved db property did not throw')
      } catch (error) {
        if (!String(error.message).includes('reserved db property')) throw error
      }

      const nextId = await fylo.patchDoc(collection, { [id]: { title: 'Blackbox updated' } })
      const updated = await fylo.getDoc(collection, nextId).once()
      if (!updated[nextId]) throw new Error('patchDoc did not return an updated document id')
      if (updated[nextId].title !== 'Blackbox updated') throw new Error('patchDoc did not update')

      await fylo.delDoc(collection, nextId)
      const deleted = await fylo.getDoc(collection, nextId).once()
      if (Object.keys(deleted).length !== 0) throw new Error('delDoc did not remove document')
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  `)
})

test('packed package exposes the browser runtime', async () => {
    await consumer.runModule(`
    import browserFylo, {
      BrowserCore,
      createBrowserClient,
      createMemoryFilesystem,
    } from '@d31ma/fylo/browser'

    const directCollection = '${uniqueName('packageusers').replaceAll('-', '')}'
    await browserFylo[directCollection].create()
    const directId = await browserFylo[directCollection].putData({ name: 'Direct package browser import' })
    const direct = await browserFylo[directCollection].getDoc(directId).once()
    if (direct[directId].name !== 'Direct package browser import') throw new Error('default browser import did not expose direct collection methods')

    const isolated = createBrowserClient({ worker: false })
    const memoryCollection = '${uniqueName('memoryusers').replaceAll('-', '')}'
    await isolated[memoryCollection].create()
    const memoryId = await isolated[memoryCollection].putData({ name: 'Factory memory fallback' })
    if ((await isolated[memoryCollection].getLatest(memoryId))[memoryId].name !== 'Factory memory fallback') throw new Error('browser factory did not use memory fallback')
    if (isolated.options.storage !== 'memory') throw new Error('browser factory did not default to memory outside OPFS')

    const fs = createMemoryFilesystem()
    const core = new BrowserCore({ fs, root: '/' })
    await core.createCollection('coreusers')
    const coreId = await core.putData('coreusers', { name: 'Injected VFS' })
    if (!(await fs.exists('/.collections/coreusers/docs/' + coreId.slice(0, 2) + '/' + coreId + '.json'))) throw new Error('browser core did not write to injected VFS')

    const fylo = createBrowserClient({ worker: false })
    await fylo.users.create()
    const events = []
    const unsubscribe = fylo.users.subscribe((event) => events.push(event))

    const id = await fylo.users.putData({
      name: 'Browser package smoke',
      role: 'admin',
      score: 11,
    })

    const found = await fylo.users.getDoc(id).once()
    if (!found[id]) throw new Error('browser getDoc did not return the created document')
    if (found[id].name !== 'Browser package smoke') throw new Error('browser getDoc did not round-trip data')

    const rows = await fylo.sql\`SELECT * FROM users WHERE role = \${'admin'}\`
    if (!rows[id]) throw new Error('browser sql tag did not query data')

    const response = await fylo.request({
      op: 'findDocs',
      collection: 'users',
      query: { $ops: [{ score: { $gte: 10 } }] },
    })
    if (!response.ok) throw new Error(response.error.message)
    if (!response.result[id]) throw new Error('browser request protocol did not return data')

    await fylo.users.delDoc(id)
    const deleted = await fylo.users.findDeletedDocs({ $ops: [{ name: { $eq: 'Browser package smoke' } }] }).collect().next()
    if (!deleted.value[id]) throw new Error('browser deleted query did not return tombstone')
    if (deleted.value[id]._deletedAt !== undefined) throw new Error('browser tombstone leaked internal deletion metadata')

    const restoredId = await fylo.users.restoreDoc(id)
    if (restoredId !== id) throw new Error('browser restore changed the TTID')
    const restored = await fylo.users.getLatest(id)
    if (restored[id].name !== 'Browser package smoke') throw new Error('browser restore did not restore data')
    if (events.length < 3) throw new Error('browser subscribe did not receive write/delete/restore events')
    unsubscribe()
  `)
})

test('packed package exposes the embeddable server gateway', async () => {
    await consumer.runModule(`
    import { mkdtemp, rm } from 'node:fs/promises'
    import os from 'node:os'
    import path from 'node:path'
    import { createFyloHttpHandler } from '@d31ma/fylo/server'

    const root = await mkdtemp(path.join(os.tmpdir(), '${uniqueName('fylo-server')}-'))
    try {
      const handler = createFyloHttpHandler({ root, token: 'package-token' })
      const create = await handler(new Request('http://fylo.test/v1/package-users', {
        method: 'POST',
        headers: {
          authorization: 'Bearer package-token',
          'content-type': 'application/json',
        },
        body: JSON.stringify({ name: 'Server package smoke', active: true }),
      }))
      if (create.status !== 201) throw new Error('server gateway did not create a document')
      const id = (await create.json()).result.id
      const read = await handler(new Request('http://fylo.test/v1/package-users/' + id, {
        headers: { authorization: 'Bearer package-token' },
      }))
      if (read.status !== 200) throw new Error('server gateway did not read a document')
      const payload = await read.json()
      if (payload.result[id].name !== 'Server package smoke') throw new Error('server gateway did not round-trip data')
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  `)
})
