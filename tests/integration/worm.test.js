import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdir, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const COLLECTION = 'worm-posts'
const root = await createTestRoot('fylo-worm-')
const fylo = new Fylo({
    root,
    worm: {
        mode: 'strict'
    }
})

beforeAll(async () => {
    await fylo.createCollection(COLLECTION)
})

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('strict WORM mode', () => {
    test('writes one read-only immutable document with no version history artifacts', async () => {
        const id = await fylo.putData(COLLECTION, { title: 'Retain me' })
        const documentPath = path.join(
            root,
            '.collections',
            COLLECTION,
            'docs',
            id.slice(0, 2),
            `${id}.json`
        )
        const mode = (await stat(documentPath)).mode & 0o777
        expect(mode).toBe(0o444)
        expect(await Bun.file(path.join(root, '.collections', COLLECTION, 'heads')).exists()).toBe(
            false
        )
        expect(
            await Bun.file(path.join(root, '.collections', COLLECTION, 'versions')).exists()
        ).toBe(false)

        const stored = await fylo.getDoc(COLLECTION, id).once()
        expect(stored[id].title).toBe('Retain me')
    })

    test('rejects patch, versioned put, and delete after first write', async () => {
        const id = await fylo.putData(COLLECTION, { title: 'Locked' })

        await expect(fylo.patchDoc(COLLECTION, { [id]: { title: 'Changed' } })).rejects.toThrow(
            'Update is not allowed in WORM mode'
        )
        await expect(fylo.putData(COLLECTION, { [id]: { title: 'Changed' } })).rejects.toThrow(
            'Update is not allowed in WORM mode'
        )
        await expect(fylo.delDoc(COLLECTION, id)).rejects.toThrow(
            'Delete is not allowed in WORM mode'
        )
        await expect(fylo.restoreDoc(COLLECTION, id)).rejects.toThrow(
            'Restore is not allowed in WORM mode'
        )
        await expect(fylo.dropCollection(COLLECTION)).rejects.toThrow(
            'Drop is not allowed for a non-empty WORM collection'
        )

        const stored = await fylo.getDoc(COLLECTION, id).once()
        expect(stored[id].title).toBe('Locked')
    })

    test('fails closed when legacy version-history metadata is present', async () => {
        const legacyCollection = 'legacy-worm'
        const headRoot = path.join(root, '.collections', legacyCollection, 'heads')
        await mkdir(headRoot, { recursive: true })
        await writeFile(path.join(headRoot, 'legacy.json'), '{}')

        await expect(fylo.createCollection(legacyCollection)).rejects.toThrow(
            'unsupported legacy WORM heads metadata'
        )
    })
})
