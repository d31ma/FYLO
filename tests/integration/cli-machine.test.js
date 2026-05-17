import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { readdir, rm } from 'node:fs/promises'
import path from 'node:path'
import { createTestRoot } from '../helpers/root.js'

const roots = []
let buildResult

async function createRoot(prefix) {
    const root = await createTestRoot(prefix)
    roots.push(root)
    return root
}

/**
 * @param {string[]} args
 * @param {string} cwd
 * @param {string=} stdinText
 */
async function run(args, cwd, stdinText) {
    const proc = Bun.spawn(['bun', ...args], {
        cwd,
        stdin: stdinText === undefined ? 'ignore' : new Blob([stdinText]),
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

/**
 * @param {string} root
 * @returns {Promise<string[]>}
 */
async function listJsFiles(root) {
    /** @type {string[]} */
    const files = []
    for (const entry of await readdir(root, { withFileTypes: true })) {
        const fullPath = path.join(root, entry.name)
        if (entry.isDirectory()) {
            files.push(...(await listJsFiles(fullPath)))
            continue
        }
        if (entry.isFile() && fullPath.endsWith('.js')) files.push(fullPath)
    }
    return files
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('CLI machine interface', () => {
    beforeAll(async () => {
        buildResult = await run(['run', 'build'], process.cwd())
    })

    test('build publishes declaration output without stray JS helpers', async () => {
        const repo = process.cwd()
        expect(buildResult.exitCode).toBe(0)
        const jsFiles = await listJsFiles(path.join(repo, 'dist', 'types'))
        expect(jsFiles).toEqual([])
    })

    test('exec handles JSON requests from inline payloads and stdin', async () => {
        const repo = process.cwd()
        const root = await createRoot('fylo-machine-')
        const schemaDir = path.join(repo, 'tests', 'schemas')

        expect(buildResult.exitCode).toBe(0)

        const createResponse = await run(
            [
                'dist/cli/index.js',
                'exec',
                '--request',
                JSON.stringify({
                    requestId: 'create-1',
                    op: 'createCollection',
                    root,
                    collection: 'machine-posts'
                })
            ],
            repo
        )
        expect(createResponse.exitCode).toBe(0)
        const createPayload = JSON.parse(createResponse.stdout)
        expect(createPayload.ok).toBe(true)
        expect(createPayload.protocolVersion).toBe(1)
        expect(createPayload.requestId).toBe('create-1')
        expect(createPayload.result.collection).toBe('machine-posts')

        const putResponse = await run(
            [
                'dist/cli/index.js',
                'exec',
                '--request',
                JSON.stringify({
                    op: 'putData',
                    root,
                    collection: 'machine-posts',
                    data: { title: 'Interop', published: true }
                })
            ],
            repo
        )
        expect(putResponse.exitCode).toBe(0)
        const putPayload = JSON.parse(putResponse.stdout)
        expect(putPayload.ok).toBe(true)
        const docId = putPayload.result

        const latestResponse = await run(
            ['dist/cli/index.js', 'exec', '--request', '-'],
            repo,
            JSON.stringify({
                requestId: 'latest-1',
                op: 'getLatest',
                root,
                collection: 'machine-posts',
                id: docId
            })
        )
        expect(latestResponse.exitCode).toBe(0)
        const latestPayload = JSON.parse(latestResponse.stdout)
        expect(latestPayload.ok).toBe(true)
        expect(Object.values(latestPayload.result)[0].title).toBe('Interop')

        const queryResponse = await run(
            [
                'dist/cli/index.js',
                'exec',
                '--request',
                JSON.stringify({
                    op: 'findDocs',
                    root,
                    collection: 'machine-posts',
                    query: { published: true }
                })
            ],
            repo
        )
        expect(queryResponse.exitCode).toBe(0)
        const queryPayload = JSON.parse(queryResponse.stdout)
        expect(queryPayload.ok).toBe(true)
        expect(queryPayload.result[docId].title).toBe('Interop')

        const schemaInspectResponse = await run(
            [
                'dist/cli/index.js',
                'exec',
                '--request',
                JSON.stringify({
                    op: 'schemaInspect',
                    schemaDir,
                    collection: 'article'
                })
            ],
            repo
        )
        expect(schemaInspectResponse.exitCode).toBe(0)
        const schemaInspectPayload = JSON.parse(schemaInspectResponse.stdout)
        expect(schemaInspectPayload.ok).toBe(true)
        expect(schemaInspectPayload.result.current).toBe('v2')
        expect(schemaInspectPayload.result.versions).toHaveLength(2)

        const schemaMaterializeResponse = await run(
            [
                'dist/cli/index.js',
                'exec',
                '--request',
                JSON.stringify({
                    op: 'schemaMaterialize',
                    schemaDir,
                    collection: 'article',
                    document: {
                        id: 9,
                        title: 'Machine Article',
                        body: 'body',
                        _v: 'v1'
                    }
                })
            ],
            repo
        )
        expect(schemaMaterializeResponse.exitCode).toBe(0)
        const schemaMaterializePayload = JSON.parse(schemaMaterializeResponse.stdout)
        expect(schemaMaterializePayload.ok).toBe(true)
        expect(schemaMaterializePayload.result.current).toBe('v2')
        expect(schemaMaterializePayload.result.document.slug).toBe('machine-article')
        expect(schemaMaterializePayload.result.document._v).toBe('v2')
    })

    test('exec returns structured JSON errors with non-zero exits', async () => {
        const repo = process.cwd()

        expect(buildResult.exitCode).toBe(0)

        const response = await run(
            ['dist/cli/index.js', 'exec', '--request', JSON.stringify({ op: 'unknownOperation' })],
            repo
        )
        expect(response.exitCode).toBe(1)
        expect(response.stderr).toBe('')
        const payload = JSON.parse(response.stdout)
        expect(payload.ok).toBe(false)
        expect(payload.error.message).toContain('Unsupported machine operation')
    })
})
