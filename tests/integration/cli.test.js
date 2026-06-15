import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const roots = []
const manualVersioning = { versioning: { autoCommit: false } }

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
    test('version control commands create isolated branches and commit history', async () => {
        const repo = process.cwd()
        const root = await createRoot('fylo-vcs-')

        const build = await run(['run', 'build'], repo)
        expect(build.exitCode).toBe(0)
        expect(build.stderr.toLowerCase()).not.toContain('error')

        const main = new Fylo(root, manualVersioning)
        await main['vc-posts'].create()
        await main['vc-posts'].put({ title: 'main only' })

        const initialCommit = await run(
            [
                'dist/cli/index.js',
                'commit',
                '-m',
                'initial main snapshot',
                '--root',
                root,
                '--json'
            ],
            repo
        )
        expect(initialCommit.exitCode).toBe(0)
        const initialCommitResult = JSON.parse(initialCommit.stdout)
        expect(initialCommitResult.branch).toBe('main')
        expect(initialCommitResult.message).toBe('initial main snapshot')

        const checkoutFeature = await run(
            ['dist/cli/index.js', 'checkout', '-b', 'feature/docs', '--root', root, '--json'],
            repo
        )
        expect(checkoutFeature.exitCode).toBe(0)
        const checkoutFeatureResult = JSON.parse(checkoutFeature.stdout)
        expect(checkoutFeatureResult).toMatchObject({
            branch: 'feature/docs',
            created: true,
            head: initialCommitResult.id
        })

        const feature = new Fylo(root, manualVersioning)
        await feature['vc-posts'].put({ title: 'feature only' })

        const featureCommit = await run(
            ['dist/cli/index.js', 'commit', '-m', 'feature snapshot', '--root', root, '--json'],
            repo
        )
        expect(featureCommit.exitCode).toBe(0)
        const featureCommitResult = JSON.parse(featureCommit.stdout)
        expect(featureCommitResult.branch).toBe('feature/docs')
        expect(featureCommitResult.parents).toEqual([initialCommitResult.id])

        const featureSelect = await run(
            ['dist/cli/index.js', 'sql', 'SELECT * FROM vc-posts', '--root', root],
            repo
        )
        expect(featureSelect.exitCode).toBe(0)
        expect(featureSelect.stdout).toContain('main only')
        expect(featureSelect.stdout).toContain('feature only')

        const branchList = await run(
            ['dist/cli/index.js', 'branch', '--root', root, '--json'],
            repo
        )
        expect(branchList.exitCode).toBe(0)
        const branchListResult = JSON.parse(branchList.stdout)
        expect(branchListResult.current).toBe('feature/docs')
        expect(branchListResult.branches.map((branch) => branch.name)).toEqual([
            'feature/docs',
            'main'
        ])

        const log = await run(['dist/cli/index.js', 'log', '--root', root, '--json'], repo)
        expect(log.exitCode).toBe(0)
        const logResult = JSON.parse(log.stdout)
        expect(logResult.map((commit) => commit.message)).toEqual([
            'feature snapshot',
            'initial main snapshot'
        ])

        const cleanStatus = await run(
            ['dist/cli/index.js', 'status', '--root', root, '--json'],
            repo
        )
        expect(cleanStatus.exitCode).toBe(0)
        expect(JSON.parse(cleanStatus.stdout).clean).toBe(true)

        await feature['vc-posts'].put({ title: 'uncommitted' })

        const dirtyStatus = await run(
            ['dist/cli/index.js', 'status', '--root', root, '--json'],
            repo
        )
        expect(dirtyStatus.exitCode).toBe(0)
        const dirtyStatusResult = JSON.parse(dirtyStatus.stdout)
        expect(dirtyStatusResult.clean).toBe(false)
        expect(dirtyStatusResult.diff.counts.added).toBe(1)

        const dirtyDiff = await run(['dist/cli/index.js', 'diff', '--root', root, '--json'], repo)
        expect(dirtyDiff.exitCode).toBe(0)
        const dirtyDiffResult = JSON.parse(dirtyDiff.stdout)
        expect(dirtyDiffResult.counts).toMatchObject({ added: 1, total: 1 })
        expect(dirtyDiffResult.changes[0]).toMatchObject({
            status: 'added',
            collection: 'vc-posts',
            kind: 'active'
        })

        const guardedRestore = await run(
            [
                'dist/cli/index.js',
                'restore-commit',
                initialCommitResult.id,
                '--root',
                root,
                '--json'
            ],
            repo
        )
        expect(guardedRestore.exitCode).toBe(1)
        expect(guardedRestore.stderr).toContain('Working tree has uncommitted changes')

        const forcedRestore = await run(
            [
                'dist/cli/index.js',
                'restore-commit',
                initialCommitResult.id,
                '--root',
                root,
                '--force',
                '--json'
            ],
            repo
        )
        expect(forcedRestore.exitCode).toBe(0)
        const forcedRestoreResult = JSON.parse(forcedRestore.stdout)
        expect(forcedRestoreResult).toMatchObject({
            branch: 'feature/docs',
            head: initialCommitResult.id,
            restored: initialCommitResult.id,
            forced: true
        })

        const restoredFeatureSelect = await run(
            ['dist/cli/index.js', 'sql', 'SELECT * FROM vc-posts', '--root', root],
            repo
        )
        expect(restoredFeatureSelect.exitCode).toBe(0)
        expect(restoredFeatureSelect.stdout).toContain('main only')
        expect(restoredFeatureSelect.stdout).not.toContain('feature only')
        expect(restoredFeatureSelect.stdout).not.toContain('uncommitted')

        const checkoutMain = await run(
            ['dist/cli/index.js', 'checkout', 'main', '--root', root, '--json'],
            repo
        )
        expect(checkoutMain.exitCode).toBe(0)
        expect(JSON.parse(checkoutMain.stdout).branch).toBe('main')

        const mainSelect = await run(
            ['dist/cli/index.js', 'sql', 'SELECT * FROM vc-posts', '--root', root],
            repo
        )
        expect(mainSelect.exitCode).toBe(0)
        expect(mainSelect.stdout).toContain('main only')
        expect(mainSelect.stdout).not.toContain('feature only')

        const duplicate = await run(
            ['dist/cli/index.js', 'checkout', '-b', 'feature/docs', '--root', root],
            repo
        )
        expect(duplicate.exitCode).toBe(1)
        expect(duplicate.stderr).toContain('Branch already exists')
    })

    test('version control merge handles clean merges and reports conflicts', async () => {
        const repo = process.cwd()
        const root = await createRoot('fylo-merge-')

        const build = await run(['run', 'build'], repo)
        expect(build.exitCode).toBe(0)

        const main = new Fylo(root, manualVersioning)
        await main['merge-posts'].create()
        await main['merge-posts'].put({ title: 'base' })
        await run(['dist/cli/index.js', 'commit', '-m', 'base', '--root', root, '--json'], repo)

        const checkoutFeature = await run(
            ['dist/cli/index.js', 'checkout', '-b', 'feature/merge', '--root', root, '--json'],
            repo
        )
        expect(checkoutFeature.exitCode).toBe(0)
        const feature = new Fylo(root, manualVersioning)
        await feature['merge-posts'].put({ title: 'feature only' })
        const featureCommit = await run(
            ['dist/cli/index.js', 'commit', '-m', 'feature work', '--root', root, '--json'],
            repo
        )
        expect(featureCommit.exitCode).toBe(0)
        const featureCommitResult = JSON.parse(featureCommit.stdout)

        const checkoutMain = await run(
            ['dist/cli/index.js', 'checkout', 'main', '--root', root, '--json'],
            repo
        )
        expect(checkoutMain.exitCode).toBe(0)
        const mainAgain = new Fylo(root, manualVersioning)
        await mainAgain['merge-posts'].put({ title: 'main only' })
        const mainCommit = await run(
            ['dist/cli/index.js', 'commit', '-m', 'main work', '--root', root, '--json'],
            repo
        )
        expect(mainCommit.exitCode).toBe(0)
        const mainCommitResult = JSON.parse(mainCommit.stdout)

        const merge = await run(
            [
                'dist/cli/index.js',
                'merge',
                'feature/merge',
                '-m',
                'merge feature',
                '--root',
                root,
                '--json'
            ],
            repo
        )
        expect(merge.exitCode).toBe(0)
        const mergeResult = JSON.parse(merge.stdout)
        expect(mergeResult).toMatchObject({
            branch: 'main',
            source: featureCommitResult.id,
            mode: 'merge',
            merged: true,
            parents: [mainCommitResult.id, featureCommitResult.id]
        })
        expect(typeof mergeResult.commit).toBe('string')
        expect(mergeResult.applied).toBe(1)

        const mergedSelect = await run(
            ['dist/cli/index.js', 'sql', 'SELECT * FROM merge-posts', '--root', root],
            repo
        )
        expect(mergedSelect.exitCode).toBe(0)
        expect(mergedSelect.stdout).toContain('base')
        expect(mergedSelect.stdout).toContain('main only')
        expect(mergedSelect.stdout).toContain('feature only')

        const conflictRoot = await createRoot('fylo-merge-conflict-')
        const conflictMain = new Fylo(conflictRoot, manualVersioning)
        await conflictMain['merge-conflicts'].create()
        const sharedId = await conflictMain['merge-conflicts'].put({ title: 'base' })
        await run(
            ['dist/cli/index.js', 'commit', '-m', 'conflict base', '--root', conflictRoot],
            repo
        )
        await run(
            ['dist/cli/index.js', 'checkout', '-b', 'feature/conflict', '--root', conflictRoot],
            repo
        )
        const conflictFeature = new Fylo(conflictRoot, manualVersioning)
        await conflictFeature['merge-conflicts'].patch(sharedId, { title: 'feature edit' })
        await run(
            ['dist/cli/index.js', 'commit', '-m', 'feature edit', '--root', conflictRoot],
            repo
        )
        await run(['dist/cli/index.js', 'checkout', 'main', '--root', conflictRoot], repo)
        const conflictMainAgain = new Fylo(conflictRoot, manualVersioning)
        await conflictMainAgain['merge-conflicts'].patch(sharedId, { title: 'main edit' })
        await run(['dist/cli/index.js', 'commit', '-m', 'main edit', '--root', conflictRoot], repo)

        const conflictMerge = await run(
            ['dist/cli/index.js', 'merge', 'feature/conflict', '--root', conflictRoot, '--json'],
            repo
        )
        expect(conflictMerge.exitCode).toBe(1)
        const conflictMergeResult = JSON.parse(conflictMerge.stdout)
        expect(conflictMergeResult).toMatchObject({
            branch: 'main',
            mode: 'conflict',
            merged: false
        })
        expect(conflictMergeResult.conflicts[0]).toMatchObject({
            collection: 'merge-conflicts',
            kind: 'active',
            id: sharedId
        })
    })

    test('build emits a working CLI with SQL and richer admin commands', async () => {
        const repo = process.cwd()
        const root = await createRoot('fylo-cli-')
        const schemaDir = path.join(repo, 'examples', 'db', 'schemas')

        const build = await run(['run', 'build'], repo)
        expect(build.exitCode).toBe(0)
        expect(build.stderr.toLowerCase()).not.toContain('error')

        const create = await run(
            ['dist/cli/index.js', 'sql', 'CREATE TABLE cli-posts', '--root', root],
            repo
        )
        expect(create.exitCode).toBe(0)
        expect(create.stdout).toContain('Successfully created schema')

        const rebuild = await run(
            ['dist/cli/index.js', 'rebuild', 'cli-posts', '--root', root, '--json'],
            repo
        )
        expect(rebuild.exitCode).toBe(0)
        const rebuildResult = JSON.parse(rebuild.stdout)
        expect(rebuildResult.collection).toBe('cli-posts')
        expect(rebuildResult.docsScanned).toBe(0)
        expect(rebuildResult.indexedDocs).toBe(0)

        const fylo = new Fylo(root)
        const cliDocId = await fylo['cli-posts'].put({ title: 'CLI' })

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

        await fylo['cli-posts'].delete(cliDocId)
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

        const wormFylo = new Fylo(root, { worm: { mode: 'strict' } })
        await wormFylo['cli-worm'].create()
        const originalId = await wormFylo['cli-worm'].put({ title: 'v1' })

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
