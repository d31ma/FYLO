import { afterEach, describe, expect, test } from 'bun:test'
import { chmod, mkdir, mkdtemp, readdir, rm, symlink, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const root = path.resolve(import.meta.dir, '../..')
const installer = path.join(root, 'website/client/shared/assets/install.sh')
const roots = []

async function fixture(checksums) {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'fylo-install-test-'))
    roots.push(directory)
    const release = path.join(directory, 'release')
    const destination = path.join(directory, 'bin')
    await mkdir(release)
    await mkdir(destination)
    const osTag = process.platform === 'darwin' ? 'macos' : 'linux'
    const archTag = process.arch === 'arm64' ? 'arm64' : 'x64'
    const asset = `fylo-${osTag}-${archTag}`
    const bytes = Buffer.from('verified replacement')
    await writeFile(path.join(release, asset), bytes)
    const digest = new Bun.CryptoHasher('sha256').update(bytes).digest('hex')
    await writeFile(path.join(release, 'SHA256SUMS'), checksums({ asset, digest }))
    await writeFile(path.join(destination, 'fylo'), 'prior executable')
    await chmod(path.join(destination, 'fylo'), 0o755)
    return { directory, release, destination, asset }
}

async function run(input) {
    return Bun.spawn(['/bin/sh', installer], {
        env: {
            ...process.env,
            FYLO_RELEASE_BASE: `file://${input.release}`,
            FYLO_INSTALL_DIR: input.destination
        },
        stdout: 'pipe',
        stderr: 'pipe'
    }).exited
}

afterEach(async () => {
    await Promise.all(roots.splice(0).map((directory) => rm(directory, { recursive: true })))
})

describe('Unix installer integrity', () => {
    test('installs only after an exact, valid asset checksum', async () => {
        const input = await fixture(({ asset, digest }) => `${digest}  ${asset}\n`)
        expect(await run(input)).toBe(0)
        expect(await Bun.file(path.join(input.destination, 'fylo')).text()).toBe(
            'verified replacement'
        )
        expect(
            (await readdir(input.destination)).some((name) => name.startsWith('.fylo-install.'))
        ).toBe(false)
    })

    for (const [name, sums] of [
        ['missing row', () => ''],
        ['malformed row', ({ asset }) => `not-a-hash  ${asset}\n`],
        ['mismatch', ({ asset }) => `${'0'.repeat(64)}  ${asset}\n`],
        ['duplicate row', ({ asset, digest }) => `${digest}  ${asset}\n${digest}  ${asset}\n`]
    ]) {
        test(`${name} aborts and preserves the prior executable`, async () => {
            const input = await fixture(sums)
            expect(await run(input)).not.toBe(0)
            expect(await Bun.file(path.join(input.destination, 'fylo')).text()).toBe(
                'prior executable'
            )
            expect(
                (await readdir(input.destination)).some((entry) =>
                    entry.startsWith('.fylo-install.')
                )
            ).toBe(false)
        })
    }

    test('statically requires a hash tool and a successful checksum download', async () => {
        const script = await Bun.file(installer).text()
        expect(script).toContain('A SHA-256 tool (sha256sum or shasum) is required')
        expect(script).toContain('curl -fsSL "${BASE}/SHA256SUMS"')
        expect(script).toContain('FYLO_VERIFY_PROVENANCE')
        expect(script).toContain('gh attestation verify "$tmp/fylo" --repo "$REPO"')
        expect(script).not.toContain('SHA256SUMS" || true')
    })

    test('missing SHA256SUMS aborts and preserves the prior executable', async () => {
        const input = await fixture(({ asset, digest }) => `${digest}  ${asset}\n`)
        await rm(path.join(input.release, 'SHA256SUMS'))
        expect(await run(input)).not.toBe(0)
        expect(await Bun.file(path.join(input.destination, 'fylo')).text()).toBe('prior executable')
    })

    test('missing hash tools abort before replacing the prior executable', async () => {
        const input = await fixture(({ asset, digest }) => `${digest}  ${asset}\n`)
        const tools = path.join(input.directory, 'tools')
        await mkdir(tools)
        for (const command of ['uname', 'mkdir', 'mktemp', 'rm', 'curl', 'awk', 'chmod', 'mv']) {
            const candidates = [`/usr/bin/${command}`, `/bin/${command}`]
            const source = candidates.find((candidate) => Bun.file(candidate).size > 0)
            if (source) await symlink(source, path.join(tools, command))
        }
        const child = Bun.spawn(['/bin/sh', installer], {
            env: {
                ...process.env,
                PATH: tools,
                FYLO_RELEASE_BASE: `file://${input.release}`,
                FYLO_INSTALL_DIR: input.destination
            },
            stdout: 'pipe',
            stderr: 'pipe'
        })
        expect(await child.exited).not.toBe(0)
        expect(await Bun.file(path.join(input.destination, 'fylo')).text()).toBe('prior executable')
    })
})
