import { describe, expect, test } from 'bun:test'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'

describe('MemoryFilesystem', () => {
    test('stores independent files and immediate directory entries', async () => {
        const fs = createMemoryFilesystem()
        await fs.writeText('/db/.collections/users/docs/4U/4UUB32VGUDW.json', '{"ok":true}')
        await fs.writeText('/db/.collections/users/docs/4V/4V6329YC0R0.json', '{"ok":true}')

        expect(await fs.list('/db/.collections/users/docs')).toEqual(['4U', '4V'])
        expect(await fs.readText('/db/.collections/users/docs/4U/4UUB32VGUDW.json')).toBe(
            '{"ok":true}'
        )
        expect(await fs.mtimeMs('/db/.collections/users/docs/4U/4UUB32VGUDW.json')).toBeNumber()
    })

    test('move copies one file and removes the source', async () => {
        const fs = createMemoryFilesystem()
        await fs.writeText('/live/doc.json', 'body')
        await fs.move('/live/doc.json', '/deleted/doc.json')

        expect(await fs.exists('/live/doc.json')).toBe(false)
        expect(await fs.readText('/deleted/doc.json')).toBe('body')
    })
})
