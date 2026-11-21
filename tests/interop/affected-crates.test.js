import { afterAll, expect, test } from 'bun:test'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { AffectedCrates } from '../../scripts/ci/affected-crates.mjs'

const script = path.resolve('scripts/ci/affected-crates.mjs')
const roots = []

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

function dependency(name) {
    return { name, path: `/work/${name}` }
}

function metadata() {
    return {
        workspace_root: '/work',
        workspace_members: ['a-id', 'b-id', 'c-id', 'd-id', 'inner-id', 'support-id'],
        packages: [
            { id: 'a-id', name: 'a', manifest_path: '/work/crates/a/Cargo.toml', dependencies: [] },
            {
                id: 'b-id',
                name: 'b',
                manifest_path: '/work/crates/b/Cargo.toml',
                dependencies: [dependency('a'), { name: 'serde' }]
            },
            {
                id: 'c-id',
                name: 'c',
                manifest_path: '/work/crates/c/Cargo.toml',
                dependencies: [dependency('b'), dependency('outside')]
            },
            { id: 'd-id', name: 'd', manifest_path: '/work/src/Cargo.toml', dependencies: [] },
            {
                id: 'inner-id',
                name: 'inner',
                manifest_path: '/work/src/browser/wasm/Cargo.toml',
                dependencies: [dependency('a')]
            },
            {
                id: 'support-id',
                name: 'fylo-test-support',
                manifest_path: '/work/crates/fylo-test-support/Cargo.toml',
                dependencies: []
            },
            {
                id: 'outside-id',
                name: 'outside',
                manifest_path: '/work/vendor/outside/Cargo.toml',
                dependencies: [dependency('a')]
            }
        ]
    }
}

const workspace = AffectedCrates.workspaceFromMetadata(metadata())

test('workspace crates keep their folder and only path dependencies on members', () => {
    const b = workspace.crates.find((crate) => crate.name === 'b')
    expect(b).toEqual({ name: 'b', directory: 'crates/b/', dependencies: ['a'] })
})

test('workspace crates leave out packages that are not members', () => {
    expect(workspace.crates.map((crate) => crate.name).sort()).toEqual([
        'a',
        'b',
        'c',
        'd',
        'fylo-test-support',
        'inner'
    ])
})

test('a change in a crate selects it and every crate that depends on it', () => {
    const result = AffectedCrates.affected(workspace, ['crates/a/src/lib.rs'])
    expect(result).toEqual({
        full: false,
        reasons: [],
        changed: ['a'],
        crates: ['a', 'b', 'c', 'inner']
    })
})

test('a change in a crate nobody depends on selects only that crate', () => {
    const result = AffectedCrates.affected(workspace, ['crates/c/tests/one.rs'])
    expect(result.crates).toEqual(['c'])
})

test('a file in a nested crate belongs to the deepest crate', () => {
    const result = AffectedCrates.affected(workspace, ['src/browser/wasm/src/lib.rs'])
    expect(result.changed).toEqual(['inner'])
})

test('a file in the outer crate folder belongs to the outer crate', () => {
    const result = AffectedCrates.affected(workspace, ['src/lib.rs'])
    expect(result.changed).toEqual(['d'])
})

test.each([
    'Cargo.toml',
    'Cargo.lock',
    'crates/b/Cargo.toml',
    'crates/b/build.rs',
    'rust-toolchain.toml',
    'rustfmt.toml',
    'clippy.toml',
    'deny.toml',
    '.cargo/config.toml',
    '.config/nextest.toml',
    'crates/fylo-test-support/src/lib.rs',
    'VERSION',
    'tests/fixtures/rust-sql-v1.json',
    'vendor/outside/src/lib.rs',
    'fuzz/fuzz_targets/query.rs',
    'tools/loose.rs'
])('a change to %s runs the full workspace', (file) => {
    const result = AffectedCrates.affected(workspace, [file])
    expect(result.full).toBe(true)
    expect(result.reasons).toEqual([file])
    expect(AffectedCrates.listText(result)).toBe('full')
    expect(AffectedCrates.argumentText(result)).toBe('--workspace')
})

test.each(['README.md', 'docs/guide.md', 'scripts/build.mjs', 'clients/node/index.mjs'])(
    'a change to %s outside every crate selects no crate',
    (file) => {
        const result = AffectedCrates.affected(workspace, [file])
        expect(result).toEqual({ full: false, reasons: [], changed: [], crates: [] })
        expect(AffectedCrates.listText(result)).toBe('none')
        expect(AffectedCrates.argumentText(result)).toBe('')
    }
)

test('the list and the cargo flags name the same crates', () => {
    const result = AffectedCrates.affected(workspace, ['crates/b/src/lib.rs'])
    expect(AffectedCrates.listText(result)).toBe('b\nc')
    expect(AffectedCrates.argumentText(result)).toBe('-p b -p c')
})

