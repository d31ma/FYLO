import { afterAll, expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'

const script = path.resolve('scripts/ci/test-disk.sh')
const onWindows = process.platform === 'win32'
const folders = []

afterAll(async () => {
    await Promise.all(folders.map((folder) => rm(folder, { recursive: true, force: true })))
})

/**
 * @param {string[]} words
 * @returns {{ status: number, errors: string }}
 */
function testDisk(words) {
    const result = Bun.spawnSync(['bash', script, ...words], { stdout: 'pipe', stderr: 'pipe' })
    return { status: result.exitCode, errors: result.stderr.toString() }
}

/** @returns {Promise<string>} */
async function plainFolder() {
    const folder = await mkdtemp(path.join(tmpdir(), 'fylo-test-disk-'))
    folders.push(folder)
    return folder
}

test.skipIf(onWindows)('test-disk.sh without a command prints its usage and exits 2', () => {
    const result = testDisk([])
    expect(result.status).toBe(2)
    expect(result.errors).toContain('test-disk.sh make --folder')
})

test.skipIf(onWindows)('test-disk.sh run without a command prints its usage and exits 2', () => {
    const result = testDisk(['run', '--mount', '/'])
    expect(result.status).toBe(2)
})

test.skipIf(onWindows)('test-disk.sh make refuses an image folder on HAVEN', () => {
    const result = testDisk(['make', '--folder', '/Volumes/HAVEN/fylo-test', '--mount', '/tmp/m'])
    expect(result.status).toBe(1)
    expect(result.errors).toContain('is on a production volume (HAVEN or ATLAS)')
})

test.skipIf(onWindows)('test-disk.sh make refuses an image folder on ATLAS', () => {
    const result = testDisk(['make', '--folder', '/Volumes/ATLAS/fylo-test', '--mount', '/tmp/m'])
    expect(result.status).toBe(1)
    expect(result.errors).toContain('is on a production volume (HAVEN or ATLAS)')
})

test.skipIf(onWindows)("test-disk.sh make refuses the build Mac's lane image folders", () => {
    const result = testDisk(['make', '--folder', '/Volumes/HANGAR/test-disks/windows-vm', '--mount', '/tmp/m'])
    expect(result.status).toBe(1)
    expect(result.errors).toContain("holds the build Mac's lane images")
})

test.skipIf(onWindows)('test-disk.sh make refuses an image folder on the boot disk', async () => {
    const folder = await plainFolder()
    const result = testDisk(['make', '--folder', folder, '--mount', path.join(folder, 'mount')])
    expect(result.status).toBe(1)
    expect(result.errors).toContain('must be on a disk that is not the boot disk')
})

test.skipIf(onWindows)('test-disk.sh run refuses / as a test disk', () => {
    const result = testDisk(['run', '--mount', '/', '--', 'true'])
    expect(result.status).toBe(1)
    expect(result.errors).toContain('FYLO test disk not ready: / is not a mounted test disk')
})

test.skipIf(onWindows)('test-disk.sh run refuses a folder that is not a mount point', async () => {
    const folder = await plainFolder()
    const result = testDisk(['run', '--mount', folder, '--', 'true'])
    expect(result.status).toBe(1)
    expect(result.errors).toContain('is not a mounted test disk')
})
