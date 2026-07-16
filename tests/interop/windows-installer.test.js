import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const root = path.resolve(import.meta.dir, '../..')
const installer = path.join(root, 'website/client/shared/assets/install.ps1')
const powershell = Bun.which('pwsh') ?? Bun.which('powershell.exe') ?? Bun.which('powershell')
const behaviorTest = powershell ? test : test.skip
/** @type {string[]} */
const roots = []

describe('Windows installer integrity', () => {
    afterAll(async () => {
        await Promise.all(roots.map((target) => rm(target, { recursive: true, force: true })))
    })

    test('does not downgrade checksum failures or report success before verifying the executable', async () => {
        const script = await Bun.file(installer).text()

        expect(script).not.toContain('best-effort')
        expect(script).not.toMatch(/catch\s*\{/)
        expect(script).toContain('Checksum metadata does not contain')
        expect(script).toContain('Test-Path -LiteralPath $exe -PathType Leaf')
        expect(script.indexOf('Test-Path -LiteralPath $exe -PathType Leaf')).toBeLessThan(
            script.indexOf('Installed fylo to $exe')
        )
    })
    behaviorTest(
        'fails closed on a checksum mismatch without installing or printing success',
        async () => {
            if (!powershell) throw new Error('PowerShell executable is unavailable')
            const temp = await mkdtemp(path.join(os.tmpdir(), 'fylo-windows-installer-'))
            roots.push(temp)
            const server = Bun.serve({
                hostname: '127.0.0.1',
                port: 0,
                fetch(request) {
                    const pathname = new URL(request.url).pathname
                    if (pathname === '/fylo-windows-x64.exe') return new Response('tampered')
                    if (pathname === '/SHA256SUMS') {
                        return new Response(`${'0'.repeat(64)}  fylo-windows-x64.exe\n`)
                    }
                    return new Response('not found', { status: 404 })
                }
            })
            try {
                const proc = Bun.spawn(
                    [
                        powershell,
                        '-NoLogo',
                        '-NoProfile',
                        '-NonInteractive',
                        '-ExecutionPolicy',
                        'Bypass',
                        '-File',
                        installer
                    ],
                    {
                        env: {
                            ...process.env,
                            LOCALAPPDATA: temp,
                            FYLO_INSTALL_BASE_URL: `http://127.0.0.1:${server.port}`
                        },
                        stdout: 'pipe',
                        stderr: 'pipe'
                    }
                )
                const [exitCode, stdout, stderr] = await Promise.all([
                    proc.exited,
                    new Response(proc.stdout).text(),
                    new Response(proc.stderr).text()
                ])

                expect(exitCode).not.toBe(0)
                expect(`${stdout}\n${stderr}`).toContain('Checksum mismatch')
                expect(`${stdout}\n${stderr}`).not.toContain('Installed fylo to')
                expect(await Bun.file(path.join(temp, 'Fylo', 'fylo.exe')).exists()).toBe(false)
            } finally {
                server.stop(true)
            }
        }
    )
})
