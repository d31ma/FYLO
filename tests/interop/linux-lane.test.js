import { expect, test } from 'bun:test'
import { readFile } from 'node:fs/promises'
import path from 'node:path'

const script = path.resolve('scripts/ci/linux-lane.sh')
const onWindows = process.platform === 'win32'

/**
 * @param {string[]} words
 * @param {Record<string, string>} [environment]
 * @returns {{ status: number, output: string, errors: string }}
 */
function linuxLane(words, environment = {}) {
    const result = Bun.spawnSync(['bash', script, ...words], {
        stdout: 'pipe',
        stderr: 'pipe',
        env: {
            ...process.env,
            FYLO_TEST_OWNER: '',
            MAC_HEAVY_OWNER: '',
            FYLO_LINUX_BUILD_VOLUME: '',
            ...environment
        }
    })
    return {
        status: result.exitCode,
        output: result.stdout.toString(),
        errors: result.stderr.toString()
    }
}

test.skipIf(onWindows)('linux-lane.sh without a command prints its usage and exits 2', () => {
    const result = linuxLane([])
    expect(result.status).toBe(2)
    expect(result.errors).toContain('--show-volumes')
})

test.skipIf(onWindows)('linux-lane.sh gives each owner its own build volume', () => {
    const result = linuxLane(['--owner', 'fylo-l1-4', '--show-volumes'])
    expect(result.status).toBe(0)
    expect(result.output).toBe('test fylo-test-volume\nbuild fylo-linux-lane-build-fylo-l1-4\n')
})

test.skipIf(onWindows)(
    'linux-lane.sh turns characters Docker refuses in an owner into dashes',
    () => {
        const result = linuxLane(['--owner', 'a b/c', '--show-volumes'])
        expect(result.output).toContain('build fylo-linux-lane-build-a-b-c\n')
    }
)

test.skipIf(onWindows)('linux-lane.sh takes the owner from FYLO_TEST_OWNER', () => {
    const result = linuxLane(['--show-volumes'], { FYLO_TEST_OWNER: 'from-env' })
    expect(result.output).toContain('build fylo-linux-lane-build-from-env\n')
})

test.skipIf(onWindows)("linux-lane.sh falls back to the worktree's folder name", () => {
    const result = linuxLane(['--show-volumes'])
    expect(result.output).toContain(`build fylo-linux-lane-build-${path.basename(process.cwd())}\n`)
})

test.skipIf(onWindows)('FYLO_LINUX_BUILD_VOLUME names the build volume', () => {
    const result = linuxLane(['--owner', 'ignored', '--show-volumes'], {
        FYLO_LINUX_BUILD_VOLUME: 'my-volume'
    })
    expect(result.output).toContain('build my-volume\n')
})

test.skipIf(onWindows)(
    'the container extracts the sources with fresh modification times',
    async () => {
        const text = await readFile(script, 'utf8')
        expect(text).toContain('tar -m -xf /stage/src.tar -C /build/src')
    }
)
