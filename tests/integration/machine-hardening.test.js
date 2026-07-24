import { afterAll, describe, expect, test } from 'bun:test'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { lstat, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { runMachineRequest, serveStdioLoop } from '../../src/cli/machine.js'
import {
    collectQueryPage,
    QUERY_CURSOR_TTL_MS,
    queryCursorScope
} from '../../src/cli/query-page.js'
import { acquireRootLease, rootLeasePaths } from '../../src/cli/root-lease.js'
import { runtimeIdentity } from '../../src/cli/runtime-identity.js'
import { createTestRoot } from '../helpers/root.js'

const roots = []

async function createRoot(prefix) {
    const root = await createTestRoot(prefix)
    roots.push(root)
    return root
}

function parseLines(lines) {
    return lines
        .trim()
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
}

async function runLoop(input, options = {}) {
    let output = ''
    const completed = await serveStdioLoop({
        input,
        write(line) {
            output += line
        },
        ...options
    })
    return { completed, responses: parseLines(output) }
}

function exactByteHandshake(size) {
    const empty = JSON.stringify({ op: 'handshake', pad: '' })
    const request = JSON.stringify({
        op: 'handshake',
        pad: 'x'.repeat(size - Buffer.byteLength(empty))
    })
    expect(Buffer.byteLength(request)).toBe(size)
    return request
}

class MachineLoopProcess {
    constructor(root) {
        this.child = spawn(
            'bun',
            [
                'src/cli/index.js',
                'exec',
                '--loop',
                '--root',
                root,
                '--exclusive-root',
                '--max-request-bytes',
                '4096',
                '--max-response-bytes',
                '4096'
            ],
            {
                cwd: process.cwd(),
                stdio: ['pipe', 'pipe', 'pipe']
            }
        )
        this.lines = []
        this.waiters = []
        this.stderr = ''
        this.child.stderr.setEncoding('utf8')
        this.child.stderr.on('data', (chunk) => {
            this.stderr += chunk
        })
        this.child.stdin.on('error', () => {
            // A rejected contender may close before its already-buffered request flushes.
        })
        this.reader = createInterface({ input: this.child.stdout })
        this.reader.on('line', (line) => {
            const waiter = this.waiters.shift()
            if (waiter) waiter.resolve(JSON.parse(line))
            else this.lines.push(line)
        })
        this.child.on('exit', () => {
            const error = new Error(`FYLO loop exited before a response: ${this.stderr}`)
            for (const waiter of this.waiters.splice(0)) waiter.reject(error)
        })
    }

    send(request) {
        this.child.stdin.write(`${JSON.stringify(request)}\n`)
    }

    nextResponse() {
        if (this.lines.length > 0) return Promise.resolve(JSON.parse(this.lines.shift()))
        return new Promise((resolve, reject) => this.waiters.push({ resolve, reject }))
    }

    async stop(signal) {
        if (this.child.exitCode !== null || this.child.signalCode !== null) return
        if (signal) this.child.kill(signal)
        else this.child.stdin.end()
        await once(this.child, 'exit')
    }
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('bounded machine protocol', () => {
    test('accepts an exact-limit frame and rejects limit-plus-one before resynchronizing', async () => {
        const exact = exactByteHandshake(256)
        const over = exactByteHandshake(257)
        const result = await runLoop(
            [Buffer.from(`${exact}\n${over}\n${JSON.stringify({ op: 'handshake' })}\n`)],
            {
                frameLimits: { maxRequestBytes: 256, maxResponseBytes: 2048 }
            }
        )

        expect(result.completed).toBe(true)
        expect(result.responses.map((response) => response.ok)).toEqual([true, false, true])
        expect(result.responses[1].error.code).toBe('EFRAME_REQUEST_TOO_LARGE')
        expect(result.responses[2].result.machine.maxRequestBytes).toBe(256)
    })

    test('bounds unterminated hostile input without retaining attacker-controlled bytes', async () => {
        const result = await runLoop([Buffer.alloc(64 * 1024, 0x78)], {
            frameLimits: { maxRequestBytes: 256, maxResponseBytes: 2048 }
        })

        expect(result.completed).toBe(true)
        expect(result.responses).toHaveLength(1)
        expect(result.responses[0].error.code).toBe('EFRAME_REQUEST_TOO_LARGE')
    })

    test('rejects malformed UTF-8, malformed JSON, and duplicate keys at LF boundaries', async () => {
        const input = Buffer.concat([
            Buffer.from([0xff, 0x0a]),
            Buffer.from('{"op":]\n'),
            Buffer.from('{"op":"handshake","op":"handshake"}\n'),
            Buffer.from('{"op":"handshake"}\n')
        ])
        const result = await runLoop([input], {
            frameLimits: { maxRequestBytes: 256, maxResponseBytes: 2048 }
        })

        expect(result.responses.map((response) => response.error?.code ?? 'ok')).toEqual([
            'EFRAME_UTF8',
            'EFRAME_JSON',
            'EFRAME_DUPLICATE_KEY',
            'ok'
        ])
    })

    test('parses number-dense hostile JSON in one bounded pass', async () => {
        const frame = `[${'0,'.repeat(20_000)}0]\n`
        const result = await runLoop([Buffer.from(frame)], {
            frameLimits: { maxRequestBytes: 65_536, maxResponseBytes: 2048 }
        })

        expect(result.responses).toHaveLength(1)
        expect(result.responses[0].ok).toBe(false)
        expect(result.responses[0].error.message).toBe('Machine request body must be a JSON object')
    })

    test('reports a truncated final frame and never executes it', async () => {
        const root = path.join(await createRoot('fylo-frame-truncated-parent-'), 'not-created')
        const result = await runLoop(
            [Buffer.from(JSON.stringify({ op: 'createCollection', root, collection: 'users' }))],
            {
                frameLimits: { maxRequestBytes: 4096, maxResponseBytes: 2048 }
            }
        )

        expect(result.responses).toHaveLength(1)
        expect(result.responses[0].error.code).toBe('EFRAME_TRUNCATED')
        await expect(lstat(root)).rejects.toMatchObject({ code: 'ENOENT' })
    })

    test('survives repeated hostile frames and emits an explicit oversized-response error', async () => {
        const root = await createRoot('fylo-frame-response-')
        const large = 'z'.repeat(1800)
        const requests = [
            '{"op":]\n',
            `${'x'.repeat(5000)}\n`,
            '{"op":"createCollection","collection":"items"}\n',
            `${JSON.stringify({
                op: 'putData',
                collection: 'items',
                data: { large }
            })}\n`,
            '{"op":"findDocs","collection":"items","query":{}}\n',
            '{"op":"handshake"}\n'
        ]
        const result = await runLoop([Buffer.from(requests.join(''))], {
            overrides: { root },
            frameLimits: { maxRequestBytes: 4096, maxResponseBytes: 1400 }
        })

        expect(result.responses.map((response) => response.error?.code ?? 'ok')).toEqual([
            'EFRAME_JSON',
            'EFRAME_REQUEST_TOO_LARGE',
            'ok',
            'ok',
            'EFRAME_RESPONSE_TOO_LARGE',
            'ok'
        ])
    })

    test('pages active and deleted query results through scoped bounded cursors', async () => {
        const root = await createRoot('fylo-machine-pages-')
        const fylo = new Fylo(root)
        await fylo.items.create()
        const ids = []
        for (let index = 0; index < 24; index++) {
            ids.push(await fylo.items.put({ index, payload: `item-${index}`.repeat(8) }))
        }
        for (const id of ids.slice(0, 5)) await fylo.items.delete(id)
        await fylo.close()

        const overrides = {
            root,
            cache: new Map(),
            frameLimits: { maxRequestBytes: 4096, maxResponseBytes: 2048 }
        }
        const readAll = async (op) => {
            const collected = []
            let cursor
            do {
                const response = await runMachineRequest(
                    {
                        op,
                        collection: 'items',
                        query: { $onlyIds: true },
                        page: { limit: 4, ...(cursor ? { cursor } : {}) }
                    },
                    overrides
                )
                expect(response.ok).toBe(true)
                expect(Buffer.byteLength(JSON.stringify(response))).toBeLessThanOrEqual(2048)
                collected.push(...response.result.items)
                cursor = response.result.nextCursor
            } while (cursor)
            return collected
        }

        expect(await readAll('findDocs')).toEqual(ids.slice(5).sort())
        expect(await readAll('findDeletedDocs')).toEqual(ids.slice(0, 5).sort())
    })

    test('rejects a cursor reused outside its query scope', async () => {
        const root = await createRoot('fylo-machine-page-scope-')
        const fylo = new Fylo(root)
        await fylo.items.create()
        await fylo.items.put({ tenant: 'a' })
        await fylo.items.put({ tenant: 'b' })
        await fylo.close()
        const overrides = {
            root,
            cache: new Map(),
            frameLimits: { maxRequestBytes: 4096, maxResponseBytes: 2048 }
        }
        const first = await runMachineRequest(
            {
                op: 'findDocs',
                collection: 'items',
                query: { $onlyIds: true },
                page: { limit: 1 }
            },
            overrides
        )
        expect(first.ok).toBe(true)
        const mismatched = await runMachineRequest(
            {
                op: 'findDeletedDocs',
                collection: 'items',
                query: { $onlyIds: true },
                page: { limit: 1, cursor: first.result.nextCursor }
            },
            overrides
        )
        expect(mismatched.ok).toBe(false)
        expect(mismatched.error.code).toBe('EINVALIDCURSOR')
    })

    test('keeps a traversal immutable across mutations and invalidates expired cursors', async () => {
        const root = await createRoot('fylo-machine-page-snapshot-')
        const fylo = new Fylo(root)
        await fylo.items.create()
        const originalIds = []
        for (let index = 0; index < 4; index++) {
            originalIds.push(await fylo.items.put({ index }))
        }
        await fylo.close()
        const overrides = {
            root,
            cache: new Map(),
            frameLimits: { maxRequestBytes: 4096, maxResponseBytes: 2048 }
        }
        const request = {
            op: 'findDocs',
            collection: 'items',
            query: { $onlyIds: true },
            page: { limit: 2 }
        }
        const first = await runMachineRequest(request, overrides)
        expect(first.ok).toBe(true)

        const writer = new Fylo(root)
        await writer.items.delete(originalIds[2])
        await writer.items.put({ index: 99 })
        await writer.close()

        const second = await runMachineRequest(
            { ...request, page: { limit: 2, cursor: first.result.nextCursor } },
            overrides
        )
        expect(second.ok).toBe(true)
        expect([...first.result.items, ...second.result.items]).toEqual(originalIds.sort())

        const cursors = new Map()
        async function* values() {
            yield { a: { value: 1 }, b: { value: 2 } }
        }
        const scope = queryCursorScope({
            op: 'findDocs',
            collection: 'items',
            query: {}
        })
        const page = await collectQueryPage(cursors, values(), {
            onlyIds: false,
            scope,
            limit: 1,
            maxResponseBytes: 2048,
            now: 1
        })
        await expect(
            collectQueryPage(cursors, undefined, {
                onlyIds: false,
                scope,
                cursor: page.nextCursor,
                limit: 1,
                maxResponseBytes: 2048,
                now: QUERY_CURSOR_TTL_MS + 2
            })
        ).rejects.toMatchObject({ code: 'EINVALIDCURSOR' })
        expect(cursors.size).toBe(0)
    })

    test('replays a result larger than the ordinary 8 MiB frame through bounded pages', async () => {
        const root = await createRoot('fylo-machine-page-large-')
        const fylo = new Fylo(root)
        await fylo.items.create()
        const ids = []
        const payload = 'p'.repeat(768 * 1024)
        for (let index = 0; index < 12; index++) {
            ids.push(await fylo.items.put({ index, payload }))
        }
        await fylo.close()

        const maximum = 1024 * 1024
        const overrides = {
            root,
            cache: new Map(),
            frameLimits: { maxRequestBytes: 4096, maxResponseBytes: maximum }
        }
        const collected = []
        let cursor
        let encodedBytes = 0
        do {
            const response = await runMachineRequest(
                {
                    op: 'findDocs',
                    collection: 'items',
                    query: {},
                    page: { limit: 4, ...(cursor ? { cursor } : {}) }
                },
                overrides
            )
            expect(response.ok).toBe(true)
            const size = Buffer.byteLength(JSON.stringify(response))
            expect(size).toBeLessThanOrEqual(maximum)
            encodedBytes += size
            collected.push(...Object.keys(response.result.items))
            cursor = response.result.nextCursor
        } while (cursor)

        expect(collected).toEqual(ids.sort())
        expect(encodedBytes).toBeGreaterThan(8 * 1024 * 1024)
    })
})

describe('runtime identity', () => {
    test('CLI identity and machine handshake use the same stable source', async () => {
        const limits = { maxRequestBytes: 2048, maxResponseBytes: 4096 }
        const result = await runLoop([Buffer.from('{"op":"handshake"}\n')], {
            frameLimits: limits
        })

        expect(result.responses[0].result).toEqual(runtimeIdentity(limits))
        expect(result.responses[0].protocolVersion).toBe(result.responses[0].result.protocolVersion)
        expect(result.responses[0].result.buildKind).toBe('development')
        expect(result.responses[0].result.commit).toBe('unknown')
        expect(result.responses[0].result.capabilities.queryPagination).toMatchObject({
            version: 1,
            operations: ['findDocs', 'findDeletedDocs']
        })
    })

    test('handshake is side-effect-free even when the configured root does not exist', async () => {
        const parent = await createRoot('fylo-handshake-parent-')
        const root = path.join(parent, 'does-not-exist')
        const result = await runLoop([Buffer.from('{"op":"handshake"}\n')], {
            overrides: { root },
            frameLimits: { maxRequestBytes: 2048, maxResponseBytes: 4096 }
        })

        expect(result.responses[0].ok).toBe(true)
        await expect(lstat(root)).rejects.toMatchObject({ code: 'ENOENT' })
    })

    test('advertised vendor requirements match the checksum-pinned installer', async () => {
        const installer = await readFile(
            path.join(process.cwd(), 'scripts', 'install-vendor-bins.sh'),
            'utf8'
        )
        const identity = runtimeIdentity()
        const ttid = installer.match(/^TTID_VERSION='v([^']+)'$/m)?.[1]
        const chex = installer.match(/^CHEX_VERSION='v([^']+)'$/m)?.[1]

        expect(identity.dependencies.ttid.requiredVersion).toBe(ttid)
        expect(identity.dependencies.chex.requiredVersion).toBe(chex)
        expect(typeof identity.dependencies.ttid.available).toBe('boolean')
        expect(typeof identity.dependencies.chex.available).toBe('boolean')
    })
})

