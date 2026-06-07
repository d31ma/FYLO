import { describe, expect, test } from 'bun:test'
import Fylo from '../../src/index.js'
const runCanary = process.env.FYLO_RUN_FILESYSTEM_CANARY === 'true'
const canaryTest = runCanary ? test : test.skip
describe('filesystem canary', () => {
    canaryTest('mounted filesystem root handles a real CRUD cycle', async () => {
        const collection = `canary_${Date.now()}`
        const fylo = new Fylo(process.env.FYLO_ROOT)
        await fylo[collection].create()
        const id = await fylo[collection].put({
            title: 'canary',
            tags: ['mounted', 'filesystem']
        })
        const doc = await fylo[collection].get(id).once()
        expect(doc[id].title).toBe('canary')
        await fylo[collection].delete(id)
        await fylo[collection].drop()
    })
})
