import { describe, expect, test } from 'bun:test'
import { execFile } from 'node:child_process'
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import { createWebArtifact } from '../../scripts/web-artifact.mjs'
import { verifyPagesRelease } from '../../scripts/pages-smoke.mjs'
import { smokeSite } from '../../scripts/web-smoke.mjs'

const execFileAsync = promisify(execFile)

describe('web release operations', () => {
    test('creates checksum-addressed deterministic artifacts', async () => {
        const temporary = await mkdtemp(path.join(os.tmpdir(), 'fylo-artifact-test-'))
        try {
            const source = path.join(temporary, 'web')
            await mkdir(path.join(source, 'assets'), { recursive: true })
            await writeFile(path.join(source, 'index.html'), '<h1>Fylo</h1>')
            await writeFile(path.join(source, 'assets', 'app.js'), 'export default 1')
            const first = await createWebArtifact(source, path.join(temporary, 'one'))
            await writeFile(path.join(source, 'index.html'), '<h1>Fylo</h1>')
            const second = await createWebArtifact(source, path.join(temporary, 'two'))
            expect(first.checksum).toBe(second.checksum)
            expect(path.basename(first.output)).toBe(`${first.checksum}.zip`)
            expect(first.files).toBe(2)
        } finally {
            await rm(temporary, { recursive: true, force: true })
        }
    })

    test('refuses symlinks in deployment artifacts', async () => {
        const temporary = await mkdtemp(path.join(os.tmpdir(), 'fylo-artifact-link-test-'))
        try {
            await writeFile(path.join(temporary, 'index.html'), 'Fylo')
            await symlink(path.join(temporary, 'index.html'), path.join(temporary, 'alias.html'))
            await expect(createWebArtifact(temporary, path.join(temporary, 'out'))).rejects.toThrow(
                'Refusing symlink'
            )
        } finally {
            await rm(temporary, { recursive: true, force: true })
        }
    })

    test('verifies immutable and latest Pages assets, checksums, and equality', async () => {
        const files = new Map([
            ['fylo.js', 'loader'],
            ['fylo-web.mjs', 'engine']
        ])
        const hashes = await Promise.all(
            [...files].map(async ([name, body]) => [
                name,
                new Bun.CryptoHasher('sha256').update(body).digest('hex')
            ])
        )
        const manifest = hashes.map(([name, hash]) => `${hash}  ${name}`).join('\n')
        const fetcher = async (input) => {
            const name = new URL(input).pathname.split('/').at(-1)
            const body = name === 'SHA256SUMS' ? manifest : files.get(name)
            return new Response(body, { status: body === undefined ? 404 : 200 })
        }
        await expect(
            verifyPagesRelease('https://pages.example/Fylo/', '26.29.03', fetcher)
        ).resolves.toEqual({
            version: '26.29.03',
            files: ['fylo.js', 'fylo-web.mjs']
        })
    })

    test('rejects stale latest Pages assets even when both manifests are internally valid', async () => {
        const immutable = new Map([
            ['fylo.js', 'loader'],
            ['fylo-web.mjs', 'engine']
        ])
        const latest = new Map(immutable)
        latest.set('fylo-web.mjs', 'stale-engine')
        const manifestFor = (files) =>
            [...files]
                .map(
                    ([name, body]) =>
                        `${new Bun.CryptoHasher('sha256').update(body).digest('hex')}  ${name}`
                )
                .join('\n')
        const fetcher = async (input) => {
            const pathname = new URL(input).pathname
            const files = pathname.includes('/version/latest/') ? latest : immutable
            const name = pathname.split('/').at(-1)
            return new Response(name === 'SHA256SUMS' ? manifestFor(files) : files.get(name))
        }

        await expect(
            verifyPagesRelease('https://pages.example/Fylo/', '26.29.03', fetcher)
        ).rejects.toThrow('latest fylo-web.mjs differs from immutable 26.29.03')
    })

    test('rejects a missing latest Pages asset', async () => {
        const files = new Map([
            ['fylo.js', 'loader'],
            ['fylo-web.mjs', 'engine']
        ])
        const manifest = [...files]
            .map(
                ([name, body]) =>
                    `${new Bun.CryptoHasher('sha256').update(body).digest('hex')}  ${name}`
            )
            .join('\n')
        const fetcher = async (input) => {
            const pathname = new URL(input).pathname
            const name = pathname.split('/').at(-1)
            if (pathname.includes('/version/latest/') && name === 'fylo-web.mjs') {
                return new Response('missing', { status: 404 })
            }
            return new Response(name === 'SHA256SUMS' ? manifest : files.get(name))
        }

        await expect(
            verifyPagesRelease('https://pages.example/Fylo/', '26.29.03', fetcher)
        ).rejects.toThrow('latest fylo-web.mjs returned HTTP 404')
    })

    test('fails a site smoke check when the marker is absent', async () => {
        const site = { origin: 'https://fylo.example', probes: [{ path: '/', contains: 'FYLO' }] }
        await expect(smokeSite(site, async () => new Response('wrong'))).rejects.toThrow(
            'expected marker'
        )
    })

    test('FXP probe accepts Explorer but rejects the real FYLO homepage artifact', async () => {
        const config = await Bun.file('ops/web-release.json').json()
        const root = path.resolve(import.meta.dir, '../..')
        await execFileAsync('bun', ['run', 'bundle'], { cwd: path.join(root, 'website') })
        await execFileAsync('bun', ['run', 'bundle'], { cwd: path.join(root, 'explorer') })

        const homepage = await Bun.file(path.join(root, 'website/dist/web/index.html')).text()
        const explorer = await Bun.file(path.join(root, 'explorer/dist/web/index.html')).text()

        const respondWith = (body) => async () => new Response(body)

        await expect(smokeSite(config.sites.fxp, respondWith(homepage))).rejects.toThrow(
            'expected marker'
        )
        await expect(smokeSite(config.sites.fxp, respondWith(explorer))).resolves.toEqual([
            { status: 200, url: 'https://fx.del.ma/' }
        ])
    })

    test('keeps operational runbooks available to a clean checkout', async () => {
        const runbooks = ['ops/s3-backup.md', 'docs/operations/web-release.md']
        for (const runbook of runbooks) {
            await expect(Bun.file(runbook).exists()).resolves.toBe(true)
            await expect(
                execFileAsync('git', ['check-ignore', '--no-index', '--quiet', runbook], {
                    cwd: path.resolve(import.meta.dir, '../..')
                })
            ).rejects.toMatchObject({ code: 1 })
        }
    })

    test('wires Pages post-deploy verification and documents rollback', async () => {
        const workflow = await Bun.file('.github/workflows/pages.yml').text()
        const runbook = await Bun.file('docs/operations/web-release.md').text()
        expect(workflow).toContain('bun scripts/pages-smoke.mjs')
        expect(runbook).toContain('git revert <bad-gh-pages-commit>')
        expect(runbook).toContain('bun scripts/amplify-release.mjs rollback fylo')
        expect(runbook).toContain('bun scripts/amplify-release.mjs rollback fxp')
    })

    test('installs every web workspace before compiled interop bundles run', async () => {
        const workflow = await Bun.file('.github/workflows/ci.yml').text()
        const binaryInterop = workflow.slice(workflow.indexOf('    binary-interop:'))

        expect(binaryInterop).toContain('(cd website && bun install --frozen-lockfile)')
        expect(binaryInterop).toContain('(cd explorer && bun install --frozen-lockfile)')
        expect(binaryInterop.indexOf('(cd website && bun install --frozen-lockfile)')).toBeLessThan(
            binaryInterop.indexOf('bun run test:interop')
        )
        expect(
            binaryInterop.indexOf('(cd explorer && bun install --frozen-lockfile)')
        ).toBeLessThan(binaryInterop.indexOf('bun run test:interop'))
    })

    test('pins both Amplify targets and preserves checksum-verified rollback artifacts', async () => {
        const config = await Bun.file('ops/web-release.json').json()
        const release = await Bun.file('scripts/amplify-release.mjs').text()
        expect(config.sites.fylo).toMatchObject({
            appId: 'dhq9jgfyq7uv2',
            origin: 'https://fylo.del.ma'
        })
        expect(config.sites.fxp).toMatchObject({
            appId: 'dnjtojrhwtus2',
            origin: 'https://fx.del.ma'
        })
        expect(release).toContain("'create-deployment'")
        expect(release).toContain("'start-deployment'")
        expect(release).toContain("'get-job'")
        expect(release).toContain('Archived artifact checksum mismatch')
        expect(release).toContain('Deployment failed; restoring')
        expect(release).toContain('previousChecksum')
    })
})
