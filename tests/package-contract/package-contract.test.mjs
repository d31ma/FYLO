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
      const fylo = new Fylo({ root })
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