test('the inverted tree output keeps one name per workspace crate', () => {
    const output = [
        'b v0.1.0 (/work/crates/b)',
        'c v0.1.0 (/work/crates/c)',
        'c v0.1.0 (/work/crates/c) (*)',
        'serde v1.0.0',
        ''
    ].join('\n')
    expect(AffectedCrates.invertedNames(output, workspace)).toEqual(['b', 'c'])
})

test('the invert check reports crates the computed list missed or added', () => {
    expect(AffectedCrates.differences('a', ['a', 'b', 'x'], ['a', 'b', 'c'])).toEqual({
        crate: 'a',
        missing: ['c'],
        extra: ['x']
    })
})

test('the invert check finds no difference when both lists match', () => {
    expect(AffectedCrates.differences('a', ['a', 'b'], ['b', 'a'])).toEqual({
        crate: 'a',
        missing: [],
        extra: []
    })
})

test('every crate in the FYLO workspace matches cargo tree --invert', () => {
    const fylo = AffectedCrates.readWorkspace(process.cwd())
    const names = fylo.crates.map((crate) => crate.name)
    expect(() => AffectedCrates.check(fylo, names)).not.toThrow()
})

function run(command, args, cwd) {
    const result = Bun.spawnSync([command, ...args], {
        cwd,
        env: { ...process.env },
        stdout: 'pipe',
        stderr: 'pipe'
    })
    return {
        exitCode: result.exitCode,
        stdout: result.stdout.toString().trim(),
        stderr: result.stderr.toString()
    }
}

function git(args, cwd) {
    const identity = ['-c', 'user.name=test', '-c', 'user.email=test@example.invalid']
    return run('git', [...identity, '-c', 'commit.gpgsign=false', ...args], cwd)
}

async function writeCrate(root, name, dependencies) {
    const folder = path.join(root, 'crates', name)
    await mkdir(path.join(folder, 'src'), { recursive: true })
    await writeFile(
        path.join(folder, 'Cargo.toml'),
        `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n\n${dependencies}\n`
    )
    await writeFile(path.join(folder, 'src', 'lib.rs'), 'pub fn value() -> u8 { 1 }\n')
}

async function fixtureRepository() {
    const root = await mkdtemp(path.join(tmpdir(), 'fylo-affected-crates-'))
    roots.push(root)
    await writeFile(
        path.join(root, 'Cargo.toml'),
        '[workspace]\nresolver = "2"\nmembers = ["crates/a", "crates/b", "crates/c", "crates/d"]\n'
    )
    await writeCrate(root, 'a', '')
    await writeCrate(root, 'b', '[dependencies]\na = { path = "../a" }')
    await writeCrate(root, 'c', '[dev-dependencies]\nb = { path = "../b" }')
    await writeCrate(root, 'd', '')
    expect(run('cargo', ['generate-lockfile', '--offline'], root).exitCode).toBe(0)
    expect(git(['init', '--quiet', '--initial-branch=main'], root).exitCode).toBe(0)
    expect(git(['add', '.'], root).exitCode).toBe(0)
    expect(git(['commit', '--quiet', '-m', 'start'], root).exitCode).toBe(0)
    return root
}

function affectedIn(root, options) {
    return run(process.execPath, ['--no-env-file', script, '--base', 'main', ...options], root)
}

test('a repository change lists the crate and its dependents, and passes the check', async () => {
    const root = await fixtureRepository()
    await writeFile(path.join(root, 'crates', 'a', 'src', 'lib.rs'), 'pub fn value() -> u8 { 2 }\n')
    const listed = affectedIn(root, ['--check'])
    expect(listed.exitCode).toBe(0)
    expect(listed.stdout).toBe('a\nb\nc')
    expect(listed.stderr).toContain('1 changed crate(s) match cargo tree --invert')
    expect(affectedIn(root, ['--args']).stdout).toBe('-p a -p b -p c')
})

test('a repository change to a new untracked file in a crate selects that crate', async () => {
    const root = await fixtureRepository()
    await writeFile(path.join(root, 'crates', 'd', 'src', 'extra.rs'), 'pub fn more() {}\n')
    expect(affectedIn(root, []).stdout).toBe('d')
})

test('a repository change to the workspace manifest prints full', async () => {
    const root = await fixtureRepository()
    await writeFile(
        path.join(root, 'Cargo.toml'),
        '[workspace]\nresolver = "2"\nmembers = ["crates/a", "crates/b", "crates/c", "crates/d"]\n\n'
    )
    const listed = affectedIn(root, [])
    expect(listed.stdout).toBe('full')
    expect(listed.stderr).toContain('full workspace because Cargo.toml changed')
})

test('a repository with no change prints none', async () => {
    const root = await fixtureRepository()
    expect(affectedIn(root, []).stdout).toBe('none')
})

test('an unknown option fails', async () => {
    const root = await fixtureRepository()
    const listed = affectedIn(root, ['--everything'])
    expect(listed.exitCode).not.toBe(0)
    expect(listed.stderr).toContain('unknown option --everything')
})
