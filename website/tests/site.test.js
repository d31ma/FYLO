import { describe, expect, test } from 'bun:test'
import { ensureBundle } from './helpers/ensure-bundle.js'

const PROJECT_ROOT = import.meta.dir.replace(/\/tests$/, '')

async function read(relativePath) {
    return Bun.file(`${PROJECT_ROOT}/${relativePath}`).text()
}

describe('package setup', () => {
    test('uses standard Tachyon workflow scripts and dependency', async () => {
        const pkg = JSON.parse(await read('package.json'))

        expect(pkg.scripts.start).toBe('bun run serve')
        expect(pkg.scripts.serve).toBe('tach.serve')
        expect(pkg.scripts.bundle).toBe('bun ./scripts/bundle.mjs')
        expect(pkg.scripts['bundle:watch']).toBe('bun ./scripts/bundle.mjs --watch')
        expect(pkg.scripts.preview).toBe('bun ./scripts/preview.mjs')
        expect(pkg.scripts.test).toBe('bun test tests/site.test.js tests/dom/site.dom.test.js')
        expect(pkg.scripts['test:dom']).toBe('bun test tests/dom/site.dom.test.js')
        expect(pkg.devDependencies['@delma/tachyon']).toMatch(/^\^/)
        expect(pkg.devDependencies['happy-dom']).toMatch(/^\^/)
    })
})

describe('bundled output', () => {
    test('emits homepage and docs HTML', { timeout: 60000 }, async () => {
        await ensureBundle()
        const homepage = await read('dist/index.html')
        const docsPage = await read('dist/docs/index.html')

        expect(homepage).toContain('FYLO')
        expect(homepage).toContain('bun add @delma/fylo')
        expect(docsPage).toMatch(/src="(?:\.\.\/|\/)main\.js"/)
        expect(docsPage).toMatch(/src="(?:\.\.\/|\/)spa-renderer\.js"/)
        expect(docsPage).toContain('class="docs-wrap"')
    })

    test('emits routes manifest', { timeout: 60000 }, async () => {
        await ensureBundle()
        const routes = await read('dist/routes.json')

        expect(routes).toContain('"/"')
        expect(routes).toContain('"/docs"')
    })
})
