import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const roots = []
const repoRoot = process.cwd()
const binaryPath = process.env.FYLO_BINARY
    ? path.resolve(process.env.FYLO_BINARY)
    : path.join(repoRoot, 'dist-bin', process.platform === 'win32' ? 'fylo.exe' : 'fylo')
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
    if (process.env.FYLO_SKIP_BINARY_BUILD === '1') return
    const build = await run(['bun', 'run', 'build:exe'], { timeout: 120_000 })
    expectSuccess('bun run build:exe', build)
})

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('client shim interop via fylo exec --loop', () => {
    test('every binary-backed language shim exposes bounded query continuation', async () => {
        const contracts = new Map([
            ['node/fylo.mjs', ['findDocsPage', 'findDeletedDocsPage']],
            ['python/fylo.py', ['find_docs_page', 'find_deleted_docs_page']],
            ['ruby/fylo.rb', ['find_docs_page', 'find_deleted_docs_page']],
            ['php/fylo.php', ['findDocsPage', 'findDeletedDocsPage']],
            ['go/fylo.go', ['FindDocsPage', 'FindDeletedDocsPage']],
            ['rust/fylo.rs', ['find_docs_page', 'find_deleted_docs_page']],
            ['java/Fylo.java', ['findDocsPage', 'findDeletedDocsPage']],
            ['csharp/Fylo.cs', ['FindDocsPage', 'FindDeletedDocsPage']],
            ['dart/fylo.dart', ['findDocsPage', 'findDeletedDocsPage']]
        ])
        for (const [relative, methods] of contracts) {
            const source = await readFile(path.join(shimRoot, relative), 'utf8')
            for (const method of methods) expect(source, relative).toContain(method)
        }
    })

    test('every binary-backed language shim exposes word fields and sorted pages', async () => {
        const contracts = new Map([
            ['node/fylo.mjs', ["'setWordFields'", 'findPage(collection, query)']],
            ['python/fylo.py', ['"setWordFields"', 'def find_page(self, collection, query)']],
            ['ruby/fylo.rb', ['"setWordFields"', 'def find_page(collection, query)']],
            ['php/fylo.php', ["'setWordFields'", 'function findPage(string $collection']],
            ['go/fylo.go', ['"setWordFields"', 'func (f *Fylo) FindPage(']],
            ['rust/fylo.rs', ['"op":"setWordFields"', 'pub fn find_page(&mut self, collection']],
            ['java/Fylo.java', ['"setWordFields"', 'public String findPage(String collection']],
            ['csharp/Fylo.cs', ['\\"op\\":\\"setWordFields\\"', 'FindPage(string collection']],
            ['dart/fylo.dart', ["'setWordFields'", 'findPage(String collection']]
        ])
        for (const [relative, markers] of contracts) {
            const source = await readFile(path.join(shimRoot, relative), 'utf8')
            for (const marker of markers) expect(source, relative).toContain(marker)
        }
    })

    test('every binary-backed language shim exposes native application configuration', async () => {
        const contracts = new Map([
            ['node/fylo.mjs', ['config?: string']],
            ['python/fylo.py', ['config=None']],
            ['ruby/fylo.rb', ['config: nil']],
            ['php/fylo.php', ['?string $config']],
            ['go/fylo.go', ['OpenWithOptions', 'Config']],
            ['rust/fylo.rs', ['config: Option<PathBuf>', 'open_with_options']],
            ['java/Fylo.java', ['class Options', 'String config']],
            ['csharp/Fylo.cs', ['class Options', 'string Config']],
            ['dart/fylo.dart', ['String? config']]
        ])
        for (const [relative, markers] of contracts) {
            const source = await readFile(path.join(shimRoot, relative), 'utf8')
            for (const marker of markers) expect(source, relative).toContain(marker)
            expect(source, relative).toContain('--config')
        }
    })

    test('the published configuration templates separate settings from secrets', async () => {
        const [example, application, ignore] = await Promise.all([
            readFile(path.join(repoRoot, '.env.example'), 'utf8'),
            readFile(path.join(repoRoot, 'application.example.json'), 'utf8'),
            readFile(path.join(repoRoot, '.gitignore'), 'utf8')
        ])
        expect(example).toContain('FYLO_ENCRYPTION_KEY=')
        expect(example).toContain('FYLO_CIPHER_SALT=')
        expect(example).not.toContain('FYLO_SHARD_WIDTH=')
        expect(example).not.toContain('FYLO_ROOT=')
        expect(JSON.parse(application).fylo.shard.width).toBe(1)
        expect(ignore.split(/\r?\n/)).toContain('.env')
    })

    test('every binary-backed language shim exposes the durable queue lifecycle', async () => {
        const contracts = new Map([
            [
                'node/fylo.mjs',
                [
                    'queuePublish',
                    'queueClaim',
                    'queueAck',
                    'queueNack',
                    'queueExtend',
                    'queueStats',
                    'queueDeadLetters'
                ]
            ],
            [
                'python/fylo.py',
                [
                    'queue_publish',
                    'queue_claim',
                    'queue_ack',
                    'queue_nack',
                    'queue_extend',
                    'queue_stats',
                    'queue_dead_letters'
                ]
            ],
            [
                'ruby/fylo.rb',
                [
                    'queue_publish',
                    'queue_claim',
                    'queue_ack',
                    'queue_nack',
                    'queue_extend',
                    'queue_stats',
                    'queue_dead_letters'
                ]
            ],
            [
                'php/fylo.php',
                [
                    'queuePublish',
                    'queueClaim',
                    'queueAck',
                    'queueNack',
                    'queueExtend',
                    'queueStats',
                    'queueDeadLetters'
                ]
            ],
            [
                'go/fylo.go',
                [
                    'QueuePublish',
                    'QueueClaim',
                    'QueueAck',
                    'QueueNack',
                    'QueueExtend',
                    'QueueStats',
                    'QueueDeadLetters'
                ]
            ],
            [
                'rust/fylo.rs',
                [
                    'queue_publish',
                    'queue_claim',
                    'queue_ack',
                    'queue_nack',
                    'queue_extend',
                    'queue_stats',
                    'queue_dead_letters'
                ]
            ],
            [
                'java/Fylo.java',
                [
                    'queuePublish',
                    'queueClaim',
                    'queueAck',
                    'queueNack',
                    'queueExtend',
                    'queueStats',
                    'queueDeadLetters'
                ]
            ],
            [
                'csharp/Fylo.cs',
                [
                    'QueuePublish',
                    'QueueClaim',
                    'QueueAck',
                    'QueueNack',
                    'QueueExtend',
                    'QueueStats',
                    'QueueDeadLetters'
                ]
            ],
            [
                'dart/fylo.dart',
                [
                    'queuePublish',
                    'queueClaim',
                    'queueAck',
                    'queueNack',
                    'queueExtend',
                    'queueStats',
                    'queueDeadLetters'
                ]
            ]
        ])
        for (const [relative, methods] of contracts) {
            const source = await readFile(path.join(shimRoot, relative), 'utf8')
            for (const method of methods) expect(source, relative).toContain(method)
        }
    })

    test('every binary-backed shim exposes an idiomatic queue consumer decorator', async () => {
        const contracts = new Map([
            ['node/fylo.mjs', ['queueProcess', 'queueConsumer', 'queue handler failed']],
            ['python/fylo.py', ['queue_process', 'queue_consumer', 'queue handler failed']],
            ['ruby/fylo.rb', ['queue_process', 'queue_consumer', 'queue handler failed']],
            [
                'php/fylo.php',
                ['FyloQueueConsumer', 'queueProcess', 'runQueueConsumer', 'queue handler failed']
            ],
            [
                'go/fylo.go',
                ['QueueConsumerOptions', 'QueueProcess', 'QueueConsumer', 'queue handler failed']
            ],
            [
                'rust/fylo.rs',
                ['QueueConsumerOptions', 'queue_process', 'queue_consumer', 'queue handler failed']
            ],
            [
                'java/Fylo.java',
                [
                    '@interface QueueConsumer',
                    'queueProcess',
                    'runQueueConsumer',
                    'queue handler failed'
                ]
            ],
            [
                'csharp/Fylo.cs',
                [
                    'FyloQueueConsumerAttribute',
                    'QueueProcess',
                    'RunQueueConsumer',
                    'queue handler failed'
                ]
            ],
            [
                'dart/fylo.dart',
                ['FyloQueueConsumer', 'queueProcess', 'queueConsumer', 'queue handler failed']
            ]
        ])
        for (const [relative, markers] of contracts) {
            const source = await readFile(path.join(shimRoot, relative), 'utf8')
            for (const marker of markers) expect(source, relative).toContain(marker)
        }
    })

    test('Python shim drives the persistent loop', async () => {
        await requireCommand('python3')
        const root = await tempRoot('fylo-python-shim-')
        const script = `
import asyncio, json, sys
sys.path.insert(0, ${JSON.stringify(path.join(shimRoot, 'python'))})
from fylo import Fylo

with Fylo(${JSON.stringify(root)}, binary=${JSON.stringify(binaryPath)}) as db:
    published = db.queue_publish('python.jobs', {'job': 1}, idempotency_key='python-1')
    delivery = db.queue_claim('python.jobs', 'python-worker')[0]
    assert delivery['id'] == published['id'], delivery
    db.queue_ack('python.jobs', 'python-worker', delivery['id'], delivery['receipt'])
    assert db.queue_stats('python.jobs', 'python-worker')['retired'] == 1
    db.queue_publish('python.decorated', {'job': 2})
    handled = []
    @db.queue_consumer('python.decorated', 'decorated-worker')
    def consume(delivery):
        handled.append(delivery['payload']['job'])
    summary = consume()
    assert handled == [2] and summary['acknowledged'] == 1, summary
    db.queue_publish('python.decorated.async', {'job': 3})
    @db.queue_consumer('python.decorated.async', 'decorated-worker')
    async def consume_async(delivery):
        await asyncio.sleep(0)
        handled.append(delivery['payload']['job'])
    async_summary = asyncio.run(consume_async())
    assert handled[-1] == 3 and async_summary['acknowledged'] == 1, async_summary
    db.queue_publish('python.decorated.fail', {'job': 4})
    @db.queue_consumer('python.decorated.fail', 'decorated-worker', max_attempts=1)
    def consume_failure(delivery):
        raise RuntimeError('FYLO_SENTINEL_SECRET_MUST_NOT_PERSIST')
    failure_summary = consume_failure()
    failure_letters = db.queue_dead_letters('python.decorated.fail', 'decorated-worker')
    assert failure_summary['deadLettered'] == 1, failure_summary
    assert failure_letters[0]['reason'] == 'queue handler failed', failure_letters
    assert 'FYLO_SENTINEL_SECRET_MUST_NOT_PERSIST' not in json.dumps(failure_letters), failure_letters
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
    sql_id = db.sql("INSERT INTO users (name, score, scope) VALUES ('SQL Ada', 91, 'sql')")
    sql_rows = db.sql("SELECT * FROM users WHERE scope = 'sql'")
    assert sql_rows[sql_id]['name'] == 'SQL Ada', sql_rows
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
const db = new Fylo(${JSON.stringify(root)}, {
  binary: ${JSON.stringify(binaryPath)},
  maxRequestBytes: 1024,
  maxResponseBytes: 4096,
})
try {
  const identity = await db.handshake()
  if (identity.protocolVersion !== 1) throw new Error('bad handshake protocol')
  if (identity.machine.maxRequestBytes !== 1024) throw new Error('bad handshake request limit')
  let bounded = false
  try {
    await db.request({ op: 'handshake', pad: 'x'.repeat(1100) })
  } catch (error) {
    bounded = error.code === 'EFRAME_REQUEST_TOO_LARGE'
  }
  if (!bounded) throw new Error('oversized Node request was not rejected')
  const published = await db.queue.publish('node.jobs', { job: 1 }, { idempotencyKey: 'node-1' })
  const retried = await db.queue.publish('node.jobs', { job: 1 }, { idempotencyKey: 'node-1' })
  if (published.id !== retried.id || !retried.deduplicated) throw new Error('queue publish was not idempotent')
  const [delivery] = await db.queue.claim('node.jobs', 'node-worker')
  if (delivery.id !== published.id) throw new Error('queue claim returned the wrong message')
  await db.queue.ack('node.jobs', 'node-worker', delivery)
  const queueStats = await db.queue.stats('node.jobs', 'node-worker')
  if (queueStats.retired !== 1 || queueStats.available !== 0) throw new Error('queue ack was not durable')
  await db.queue.publish('node.decorated', { job: 2 })
  const handled = []
  const consume = db.queue.consumer('node.decorated', 'decorated-worker')(async (item) => {
    handled.push(item.payload.job)
  })
  const consumed = await consume()
  if (handled[0] !== 2 || consumed.acknowledged !== 1) throw new Error('queue decorator did not ack')
  await db.queue.publish('node.decorated.fail', { job: 3 })
  const fail = db.queue.consumer('node.decorated.fail', 'decorated-worker', { maxAttempts: 1 })(
    async () => { throw new Error('FYLO_SENTINEL_SECRET_MUST_NOT_PERSIST') }
  )
  const failed = await fail()
  if (failed.deadLettered !== 1) throw new Error('queue decorator did not dead-letter')
  const failureLetters = await db.queue.deadLetters('node.decorated.fail', 'decorated-worker')
  if (failureLetters[0]?.reason !== 'queue handler failed') {
    throw new Error('queue decorator did not use the generic failure reason')
  }
  if (JSON.stringify(failureLetters).includes('FYLO_SENTINEL_SECRET_MUST_NOT_PERSIST')) {
    throw new Error('queue decorator persisted handler exception text')
  }
  await db.createCollection('users')
  const id = await db.putData('users', { name: 'Ada', score: 90 })
  await db.batchPutData('users', Array.from({ length: 12 }, (_, index) => ({
    name: \`page-\${index}\`,
    score: index,
  })))
  const doc = await db.getLatest('users', id)
  const found = await db.findDocs('users', { $ops: [{ score: { $gte: 50 } }] })
  const pagedIds = []
  let page = { limit: 5 }
  do {
    const result = await db.findDocsPage('users', { $onlyIds: true }, page)
    pagedIds.push(...result.items)
    page = result.nextCursor ? { limit: 5, cursor: result.nextCursor } : null
  } while (page)
  if (pagedIds.length !== 13 || new Set(pagedIds).size !== 13) throw new Error('bad page traversal')
  await db.setMeta('users', id, { source: 'node', reviewed: false })
  const initialMeta = await db.getMeta('users', id)
  await db.setMeta('users', id, { source: null })
  const meta = await db.getMeta('users', id)
  if (initialMeta.source !== 'node' || initialMeta.reviewed !== false) throw new Error('bad initial metadata')
  if ('source' in meta || meta.reviewed !== false) throw new Error('bad updated metadata')
  const sqlId = await db.sql\`INSERT INTO users (name, score, scope) VALUES (\${'SQL Ada'}, \${91}, \${'sql'})\`
  const sqlRows = await db.sql\`SELECT * FROM users WHERE scope = \${'sql'}\`
  if (sqlRows[sqlId]?.name !== 'SQL Ada') throw new Error('SQL could not read row')
  const updated = await db.sql\`UPDATE users SET score = \${92} WHERE scope = \${'sql'}\`
  if (updated !== 1) throw new Error('SQL could not update row')
  const deleted = await db.sql\`DELETE FROM users WHERE scope = \${'sql'}\`
  if (deleted !== 1) throw new Error('SQL could not delete row')
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

    test('Node shim can enforce one exclusive owner for a root', async () => {
        await requireCommand('node')
        const root = await tempRoot('fylo-node-exclusive-')
        const script = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const first = new Fylo(${JSON.stringify(root)}, {
  binary: ${JSON.stringify(binaryPath)},
  exclusiveRoot: true,
})
let second
try {
  await first.handshake()
  second = new Fylo(${JSON.stringify(root)}, {
    binary: ${JSON.stringify(binaryPath)},
    exclusiveRoot: true,
  })
  let rejected = false
  try {
    await second.handshake()
  } catch (error) {
    rejected = error.code === 'EROOTLOCKED'
  }
  if (!rejected) throw new Error('competing root owner was not rejected')
  await first.createCollection('still-owned')
  console.log(JSON.stringify({ ok: true }))
} finally {
  if (second) await second.close()
  await first.close()
}
`
        const result = await run(['node', '--input-type=module', '-e', script])
        expectSuccess('Node exclusive-root interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Node shim reads application.json without environment overrides or process mutation', async () => {
        await requireCommand('node')
        const fileRoot = await tempRoot('fylo-node-config with spaces-')
        const optionRoot = await tempRoot('fylo-node-config-option-')
        const config = path.join(fileRoot, 'application.json')
        await writeFile(
            config,
            JSON.stringify({
                application: { name: 'host-app' },
                fylo: { root: '/this/root/must/not/win', shard: { width: 2 } }
            })
        )
        const script = `
import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const inherited = JSON.stringify(process.env)
for (const [root, options, width] of [
  [${JSON.stringify(fileRoot)}, {}, 2],
  [${JSON.stringify(optionRoot)}, { shardWidth: 1 }, 1],
]) {
  const db = new Fylo(root, {
    binary: ${JSON.stringify(binaryPath)}, config: ${JSON.stringify(config)}, ...options,
  })
  let id
  try {
    await db.createCollection('users')
    id = await db.putData('users', { source: 'config' })
  } finally {
    await db.close()
  }
  if (!existsSync(join(root, '.collections', 'users', 'docs', id.slice(-width), id + '.json'))) {
    throw new Error('application.json or explicit option precedence was not applied')
  }
}
if (JSON.stringify(process.env) !== inherited) throw new Error('configuration mutated process.env')
console.log(JSON.stringify({ ok: true }))
`
        const result = await run(['node', '--input-type=module', '-e', script], {
            env: { FYLO_SHARD_WIDTH: '4', FYLO_ROOT: '/this/env/root/must/not/win' }
        })
        expectSuccess('Node application configuration', result)
        expect(JSON.parse(result.stdout)).toEqual({ ok: true })
    })

    test('Node shim rejects legacy env and malformed config options before spawning', async () => {
        const { Fylo } = await import('../../clients/node/fylo.mjs')
        for (const env of ['legacy.env', { SECRET: 'must-not-be-logged' }, undefined]) {
            expect(() => new Fylo('/unused', { binary: '/missing-binary', env })).toThrow(
                'env is no longer supported; use config'
            )
        }
        for (const config of ['', '   ', null, {}, [], 123, 'secret-value\0tail']) {
            let failure
            try {
                new Fylo('/unused', { binary: '/missing-binary', config })
            } catch (error) {
                failure = error
            }
            expect(failure).toBeInstanceOf(TypeError)
            expect(failure.message).toContain(
                'config must be a non-empty application.json file path'
            )
            expect(failure.message).not.toContain('secret-value')
        }
        for (const shardWidth of [0, -1, 5, 1.5]) {
            expect(() => new Fylo('/unused', { binary: '/missing-binary', shardWidth })).toThrow(
                'shardWidth must be an integer from 1 to 4'
            )
        }
    })

    test('Node shim propagates missing, malformed, and invalid application configuration failures', async () => {
        await requireCommand('node')
        const root = await tempRoot('fylo-node-config-invalid-')
        const cases = [
            ['missing.json', undefined],
            ['malformed.json', '{"fylo":'],
            ['invalid.json', JSON.stringify({ fylo: { shard: { width: '2' } } })],
            ['unknown.json', JSON.stringify({ fylo: { shard_width: 2 } })]
        ]
        for (const [name, source] of cases) {
            const config = path.join(root, name)
            if (source !== undefined) await writeFile(config, source)
            const script = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = new Fylo(${JSON.stringify(root)}, {
  binary: ${JSON.stringify(binaryPath)}, config: ${JSON.stringify(config)},
})
let rejected = false
try { await db.ready } catch { rejected = true } finally { await db.close() }
if (!rejected) throw new Error('invalid configuration was accepted')
console.log(JSON.stringify({ ok: true }))
`
            const result = await run(['node', '--input-type=module', '-e', script])
            expectSuccess('Node configuration failure: ' + name, result)
            expect(JSON.parse(result.stdout)).toEqual({ ok: true })
        }
    })

    test('Node shim leaves secret resolution to the native application configuration loader', async () => {
        await requireCommand('node')
        const root = await tempRoot('fylo-node-config-secret-')
        const config = path.join(root, 'application.json')
        const schema = path.join(root, 'schema')
        await mkdir(path.join(schema, 'users', 'history'), { recursive: true })
        await writeFile(
            path.join(schema, 'users', 'manifest.json'),
            JSON.stringify({ current: 'v1', versions: [{ v: 'v1' }] })
        )
        await writeFile(
            path.join(schema, 'users', 'history', 'v1.schema.json'),
            JSON.stringify({ type: 'object', $encrypted: ['name'] })
        )
        await writeFile(
            config,
            JSON.stringify({
                fylo: {
                    schema,
                    encryption: { key: '$' + '{FYLO_INTEROP_SECRET}' },
                    cipher: { salt: '$' + '{FYLO_INTEROP_SALT}' }
                }
            })
        )
        await writeFile(
            path.join(root, '.env'),
            [
                'FYLO_INTEROP_SECRET=interop-local-secret-at-least-thirty-two-bytes',
                'FYLO_INTEROP_SALT=local-file-salt',
                ''
            ].join('\n')
        )
        const script = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const inherited = JSON.stringify(process.env)
const db = new Fylo(${JSON.stringify(root)}, {
  binary: ${JSON.stringify(binaryPath)}, config: ${JSON.stringify(config)},
})
try {
  await db.createCollection('users')
  const id = await db.putData('users', { name: 'Secret Ada' })
  const doc = await db.getLatest('users', id)
  if (doc[id]?.name !== 'Secret Ada') throw new Error('secret-backed configuration failed round trip')
  if (JSON.stringify(process.env) !== inherited) throw new Error('.env mutated host process.env')
  console.log(JSON.stringify({ ok: true, id }))
} finally { await db.close() }
`
        const result = await run(['node', '--input-type=module', '-e', script])
        expectSuccess('Node secret references', result)
        const { id } = JSON.parse(result.stdout)
        const document = await readFile(
            path.join(root, '.collections', 'users', 'docs', id.slice(-1), id + '.json'),
            'utf8'
        )
        expect(document).not.toContain('Secret Ada')
        expect(result.stderr).not.toContain('interop-local-secret-at-least-thirty-two-bytes')

        // A referenced deployment secret overrides the adjacent .env value.
        await writeFile(
            path.join(root, '.env'),
            ['FYLO_INTEROP_SECRET=wrong-secret', 'FYLO_INTEROP_SALT=wrong-salt', ''].join('\n')
        )
        const reopen = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = new Fylo(${JSON.stringify(root)}, {
  binary: ${JSON.stringify(binaryPath)}, config: ${JSON.stringify(config)},
})
try {
  const doc = await db.getLatest('users', ${JSON.stringify(id)})
  if (doc[${JSON.stringify(id)}]?.name !== 'Secret Ada') throw new Error('injected secret did not take precedence')
  console.log(JSON.stringify({ ok: true }))
} finally { await db.close() }
`
        const reopened = await run(['node', '--input-type=module', '-e', reopen], {
            env: {
                FYLO_INTEROP_SECRET: 'interop-local-secret-at-least-thirty-two-bytes',
                FYLO_INTEROP_SALT: 'local-file-salt'
            }
        })
        expectSuccess('Node injected secret precedence', reopened)
        expect(JSON.parse(reopened.stdout)).toEqual({ ok: true })
    })

    test('Node shim keeps the compiled loop alive after a raw-file path put (#65)', async () => {
        await requireCommand('node')
        const root = await tempRoot('fylo-node-raw-loop-')
        const source = path.join(root, 'hello.txt')
        await writeFile(source, 'hello attachment')
        const script = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = new Fylo(${JSON.stringify(root)}, { binary: ${JSON.stringify(binaryPath)} })
try {
  await db.createCollection('assets', 'file')
  const response = await db.request({
    op: 'putData',
    collection: 'assets',
    file: { path: ${JSON.stringify(source)}, key: '/hello.txt' },
    fileOptions: { maxBytes: 16 },
  })
  if (!response.ok) throw new Error(response.error?.message ?? 'raw-file put failed')
  const first = await db.getDoc('assets', response.result)
  const second = await db.getDoc('assets', response.result)
  if (!first[response.result] || !second[response.result]) {
    throw new Error('compiled loop lost the raw file')
  }
  console.log(JSON.stringify({ ok: true }))
} finally {
  await db.close()
}
`
        const result = await run(['node', '--input-type=module', '-e', script])
        expectSuccess('node raw-file loop regression', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
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
  db.queue_publish('ruby.decorated', { 'job' => 2 })
  handled = []
  consume = db.queue_consumer('ruby.decorated', 'decorated-worker') do |delivery|
    handled << delivery.fetch('payload').fetch('job')
  end
  summary = consume.call
  raise 'queue consumer wrapper did not ack' unless handled == [2] && summary['acknowledged'] == 1
  sql_id = db.sql("INSERT INTO users (name, score, scope) VALUES ('SQL Ada', 91, 'sql')")
  sql_rows = db.sql("SELECT * FROM users WHERE scope = 'sql'")
  raise 'SQL could not read row' unless sql_rows[sql_id]['name'] == 'SQL Ada'
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

    test('Ruby shim forwards application configuration and ignores legacy setting variables', async () => {
        await requireCommand('ruby')
        const root = await tempRoot('fylo-ruby-config-')
        const config = path.join(root, 'application.json')
        await writeFile(config, JSON.stringify({ fylo: { shard: { width: 2 } } }))
        const script = `
$LOAD_PATH.unshift(${JSON.stringify(path.join(shimRoot, 'ruby'))})
require 'fylo'
require 'json'

db = Fylo.new(
  ${JSON.stringify(root)},
  binary: ${JSON.stringify(binaryPath)},
  config: ${JSON.stringify(config)}
)
begin
  db.create_collection('users')
  id = db.put_data('users', { 'source' => 'ruby-config' })
  docs = File.join(${JSON.stringify(root)}, '.collections', 'users', 'docs')
  puts JSON.generate({
    two: File.exist?(File.join(docs, id[-2, 2], id + '.json')),
    four: File.exist?(File.join(docs, id[-4, 4], id + '.json'))
  })
ensure
  db.close
end
`
        const result = await run(['ruby', '-e', script], {
            env: { FYLO_SHARD_WIDTH: '4' }
        })
        expectSuccess('Ruby application configuration', result)
        expect(JSON.parse(result.stdout)).toEqual({ two: true, four: false })
    })
})

const INVALID_ID_MESSAGE = 'EENGINE_STORAGE: EINVALIDDOCID: invalid FYLO document identifier'
const COMPILED_CLIENT_TIMEOUT = 180_000
const FAKE_HANDSHAKE =
    '{"protocolVersion":1,"ok":true,"op":"handshake","requestId":null,"durationMs":0,"result":{"machine":{"maxRequestBytes":1048576,"maxResponseBytes":8388608}}}'

/**
 * @param {string} directory
 * @param {string} afterHandshake
 */
async function fakeBinary(directory, afterHandshake) {
    const file = path.join(directory, 'fake-fylo.sh')
    await writeFile(
        file,
        `#!/bin/sh\nread line\nprintf '%s\\n' '${FAKE_HANDSHAKE}'\n${afterHandshake}\n`,
        { mode: 0o755 }
    )
    return file
}

/**
 * @param {string} stdout
 */
function lastJsonLine(stdout) {
    const lines = stdout.trim().split(/\r?\n/)
    return JSON.parse(lines[lines.length - 1])
}

/**
 * @param {string} runtime
 * @param {string} binary
 */
function nodeFailureScript(runtime, binary) {
    return `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const settle = (promise) =>
  Promise.race([
    promise.then(() => 'resolved', (error) => error.message),
    new Promise((resolve) => setTimeout(() => resolve('hung'), 5000)),
  ])
const db = new Fylo('/unused-root', { binary: ${JSON.stringify(binary)} })
await db.handshake()
const first = await settle(db.findDocs('users', {}))
const second = await settle(db.findDocs('users', {}))
const closed = await settle(db.close())
console.log(JSON.stringify({ runtime: ${JSON.stringify(runtime)}, first, second, closed }))
process.exit(0)
`
}

describe('client shim failure contracts via fylo exec --loop', () => {
    test.each(['node', 'bun'])(
        'Node shim settles later calls and close after the child dies by a signal (%s)',
        async (runtime) => {
            await requireCommand(runtime)
            const directory = await tempRoot('fylo-node-signal-')
            const binary = await fakeBinary(directory, 'read line\nkill -9 $$')
            const script = path.join(directory, 'signal.mjs')
            await writeFile(script, nodeFailureScript(runtime, binary))
            const result = await run([runtime, script], { timeout: 30_000 })
            expectSuccess(`${runtime} signal death`, result)
            expect(JSON.parse(result.stdout)).toEqual({
                runtime,
                first: 'fylo process exited',
                second: 'fylo process exited',
                closed: 'resolved'
            })
        }
    )

    test.each(['node', 'bun'])(
        'Node shim settles later calls and close after a malformed response frame (%s)',
        async (runtime) => {
            await requireCommand(runtime)
            const directory = await tempRoot('fylo-node-malformed-')
            const binary = await fakeBinary(
                directory,
                "while read line; do printf '%s\\n' '{not json'; done"
            )
            const script = path.join(directory, 'malformed.mjs')
            await writeFile(script, nodeFailureScript(runtime, binary))
            const result = await run([runtime, script], { timeout: 30_000 })
            expectSuccess(`${runtime} malformed frame`, result)
            expect(JSON.parse(result.stdout)).toEqual({
                runtime,
                first: 'FYLO returned malformed UTF-8 or JSON',
                second: 'fylo process exited',
                closed: 'resolved'
            })
        }
    )

    test('Node shim exposes the machine error code', async () => {
        await requireCommand('node')
        const root = await tempRoot('fylo-node-code-')
        const script = `
import { Fylo } from ${JSON.stringify(path.join(shimRoot, 'node', 'fylo.mjs'))}
const db = new Fylo(${JSON.stringify(root)}, { binary: ${JSON.stringify(binaryPath)} })
try {
  await db.createCollection('users')
  const error = await db.getDoc('users', 'bad').then(() => null, (failure) => failure)
  console.log(JSON.stringify({ code: error.code, message: error.message }))
} finally {
  await db.close()
}
`
        const result = await run(['node', '--input-type=module', '-e', script])
        expectSuccess('node error code', result)
        expect(JSON.parse(result.stdout)).toEqual({
            code: 'EINVALIDDOCID',
            message: INVALID_ID_MESSAGE
        })
    })

    test('Python shim runs async handlers in queue_process and exposes the error code', async () => {
        await requireCommand('python3')
        const root = await tempRoot('fylo-python-failures-')
        const script = `
import asyncio, json, sys
sys.path.insert(0, ${JSON.stringify(path.join(shimRoot, 'python'))})
from fylo import Fylo, FyloError

handled = []

async def handle(delivery):
    await asyncio.sleep(0)
    handled.append(delivery['payload']['job'])

async def fail(delivery):
    await asyncio.sleep(0)
    raise RuntimeError('FYLO_SENTINEL_SECRET_MUST_NOT_PERSIST')

with Fylo(${JSON.stringify(root)}, binary=${JSON.stringify(binaryPath)}) as db:
    db.queue_publish('async.jobs', {'job': 1})
    without_loop = db.queue_process('async.jobs', 'worker', handle)

    async def inside_loop():
        db.queue_publish('async.jobs', {'job': 2})
        return db.queue_process('async.jobs', 'worker', handle)

    with_loop = asyncio.run(inside_loop())
    db.queue_publish('async.failures', {'job': 3})
    failed = db.queue_process('async.failures', 'worker', fail, max_attempts=1)
    letters = db.queue_dead_letters('async.failures', 'worker')
    db.create_collection('users')
    try:
        db.get_doc('users', 'bad')
        error = None
    except FyloError as failure:
        error = failure
    print(json.dumps({
        'handled': handled,
        'withoutLoop': without_loop,
        'withLoop': with_loop,
        'failed': failed,
        'reasons': [letter['reason'] for letter in letters],
        'code': error.code,
        'message': str(error),
    }))
`
        const result = await run(['python3', '-c', script])
        expectSuccess('python async queue_process and error code', result)
        const parsed = JSON.parse(result.stdout)
        expect(parsed.handled).toEqual([1, 2])
        expect(parsed.withoutLoop.acknowledged).toBe(1)
        expect(parsed.withLoop.acknowledged).toBe(1)
        expect(parsed.failed.deadLettered).toBe(1)
        expect(parsed.reasons).toEqual(['queue handler failed'])
        expect(parsed.code).toBe('EINVALIDDOCID')
        expect(parsed.message).toBe(INVALID_ID_MESSAGE)
    })

    test('Ruby shim exposes the machine error code', async () => {
        await requireCommand('ruby')
        const root = await tempRoot('fylo-ruby-code-')
        const script = `
$LOAD_PATH.unshift(${JSON.stringify(path.join(shimRoot, 'ruby'))})
require 'fylo'
require 'json'

db = Fylo.new(${JSON.stringify(root)}, binary: ${JSON.stringify(binaryPath)})
begin
  db.create_collection('users')
  begin
    db.get_doc('users', 'bad')
  rescue FyloError => error
    puts JSON.generate({ code: error.code, message: error.message })
  end
ensure
  db.close
end
`
        const result = await run(['ruby', '-e', script])
        expectSuccess('ruby error code', result)
        expect(JSON.parse(result.stdout)).toEqual({
            code: 'EINVALIDDOCID',
            message: INVALID_ID_MESSAGE
        })
    })

    test('PHP shim sends empty map arguments as objects and exposes the error code', async () => {
        await requireCommand('php')
        const root = await tempRoot('fylo-php-failures-')
        const script = `<?php
require ${JSON.stringify(path.join(shimRoot, 'php', 'fylo.php'))};

function attempt(callable $call): string
{
    try {
        $call();
        return 'ok';
    } catch (FyloError $error) {
        return $error->getMessage();
    }
}

$db = new Fylo(${JSON.stringify(root)}, ${JSON.stringify(binaryPath)});
try {
    $db->createCollection('users');
    $id = $db->putData('users', []);
    $results = [
        'setMeta' => attempt(fn () => $db->setMeta('users', $id, [])),
        'findDocs' => attempt(fn () => $db->findDocs('users', [])),
        'findDeletedDocs' => attempt(fn () => $db->findDeletedDocs('users')),
        'findDocsPage' => attempt(fn () => $db->findDocsPage('users', [])),
        'findDeletedDocsPage' => attempt(fn () => $db->findDeletedDocsPage('users')),
        'collectionFind' => attempt(fn () => $db->collection('users')->find([])),
    ];
    try {
        $db->getDoc('users', 'bad');
    } catch (FyloError $error) {
        $failure = ['code' => $error->getFyloCode(), 'message' => $error->getMessage(), 'number' => $error->getCode()];
    }
    echo json_encode(['created' => is_string($id), 'results' => $results, 'failure' => $failure]);
} finally {
    $db->close();
}
`
        const file = path.join(root, 'probe.php')
        await writeFile(file, script)
        const result = await run(['php', file])
        expectSuccess('php empty maps and error code', result)
        expect(JSON.parse(result.stdout)).toEqual({
            created: true,
            results: {
                setMeta: 'ok',
                findDocs: 'ok',
                findDeletedDocs: 'ok',
                findDocsPage: 'ok',
                findDeletedDocsPage: 'ok',
                collectionFind: 'ok'
            },
            failure: { code: 'EINVALIDDOCID', message: INVALID_ID_MESSAGE, number: 0 }
        })
    })

    test(
        'Go shim sends nil maps as objects and exposes the error code through errors.As',
        async () => {
            await requireCommand('go')
            const directory = await tempRoot('fylo-go-failures-')
            await mkdir(path.join(directory, 'fylo'))
            await writeFile(
                path.join(directory, 'fylo', 'fylo.go'),
                await readFile(path.join(shimRoot, 'go', 'fylo.go'))
            )
            await writeFile(path.join(directory, 'go.mod'), 'module fylo-shim-test\n\ngo 1.22\n')
            await writeFile(
                path.join(directory, 'main.go'),
                `package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"

	fylo "fylo-shim-test/fylo"
)

func outcome(err error) string {
	if err != nil {
		return err.Error()
	}
	return "ok"
}

func main() {
	db, err := fylo.OpenWithOptions(os.Args[2], fylo.Options{Binary: os.Args[1]})
	if err != nil {
		panic(err)
	}
	defer db.Close()
	if _, err := db.CreateCollection("users", "document"); err != nil {
		panic(err)
	}
	created, err := db.PutData("users", nil)
	id, _ := created.(string)
	results := map[string]string{"putData": outcome(err)}
	_, err = db.SetMeta("users", id, nil)
	results["setMeta"] = outcome(err)
	_, err = db.FindDocs("users", nil)
	results["findDocs"] = outcome(err)
	_, err = db.FindDocsPage("users", nil, nil)
	results["findDocsPage"] = outcome(err)
	_, err = db.FindDeletedDocsPage("users", nil, nil)
	results["findDeletedDocsPage"] = outcome(err)
	_, err = db.GetDoc("users", "bad")
	var failure *fylo.Error
	matched := errors.As(err, &failure)
	output, _ := json.Marshal(map[string]any{
		"results": results,
		"matched": matched,
		"code":    failure.Code,
		"message": err.Error(),
	})
	fmt.Print(string(output))
}
`
            )
            const result = await run(['go', 'run', '.', binaryPath, path.join(directory, 'root')], {
                cwd: directory,
                timeout: COMPILED_CLIENT_TIMEOUT
            })
            expectSuccess('go nil maps and error code', result)
            expect(JSON.parse(result.stdout)).toEqual({
                results: {
                    putData: 'ok',
                    setMeta: 'ok',
                    findDocs: 'ok',
                    findDocsPage: 'ok',
                    findDeletedDocsPage: 'ok'
                },
                matched: true,
                code: 'EINVALIDDOCID',
                message: INVALID_ID_MESSAGE
            })
        },
        COMPILED_CLIENT_TIMEOUT
    )

    test(
        'Java shim rejects a multi-line raw request and exposes the error code',
        async () => {
            await requireCommand('javac')
            const directory = await tempRoot('fylo-java-failures-')
            await writeFile(
                path.join(directory, 'Fylo.java'),
                await readFile(path.join(shimRoot, 'java', 'Fylo.java'))
            )
            await writeFile(
                path.join(directory, 'Probe.java'),
                `import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public final class Probe {
    public static void main(String[] args) throws Exception {
        Fylo.Options options = new Fylo.Options();
        options.binary = args[0];
        try (Fylo db = new Fylo(args[1], options)) {
            db.createCollection("users");
            Matcher created = Pattern.compile("\\"result\\":\\"([^\\"]+)\\"").matcher(db.putData("users", Map.of("name", "Ada")));
            created.find();
            String id = created.group(1);
            String lineBreak = "accepted";
            try {
                db.request("{\\"op\\":\\"inspectCollection\\",\\"collection\\":\\"users\\"}\\n{\\"op\\":\\"handshake\\"}");
            } catch (IllegalArgumentException error) {
                lineBreak = error.getMessage();
            }
            String next = db.getLatest("users", id);
            Fylo.FyloException failure = null;
            try {
                db.getDoc("users", "bad");
            } catch (Fylo.FyloException error) {
                failure = error;
            }
            System.out.print("{\\"lineBreak\\":" + Fylo.toJson(lineBreak)
                + ",\\"nextIsOwnResponse\\":" + (next.contains("\\"op\\":\\"getLatest\\"") && next.contains(id))
                + ",\\"code\\":" + Fylo.toJson(failure.getCode())
                + ",\\"messageIsResponse\\":" + failure.getMessage().contains(${JSON.stringify(INVALID_ID_MESSAGE)}) + "}");
        }
    }
}
`
            )
            const compile = await run(['javac', 'Fylo.java', 'Probe.java'], {
                cwd: directory,
                timeout: COMPILED_CLIENT_TIMEOUT
            })
            expectSuccess('javac', compile)
            const result = await run(
                ['java', '-cp', directory, 'Probe', binaryPath, path.join(directory, 'root')],
                { timeout: COMPILED_CLIENT_TIMEOUT }
            )
            expectSuccess('java line breaks and error code', result)
            expect(JSON.parse(result.stdout)).toEqual({
                lineBreak: 'FYLO request must be a single line without CR or LF characters',
                nextIsOwnResponse: true,
                code: 'EINVALIDDOCID',
                messageIsResponse: true
            })
        },
        COMPILED_CLIENT_TIMEOUT
    )

    test(
        'C# shim rejects a multi-line raw request and exposes the error code',
        async () => {
            await requireCommand('dotnet')
            const directory = await tempRoot('fylo-csharp-failures-')
            await writeFile(
                path.join(directory, 'Fylo.cs'),
                await readFile(path.join(shimRoot, 'csharp', 'Fylo.cs'))
            )
            await writeFile(
                path.join(directory, 'Probe.csproj'),
                '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings><Nullable>disable</Nullable></PropertyGroup></Project>'
            )
            await writeFile(
                path.join(directory, 'Program.cs'),
                `using System.Text.Json;

using var db = new Fylo.Fylo(args[1], new Fylo.Fylo.Options { Binary = args[0] });
db.CreateCollection("users");
string id = db.PutData("users", new Dictionary<string, object> { ["name"] = "Ada" }).GetString();
string lineBreak = "accepted";
try
{
    db.Request("{\\"op\\":\\"inspectCollection\\",\\"collection\\":\\"users\\"}\\n{\\"op\\":\\"handshake\\"}").Dispose();
}
catch (ArgumentException error)
{
    lineBreak = error.Message;
}
JsonElement next = db.GetLatest("users", id);
Fylo.FyloException failure = null;
try
{
    db.GetDoc("users", "bad");
}
catch (Fylo.FyloException error)
{
    failure = error;
}
Console.WriteLine(JsonSerializer.Serialize(new Dictionary<string, object>
{
    ["lineBreak"] = lineBreak,
    ["nextIsOwnResponse"] = next.TryGetProperty(id, out _),
    ["code"] = failure.Code,
    ["message"] = failure.Message,
}));
`
            )
            const result = await run(
                [
                    'dotnet',
                    'run',
                    '--project',
                    directory,
                    '--',
                    binaryPath,
                    path.join(directory, 'root')
                ],
                {
                    timeout: COMPILED_CLIENT_TIMEOUT,
                    env: { DOTNET_CLI_TELEMETRY_OPTOUT: '1', DOTNET_NOLOGO: '1' }
                }
            )
            expectSuccess('csharp line breaks and error code', result)
            const parsed = lastJsonLine(result.stdout)
            expect(parsed.lineBreak).toContain(
                'FYLO request must be a single line without CR or LF characters'
            )
            expect(parsed.nextIsOwnResponse).toBe(true)
            expect(parsed.code).toBe('EINVALIDDOCID')
            expect(parsed.message).toBe(INVALID_ID_MESSAGE)
        },
        COMPILED_CLIENT_TIMEOUT
    )

    test(
        'Rust shim rejects a multi-line raw request and exposes the error code',
        async () => {
            await requireCommand('rustc')
            const directory = await tempRoot('fylo-rust-failures-')
            await writeFile(
                path.join(directory, 'fylo.rs'),
                await readFile(path.join(shimRoot, 'rust', 'fylo.rs'))
            )
            await writeFile(
                path.join(directory, 'probe.rs'),
                `mod fylo;
use fylo::{Fylo, FyloError, FyloOptions, Json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let options = FyloOptions { binary: args[1].clone(), config: None };
    let mut db = Fylo::open_with_options(&args[2], options).unwrap();
    db.create_collection("users", "document").unwrap();
    let created = db.put_data("users", Json::obj(vec![("name", "Ada".into())])).unwrap();
    let id = created.split("\\"result\\":\\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let line_break = db
        .request("{\\"op\\":\\"inspectCollection\\",\\"collection\\":\\"users\\"}\\n{\\"op\\":\\"handshake\\"}")
        .unwrap_err()
        .to_string();
    let next = db.get_latest("users", &id).unwrap();
    let failure = db.get_doc("users", "bad").unwrap_err();
    println!(
        "{{\\"lineBreak\\":{:?},\\"nextIsOwnResponse\\":{},\\"code\\":{:?},\\"messageIsResponse\\":{}}}",
        line_break,
        next.contains("\\"op\\":\\"getLatest\\"") && next.contains(&id),
        FyloError::code_of(&failure).unwrap_or_default(),
        failure.to_string().contains(${JSON.stringify(INVALID_ID_MESSAGE)})
    );
}
`
            )
            const executable = path.join(directory, 'probe')
            const compile = await run(
                ['rustc', '--edition', '2021', '-A', 'dead_code', 'probe.rs', '-o', executable],
                { cwd: directory, timeout: COMPILED_CLIENT_TIMEOUT }
            )
            expectSuccess('rustc', compile)
            const result = await run([executable, binaryPath, path.join(directory, 'root')])
            expectSuccess('rust line breaks and error code', result)
            expect(JSON.parse(result.stdout)).toEqual({
                lineBreak: 'FYLO request must be a single line without CR or LF characters',
                nextIsOwnResponse: true,
                code: 'EINVALIDDOCID',
                messageIsResponse: true
            })
        },
        COMPILED_CLIENT_TIMEOUT
    )

    test(
        'Dart shim fails requests fast after the child exits and exposes the error code',
        async () => {
            await requireCommand('dart')
            const directory = await tempRoot('fylo-dart-failures-')
            await writeFile(
                path.join(directory, 'fylo.dart'),
                await readFile(path.join(shimRoot, 'dart', 'fylo.dart'))
            )
            await writeFile(
                path.join(directory, 'probe.dart'),
                `import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'fylo.dart';

Future<String> outcome(Future<Object?> request) => request
    .then((_) => 'resolved', onError: (Object error) => '$error')
    .timeout(const Duration(seconds: 5), onTimeout: () => 'hung');

Future<void> main(List<String> args) async {
  final binary = args[0];
  final root = args[1];
  final broken = await Fylo.open('$root/broken',
      binary: binary, config: '$root/missing-application.json');
  final first = await outcome(broken.request({'op': 'handshake'}));
  final second = await outcome(broken.request({'op': 'handshake'}));
  final closed = await outcome(broken.close());
  final db = await Fylo.open('$root/db', binary: binary);
  FyloException? failure;
  try {
    await db.createCollection('users');
    await db.getDoc('users', 'bad');
  } on FyloException catch (error) {
    failure = error;
  } finally {
    await db.close();
  }
  stdout.write(jsonEncode({
    'first': first.startsWith('FyloException: fylo process exited'),
    'second': second.startsWith('FyloException: fylo process exited'),
    'closed': closed,
    'code': failure?.code,
    'message': failure?.message,
  }));
}
`
            )
            const result = await run(
                ['dart', 'run', path.join(directory, 'probe.dart'), binaryPath, directory],
                { timeout: COMPILED_CLIENT_TIMEOUT }
            )
            expectSuccess('dart after exit and error code', result)
            expect(JSON.parse(result.stdout)).toEqual({
                first: true,
                second: true,
                closed: 'resolved',
                code: 'EINVALIDDOCID',
                message: INVALID_ID_MESSAGE
            })
        },
        COMPILED_CLIENT_TIMEOUT
    )
})
