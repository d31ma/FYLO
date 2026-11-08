import { describe, expect, test } from 'bun:test'
import path from 'node:path'

const headerRoot = path.resolve(import.meta.dir, '../../website/client/components/site/header')

describe('website primary navigation', () => {
    test('keeps documentation and downloads available at every screen width', async () => {
        const markup = await Bun.file(path.join(headerRoot, 'tac.html')).text()
        const navigation = markup.match(
            /<nav\b[^>]*aria-label="Primary navigation"[^>]*>([\s\S]*?)<\/nav>/
        )?.[1]

        expect(navigation).toBeDefined()
        for (const destination of ['/docs', '/download']) {
            const anchor = navigation.match(
                new RegExp(`<a\\b[^>]*href="${destination}"[^>]*>[\\s\\S]*?<\\/a>`)
            )?.[0]
            expect(anchor).toBeDefined()
            expect(anchor).not.toContain('w-hide-mobile')
            expect(anchor).not.toMatch(/\bhidden(?:\s|=|>)/)
        }
        expect(markup).toContain('aria-label="FYLO home"')
        expect(markup).toContain('src="/shared/assets/fylo-mark.svg"')
        expect(markup).toContain('aria-label="Switch color theme"')
        expect(markup).toContain('aria-label="FYLO on GitHub"')
        expect(markup).not.toContain('href="/explorer"')
    })

    test('allows the navigation to shrink and scroll without squeezing its controls', async () => {
        const css = await Bun.file(path.join(headerRoot, 'tac.css')).text()

        expect(css).toMatch(/\.site-nav\s*\{[^}]*min-width:\s*0;/)
        expect(css).toMatch(/\.site-nav\s*\{[^}]*overflow-x:\s*auto;/)
        expect(css).toMatch(/\.header-actions\s*\{[^}]*flex-shrink:\s*0;/)
        expect(css).toContain('env(safe-area-inset-left)')
        expect(css).toContain('env(safe-area-inset-right)')
    })

    test('uses external SVG images so compiled templates keep icon namespaces intact', async () => {
        const [header, footer, github] = await Promise.all([
            Bun.file(path.join(headerRoot, 'tac.html')).text(),
            Bun.file(path.join(headerRoot, '../footer/tac.html')).text(),
            Bun.file(path.join(headerRoot, '../../../shared/assets/github.svg')).text()
        ])

        expect(header).toContain('src="/shared/assets/github.svg"')
        expect(footer).toContain('src="/shared/assets/fylo-mark.svg"')
        expect(header).not.toContain('<svg')
        expect(footer).not.toContain('<svg')
        expect(github).toContain('xmlns="http://www.w3.org/2000/svg"')
        expect(github).toContain('viewBox="0 0 24 24"')
    })
})
