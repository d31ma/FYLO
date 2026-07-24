import { afterAll, describe, expect, test } from 'bun:test'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const required = process.env.FYLO_REQUIRE_NATIVE_RELEASE === '1'
const binary = path.resolve(process.env.FYLO_BINARY ?? 'dist-bin/fylo')
const expectedTarget =
    process.env.FYLO_EXPECT_TARGET ??
    `${process.platform === 'darwin' ? 'macos' : process.platform}-${process.arch}`
const roots = []

class ReleaseLoop {
    constructor(root) {
        this.child = spawn(binary, ['exec', '--loop', '--root', root, '--exclusive-root'], {
            stdio: ['pipe', 'pipe', 'pipe']
        })
        this.stderr = ''
        this.responses = []
        this.waiters = []
        this.exited = once(this.child, 'exit')
        this.child.stderr.setEncoding('utf8')
        this.child.stderr.on('data', (chunk) => {
            this.stderr += chunk
        })
        this.reader = createInterface({ input: this.child.stdout })
        this.reader.on('line', (line) => {
            const response = JSON.parse(line)
            const waiter = this.waiters.shift()
            if (waiter) waiter(response)
            else this.responses.push(response)
        })
    }

    async request(request) {
        this.child.stdin.write(`${JSON.stringify(request)}\n`)
        if (this.responses.length > 0) return this.responses.shift()
        return await new Promise((resolve) => this.waiters.push(resolve))
    }

    async stop(crash = false) {
        if (this.child.exitCode !== null) return
        if (crash) this.child.kill(process.platform === 'win32' ? undefined : 'SIGKILL')
        else this.child.stdin.end()
        await this.exited
    }
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe.skipIf(!required)('exact native release root lease', () => {
    test('binds identity, canonical aliases, stale metadata, and crash takeover', async () => {
        const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-native-release-lease-'))
        roots.push(root)
        const alias = `${root}-alias`
        roots.push(alias)
        await symlink(root, alias, process.platform === 'win32' ? 'junction' : 'dir')

        const first = new ReleaseLoop(root)
        const identity = await first.request({ op: 'handshake' })
        expect(identity).toMatchObject({
            ok: true,
            result: {
                buildKind: 'release',
                buildTarget: expectedTarget,
                capabilities: { exclusiveRoot: true }
            }
        })

        const contender = new ReleaseLoop(alias)
        expect(await contender.request({ op: 'handshake' })).toMatchObject({
            ok: false,
            error: { code: 'EROOTLOCKED' }
        })
        await contender.stop()
        await first.stop()

        const canonical = await realpath(root)
        const sentinel = path.join(
            path.dirname(canonical),
            `.${path.basename(canonical)}.fylo-root-owner.lock`
        )
        await writeFile(
            `${sentinel}.json`,
            JSON.stringify({ version: 1, root: canonical, owner: 'stale', pid: process.pid })
        )

        const crashOwner = new ReleaseLoop(alias)
        expect((await crashOwner.request({ op: 'handshake' })).ok).toBe(true)
        const currentMetadata = JSON.parse(await readFile(`${sentinel}.json`, 'utf8'))
        expect(currentMetadata.owner).not.toBe('stale')
        await crashOwner.stop(true)

        const replacement = new ReleaseLoop(root)
        expect((await replacement.request({ op: 'handshake' })).ok).toBe(true)
        await replacement.stop()
    })
})
