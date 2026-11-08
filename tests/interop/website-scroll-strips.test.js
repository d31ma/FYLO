import { describe, expect, test } from 'bun:test'
import { scrollState } from '../../website/client/shared/scripts/scroll-strips.js'

describe('horizontal strip controls', () => {
    test('disables arrows when all items fit, including empty strips', () => {
        expect(scrollState(0, 300, 300)).toEqual({ overflow: false, start: true, end: true })
        expect(scrollState(0, 300, 200)).toEqual({ overflow: false, start: true, end: true })
        expect(scrollState(0, 0, 0)).toEqual({ overflow: false, start: true, end: true })
    })
    test('supports both directions between the ends, including fractional pixels and overscroll', () => {
        expect(scrollState(0, 300, 900)).toEqual({ overflow: true, start: true, end: false })
        expect(scrollState(300, 300, 900)).toEqual({ overflow: true, start: false, end: false })
        expect(scrollState(599.5, 300, 900)).toEqual({ overflow: true, start: false, end: true })
        expect(scrollState(-10, 300, 900).start).toBe(true)
        expect(scrollState(610, 300, 900).end).toBe(true)
    })
    test('both menus provide named button controls independent of touch gestures', async () => {
        for (const [file, name] of [
            ['doc/sidebar', 'documentation'],
            ['docs/sample', 'languages']
        ]) {
            const html = await Bun.file(`website/client/components/${file}/tac.html`).text()
            expect(html).toContain('data-scroll-direction="-1"')
            expect(html).toContain('data-scroll-direction="1"')
            expect(html).toContain(`aria-label="Scroll ${name} left"`)
            expect(html).toContain(`aria-label="Scroll ${name} right"`)
        }
    })
})
