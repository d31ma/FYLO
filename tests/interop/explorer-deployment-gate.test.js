import { afterEach, describe, expect, test } from 'bun:test'
import { cp, mkdir, mkdtemp, rm, stat, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const workspaces = []

afterEach(async () => {
    await Promise.all(
        workspaces.splice(0).map((workspace) => rm(workspace, { recursive: true, force: true }))
    )
})

async function exists(file) {
    try {
        await stat(file)
        return true
    } catch {
        return false
    }
}

async function run(command, cwd, enabled = false) {
    const process = Bun.spawn(command, {
        cwd,
        env: {
            ...globalThis.process.env,
            FYLO_EXPLORER_DEDICATED_ORIGIN: enabled ? '1' : ''
        },
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const [stdout, stderr, exitCode] = await Promise.all([
        new Response(process.stdout).text(),
        new Response(process.stderr).text(),
        process.exited
    ])
    expect(exitCode, `${stderr}\n${stdout}`).toBe(0)
}

async function runGate(workspace, enabled = false) {
    await run(['bun', 'scripts/gate-explorer.mjs'], workspace, enabled)
}

async function createGateWorkspace(rendererSource) {
    const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-explorer-gate-'))
    workspaces.push(workspace)
    await mkdir(path.join(workspace, 'scripts'), { recursive: true })
    await cp('website/scripts/gate-explorer.mjs', path.join(workspace, 'scripts/gate-explorer.mjs'))

    const route = path.join(workspace, 'dist/web/explorer/index.html')
    const runtime = path.join(workspace, 'dist/web/shared/assets/fylo-web.mjs')
    const renderer = path.join(workspace, 'dist/web/spa-renderer.js')
    await mkdir(path.dirname(route), { recursive: true })
    await mkdir(path.dirname(runtime), { recursive: true })
    await writeFile(route, 'explorer')
    await writeFile(runtime, 'runtime')
    await writeFile(renderer, rendererSource)
    return { workspace, route, runtime, renderer }
}

async function expectRendererBuilds(renderer) {
    const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-renderer-build-'))
    workspaces.push(workspace)
    const output = path.join(workspace, 'out.js')
    await run(['bun', 'build', renderer, '--target=browser', '--outfile', output], '.')
    expect(await exists(output)).toBe(true)
}

describe('Explorer deployment gate', () => {
    test('prunes first, middle, and terminal route entries', async () => {
        const fixtures = [
            `const routes='{"/explorer":{},"/":{},"/docs":{}}';const pages='{"/explorer":{"path":"/pages/explorer/tac.js","allowSelf":false},"/":{"path":"/pages/tac.js"}}'`,
            `const routes='{"/":{},"/explorer":{},"/docs":{}}';const pages='{"/":{"path":"/pages/tac.js"},"/explorer":{"path":"/pages/explorer/tac.js","allowSelf":false},"/docs":{}}'`,
            `const routes='{"/":{},"/explorer":{}}';const pages='{"/":{"path":"/pages/tac.js"},"/explorer":{"path":"/pages/explorer/tac.js","allowSelf":false}}'`
        ]

        for (const source of fixtures) {
            const { workspace, route, runtime, renderer } = await createGateWorkspace(source)
            await runGate(workspace)
            expect(await exists(route)).toBe(false)
            expect(await exists(runtime)).toBe(false)
            expect(await Bun.file(renderer).text()).not.toContain('/explorer')
            await expectRendererBuilds(renderer)
        }
    })

    test('retains Explorer only for an explicit dedicated-origin build', async () => {
        const { workspace, route } = await createGateWorkspace(
            `const routes='{"/":{},"/explorer":{}}';const pages='{"/":{"path":"/pages/tac.js"},"/explorer":{"path":"/pages/explorer/tac.js","allowSelf":false}}'`
        )

        await runGate(workspace, true)
        expect(await exists(route)).toBe(true)
    })

    test('prunes the real shared website bundle and retains the dedicated bundle', async () => {
        // The compiler is distributed from a separate authenticated GitHub
        // Packages scope. Keep fixture-level gate coverage portable in CI;
        // release workstations with Tachyon installed also verify the complete
        // generated marketing and dedicated-origin bundles.
        if (!Bun.which('tac.bundle')) return

        const output = path.resolve('website/dist/web')
        const generatedExplorerPaths = [
            'explorer',
            'pages/explorer',
            'components/explorer',
            'shared/assets/explorer.css',
            'shared/assets/fylo-web.mjs',
            'shared/assets/highlight-theme.css',
            'shared/assets/highlight.min.js',
            'shared/assets/duvay/duvay-wc.min.js'
        ]

        await run(['bun', 'run', 'bundle'], 'website')
        for (const relativePath of generatedExplorerPaths) {
            expect(await exists(path.join(output, relativePath))).toBe(false)
        }

        const renderer = path.join(output, 'spa-renderer.js')
        const rendererSource = await Bun.file(renderer).text()
        expect(rendererSource).not.toContain('"/explorer"')
        expect(rendererSource).not.toContain('/pages/explorer/')
        await expectRendererBuilds(renderer)

        const header = await Bun.file(path.join(output, 'components/site/header/tac.js')).text()
        expect(header).not.toContain('/explorer')
        for (const page of ['index.html', 'docs/index.html', 'download/index.html']) {
            expect(await Bun.file(path.join(output, page)).text()).not.toContain('href="/explorer"')
        }

        try {
            await run(['bun', 'run', 'bundle'], 'website', true)
            for (const relativePath of generatedExplorerPaths) {
                expect(await exists(path.join(output, relativePath))).toBe(true)
            }
            const dedicatedRenderer = await Bun.file(renderer).text()
            expect(dedicatedRenderer).toContain('"/explorer"')
            expect(dedicatedRenderer).toContain('/pages/explorer/')
        } finally {
            if (await exists(renderer)) await runGate('website')
        }
    })
})
