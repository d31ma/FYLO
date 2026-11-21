import { afterAll, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { PublishDevBuild } from '../../scripts/ci/publish-dev-build.mjs'

const commit = 'a'.repeat(40)
const otherCommit = 'b'.repeat(40)
const roots = []

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

async function temporaryRoot() {
    const root = await mkdtemp(path.join(tmpdir(), 'fylo-publish-dev-build-'))
    roots.push(root)
    return root
}

function platform(name) {
    return PublishDevBuild.PLATFORMS.find((entry) => entry.name === name)
}

function digest(text) {
    return createHash('sha256').update(text).digest('hex')
}

function identity(name, overrides = {}) {
    return {
        runtimeVersion: '26.45.07-develop.aaaaaaa',
        commit,
        buildKind: 'development-compiled',
        buildTarget: name,
        ...overrides
    }
}

async function stageLane(staging, name, report = {}) {
    const entry = platform(name)
    const content = `binary for ${name}`
    await mkdir(path.join(staging, name), { recursive: true })
    await writeFile(path.join(staging, name, entry.file), content)
    const lane = {
        sha256: digest(content),
        rustc: 'rustc 1.97.1 (test)',
        target: entry.triple,
        identity: identity(name),
        ...report
    }
    await writeFile(path.join(staging, `${name}.lane.json`), JSON.stringify(lane))
    return digest(content)
}

test('parse reads the commit and gate log for a publish', () => {
    expect(PublishDevBuild.parse(['88c31fa', '--gate-log', '/logs/gate.txt'])).toEqual({
        mode: 'publish',
        commit: '88c31fa',
        platform: '',
        out: '',
        logs: '',
        gateLog: '/logs/gate.txt',
        ungated: false,
        contains: []
    })
})

test('parse reads an ungated publish with contained commits', () => {
    const argv = ['--ungated', '88c31fa', '--contains', '0217f2b', '--contains', 'a708ed0']
    const options = PublishDevBuild.parse(argv)
    expect(options.ungated).toBe(true)
    expect(options.contains).toEqual(['0217f2b', 'a708ed0'])
})

test('parse refuses a publish that neither names a gate log nor says ungated', () => {
    expect(() => PublishDevBuild.parse(['88c31fa'])).toThrow(
        'exactly one of --gate-log or --ungated'
    )
})

test('parse refuses a publish with both a gate log and ungated', () => {
    const argv = ['88c31fa', '--gate-log', '/g', '--ungated']
    expect(() => PublishDevBuild.parse(argv)).toThrow('exactly one of --gate-log or --ungated')
})

test('parse reads a lane run', () => {
    const options = PublishDevBuild.parse([
        '--lane',
        'linux-arm64',
        '--sha',
        commit,
        '--out',
        '/tmp/out',
        '--logs',
        '/tmp/logs'
    ])
    expect(options).toEqual({
        mode: 'lane',
        commit,
        platform: 'linux-arm64',
        out: '/tmp/out',
        logs: '/tmp/logs',
        gateLog: '',
        ungated: false,
        contains: []
    })
})

test('parse refuses an unknown option', () => {
    expect(() => PublishDevBuild.parse(['--force', commit, '--ungated'])).toThrow(
        'unknown option --force'
    )
})

test('parse refuses a run without a commit', () => {
    expect(() => PublishDevBuild.parse([])).toThrow('Usage:')
})

test('parse refuses two commits', () => {
    expect(() => PublishDevBuild.parse([commit, otherCommit, '--ungated'])).toThrow(
        'only one commit'
    )
})

test('parse refuses an unknown lane platform', () => {
    const argv = ['--lane', 'linux-x64', '--sha', commit, '--out', '/o', '--logs', '/l']
    expect(() => PublishDevBuild.parse(argv)).toThrow('unknown platform linux-x64')
})

test('parse refuses a lane with a short commit', () => {
    const argv = ['--lane', 'macos-arm64', '--sha', '0217f2b', '--out', '/o', '--logs', '/l']
    expect(() => PublishDevBuild.parse(argv)).toThrow('full 40-character commit')
})

test('parse refuses a lane without an output folder', () => {
    const argv = ['--lane', 'macos-arm64', '--sha', commit, '--logs', '/l']
    expect(() => PublishDevBuild.parse(argv)).toThrow('--out and --logs')
})

test('the platforms match the TACHYON_CI_FYLO_BUILD layout', () => {
    expect(PublishDevBuild.PLATFORMS.map((entry) => `${entry.name}/${entry.file}`)).toEqual([
        'macos-arm64/fylo',
        'linux-arm64/fylo',
        'windows-arm64/fylo.exe'
    ])
})

test('the builds root is refused on HAVEN', async () => {
    await expect(PublishDevBuild.refuseProductionVolume('/Volumes/HAVEN/builds')).rejects.toThrow(
        'HAVEN or ATLAS'
    )
})

test('the builds root is refused on ATLAS', async () => {
    await expect(PublishDevBuild.refuseProductionVolume('/Volumes/ATLAS')).rejects.toThrow(
        'HAVEN or ATLAS'
    )
})

test('the builds root is accepted elsewhere', async () => {
    await expect(
        PublishDevBuild.refuseProductionVolume(await temporaryRoot())
    ).resolves.toBeUndefined()
})

test('a volume that only starts with HAVEN is not refused', async () => {
    await expect(
        PublishDevBuild.refuseProductionVolume('/Volumes/HAVEN-copy/builds')
    ).resolves.toBeUndefined()
})

test('compareVersions orders glibc versions as numbers', () => {
    expect(PublishDevBuild.compareVersions('2.39', '2.39')).toBe(0)
    expect(PublishDevBuild.compareVersions('2.17', '2.39')).toBeLessThan(0)
    expect(PublishDevBuild.compareVersions('2.40', '2.39')).toBeGreaterThan(0)
    expect(PublishDevBuild.compareVersions('2.3.4', '2.39')).toBeLessThan(0)
})

test('sha256 hashes a file', async () => {
    const file = path.join(await temporaryRoot(), 'file')
    await writeFile(file, 'fylo')
    expect(await PublishDevBuild.sha256(file)).toBe(digest('fylo'))
})

test('collect writes fylo.sha256 and returns the manifest entry', async () => {
    const staging = await temporaryRoot()
    const expected = await stageLane(staging, 'windows-arm64')
    const entry = await PublishDevBuild.collect(staging, platform('windows-arm64'), commit)
    expect(entry).toEqual({
        file: 'fylo.exe',
        sha256: expected,
        bytes: 'binary for windows-arm64'.length,
        rustc: 'rustc 1.97.1 (test)',
        rustTarget: 'aarch64-pc-windows-msvc',
        version: '26.45.07-develop.aaaaaaa',
        buildTarget: 'windows-arm64'
    })
    const checksum = await readFile(path.join(staging, 'windows-arm64', 'fylo.sha256'), 'utf8')
    expect(checksum).toBe(`${expected}  fylo.exe\n`)
    expect(await readdir(staging)).toEqual(['windows-arm64'])
})

test('collect makes the published files read-only', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'macos-arm64')
    await PublishDevBuild.collect(staging, platform('macos-arm64'), commit)
    const binary = await stat(path.join(staging, 'macos-arm64', 'fylo'))
    const checksum = await stat(path.join(staging, 'macos-arm64', 'fylo.sha256'))
    expect(binary.mode & 0o777).toBe(0o555)
    expect(checksum.mode & 0o777).toBe(0o444)
})

