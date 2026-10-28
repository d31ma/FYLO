import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const roots = []
const servers = []
const repoRoot = process.cwd()
const binaryPath = process.env.FYLO_BINARY
    ? path.resolve(process.env.FYLO_BINARY)
    : path.join(repoRoot, 'dist-bin', process.platform === 'win32' ? 'fylo.exe' : 'fylo')
const shimRoot = path.join(repoRoot, 'clients')
const token = 'network-server-interop-token-0123456789abcdef'

/**
 * @param {string[]} args
 * @param {{ timeout?: number }} [options]
 */
async function run(args, options = {}) {
    const proc = Bun.spawn(args, { cwd: repoRoot, stdout: 'pipe', stderr: 'pipe' })
    const timeout = setTimeout(() => proc.kill(), options.timeout ?? 60_000)
    try {
        const [stdout, stderr, exitCode] = await Promise.all([
            new Response(proc.stdout).text(),
            new Response(proc.stderr).text(),
            proc.exited
        ])
        return { stdout, stderr, exitCode }
    } finally {
        clearTimeout(timeout)
    }
}

/**
 * @param {string} label
 * @param {Awaited<ReturnType<typeof run>>} result
 */
function expectSuccess(label, result) {
    expect(result.exitCode, `${label} stderr:\n${result.stderr}\nstdout:\n${result.stdout}`).toBe(0)
}

async function tempDirectory(prefix) {
    const directory = await mkdtemp(path.join(os.tmpdir(), prefix))
    roots.push(directory)
    return directory
}

async function tokenFile(directory) {
    const file = path.join(directory, 'serve.token')
    await writeFile(file, `${token}\n`, { mode: 0o600 })
    return file
}

/**
 * Start `fylo serve` on an ephemeral loopback port and wait for its ready line.
 * @param {string} root
 * @param {string} secretFile
 */
async function startServer(root, secretFile) {
    const proc = Bun.spawn(
        [
            binaryPath,
            'serve',
            '--root',
            root,
            '--listen',
            '127.0.0.1:0',
            '--token-file',
            secretFile
        ],
        { cwd: repoRoot, stdout: 'pipe', stderr: 'pipe' }
    )
    servers.push(proc)
    const reader = proc.stdout.getReader()
    let text = ''
    while (!text.includes('\n')) {
        const { value, done } = await reader.read()
        if (done) {
            const stderr = await new Response(proc.stderr).text()
            throw new Error(`fylo serve exited before it was ready: ${stderr}`)
        }
        text += new TextDecoder().decode(value)
    }
    reader.releaseLock()
    const ready = JSON.parse(text.slice(0, text.indexOf('\n')))
    const port = Number(ready.listening.split(':').pop())
    return { proc, port }
}

/** @param {string} script */
function nodeScript(script) {
    return run(['node', '--input-type=module', '-e', script])
}

/** @param {number} port */
function nodeClient(port) {
    return `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = Fylo.connect({ host: '127.0.0.1', port: ${port}, token: ${JSON.stringify(token)} })
`
}

beforeAll(async () => {
    await mkdir(path.dirname(binaryPath), { recursive: true })
    if (process.env.FYLO_SKIP_BINARY_BUILD === '1') return
    const build = await run(['bun', 'run', 'build:exe'], { timeout: 120_000 })
    expectSuccess('bun run build:exe', build)
})

