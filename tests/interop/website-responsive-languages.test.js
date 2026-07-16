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
