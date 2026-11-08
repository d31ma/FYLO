import { describe, expect, test } from 'bun:test'
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

class SoakRun {
    static script() {
        return process.env.FYLO_TEST_SOAK_RUNNER ?? 'scripts/run-rust-soak.mjs'
    }

    static async start(mode, arguments_ = []) {
        const run = new SoakRun()
        run.directory = await mkdtemp(join(tmpdir(), 'fylo-soak-runner-test-'))
        run.output = join(run.directory, 'report.json')
        run.eventsPath = join(run.directory, 'events.jsonl')
        run.binary = join(run.directory, 'test-engine')
        const source = await readFile('tests/fixtures/soak-process.mjs', 'utf8')
        await writeFile(run.binary, `#!${process.execPath}\n${source}`)
        await chmod(run.binary, 0o700)
        await writeFile(run.eventsPath, '')
        if (mode.startsWith('resource-')) {
            const sample = join(run.directory, 'ps')
            const behavior =
                mode === 'resource-limit'
                    ? "process.stdout.write('600000\\n')"
                    : "process.stderr.write('sample unavailable\\n'); process.exit(1)"
            await writeFile(sample, `#!${process.execPath}\n${behavior}\n`)
            await chmod(sample, 0o700)
        }
        run.child = Bun.spawn(
            [
                process.execPath,
                SoakRun.script(),
                ...arguments_,
                '--iterations',
                '2',
                '--checkpoint-every',
                '1',
                '--restart-every',
                '1',
                '--operation-timeout-ms',
                mode === 'hang-handshake' || mode === 'hang-request' ? '1000' : '5000',
                '--shutdown-timeout-ms',
                '200',
                '--output',
                run.output
            ],
            {
                env: {
                    ...process.env,
                    FYLO_SKIP_RUST_BUILD: '1',
                    FYLO_RUST_BINARY: run.binary,
                    TEST_SOAK_MODE: mode,
                    TEST_SOAK_EVENTS: run.eventsPath,
                    PATH: `${run.directory}:${process.env.PATH}`
                },
                stdout: 'pipe',
                stderr: 'pipe'
            }
        )
        run.stdout = new Response(run.child.stdout).text()
        run.stderr = new Response(run.child.stderr).text()
        return run
    }

    async events() {
        const encoded = await readFile(this.eventsPath, 'utf8')
        return encoded.trim() ? encoded.trim().split('\n').map(JSON.parse) : []
    }

    async waitForHang() {
        const deadline = performance.now() + 5000
        while (performance.now() < deadline) {
            if ((await this.events()).some((event) => event.event === 'hung')) return
            await Bun.sleep(20)
        }
        throw new Error('fixture never reached its hanging operation')
    }

    async finish() {
        let timer
        try {
            const code = await Promise.race([
                this.child.exited,
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error('soak runner did not exit')), 8000)
                })
            ])
            return {
                code,
                stdout: await this.stdout,
                stderr: await this.stderr,
                report: JSON.parse(await readFile(this.output, 'utf8'))
            }
        } finally {
            clearTimeout(timer)
        }
    }

    async assertChildrenExited() {
        const children = (await this.events()).filter((event) => event.event === 'started')
        expect(children.length).toBeGreaterThan(0)
        for (const child of children) {
            expect(() => process.kill(child.pid, 0)).toThrow()
        }
    }

    async dispose() {
        if (this.child.exitCode === null) this.child.kill('SIGKILL')
        await this.child.exited
        const events = await this.events()
        for (const child of events.filter((event) => event.event === 'started')) {
            try {
                process.kill(child.pid, 'SIGKILL')
            } catch (error) {
                if (error.code !== 'ESRCH') throw error
            }
        }
        const roots = new Set(events.map((event) => resolve(event.root, '..')))
        for (const root of roots) await rm(root, { recursive: true, force: true })
        await rm(this.directory, { recursive: true, force: true })
    }
}

