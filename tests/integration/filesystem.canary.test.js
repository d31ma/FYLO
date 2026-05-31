import { describe, expect, test } from 'bun:test'
import Fylo from '../../src/index.js'
const runCanary = process.env.FYLO_RUN_FILESYSTEM_CANARY === 'true'
const canaryTest = runCanary ? test : test.skip
describe('filesystem canary', () => {
    canaryTest('mounted filesystem root handles a real CRUD cycle', async () => {
        const collection = `canary_${Date.now()}`
        const fylo = new Fylo(process.env.FYLO_ROOT)
        await fylo.createCollection(collection)
        const id = await fylo.putData(collection, {
            title: 'canary',
            tags: ['mounted', 'filesystem']
        })
        const doc = await fylo.getDoc(collection, id).once()
        expect(doc[id].title).toBe('canary')
        await fylo.delDoc(collection, id)
        await fylo.dropCollection(collection)
    })
})
