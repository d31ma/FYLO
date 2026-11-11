import { cp, mkdir, readdir, readFile, rm } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { homedir } from 'node:os'
import { join, resolve } from 'node:path'

const root = fileURLToPath(new URL('../', import.meta.url))
const engineOnly = process.argv.includes('--engine-only')
const withWasm = process.argv.includes('--wasm') || engineOnly
const bunVersion = (
    process.env.FYLO_BUN_VERSION ??
    (await readFile(new URL('../.bun-version', import.meta.url), 'utf8'))
).trim()

if (Bun.version !== bunVersion) {
    throw new Error(`Browser builds require Bun ${bunVersion}; running ${Bun.version}`)
}

await mkdir(new URL('../dist-web/', import.meta.url), { recursive: true })
if (!engineOnly) {
    await run('bun', [
        'build',
        './src/browser/index.js',
        '--target=browser',
        '--outfile',
        './dist-web/fylo.mjs'
    ])
    await run('bun', [
        'build',
        './src/browser/worker/shared.js',
        '--target=browser',
        '--outfile',
        './dist-web/shared.js'
    ])
    await run('bun', [
        'build',
        './src/browser/worker/dedicated.js',
        '--target=browser',
        '--outfile',
        './dist-web/dedicated.js'
    ])
    // The single-file `.vdbx` Worker (ADR 0016): no SharedArrayBuffer bridge.
    await run('bun', [
        'build',
        './src/browser/fylo-single-file-worker.mjs',
        '--target=browser',
        '--outfile',
        './dist-web/vdbx-worker.js'
    ])
    // The crypto bridge Worker a protected `.vdbx` opens through (ADR 0021).
    await run('bun', [
        'build',
        './src/browser/fylo-identity-worker.mjs',
        '--target=browser',
        '--outfile',
        './dist-web/identity-worker.js'
    ])
}

// The index kernel accelerates queries beside the JavaScript core; the browser
// engine is the whole engine. Both are wasm32-unknown-unknown, so they share
// one toolchain setup.
// The engine reaches `getrandom` through both 0.3 and 0.4 (aes-gcm pulls the
// latter), and neither supports wasm32-unknown-unknown without a backend. Both
// majors call the same `__getrandom_v03_custom` symbol, which `fylo-wasm`
// exports over the host table, so one cfg covers them. It stays scoped to this
// build: the index kernel has no such shim to satisfy the flag.
const CUSTOM_ENTROPY = '--cfg getrandom_backend="custom"'

// The whole engine is transfer-bound in a browser, so it is optimized for size;
// the index kernel is the scan hot path and keeps the release profile's speed.
// `z` rather than `s` since the engine links the CHEX validator (ADR 0017
// section 4): at `s` the module measured 1,900,375 bytes, past its 1,792 KiB
// budget, and at `z` 1,750,942 (578,155 gzipped).
const ENGINE_PROFILE = { CARGO_PROFILE_RELEASE_OPT_LEVEL: 'z' }

const WASM_ARTIFACTS = [
    ['fylo_browser_index.wasm', 'fylo-index.wasm', 'src/browser/wasm/Cargo.toml', '', {}, false],
    [
        'fylo_wasm.wasm',
        'fylo-browser.wasm',
        'crates/fylo-wasm/Cargo.toml',
        CUSTOM_ENTROPY,
        ENGINE_PROFILE,
        true
    ]
]

const ENGINE_FEATURES = [
    '--enable-bulk-memory',
    '--enable-multivalue',
    '--enable-mutable-globals',
    '--enable-nontrapping-float-to-int',
    '--enable-reference-types',
    '--enable-sign-ext'
]

if (withWasm) {
    const artifacts = engineOnly ? WASM_ARTIFACTS.slice(1) : WASM_ARTIFACTS
    if (artifacts.some(([, , , , , reencode]) => reencode)) await requireWasmOpt()
    for (const [built, published, manifest, rustflags, profile, reencode] of artifacts) {
        await buildWasm(manifest, rustflags, profile)
        const output = fileURLToPath(new URL(`../dist-web/${published}`, import.meta.url))
        await rm(output, { force: true })
        await cp(
            resolve(
                root,
                process.env.CARGO_TARGET_DIR ?? 'target',
                'wasm32-unknown-unknown',
                'release',
                built
            ),
            output
        )
        if (reencode) await reencodeWasm(output)
    }
}