test('collect refuses a file that changed after the lane built it', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'macos-arm64', { sha256: digest('something else') })
    await expect(PublishDevBuild.collect(staging, platform('macos-arm64'), commit)).rejects.toThrow(
        'the copied file is'
    )
})

test('collect refuses a binary built from another commit', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'macos-arm64', {
        identity: identity('macos-arm64', { commit: otherCommit })
    })
    await expect(PublishDevBuild.collect(staging, platform('macos-arm64'), commit)).rejects.toThrow(
        `reports commit ${otherCommit}`
    )
})

test('collect refuses a binary for another target', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'linux-arm64', {
        glibc: '2.39',
        identity: identity('linux-arm64', { buildTarget: 'linux-x64' })
    })
    await expect(PublishDevBuild.collect(staging, platform('linux-arm64'), commit)).rejects.toThrow(
        'reports target linux-x64'
    )
})

test('collect refuses another build kind', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'macos-arm64', {
        identity: identity('macos-arm64', { buildKind: 'release' })
    })
    await expect(PublishDevBuild.collect(staging, platform('macos-arm64'), commit)).rejects.toThrow(
        'reports build kind release'
    )
})

test('collect refuses a binary built for another Rust target', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'windows-arm64', { target: 'x86_64-pc-windows-msvc' })
    await expect(
        PublishDevBuild.collect(staging, platform('windows-arm64'), commit)
    ).rejects.toThrow('rustc built for x86_64-pc-windows-msvc, not aarch64-pc-windows-msvc')
})

test('collect accepts a linux binary that needs glibc 2.39', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'linux-arm64', { glibc: '2.39' })
    const entry = await PublishDevBuild.collect(staging, platform('linux-arm64'), commit)
    expect(entry.glibc).toBe('2.39')
})

test('collect refuses a linux binary that needs glibc newer than 2.39', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'linux-arm64', { glibc: '2.40' })
    await expect(PublishDevBuild.collect(staging, platform('linux-arm64'), commit)).rejects.toThrow(
        'needs glibc 2.40'
    )
})

