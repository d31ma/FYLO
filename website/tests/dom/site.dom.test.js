import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { Window } from 'happy-dom'
import { ensureBundle } from '../helpers/ensure-bundle.js'

const PROJECT_ROOT = import.meta.dir.replace(/\/tests\/dom$/, '')

async function read(relativePath) {
    return Bun.file(`${PROJECT_ROOT}/${relativePath}`).text()
}

let homepageWindow
let homepageDocument
let docsWindow
let docsDocument

beforeAll(async () => {
    await ensureBundle()

    homepageWindow = new Window()
    Object.assign(homepageWindow, { SyntaxError })
    homepageDocument = homepageWindow.document.implementation.createHTMLDocument()
    homepageDocument.documentElement.innerHTML = await read('dist/index.html')

    docsWindow = new Window()
    Object.assign(docsWindow, { SyntaxError })
    docsDocument = docsWindow.document.implementation.createHTMLDocument()
    docsDocument.documentElement.innerHTML = await read('dist/docs/index.html')
})

afterAll(() => {
    homepageWindow?.close()
    docsWindow?.close()
})

describe('homepage DOM', () => {
    test('ships the Tachyon bootstrap shell', () => {
        const script = homepageDocument.querySelector('script[src="main.js"], script[src="./main.js"]')
        expect(script).toBeTruthy()
    })

    test('includes the SPA renderer hook', async () => {
        const shell = await read('dist/index.html')
        expect(shell).toMatch(/src="(?:\.\/)?spa-renderer\.js"/)
    })

    test('renders main navigation with expected links', () => {
        const nav = homepageDocument.querySelector('nav[aria-label="Main navigation"]')
        const links = [...homepageDocument.querySelectorAll('.nav-link-group a')].map((a) => ({
            text: a.textContent?.trim(),
            href: a.getAttribute('href'),
        }))
        const docsCta = homepageDocument.querySelector('.nav-install-btn')

        expect(nav).toBeTruthy()
        expect(links.length).toBe(2)
        expect(JSON.stringify(links)).toContain('github.com/d31ma/Fylo')
        expect(JSON.stringify(links)).toContain('npmjs.com/package/@delma/fylo')
        expect(docsCta?.getAttribute('href')).toBe('/docs')
    })

    test('renders footer with site branding', () => {
        const footer = homepageDocument.querySelector('footer.site-footer')
        expect(footer).toBeTruthy()
        expect(footer?.textContent).toContain('FYLO')
        expect(footer?.textContent).toContain('MIT License')
    })

    test('renders hero section with install command', () => {
        const hero = homepageDocument.querySelector('section[aria-labelledby="hero-heading"]')
        const heading = homepageDocument.querySelector('h1#hero-heading')
        const installCmd = [...homepageDocument.querySelectorAll('[role="button"]')]
            .find((node) => node.textContent?.includes('bun add @delma/fylo'))

        expect(hero).toBeTruthy()
        expect(heading?.textContent?.replace(/\s+/g, ' ').trim()).toContain('Filesystem-first')
        expect(installCmd?.textContent).toContain('bun add @delma/fylo')
    })

    test('renders six feature cards', () => {
        const featureSection = homepageDocument.querySelector('section[aria-labelledby="features-heading"]')
        const featureCards = featureSection?.querySelectorAll('h3') ?? []
        expect(featureCards.length).toBe(6)
    })

    test('renders stats strip', () => {
        const statsStrip = homepageDocument.querySelector('section.stats-strip')
        const statItems = homepageDocument.querySelectorAll('.stat-item')

        expect(statsStrip).toBeTruthy()
        expect(statItems.length).toBe(4)
    })

    test('keeps key copy and commands visible in the bundle', async () => {
        const homepage = await read('dist/index.html')

        expect(homepage).toContain('Filesystem-first')
        expect(homepage).toContain('bun add @delma/fylo')
        expect(homepage).toContain('Canonical Documents')
        expect(homepage).toContain('Sync Hooks')
        expect(homepage).toContain('Validation, Auth + Encryption')
    })

    test('keeps homepage code examples readable', () => {
        const sqlDemo = homepageDocument.querySelector('section[aria-labelledby="sql-demo-heading"] .code-body')
        const syncDemo = homepageDocument.querySelector('section[aria-labelledby="stream-demo-heading"] .code-body')

        expect(sqlDemo?.textContent).toContain('import Fylo from "@delma/fylo"')
        expect(sqlDemo?.textContent).toContain('const fylo = new Fylo({ root: "/mnt/fylo" })')
        expect(syncDemo?.textContent).toContain('syncMode: "await-sync"')
        expect(syncDemo?.textContent).toContain('const file = Bun.file(event.path)')
    })
})

describe('docs DOM', () => {
    test('uses nested-route relative runtime scripts', async () => {
        const docsPage = await read('dist/docs/index.html')

        expect(docsPage).toContain('src="../main.js"')
        expect(docsPage).toContain('src="../spa-renderer.js"')
    })

    test('renders main navigation', () => {
        const nav = docsDocument.querySelector('nav[aria-label="Main navigation"]')
        expect(nav).toBeTruthy()
    })

    test('renders docs navigation', () => {
        const docsNav = docsDocument.querySelector('aside[aria-label="Documentation navigation"]')
        expect(docsNav).toBeTruthy()
    })

    test('renders docs sidebar links as hash anchors', () => {
        const firstLink = docsDocument.querySelector('.docs-sidebar a')
        const schemaLink = [...docsDocument.querySelectorAll('.docs-sidebar a')]
            .find((link) => link.textContent?.trim() === 'Schema Validation')
        const authLink = [...docsDocument.querySelectorAll('.docs-sidebar a')]
            .find((link) => link.textContent?.trim() === 'Authorization')

        expect(firstLink?.getAttribute('href')).toBe('#overview')
        expect(schemaLink?.getAttribute('href')).toBe('#schema')
        expect(authLink?.getAttribute('href')).toBe('#authorization')
    })

    test('contains environment variable configuration table', async () => {
        const docsPage = await read('dist/docs/index.html')

        expect(docsPage).toContain('FYLO_ROOT')
        expect(docsPage).toContain('FYLO_S3FILES_ROOT')
        expect(docsPage).toContain('await-sync')
        expect(docsPage).toContain('fylo.query')
        expect(docsPage).toContain('ENCRYPTION_KEY')
        expect(docsPage).toContain('fylo.as')
        expect(docsPage).toContain('sql:execute')
        expect(docsPage).toContain('allowedHosts')
        expect(docsPage).toContain('@delma/fylo@2.2.0')
    })

    test('keeps docs code snippets readable', () => {
        const setupSnippet = docsDocument.querySelector('#code-basic')
        const configSnippet = docsDocument.querySelector('#code-root')
        const sqlSnippet = docsDocument.querySelector('#code-sql')
        const authSnippet = docsDocument.querySelector('#code-auth')

        expect(setupSnippet?.textContent).toContain("import Fylo from '@delma/fylo'")
        expect(setupSnippet?.textContent).toContain("const fylo = new Fylo({")
        expect(configSnippet?.textContent).toContain('# Preferred root config')
        expect(configSnippet?.textContent).toContain('FYLO_ROOT=/mnt/fylo')
        expect(sqlSnippet?.textContent).toContain("await fylo.executeSQL('CREATE TABLE posts')")
        expect(authSnippet?.textContent).toContain('auth: {')
        expect(authSnippet?.textContent).toContain('const db = fylo.as({')
        expect(authSnippet?.textContent).toContain("action === 'doc:read'")
    })
})
