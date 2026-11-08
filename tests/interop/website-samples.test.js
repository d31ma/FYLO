import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import DocsSample from '../../website/client/components/docs/sample/tac.js'
import Showcase from '../../website/client/components/code/showcase/tac.js'
import DocsContent from '../../website/client/components/docs/content/tac.js'

function sampleCode(topic, lang) {
    const sample = new DocsSample({ topic })
    sample.$lang = lang
    return sample.code()
}

function showcaseCode(tab, lang) {
    const showcase = new Showcase()
    showcase.$tab = tab
    showcase.$lang = lang
    return showcase.currentCode()
}

function contentCode(section, lang) {
    const content = new DocsContent()
    content.$section = section
    content.$lang = lang
    return content.code()
}

describe('website language samples match the shipped clients', () => {
    test('Go and Rust pass the collection kind their create methods require', () => {
        expect(sampleCode('crud', 'go')).toContain('db.Collection("users").Create("document")')
        expect(sampleCode('crud', 'rust')).toContain('db.collection("users").create("document")?;')
    })

    test('typed clients read the put id as a string', () => {
        expect(sampleCode('crud', 'go')).toContain('id := idValue.(string)')
        expect(sampleCode('crud', 'csharp')).toContain('.GetString();')
        expect(sampleCode('crud', 'swift')).toContain('as! String')
        expect(sampleCode('crud', 'kotlin')).toContain('as String')
        expect(sampleCode('crud', 'rust')).toContain('reply["result"].as_str()')
        expect(sampleCode('crud', 'rust')).toContain('.get(&id)?;')
        expect(sampleCode('crud', 'java')).toContain('.readTree(response).get("result")')
    })

    test('Go samples import fmt and use every variable they declare', () => {
        const query = sampleCode('query', 'go')
        expect(query).toContain('import "fmt"')
        expect(query).toContain('fmt.Println(admins, rows)')
        expect(sampleCode('find-ops', 'go')).toContain(
            'fmt.Println(exact, adults, engineers, privileged)'
        )
        expect(showcaseCode('sql', 'go')).toContain('import "fmt"')
        expect(contentCode('sql', 'go')).toContain('import "fmt"')
    })

    test('browser samples resolve cursors and use the escaping sql tag', () => {
        const crud = sampleCode('crud', 'web')
        expect(crud).toContain('await db.users.get(id).once()')
        const query = sampleCode('query', 'web')
        expect(query).toContain('await Array.fromAsync(db.users.find(')
        expect(query).toContain('.collect())')
        expect(query).toContain("await db.sql`SELECT * FROM users WHERE role = 'admin'`")
        expect(query).not.toContain('executeSQL')
    })

    test('Swift samples add Fylo.swift to the app target instead of importing a module', () => {
        for (const code of [
            sampleCode('crud', 'swift'),
            sampleCode('configuration', 'swift'),
            showcaseCode('start', 'swift'),
            contentCode('install', 'swift'),
            contentCode('crud', 'swift')
        ]) {
            expect(code).not.toContain('import Fylo')
        }
    })

    test('raw-file bucket samples are shown only for binary-backed clients', () => {
        for (const lang of ['web', 'swift', 'kotlin', 'flutter']) {
            const code = sampleCode('bucket', lang)
            expect(code).toContain('need the native fylo binary')
            expect(code).not.toContain('create(')
        }
        expect(sampleCode('bucket', 'node')).toContain("await db.assets.create('file')")
    })

    test('local-only samples create the collection before the first put', () => {
        for (const code of [
            showcaseCode('start', 'web'),
            contentCode('install', 'web'),
            contentCode('install', 'swift'),
            contentCode('install', 'kotlin'),
            contentCode('install', 'flutter')
        ]) {
            expect(code.indexOf('create()')).toBeGreaterThan(-1)
            expect(code.indexOf('create()')).toBeLessThan(code.indexOf('put('))
        }
    })
})

class BucketSampleRunner {
    static repositoryRoot = path.resolve(import.meta.dir, '../..')
    static binary = process.env.FYLO_BINARY
        ? path.resolve(process.env.FYLO_BINARY)
        : path.join(BucketSampleRunner.repositoryRoot, 'dist-bin', 'fylo')
    static workspaces = []
    static fileContent = 'hello bucket'
    static removedFacadeMethods = [
        '.rekey(',
        '.folder(',
        '.verify()',
        '.bytes()',
        '.blob()',
        '.stream()',
        'maxBytes'
    ]
    static checks = `
const assert = (condition, label) => { if (!condition) throw new Error('bucket sample check failed: ' + label) }
assert(new TextDecoder().decode(bytes) === 'hello bucket', 'getFileData bytes')
assert(manifest[id].key === '/reports/2026/summary.pdf', 'manifest key')
assert(Object.keys(exact).includes(id), 'key $eq find')
assert(Object.keys(reports).includes(id), 'key $like find')
assert(report.verified === 1 && report.corrupt.length === 0, 'verifyCollection report')
const meta = await db.getMeta('assets', id)
assert(meta.rating === 4 && !('starred' in meta), 'setMeta update and remove')
`