test('collect refuses a linux binary without a glibc reading', async () => {
    const staging = await temporaryRoot()
    await stageLane(staging, 'linux-arm64', { glibc: '' })
    await expect(PublishDevBuild.collect(staging, platform('linux-arm64'), commit)).rejects.toThrow(
        'needs glibc unknown'
    )
})

test('manifest lists every platform with one version', () => {
    const entries = PublishDevBuild.PLATFORMS.map((entry) => ({
        file: entry.file,
        sha256: digest(entry.name),
        bytes: 1,
        rustc: 'rustc 1.97.1 (test)',
        rustTarget: entry.triple,
        version: '26.45.07-develop.aaaaaaa',
        buildTarget: entry.name
    }))
    const builtAt = new Date('2026-11-21T12:00:00.000Z')
    const provenance = { gateLog: '/logs/gate.txt', contains: [otherCommit] }
    expect(PublishDevBuild.manifest(commit, entries, builtAt, provenance)).toEqual({
        schema: 1,
        sha: commit,
        contains: [otherCommit],
        gated: true,
        gateLog: '/logs/gate.txt',
        buildKind: 'development-compiled',
        version: '26.45.07-develop.aaaaaaa',
        builtAt: '2026-11-21T12:00:00.000Z',
        rustc: 'rustc 1.97.1 (test)',
        rustTargets: [
            'aarch64-apple-darwin',
            'aarch64-unknown-linux-gnu',
            'aarch64-pc-windows-msvc'
        ],
        platforms: {
            'macos-arm64': entries[0],
            'linux-arm64': entries[1],
            'windows-arm64': entries[2]
        }
    })
})

test('manifest marks an ungated build', () => {
    const entries = [1, 2, 3].map(() => ({ version: '1', rustc: 'rustc', rustTarget: 't' }))
    const manifest = PublishDevBuild.manifest(commit, entries, new Date(), {
        gateLog: '',
        contains: []
    })
    expect(manifest.gated).toBe(false)
    expect(manifest.gateLog).toBeNull()
})

test('manifest refuses platforms that report different versions', () => {
    const entries = ['1', '1', '2'].map((version) => ({ version, rustc: 'rustc' }))
    const provenance = { gateLog: '', contains: [] }
    expect(() => PublishDevBuild.manifest(commit, entries, new Date(), provenance)).toThrow(
        'different versions'
    )
})

test('manifest refuses platforms built by different Rust compilers', () => {
    const entries = ['a', 'a', 'b'].map((rustc) => ({ version: '1', rustc }))
    const provenance = { gateLog: '', contains: [] }
    expect(() => PublishDevBuild.manifest(commit, entries, new Date(), provenance)).toThrow(
        'different Rust compilers'
    )
})

test('finish moves the staging folder into place', async () => {
    const root = await temporaryRoot()
    const staging = path.join(root, `.staging-${commit}`)
    await mkdir(path.join(staging, 'macos-arm64'), { recursive: true })
    await PublishDevBuild.finish(staging, path.join(root, commit))
    expect(await readdir(root)).toEqual([commit])
    expect(await readdir(path.join(root, commit))).toEqual(['macos-arm64'])
})

test('finish never replaces a finished build', async () => {
    const root = await temporaryRoot()
    const staging = path.join(root, `.staging-${commit}`)
    await mkdir(staging)
    await mkdir(path.join(root, commit))
    await expect(PublishDevBuild.finish(staging, path.join(root, commit))).rejects.toThrow(
        'never replaced'
    )
})

test('acquireLock refuses a second publisher while the first runs', async () => {
    const root = await temporaryRoot()
    const lock = await PublishDevBuild.acquireLock(root)
    await expect(PublishDevBuild.acquireLock(root)).rejects.toThrow('another publisher holds')
    await PublishDevBuild.releaseLock(lock)
})

test('acquireLock takes over the lock of a stopped publisher', async () => {
    const root = await temporaryRoot()
    await mkdir(path.join(root, '.publish.lock'))
    await writeFile(path.join(root, '.publish.lock', 'owner'), '999999\n')
    const lock = await PublishDevBuild.acquireLock(root)
    expect(await readFile(path.join(lock, 'owner'), 'utf8')).toBe(`${process.pid}\n`)
    await PublishDevBuild.releaseLock(lock)
})

test('releaseLock lets the next publisher in', async () => {
    const root = await temporaryRoot()
    await PublishDevBuild.releaseLock(await PublishDevBuild.acquireLock(root))
    const lock = await PublishDevBuild.acquireLock(root)
    expect(await readdir(root)).toEqual(['.publish.lock'])
    await PublishDevBuild.releaseLock(lock)
})
