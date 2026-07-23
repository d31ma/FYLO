import { describe, expect, test } from 'bun:test'
import path from 'node:path'

const root = path.resolve(import.meta.dir, '../..')

describe('website language selector', () => {
    test('is a swipeable horizontal strip on narrow screens', async () => {
        const css = await Bun.file(
            path.join(root, 'website/client/components/code/showcase/tac.css')
        ).text()

        expect(css).toMatch(
            /@media \(max-width: 599\.98px\)[\s\S]*\.showcase-langs\s*\{[\s\S]*?flex-wrap:\s*nowrap;[\s\S]*?overflow-x:\s*auto;/
        )
        expect(css).toContain('.showcase-langs::-webkit-scrollbar')
        expect(css).toContain('-webkit-overflow-scrolling: touch')
    })
})

describe('website POSIX access guidance', () => {
    test('explains UID, GID, mode-only writes, and trusted group resolution', async () => {
        const [features, faq, docs] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/features/grid/tac.js')).text(),
            Bun.file(path.join(root, 'website/client/components/faq/panels/tac.js')).text(),
            Bun.file(path.join(root, 'website/client/components/docs/content/tac.js')).text()
        ])

        expect(features).toContain('POSIX UID/GID/mode enforcement')
        expect(faq).toContain('gid: editorsGid, mode: 0o660')
        expect(faq).toContain('Group write permission')
        expect(docs).toContain('groupsForUid')
        expect(docs).toContain('.as({ uid: 1001, gid: editorsGid, mode: 0o660 })')
        expect(docs).toContain('.as({ mode: 0o600 })')
    })
})

describe('website Explorer release download', () => {
    test('links the versioned self-hosting ZIP from the download table', async () => {
        const version = (await Bun.file('package.json').json()).version
        const [download, header] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/download/content/tac.js')).text(),
            Bun.file(path.join(root, 'website/client/components/site/header/tac.html')).text()
        ])

        expect(download).toContain(`fylo-explorer-${version}.zip`)
        expect(download).toContain('Web (self-hosted Explorer)')
        expect(header).not.toContain('https://fx.del.ma')
        expect(header).not.toContain('>Explorer</a>')
    })
})
