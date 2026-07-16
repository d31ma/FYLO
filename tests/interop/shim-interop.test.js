import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const roots = []
const repoRoot = process.cwd()
const binaryPath = path.join(
    repoRoot,
    'dist-bin',
    process.platform === 'win32' ? 'fylo.exe' : 'fylo'
)
const shimRoot = path.join(repoRoot, 'clients')

/**
 * @param {string[]} args
 * @param {{ cwd?: string, stdin?: string, timeout?: number, env?: Record<string, string> }} [options]
 */
async function run(args, options = {}) {
    const proc = Bun.spawn(args, {
        cwd: options.cwd ?? repoRoot,
        env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1', ...options.env },
        stdin: options.stdin === undefined ? 'ignore' : new Blob([options.stdin]),
        stdout: 'pipe',
        stderr: 'pipe'
    })
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

/**
 * @param {string} prefix
 */
async function tempRoot(prefix) {
    const root = await mkdtemp(path.join(os.tmpdir(), prefix))
    roots.push(root)
    return root
}

/**
 * @param {string} command
 */
async function requireCommand(command) {
    const result = await run(['bash', '-lc', `command -v ${command}`])
    expectSuccess(`required command ${command}`, result)
}

beforeAll(async () => {
    await mkdir(path.dirname(binaryPath), { recursive: true })
    const build = await run(['bun', 'run', 'build:exe'], { timeout: 120_000 })
    expectSuccess('bun run build:exe', build)
})

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('client shim interop via fylo exec --loop', () => {
    test('Python shim drives the persistent loop', async () => {
        await requireCommand('python3')
        const root = await tempRoot('fylo-python-shim-')
        const script = `
import json, os, sys
sys.path.insert(0, ${JSON.stringify(path.join(shimRoot, 'python'))})
from fylo import Fylo

with Fylo(${JSON.stringify(root)}, binary=${JSON.stringify(binaryPath)}) as db:
    db.create_collection('users')
    doc_id = db.put_data('users', {'name': 'Ada', 'score': 90})
    doc = db.get_latest('users', doc_id)
    found = db.find_docs('users', {'$ops': [{'score': {'$gte': 50}}]})
    db.set_meta('users', doc_id, {'source': 'python', 'reviewed': False})
    initial_meta = db.get_meta('users', doc_id)
    db.set_meta('users', doc_id, {'source': None})
    meta = db.get_meta('users', doc_id)
    assert initial_meta['source'] == 'python' and initial_meta['reviewed'] is False, initial_meta
    assert initial_meta['id'] == doc_id and initial_meta['createdAt'] > 0, initial_meta
    assert 'source' not in meta and meta['reviewed'] is False, meta
    print(json.dumps({'ok': True, 'id': doc_id, 'doc': doc, 'found': found, 'meta': meta}))
`
        const result = await run(['python3', '-c', script])
        expectSuccess('python shim interop', result)
        expect(existsSync(path.join(shimRoot, 'python', '__pycache__'))).toBe(false)
        const parsed = JSON.parse(result.stdout)
        expect(parsed.ok).toBe(true)
        expect(parsed.found[parsed.id].name).toBe('Ada')
    })

    test('Node shim drives the persistent loop', async () => {
        await requireCommand('node')
        const root = await tempRoot('fylo-node-shim-')
        const script = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = new Fylo(${JSON.stringify(root)}, { binary: ${JSON.stringify(binaryPath)} })
try {
  await db.createCollection('users')
  const id = await db.putData('users', { name: 'Ada', score: 90 })
  const doc = await db.getLatest('users', id)
  const found = await db.findDocs('users', { $ops: [{ score: { $gte: 50 } }] })
  await db.setMeta('users', id, { source: 'node', reviewed: false })
  const initialMeta = await db.getMeta('users', id)
  await db.setMeta('users', id, { source: null })
  const meta = await db.getMeta('users', id)
  if (initialMeta.source !== 'node' || initialMeta.reviewed !== false) throw new Error('bad initial metadata')
  if ('source' in meta || meta.reviewed !== false) throw new Error('bad updated metadata')
  console.log(JSON.stringify({ ok: true, id, doc, found, meta }))
} finally {
  await db.close()
}
`
        const result = await run(['node', '--input-type=module', '-e', script])
        expectSuccess('node shim interop', result)
        const parsed = JSON.parse(result.stdout)
        expect(parsed.ok).toBe(true)
        expect(parsed.found[parsed.id].name).toBe('Ada')
    })

    test('Ruby shim drives the persistent loop', async () => {
        await requireCommand('ruby')
        const root = await tempRoot('fylo-ruby-shim-')
        const script = `
$LOAD_PATH.unshift(${JSON.stringify(path.join(shimRoot, 'ruby'))})
require 'fylo'
require 'json'

db = Fylo.new(${JSON.stringify(root)}, binary: ${JSON.stringify(binaryPath)})
begin
  db.create_collection('users')
  id = db.put_data('users', { 'name' => 'Ada', 'score' => 90 })
  doc = db.get_latest('users', id)
  found = db.find_docs('users', { '$ops' => [{ 'score' => { '$gte' => 50 } }] })
  db.set_meta('users', id, { 'source' => 'ruby', 'reviewed' => false })
  initial_meta = db.get_meta('users', id)
  db.set_meta('users', id, { 'source' => nil })
  meta = db.get_meta('users', id)
  raise 'bad initial metadata' unless initial_meta['source'] == 'ruby' && initial_meta['reviewed'] == false
  raise 'missing canonical metadata' unless initial_meta['id'] == id && initial_meta['createdAt'] > 0
  raise 'bad updated metadata' unless !meta.key?('source') && meta['reviewed'] == false
  puts JSON.generate({ ok: true, id: id, doc: doc, found: found, meta: meta })
ensure
  db.close
end
`
        const result = await run(['ruby', '-e', script])
        expectSuccess('ruby shim interop', result)
        const parsed = JSON.parse(result.stdout)
        expect(parsed.ok).toBe(true)
        expect(parsed.found[parsed.id].name).toBe('Ada')
    })
})
