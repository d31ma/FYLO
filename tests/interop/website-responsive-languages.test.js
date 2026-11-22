import { describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import path from 'node:path'
import { sampleCode } from '../../website/client/shared/scripts/samples.js'

const root = path.resolve(import.meta.dir, '../..')
globalThis.Service ??= () => undefined
globalThis.SessionState ??= () => undefined
const { default: LanguageService } =
    await import('../../website/client/services/LanguageService.js')

describe('website DuVay dependency', () => {
    test('vendors the checksum-pinned latest release stylesheet', async () => {
        const vendorRoot = path.join(root, 'website/client/shared/assets/duvay')
        const provenance = await Bun.file(path.join(vendorRoot, 'provenance.json')).json()
        const stylesheet = await Bun.file(path.join(vendorRoot, provenance.artifact)).arrayBuffer()
        const checksum = createHash('sha256').update(new Uint8Array(stylesheet)).digest('hex')

        expect(provenance).toEqual({
            name: '@d31ma/duvay-css',
            version: '26.35.01-1',
            artifact: 'duvay.min.css',
            sha256: 'dd0b9a6841313bc3b4bf30da476dc1aa88821a58220d662b679f83873b92c768',
            source: 'https://github.com/d31ma/DUVAY/releases/tag/v26.35.01-1',
            license: 'MIT'
        })
        expect(checksum).toBe(provenance.sha256)
    })
})

describe('website language selector', () => {
    test('is a swipeable horizontal strip on narrow screens', async () => {
        const [css, markup] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/docs/sample/tac.css')).text(),
            Bun.file(path.join(root, 'website/client/components/docs/sample/tac.html')).text()
        ])

        expect(css).toMatch(
            /@media \(max-width: 599\.98px\)[\s\S]*\.doc-sample-langs[\s\S]*?flex-wrap:\s*nowrap;[\s\S]*?overflow-x:\s*auto;/
        )
        expect(css).toContain('-webkit-overflow-scrolling: touch')
        expect(markup.match(/role="group"/g)).toHaveLength(1)
        expect(markup.match(/aria-pressed=/g)).toHaveLength(9)
        expect(markup).not.toContain('role="tab"')
        expect(markup).not.toContain('role="tablist"')
        expect(markup).toContain('aria-live="polite"')

        const quickstart = sampleCode('web', 'first-write')
        expect(quickstart).toContain('<script type="module">')
        expect(quickstart).toContain('const db = await Fylo.open()')
        expect(quickstart).toContain('</script>')
    })
})

describe('website template companion state', () => {
    test('keeps static feature content out of shipped services', async () => {
        const services = await Array.fromAsync(
            new Bun.Glob('*.js').scan(path.join(root, 'website/client/services'))
        )
        expect(
            await Bun.file(
                path.join(root, 'website/client/components/features/grid/tac.js')
            ).exists()
        ).toBe(false)
        expect(services.filter((name) => /Feature|Question/.test(name))).toEqual([])
    })
})

describe('website application identity guidance', () => {
    test('explains identities, grants, and trust anchors instead of per-record access', async () => {
        const [features, security, securityPage] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/features/grid/tac.html')).text(),
            Bun.file(
                path.join(root, 'website/client/services/DocumentationSecurityService.js')
            ).text(),
            Bun.file(
                path.join(root, 'website/client/components/page/docs-security/tac.html')
            ).text()
        ])

        expect(features).toContain('Encryption & application identities')
        expect(features).toMatch(/signed grants for application\s+identities on protected \.vdbx/)
        expect(security).toContain('fylo identity create')
        expect(security).toContain('fylo create app.vdbx --admin file:admin.pem')
        expect(security).toContain('fylo access grant app.vdbx --grantee web')
        expect(security).toContain('--trust-anchor fid1:')
        expect(security).not.toContain('.as(')
        expect(security).not.toContain('POSTIX')
        expect(securityPage).toContain('Per-record UID, GID, and mode access was removed.')
        // ADR 0016 section 9: wherever .vdbx is introduced, say encryption is off by default.
        expect(securityPage).toMatch(
            /<code>\.vdbx<\/code> file is not encrypted by default: encryption is opt-in/
        )
    })
})

