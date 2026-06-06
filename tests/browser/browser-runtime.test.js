import { describe, expect, test } from 'bun:test'
import path from 'node:path'
import fylo, { createBrowserClient, createBrowserFylo } from '../../src/browser/index.js'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'
import { runBrowserConformance } from './helpers/conformance.js'

describe('browser runtime', () => {
    test('reference browser runtime satisfies the browser conformance contract', async () => {
        const result = await runBrowserConformance(() => createBrowserFylo({ worker: false }))
        expect(result.userId).toBeString()
        expect(result.orderId).toBeString()
    })

    test('browser export bundles for browser target without Bun or Node builtins', async () => {
        const result = await Bun.build({
            entrypoints: [path.resolve('src/browser/index.js')],
            target: 'browser',
            format: 'esm'
        })
        expect(result.success).toBe(true)
        const combined = (await Promise.all(result.outputs.map((output) => output.text()))).join(
            '\n'
        )
        expect(combined).not.toContain('Bun.')
        expect(combined).not.toContain('node:')
        expect(combined).not.toContain('node:fs')
        expect(combined).not.toContain('node:path')
        expect(combined).not.toContain('process.env')
        expect(combined).not.toContain('Cipher.configure')
    })

    test('strict WORM mode remains write-once in the browser runtime', async () => {
        const fylo = createBrowserFylo({ worker: false, worm: { mode: 'strict' } })
        await fylo.createCollection('worm-docs')
        const id = await fylo.putData('worm-docs', { title: 'Immutable' })

        await expect(fylo.patchDoc('worm-docs', { [id]: { title: 'Changed' } })).rejects.toThrow(
            'Update is not allowed in WORM mode'
        )
        await expect(fylo.delDoc('worm-docs', id)).rejects.toThrow(
            'Delete is not allowed in WORM mode'
        )
        expect((await fylo.getLatest('worm-docs', id))[id].title).toBe('Immutable')
    })

    test('browser facade can use an injected per-document filesystem', async () => {
        const fs = createMemoryFilesystem()
        const fylo = createBrowserFylo({ worker: false, fs })
        await fylo.db.users.create()
        const id = await fylo.db.users.putData({ name: 'Lin' })

        expect(await fs.exists(`/.collections/users/docs/${id.slice(0, 2)}/${id}.json`)).toBe(true)
        expect(await fs.exists('/.collections/users/collection.json')).toBe(false)
    })

    test('direct browser runtime exposes collection subscriptions', async () => {
        const fylo = createBrowserFylo({ worker: false })
        await fylo.db.users.create()
        const events = []
        const unsubscribe = fylo.db.users.subscribe((event) => events.push(event))

        const id = await fylo.db.users.putData({ name: 'Sub' })
        unsubscribe()

        expect(events).toHaveLength(1)
        expect(events[0]).toMatchObject({ action: 'insert', id })
    })

    test('top-level browser default exposes direct collection facades', async () => {
        const collection = `users${Date.now()}`
        await fylo.collection(collection).create()
        const id = await fylo[collection].putData({ name: 'Default browser import' })

        const found = await fylo[collection].getDoc(id).once()
        expect(found[id].name).toBe('Default browser import')
    })

    test('top-level browser factory defaults to memory when OPFS is unavailable', async () => {
        const local = createBrowserClient({ worker: false })
        await local.users.create()
        const id = await local.users.putData({ name: 'Memory fallback' })

        expect(local.options.storage).toBe('memory')
        expect((await local.users.getLatest(id))[id].name).toBe('Memory fallback')
    })
})