describe.skipIf(process.platform === 'win32')('Rust soak process lifecycle', () => {
    for (const mode of ['hang-handshake', 'hang-request', 'hang-close']) {
        test(`${mode} is bounded, reported, and reaped`, async () => {
            const run = await SoakRun.start(mode)
            try {
                const result = await run.finish()
                expect(result.code).not.toBe(0)
                expect(result.report.status).toBe('incomplete')
                expect(result.report.error).toMatch(/tim(e|ed).*out|timeout/i)
                expect(result.report.rootsRetainedOnFailure).toBe(true)
                await run.assertChildrenExited()
            } finally {
                await run.dispose()
            }
        }, 15000)
    }

    test('SIGTERM interrupts a pending operation and still reaps its child', async () => {
        const run = await SoakRun.start('hang-request')
        try {
            await run.waitForHang()
            run.child.kill('SIGTERM')
            const result = await run.finish()
            expect(result.code).not.toBe(0)
            expect(result.report.interruption).toBe('SIGTERM')
            expect(result.report.status).toBe('incomplete')
            await run.assertChildrenExited()
        } finally {
            await run.dispose()
        }
    }, 15000)

    test('checks prior state immediately after restart', async () => {
        const run = await SoakRun.start('stale-restart')
        try {
            const result = await run.finish()
            expect(result.code).not.toBe(0)
            expect(result.report.error).toMatch(/restart|sequence|stale/i)
            expect(result.report.workload.iterations).toBe(1)
            await run.assertChildrenExited()
        } finally {
            await run.dispose()
        }
    }, 15000)

    test('SIGTERM during final cleanup cannot produce a passing report', async () => {
        const run = await SoakRun.start('delayed-final-close', [
            '--iterations',
            '1',
            '--restart-every',
            '100',
            '--checkpoint-every',
            '100',
            '--shutdown-timeout-ms',
            '1000'
        ])
        try {
            await run.waitForHang()
            run.child.kill('SIGTERM')
            const result = await run.finish()
            expect(result.report.interruption).toBe('SIGTERM')
            expect(result.code).not.toBe(0)
            expect(result.report.status).toBe('incomplete')
            await run.assertChildrenExited()
        } finally {
            await run.dispose()
        }
    }, 15000)

    test('records candidate identity and passes coherent restart checks', async () => {
        const run = await SoakRun.start('normal')
        try {
            const result = await run.finish()
            expect(result.stderr).toBe('')
            expect(result.code).toBe(0)
            expect(result.report.status).toBe('passed')
            expect(result.report.binarySha256).toMatch(/^[a-f0-9]{64}$/)
            expect(result.report.sourceBinary).toBe(run.binary)
            expect(result.report.binary).not.toBe(run.binary)
            expect(result.report.metrics.processRestarts).toBe(2)
            await run.assertChildrenExited()
        } finally {
            await run.dispose()
        }
    }, 15000)

    for (const mode of ['resource-limit', 'resource-failure']) {
        test(`${mode} stops the workload and preserves evidence`, async () => {
            const run = await SoakRun.start(mode, ['--checkpoint-every', '100'])
            try {
                const result = await run.finish()
                expect(result.code).not.toBe(0)
                expect(result.report.status).toBe('incomplete')
                expect(result.report.error).toMatch(/resource/)
                expect(result.report.workload.iterations).toBe(1)
                expect(result.report.rootsRetainedOnFailure).toBe(true)
                await run.assertChildrenExited()
            } finally {
                await run.dispose()
            }
        }, 15000)
    }
})

test('release profile rejects shortened duration before starting a process', async () => {
    const child = Bun.spawn(
        [process.execPath, SoakRun.script(), '--profile', 'release', '--duration-hours', '1'],
        { stdout: 'pipe', stderr: 'pipe' }
    )
    const error = await new Response(child.stderr).text()
    expect(await child.exited).not.toBe(0)
    expect(error).toContain('at least 72')
})