    static async buildBinary() {
        if (process.env.FYLO_SKIP_BINARY_BUILD === '1') return
        const build = Bun.spawn(['bun', 'run', 'build:exe'], {
            cwd: BucketSampleRunner.repositoryRoot,
            stdout: 'pipe',
            stderr: 'pipe'
        })
        const [stderr, exitCode] = await Promise.all([
            new Response(build.stderr).text(),
            build.exited
        ])
        expect(exitCode, stderr).toBe(0)
    }

    static readmeBucketBlocks(readme) {
        const start = readme.indexOf('### Raw Files (Buckets)')
        const end = readme.indexOf('\n## ', start)
        const section = readme.slice(start, end)
        const blocks = []
        for (const match of section.matchAll(/```js\n([\s\S]*?)```/g)) blocks.push(match[1])
        return blocks
    }

    static async bucketsPage() {
        globalThis.document ??= { title: '' }
        const module = await import('../../website/client/pages/docs/buckets/tac.js')
        return new module.default()
    }

    static async run(blocks, checks) {
        const workspace = await mkdtemp(path.join(os.tmpdir(), 'fylo-bucket-samples-'))
        BucketSampleRunner.workspaces.push(workspace)
        const upload = path.join(workspace, 'summary.pdf')
        await writeFile(upload, BucketSampleRunner.fileContent)
        const shim = pathToFileURL(
            path.join(BucketSampleRunner.repositoryRoot, 'clients/node/fylo.mjs')
        )
        const program = blocks
            .join('\n')
            .replace(
                "import { Fylo } from './fylo.mjs'",
                `import { Fylo } from ${JSON.stringify(shim.href)}`
            )
            .replace(
                "new Fylo('/mnt/fylo')",
                `new Fylo(${JSON.stringify(path.join(workspace, 'root'))}, { binary: ${JSON.stringify(BucketSampleRunner.binary)} })`
            )
            .replaceAll('/uploads/summary.pdf', upload)
        const script = path.join(workspace, 'bucket-sample.mjs')
        await writeFile(
            script,
            `${program}\n${checks}\nawait db.close()\nconsole.log('bucket sample ok')\n`
        )
        const child = Bun.spawn(['node', script], { stdout: 'pipe', stderr: 'pipe' })
        const [stdout, stderr, exitCode] = await Promise.all([
            new Response(child.stdout).text(),
            new Response(child.stderr).text(),
            child.exited
        ])
        expect(exitCode, `${stderr}\n${program}`).toBe(0)
        expect(stdout).toContain('bucket sample ok')
    }
}

describe('bucket documentation runs against the shipped Node shim', () => {
    beforeAll(async () => {
        await mkdir(path.dirname(BucketSampleRunner.binary), { recursive: true })
        await BucketSampleRunner.buildBinary()
    }, 300_000)

    afterAll(async () => {
        await Promise.all(
            BucketSampleRunner.workspaces.map((workspace) =>
                rm(workspace, { recursive: true, force: true })
            )
        )
    })

    test('the README bucket examples run in order', async () => {
        const readme = await readFile(
            path.join(BucketSampleRunner.repositoryRoot, 'README.md'),
            'utf8'
        )
        const blocks = BucketSampleRunner.readmeBucketBlocks(readme)
        expect(blocks.length).toBe(5)
        await BucketSampleRunner.run(blocks, BucketSampleRunner.checks)
    })

    test('the website bucket page examples run in order', async () => {
        const page = await BucketSampleRunner.bucketsPage()
        await BucketSampleRunner.run(
            [page.basicCode, page.keysCode, page.verifyCode, page.metaCode, page.metaQueryCode],
            BucketSampleRunner.checks
        )
    })

    test('the generated Node bucket sample runs', async () => {
        await BucketSampleRunner.run(
            [sampleCode('bucket', 'node')],
            "if (typeof reports !== 'object') throw new Error('bucket sample check failed: find result')"
        )
    })

    test.each(BucketSampleRunner.removedFacadeMethods)(
        'no bucket documentation names the removed %s facade member',
        async (removed) => {
            const readme = await readFile(
                path.join(BucketSampleRunner.repositoryRoot, 'README.md'),
                'utf8'
            )
            const page = await BucketSampleRunner.bucketsPage()
            const documented = [
                ...BucketSampleRunner.readmeBucketBlocks(readme),
                ...Object.values(page)
            ].join('\n')
            expect(documented).not.toContain(removed)
        }
    )
})