if (engineOnly) {
    console.log('Built dist-web/fylo-browser.wasm')
} else {
    console.log(
        `Built dist-web/fylo.mjs, shared.js, dedicated.js, vdbx-worker.js, identity-worker.js${
            withWasm ? `, ${WASM_ARTIFACTS.map(([, published]) => published).join(', ')}` : ''
        }`
    )
}

async function buildWasm(manifest, rustflags, profile) {
    const toolchain = await rustToolchain()
    const rustc = await capture('rustup', ['which', 'rustc', '--toolchain', toolchain]).catch(
        async () => {
            await run('rustup', ['toolchain', 'install', toolchain, '--profile', 'minimal'])
            return await capture('rustup', ['which', 'rustc', '--toolchain', toolchain])
        }
    )
    await run('rustup', ['target', 'add', 'wasm32-unknown-unknown', '--toolchain', toolchain])
    await run('rustup', [
        'run',
        toolchain,
        'cargo',
        'fetch',
        '--manifest-path',
        manifest,
        '--locked'
    ])
    await run(
        'rustup',
        [
            'run',
            toolchain,
            'cargo',
            'build',
            '--manifest-path',
            manifest,
            '--release',
            '--target',
            'wasm32-unknown-unknown',
            '--locked'
        ],
        {
            ...process.env,
            ...profile,
            RUSTC: rustc.trim(),
            RUSTFLAGS: [process.env.RUSTFLAGS, rustflags, await dependencySourceRemaps()]
                .filter(Boolean)
                .join(' ')
        }
    )
}

async function requireWasmOpt() {
    const version = (
        process.env.FYLO_BINARYEN_VERSION ??
        (await readFile(new URL('../.binaryen-version', import.meta.url), 'utf8'))
    ).trim()
    const installed = await capture('wasm-opt', ['--version']).catch(() => '')
    if (!installed.includes(`version ${version}`)) {
        throw new Error(
            `Browser engine builds require wasm-opt from binaryen ${version}; found ${
                installed.trim() || 'none'
            }. Run scripts/install-binaryen.sh or install binaryen ${version}.`
        )
    }
}

/** @param {string} path */
async function reencodeWasm(path) {
    await run('wasm-opt', ['-O0', ...ENGINE_FEATURES, path, '-o', path])
}

async function dependencySourceRemaps() {
    const cargoHome = process.env.CARGO_HOME ?? join(homedir(), '.cargo')
    const registry = join(cargoHome, 'registry', 'src')
    const indexes = await readdir(registry).catch(() => [])
    const prefixes = [
        registry,
        ...indexes.map((index) => join(registry, index)),
        join(cargoHome, 'git', 'checkouts')
    ]
    return prefixes
        .filter((prefix) => !/\s/.test(prefix))
        .map((prefix) => `--remap-path-prefix=${prefix}/=`)
        .join(' ')
}

async function rustToolchain() {
    const config = await readFile(new URL('../rust-toolchain.toml', import.meta.url), 'utf8')
    const channel = config.match(/^channel\s*=\s*"([^"]+)"\s*$/m)?.[1]
    if (!channel) throw new Error('rust-toolchain.toml must define an exact channel')
    return channel
}

/** @param {string} command @param {string[]} args @param {NodeJS.ProcessEnv=} env */
async function run(command, args, env = process.env) {
    await new Promise((resolve, reject) => {
        const child = spawn(command, args, { cwd: root, stdio: 'inherit', env })
        child.once('error', reject)
        child.once('exit', (code) =>
            code === 0 ? resolve(undefined) : reject(new Error(`${command} exited with ${code}`))
        )
    })
}

/** @param {string} command @param {string[]} args */
async function capture(command, args) {
    return await new Promise((resolve, reject) => {
        const child = spawn(command, args, { cwd: root, stdio: ['ignore', 'pipe', 'inherit'] })
        let output = ''
        child.stdout.on('data', (chunk) => (output += chunk))
        child.once('error', reject)
        child.once('exit', (code) =>
            code === 0 ? resolve(output) : reject(new Error(`${command} exited with ${code}`))
        )
    })
}
