import { describe, expect, test } from 'bun:test'
import path from 'node:path'
import { sampleCode, sampleLanguages } from '../../website/client/shared/scripts/samples.js'

const root = path.resolve(import.meta.dir, '../..')
const homepage = await Bun.file(
    path.join(root, 'website/client/components/home/quickstart/tac.html')
).text()

describe('website guided homepage', () => {
    test('offers working documentation destinations', async () => {
        const links = [...homepage.matchAll(/href="(\/[^"#]*)"/g)].map((match) => match[1])
        expect(links).toContain('/docs/operations')
        expect(links).toContain('/docs/limitations')
        for (const link of links) {
            expect(
                await Bun.file(path.join(root, 'website/client/pages', link, 'tac.html')).exists()
            ).toBe(true)
        }
    })

    test('shows nested configuration and secret references, not credentials', () => {
        const code = homepage.match(/<pre class="code-block"><code>([\s\S]*?)<\/code><\/pre>/)?.[1]
        expect(code).toBeDefined()
        const configuration = JSON.parse(code.replaceAll('&#123;', '{').replaceAll('&#125;', '}'))
        expect(configuration).toEqual({
            fylo: {
                root: './data',
                shard: { width: 1 },
                encryption: { key: '${FYLO_ENCRYPTION_KEY}' },
                cipher: { salt: '${FYLO_CIPHER_SALT}' }
            }
        })
        expect(homepage).toContain('Encryption is optional.')
        expect(homepage).toContain('environment variables no longer implicitly configure FYLO')
    })

    test('reuses the documentation recipe for every language', () => {
        expect(homepage).toContain('<docs-sample topic="first-write" hydrate="load" />')
        const languages = sampleLanguages()
        expect(languages).toHaveLength(9)
        for (const language of languages) {
            const code = sampleCode(language.key, 'first-write')
            expect(code).toContain('Ada')
            expect(code).not.toContain('undefined')
        }
    })
})