afterAll(async () => {
    for (const server of servers) server.kill()
    await Promise.all(servers.map((server) => server.exited))
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('fylo serve over TCP', () => {
    test('the Node shim puts, patches, reads and deletes through one server', async () => {
        const directory = await tempDirectory('fylo-serve-node-')
        const server = await startServer(path.join(directory, 'root'), await tokenFile(directory))
        const result = await nodeScript(`${nodeClient(server.port)}
try {
  const identity = await db.handshake()
  if (!identity.capabilities.networkServer) throw new Error('not a network session')
  await db.createCollection('users')
  const id = await db.putData('users', { name: 'Ada', role: 'admin' })
  await db.patchDoc('users', id, { role: 'owner' })
  const latest = await db.getLatest('users', id)
  const found = await db.findDocs('users', { $ops: [{ role: { $eq: 'owner' } }] })
  await db.delDoc('users', id)
  const after = await db.findDocs('users', { $ops: [{ role: { $eq: 'owner' } }] })
  console.log(JSON.stringify({ id, latest, found, after }))
} finally {
  await db.close()
}
`)
        expectSuccess('node network client', result)
        const parsed = JSON.parse(result.stdout)
        expect(JSON.stringify(parsed.latest)).toContain('owner')
        expect(parsed.found[parsed.id].name).toBe('Ada')
        expect(parsed.after[parsed.id]).toBeUndefined()
    })

    test('the Node shim reports a wrong token as EAUTH', async () => {
        const directory = await tempDirectory('fylo-serve-auth-')
        const server = await startServer(path.join(directory, 'root'), await tokenFile(directory))
        const result = await nodeScript(`
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = Fylo.connect({ host: '127.0.0.1', port: ${server.port}, token: 'w'.repeat(48) })
try {
  await db.handshake()
  console.log('admitted')
} catch (error) {
  console.log(error.code)
} finally {
  await db.close()
}
`)
        expectSuccess('node wrong token', result)
        expect(result.stdout.trim()).toBe('EAUTH')
    })

    test('the Rust shim drives a served root', async () => {
        const directory = await tempDirectory('fylo-serve-rust-')
        const server = await startServer(path.join(directory, 'root'), await tokenFile(directory))
        const program = path.join(directory, 'main.rs')
        const executable = path.join(
            directory,
            process.platform === 'win32' ? 'client.exe' : 'client'
        )
        await writeFile(
            program,
            `#[path = ${JSON.stringify(path.join(shimRoot, 'rust', 'fylo.rs'))}]
mod fylo;
use fylo::{Fylo, Json};
fn main() {
    let mut db = Fylo::connect("127.0.0.1:${server.port}", ${JSON.stringify(token)}).unwrap();
    db.create_collection("users", "document").unwrap();
    let put = db.put_data("users", Json::obj(vec![("name", "Grace".into())])).unwrap();
    let id = put.split("\\"result\\":\\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    db.patch_doc("users", &id, Json::obj(vec![("role", "owner".into())])).unwrap();
    print!("{}", db.get_latest("users", &id).unwrap());
    assert!(Fylo::connect("127.0.0.1:${server.port}", &"w".repeat(48)).is_err());
}
`
        )
        const build = await run(['rustc', '--edition', '2021', '-o', executable, program], {
            timeout: 120_000
        })
        expectSuccess('rustc network client', build)
        const result = await run([executable])
        expectSuccess('rust network client', result)
        expect(result.stdout).toContain('Grace')
        expect(result.stdout).toContain('owner')
    })

    test('a killed server leaves a root the next server recovers', async () => {
        const directory = await tempDirectory('fylo-serve-crash-')
        const root = path.join(directory, 'root')
        const secretFile = await tokenFile(directory)
        const first = await startServer(root, secretFile)
        const written = await nodeScript(`${nodeClient(first.port)}
await db.createCollection('ledger')
const ids = []
for (let index = 0; index < 20; index++) ids.push(await db.putData('ledger', { index }))
console.log(JSON.stringify(ids))
await db.close()
`)
        expectSuccess('writes before the crash', written)
        const ids = JSON.parse(written.stdout)
        first.proc.kill('SIGKILL')
        await first.proc.exited
        const second = await startServer(root, secretFile)
        const read = await nodeScript(`${nodeClient(second.port)}
const all = await db.findDocs('ledger', {})
console.log(JSON.stringify(Object.keys(all)))
await db.close()
`)
        expectSuccess('reads after the crash', read)
        expect(JSON.parse(read.stdout).sort()).toEqual([...ids].sort())
    })

    test('fylo serve --stop releases the root for the next owner', async () => {
        const directory = await tempDirectory('fylo-serve-stop-')
        const root = path.join(directory, 'root')
        const secretFile = await tokenFile(directory)
        const server = await startServer(root, secretFile)
        const contender = await run([
            binaryPath,
            'exec',
            '--root',
            root,
            '--request',
            '{"op":"createCollection","collection":"contender"}'
        ])
        expect(contender.stdout + contender.stderr).toContain('EROOTLOCKED')
        const stop = await run([
            binaryPath,
            'serve',
            '--stop',
            '--connect',
            `127.0.0.1:${server.port}`,
            '--token-file',
            secretFile
        ])
        expectSuccess('fylo serve --stop', stop)
        expect(await server.proc.exited).toBe(0)
        const owner = await run([
            binaryPath,
            'exec',
            '--root',
            root,
            '--request',
            '{"op":"createCollection","collection":"after"}'
        ])
        expectSuccess('next owner after stop', owner)
    })
})