describe('website child-scoped FYLO configuration', () => {
    test('documents every binary shim and the nested-application isolation model', async () => {
        const [sample, sampleMarkup, clients, operations, features, browserEntry] =
            await Promise.all([
                Bun.file(path.join(root, 'website/client/shared/scripts/samples.js')).text(),
                Bun.file(path.join(root, 'website/client/components/docs/sample/tac.html')).text(),
                Bun.file(
                    path.join(root, 'website/client/components/page/docs-clients/tac.html')
                ).text(),
                Bun.file(
                    path.join(root, 'website/client/components/page/docs-operations/tac.html')
                ).text(),
                Bun.file(
                    path.join(root, 'website/client/components/features/grid/tac.html')
                ).text(),
                Bun.file(path.join(root, 'website/client/shared/scripts/imports.js')).text()
            ])

        for (const marker of [
            'with Fylo(',
            'Fylo.open(',
            'new Fylo(',
            'OpenWithOptions',
            'new Fylo.Fylo(',
            'Fylo.Options options',
            'final db = await Fylo.open('
        ]) {
            expect(sample).toContain(marker)
        }
        expect(clients).toContain('<docs-sample topic="configuration" hydrate="load" />')
        for (const client of [
            'Node/TypeScript',
            'Python',
            'Ruby',
            'PHP',
            'Go',
            'Java',
            'C#',
            'Dart'
        ]) {
            expect(clients).toContain(client)
        }
        expect(clients).toContain('"shard": &#123; "width": 2 &#125;')
        expect(operations).toContain('<code>fylo.shard.width</code>')
        expect(operations).toMatch(
            /<td><code>fylo.shard.width<\/code><\/td>[\s\S]*?<td><code>1<\/code><\/td>/
        )
        expect(operations).not.toContain('<code>FYLO_LOGGING</code>')
        expect(operations).toContain('Resolution and isolation')
        expect(operations).toMatch(/without\s+modifying\s+global environment state/)
        expect(features).toContain('independently configured roots can share one application')
        expect(features).toContain('Encryption & application identities')
        expect(sampleMarkup).toContain('role="group" aria-label="Language"')
        expect(sampleMarkup).not.toContain('role="tab"')
        expect(sampleMarkup).not.toContain('role="tablist"')
        expect(sampleMarkup.match(/aria-pressed=/g)).toHaveLength(9)
        expect(browserEntry).toContain('requestAnimationFrame(revealLanguageSelections)')
        expect(browserEntry).toContain('strip.querySelector(\'[aria-pressed="true"]\')')
        expect(clients).toContain('trusted bootstrap input')
        expect(operations).toContain('administrator-controlled bootstrap input')
        expect(operations).toMatch(/Never\s+derive them from a request/)
        expect(sample).not.toContain('./config/fylo.env')
        expect(operations).toContain('"key": "$&#123;FYLO_ENCRYPTION_KEY&#125;"')
        expect(operations).toContain('Unreferenced variables are ignored')
        expect(clients).toContain('former <code>env</code> file/map option is replaced')

        const language = new LanguageService()
        language.choose(null, 'ruby')
        expect(['python', 'ruby'].map((key) => language.pressed(key))).toEqual(['false', 'true'])

        const browserCrud = sampleCode('web', 'crud')
        expect(browserCrud).toContain('await db.users.create()')
        expect(browserCrud).toContain('await db.users.put(')
        expect(browserCrud).toContain('await db.users.get(')
        expect(browserCrud).toContain('await db.users.patch(')
        expect(browserCrud).toContain('await db.users.delete(')

        const browserEnvironment = sampleCode('web', 'configuration')
        expect(browserEnvironment).toContain(
            'https://d31ma.github.io/FYLO/version/26.45.07/fylo.js'
        )
        expect(browserEnvironment).toContain('const db = await Fylo.open({')
        expect(browserEnvironment).toContain('<script type="module">')
        expect(browserEnvironment).not.toContain('createBrowserClient(')

        expect(sampleCode('dart', 'configuration')).toContain('Future<void> main() async {')
    })
})

