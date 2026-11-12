import { expect, test } from 'bun:test'
import { fileURLToPath } from 'node:url'

test('the browser and client sources pass the TypeScript check', () => {
    const root = fileURLToPath(new URL('../../', import.meta.url))
    const result = Bun.spawnSync(['bun', 'run', 'typecheck', '--pretty', 'false'], {
        cwd: root,
        stdout: 'pipe',
        stderr: 'pipe'
    })
    expect(result.stdout.toString() + result.stderr.toString()).not.toContain('error TS')
    expect(result.exitCode).toBe(0)
})
