import { describe, expect, test } from 'bun:test'
import { spawn } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'

import { SoakProcesses } from '../../scripts/rust-soak/processes.mjs'

class ProcessFixture {
    constructor() {
        this.cancellation = new AbortController()
        this.processes = new SoakProcesses(
            { operationTimeoutMs: 100, shutdownTimeoutMs: 1000 },
            this.cancellation.signal
        )
    }

    command(source, timeoutMs = 1000) {
        return this.processes.command([process.execPath, '-e', source], {
            capture: true,
            timeoutMs,
            label: 'fixture command'
        })
    }

    assertReaped(pid) {
        expect(this.processes.children.size).toBe(0)
        expect(() => process.kill(pid, 0)).toThrow()
    }
}

describe('Rust soak owned process cleanup', () => {
    test('a missing command rejects and leaves no owned process', async () => {
        const fixture = new ProcessFixture()
        const directory = await mkdtemp(join(tmpdir(), 'fylo-soak-command-test-'))
        try {
            await expect(
                fixture.processes.command([join(directory, 'missing-command')], {
                    timeoutMs: 1000,
                    label: 'missing command'
                })
            ).rejects.toThrow()
            expect(fixture.processes.children.size).toBe(0)
        } finally {
            await fixture.processes.stopAll()
            await rm(directory, { recursive: true, force: true })
        }
    })

    test('an unsuccessful command is reaped before rejecting', async () => {
        const fixture = new ProcessFixture()
        try {
            const result = fixture.command('process.exit(7)')
            const child = fixture.processes.children.keys().next().value
            await expect(result).rejects.toThrow('fixture command failed (7)')
            fixture.assertReaped(child.pid)
        } finally {
            await fixture.processes.stopAll()
        }
    })

    test('command timeout terminates and reaps an unresponsive child', async () => {
        const fixture = new ProcessFixture()
        try {
            const result = fixture.command(
                'process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)',
                100
            )
            const child = fixture.processes.children.keys().next().value
            await expect(result).rejects.toThrow('fixture command timed out')
            fixture.assertReaped(child.pid)
        } finally {
            await fixture.processes.stopAll()
        }
    })

    test('cancellation terminates and reaps an owned command', async () => {
        const fixture = new ProcessFixture()
        try {
            const result = fixture.command('setInterval(() => {}, 1000)')
            const child = fixture.processes.children.keys().next().value
            fixture.cancellation.abort('SIGTERM')
            await expect(result).rejects.toThrow('soak interrupted by SIGTERM')
            fixture.assertReaped(child.pid)
        } finally {
            await fixture.processes.stopAll()
        }
    })

    test('abnormal exit before close cannot be mistaken for a clean shutdown', async () => {
        const fixture = new ProcessFixture()
        const child = fixture.processes.own(
            spawn(process.execPath, ['-e', 'process.exit(9)'], { stdio: 'pipe' })
        )
        try {
            await fixture.processes.children.get(child).closed
            await expect(fixture.processes.close({ _proc: child })).rejects.toThrow(
                'native shutdown failed (9)'
            )
            fixture.assertReaped(child.pid)
        } finally {
            await fixture.processes.stopAll()
        }
    })
})