describe('exclusive root ownership', () => {
    test('canonical aliases and stale lease metadata cannot create a second owner', async () => {
        const root = await createRoot('fylo-root-lease-')
        const alias = `${root}-alias`
        roots.push(alias)
        await symlink(root, alias, process.platform === 'win32' ? 'junction' : 'dir')

        const first = await acquireRootLease(root)
        try {
            await expect(acquireRootLease(alias)).rejects.toMatchObject({
                code: 'EROOTLOCKED'
            })
        } finally {
            await first.release()
        }

        const canonicalPaths = rootLeasePaths(await realpath(root))
        await writeFile(
            canonicalPaths.metadata,
            JSON.stringify({ version: 1, root, owner: 'stale', pid: process.pid })
        )
        const recovered = await acquireRootLease(alias)
        await recovered.assertOwned()
        await recovered.release()
    })

    test('metadata generation fences a former owner without deleting its successor', async () => {
        const root = await createRoot('fylo-root-fence-')
        const lease = await acquireRootLease(root)
        const successor = {
            version: 1,
            root: lease.root,
            owner: 'successor-generation',
            pid: 1
        }
        await writeFile(lease.paths.metadata, JSON.stringify(successor))

        await expect(lease.assertOwned()).rejects.toEqual(
            expect.objectContaining({
                name: 'FyloRootLeaseError',
                code: 'EROOTLEASELOST'
            })
        )
        await lease.release()
        expect(JSON.parse(await readFile(lease.paths.metadata, 'utf8')).owner).toBe(
            'successor-generation'
        )
        await rm(lease.paths.metadata, { force: true })
    })

    test('two simultaneous processes produce exactly one owner and one stable rejection', async () => {
        const root = await createRoot('fylo-root-race-')
        const first = new MachineLoopProcess(root)
        const second = new MachineLoopProcess(root)
        first.send({ op: 'handshake' })
        second.send({ op: 'handshake' })

        try {
            const responses = await Promise.all([first.nextResponse(), second.nextResponse()])
            expect(responses.filter((response) => response.ok)).toHaveLength(1)
            expect(
                responses.filter((response) => response.error?.code === 'EROOTLOCKED')
            ).toHaveLength(1)
        } finally {
            await Promise.all([first.stop(), second.stop()])
        }
    })

    test.skipIf(process.platform === 'win32')(
        'SIGKILL releases the kernel lease and permits crash recovery',
        async () => {
            const root = await createRoot('fylo-root-crash-')
            const owner = new MachineLoopProcess(root)
            owner.send({ op: 'handshake' })
            expect((await owner.nextResponse()).ok).toBe(true)

            const contender = new MachineLoopProcess(root)
            contender.send({ op: 'createCollection', collection: 'must-not-run' })
            const rejected = await contender.nextResponse()
            expect(rejected.error.code).toBe('EROOTLOCKED')
            await contender.stop()

            await owner.stop('SIGKILL')

            const replacement = new MachineLoopProcess(root)
            replacement.send({ op: 'handshake' })
            expect((await replacement.nextResponse()).ok).toBe(true)
            replacement.send({
                op: 'inspectCollection',
                collection: 'must-not-run'
            })
            const inspection = await replacement.nextResponse()
            expect(inspection.ok).toBe(true)
            expect(inspection.result.exists).toBe(false)
            await replacement.stop()
        }
    )
})
