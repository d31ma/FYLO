import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const roots = []

async function createRoot(prefix) {
    const root = await createTestRoot(prefix)
    roots.push(root)
    return root
}

async function run(args, cwd) {
    const proc = Bun.spawn(['bun', ...args], {
        cwd,
        stdout: 'pipe',
        stderr: 'pipe'
    })

    const [stdout, stderr, exitCode] = await Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited
    ])

    return { stdout, stderr, exitCode }
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('CLI', () => {
    test('build emits a working CLI with SQL and richer admin commands', async () => {
        const repo = process.cwd()
        const root = await createRoot('fylo-cli-')
        const schemaDir = path.join(repo, 'examples', 'db', 'schemas')

        const build = await run(['run', 'build'], repo)
        expect(build.exitCode).toBe(0)
        expect(build.stderr.toLowerCase()).not.toContain('error')

        const rebuild = await run(
            ['dist/cli/index.js', 'rebuild', 'cli-posts', '--root', root, '--json'],
            repo
        )
        expect(rebuild.exitCode).toBe(0)
        const rebuildResult = JSON.parse(rebuild.stdout)
        expect(rebuildResult.collection).toBe('cli-posts')
        expect(rebuildResult.docsScanned).toBe(0)
        expect(rebuildResult.indexedDocs).toBe(0)

        const create = await run(
            ['dist/cli/index.js', 'sql', 'CREATE TABLE cli-posts', '--root', root],
            repo
        )
        expect(create.exitCode).toBe(0)
        expect(create.stdout).toContain('Successfully created schema')

        const fylo = new Fylo({ root })
        const cliDocId = await fylo.putData('cli-posts', { title: 'CLI' })

        const select = await run(
            ['dist/cli/index.js', 'sql', 'SELECT * FROM cli-posts', '--root', root],
            repo
        )
        expect(select.exitCode).toBe(0)
        expect(select.stdout).toContain('title')
        expect(select.stdout).toContain('CLI')

        const pagedSelect = await run(
            [
                'dist/cli/index.js',
                'sql',
                'SELECT * FROM cli-posts',
                '--root',
                root,
                '--page-size',
                '1',
                '--align',
                'left',
                '--no-pager'
            ],
            repo
        )
        expect(pagedSelect.exitCode).toBe(0)
        expect(pagedSelect.stdout).toContain('title')
        expect(pagedSelect.stdout).toContain('CLI')

        const inspect = await run(
            ['dist/cli/index.js', 'inspect', 'cli-posts', '--root', root, '--json'],
            repo
        )
        expect(inspect.exitCode).toBe(0)
        const inspectResult = JSON.parse(inspect.stdout)
        expect(inspectResult.collection).toBe('cli-posts')
        expect(inspectResult.exists).toBe(true)
        expect(inspectResult.docsStored).toBe(1)
        expect(inspectResult.deletedDocs).toBe(0)
        expect(inspectResult.indexedDocs).toBe(1)
        expect(inspectResult.worm).toBe(false)

        await fylo.delDoc('cli-posts', cliDocId)
        const deleted = await run(
            ['dist/cli/index.js', 'deleted', 'cli-posts', '--root', root, '--json'],
            repo
        )
        expect(deleted.exitCode).toBe(0)
        expect(JSON.parse(deleted.stdout)[cliDocId].title).toBe('CLI')

        const inspectDeleted = await run(
            ['dist/cli/index.js', 'inspect', 'cli-posts', '--root', root, '--json'],
            repo
        )
        expect(JSON.parse(inspectDeleted.stdout).deletedDocs).toBe(1)

        const restore = await run(
            ['dist/cli/index.js', 'restore', 'cli-posts', cliDocId, '--root', root, '--json'],
            repo
        )
        expect(restore.exitCode).toBe(0)
        expect(JSON.parse(restore.stdout)).toEqual({ restored: true, id: cliDocId })

        const wormFylo = new Fylo({
            root,
            worm: { mode: 'strict' }
        })
        await wormFylo.createCollection('cli-worm')
        const originalId = await wormFylo.putData('cli-worm', { title: 'v1' })

        const inspectWorm = await run(
            ['dist/cli/index.js', 'inspect', 'cli-worm', '--root', root, '--worm', '--json'],
            repo
        )
        expect(inspectWorm.exitCode).toBe(0)
        const inspectWormResult = JSON.parse(inspectWorm.stdout)
        expect(inspectWormResult.worm).toBe(true)
        expect(inspectWormResult.docsStored).toBe(1)
        expect(inspectWormResult.indexedDocs).toBe(1)

        const getHistorical = await run(
            [
                'dist/cli/index.js',
                'get',
                'cli-worm',
                originalId,
                '--root',
                root,
                '--worm',
                '--json'
            ],
            repo
        )
        expect(getHistorical.exitCode).toBe(0)
        const getHistoricalResult = JSON.parse(getHistorical.stdout)
        expect(getHistoricalResult[originalId].title).toBe('v1')

        const latest = await run(
            [
                'dist/cli/index.js',
                'latest',
                'cli-worm',
                originalId,
                '--root',
                root,
                '--worm',
                '--json'
            ],
            repo
        )
        expect(latest.exitCode).toBe(0)
        const latestResult = JSON.parse(latest.stdout)
        expect(latestResult[originalId].title).toBe('v1')

        const latestIdOnly = await run(
            [
                'dist/cli/index.js',
                'latest',
                'cli-worm',
                originalId,
                '--root',
                root,
                '--worm',
                '--id-only'
            ],
            repo
        )
        expect(latestIdOnly.exitCode).toBe(0)
        expect(latestIdOnly.stdout.trim()).toBe(originalId)

        const schemaCurrent = await run(
            ['dist/cli/index.js', 'schema', 'current', 'article', '--schema-dir', schemaDir],
            repo
        )
        expect(schemaCurrent.exitCode).toBe(0)
        expect(schemaCurrent.stdout.trim()).toBe('v2')

        const schemaHistory = await run(
            [
                'dist/cli/index.js',
                'schema',
                'history',
                'article',
                '--schema-dir',
                schemaDir,
                '--json'
            ],
            repo
        )
        expect(schemaHistory.exitCode).toBe(0)
        const schemaHistoryResult = JSON.parse(schemaHistory.stdout)
        expect(schemaHistoryResult).toHaveLength(2)
        expect(schemaHistoryResult[0].version).toBe('v1')
        expect(schemaHistoryResult[1].version).toBe('v2')
        expect(schemaHistoryResult[1].current).toBe(true)

        const schemaDoctor = await run(
            [
                'dist/cli/index.js',
                'schema',
                'doctor',
                'article',
                '--schema-dir',
                schemaDir,
                '--json'
            ],
            repo
        )
        expect(schemaDoctor.exitCode).toBe(0)
        const schemaDoctorResult = JSON.parse(schemaDoctor.stdout)
        expect(schemaDoctorResult.ok).toBe(true)
        expect(schemaDoctorResult.issues).toEqual([])

        const schemaValidate = await run(
            [
                'dist/cli/index.js',
                'schema',
                'validate',
                'article',
                JSON.stringify({
                    id: 7,
                    title: 'Strict Insert',
                    body: 'body',
                    slug: 'strict-insert'
                }),
                '--schema-dir',
                schemaDir,
                '--json'
            ],
            repo
        )
        expect(schemaValidate.exitCode).toBe(0)
        const schemaValidateResult = JSON.parse(schemaValidate.stdout)
        expect(schemaValidateResult.valid).toBe(true)
        expect(schemaValidateResult.document._v).toBe('v2')

        const schemaMaterialize = await run(
            [
                'dist/cli/index.js',
                'schema',
                'materialize',
                'article',
                JSON.stringify({
                    id: 8,
                    title: 'Hello World',
                    body: 'body',
                    _v: 'v1'
                }),
                '--schema-dir',
                schemaDir,
                '--json'
            ],
            repo
        )
        expect(schemaMaterialize.exitCode).toBe(0)
        const schemaMaterializeResult = JSON.parse(schemaMaterialize.stdout)
        expect(schemaMaterializeResult.current).toBe('v2')
        expect(schemaMaterializeResult.document.slug).toBe('hello-world')
        expect(schemaMaterializeResult.document._v).toBe('v2')
    })
})