describe('website Explorer release download', () => {
    test('links the versioned self-hosting ZIP from the download table', async () => {
        const version = (await Bun.file('package.json').json()).version
        const [download, header, footer] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/download/content/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/site/header/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/site/footer/tac.html')).text()
        ])

        expect(download).toContain(`fylo-explorer-${version}.zip`)
        expect(download).toContain('Web (self-hosted Explorer)')
        expect(header).not.toContain('https://fx.del.ma')
        expect(header).not.toContain('>Explorer</a>')
        expect(footer).toContain('<a href="/docs/browser">Explorer</a>')
        expect(footer).not.toContain('href="/explorer"')
    })
})

describe('website serverless queue guidance', () => {
    test('documents bounded decorator invocations without implying a hidden poll loop', async () => {
        const client = path.join(root, 'website/client')
        const [protocolHtml, protocolJs, queueHtml, queueJs, nav, manifest, features] =
            await Promise.all([
                Bun.file(path.join(client, 'components/page/docs-protocol/tac.html')).text(),
                Bun.file(path.join(client, 'services/DocumentationProtocolService.js')).text(),
                Bun.file(path.join(client, 'components/page/docs-queue/tac.html')).text(),
                Bun.file(path.join(client, 'services/DocumentationQueueService.js')).text(),
                Bun.file(path.join(client, 'components/doc/sidebar/tac.html')).text(),
                Bun.file(path.join(client, 'manifest.json')).json(),
                Bun.file(path.join(client, 'components/features/grid/tac.html')).text()
            ])

        expect(protocolHtml).toContain('consumer decorator, annotation, or callable')
        expect(protocolHtml).toContain('one bounded batch')
        expect(protocolJs).toContain("db.queue.consumer('email.welcome', 'email-service'")
        expect(queueHtml).toContain('Serverless does not mean distributed multi-writer storage')
        expect(queueHtml).toContain('Seven machine operations')
        expect(queueHtml).toContain('One-batch consumer decorators')
        expect(queueHtml).toContain('The browser client keeps')
        expect(queueHtml).toContain('does not advertise the native queue capability')
        expect(queueHtml).toMatch(/1,000 most recently acknowledged\s+ID\/receipt\s+pairs/)
        expect(queueHtml).toMatch(/64 MiB of aggregate\s+message-file scan\s+work/)
        expect(queueJs).toContain('@db.queue_consumer(')
        expect(queueJs).toContain('One bounded serverless invocation')
        expect(nav).toContain('<a href="/docs/queue">Serverless queue</a>')
        expect(manifest.tac.metadata.routes['/docs/queue'].title).toBe('Serverless queue — FYLO')
        expect(features).toContain('Serverless without a broker')
    })

    test('canonical queue examples never persist raw exception text', async () => {
        const readme = await Bun.file(path.join(root, 'README.md')).text()
        const queueGuide = readme.match(/## Serverless Queue([\s\S]*?)### Machine Interface/)?.[1]

        expect(queueGuide).toBeDefined()
        expect(queueGuide?.match(/receipt-key\.json/g)).toHaveLength(1)
        expect(queueGuide).toMatch(
            /reason:\s*(?:['"]queue handler failed['"]|(?:sanitize|safe)\w*Reason\(error\))/i
        )
        expect(queueGuide).not.toMatch(
            /reason:\s*(?:String\(error\)|error(?:\.message|\.toString\(\))?|`[^`]*\$\{error(?:\.message)?\}[^`]*`)/
        )
    })
})

describe('website DuVay-derived structure', () => {
    test('uses a focused homepage and one shared documentation shell', async () => {
        expect(await Bun.file(path.join(root, 'website/client/pages/tac.html')).text()).toBe(
            '<page-home />\n'
        )
        const [home, header, hero, features, layout, sidebar, toc, download] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/page/home/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/site/header/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/home/hero/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/features/grid/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/doc/layout/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/doc/sidebar/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/components/doc/toc/tac.html')).text(),
            Bun.file(path.join(root, 'website/client/pages/download/tac.html')).text()
        ])

        expect(home).toContain('<site-header />')
        expect(header).toContain('src="/shared/assets/fylo-mark.svg"')
        expect(header).not.toContain('aria-hidden="true">F</span>')
        expect(home).toContain('<home-hero hydrate="load" />')
        expect(home).toContain('<features-grid />')
        for (const retiredSection of [
            '<stats-strip',
            '<code-showcase',
            '<arch-section',
            '<faq-panels',
            '<cta-banner'
        ]) {
            expect(home).not.toContain(retiredSection)
        }
        expect(hero).toContain('Filesystem-first document store')
        expect(home).toContain('<home-quickstart />')
        expect(home).toContain('<site-footer />')
        expect(hero).toContain('Query the files you already own.')
        expect(features.match(/class="feature-card"/g)).toHaveLength(6)
        expect(layout).toContain('<site-header />')
        expect(layout).toContain('<doc-sidebar />')
        expect(layout).toContain('<main class="doc-main doc-page">')
        expect(layout).toContain('<doc-toc />')
        expect(sidebar).toContain('aria-label="Documentation"')
        expect(toc).toContain('<loop :for="heading of outline.headings">')
        expect(download).toContain('<site-header />')
    })

    test('wraps every documentation route in the shared layout', async () => {
        const glob = new Bun.Glob('website/client/pages/docs/**/tac.html')
        const pages = []
        for await (const file of glob.scan({ cwd: root })) pages.push(file)

        expect(pages).toHaveLength(18)
        for (const file of pages) {
            const page = await Bun.file(path.join(root, file)).text()
            const component = page.match(/^<page-([a-z-]+) \/>\n$/)?.[1]
            expect(component).toBeDefined()
            const source = await Bun.file(
                path.join(root, 'website/client/components/page', component, 'tac.html')
            ).text()
            expect(source).toContain('<doc-layout>')
            expect(source).toContain('</doc-layout>')
            expect(source).not.toContain('<div class="doc-shell">')
        }
    })

    test('links the shared styles from the manifest into every page', async () => {
        const manifest = await Bun.file(path.join(root, 'website/client/manifest.json')).json()

        expect(manifest.tac.styles).toEqual([
            '/shared/assets/duvay/duvay.min.css',
            '/shared/assets/site.css'
        ])
        expect(manifest.tac.styleSources['/shared/assets/duvay/duvay.min.css'].sha256).toBe(
            'dd0b9a6841313bc3b4bf30da476dc1aa88821a58220d662b679f83873b92c768'
        )
        expect(await Bun.file(path.join(root, 'website/tac.config.js')).exists()).toBe(false)
    })
})

describe('website mobile touch targets', () => {
    test('keeps navigation and swipeable tabs at least 44px high', async () => {
        const [header, footer, sample] = await Promise.all([
            Bun.file(path.join(root, 'website/client/components/site/header/tac.css')).text(),
            Bun.file(path.join(root, 'website/client/components/site/footer/tac.css')).text(),
            Bun.file(path.join(root, 'website/client/components/docs/sample/tac.css')).text()
        ])

        expect(header).toMatch(
            /@media \(max-width: 760px\)[\s\S]*min-width:\s*44px;[\s\S]*min-height:\s*44px/
        )
        expect(footer).toMatch(
            /@media \(max-width: 760px\)[\s\S]*min-width:\s*44px;[\s\S]*min-height:\s*44px/
        )
        expect(sample).toContain('@media (hover: none) and (pointer: coarse)')
        expect(sample).toContain('.doc-sample-langs .w-btn')
        expect(sample).toMatch(/min-width:\s*44px;[\s\S]*min-height:\s*44px/)
    })
})
