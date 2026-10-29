// Phase 5 crash gate: kill the native writer at every declared durable
// transition and prove the root still recovers to one valid state.
//
// The failpoint list comes from the binary, not from this file, and every
// declared point must be reached by some scenario. A new failpoint therefore
// cannot be added without either being exercised here or failing this gate,
// which is the property a hand-maintained list cannot give.
import { CargoArtifacts } from './cargo-artifacts.mjs'
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { platform, tmpdir } from 'node:os'
import { basename, join, relative, sep } from 'node:path'
import { parseArgs } from 'node:util'

import { Fylo } from '../clients/node/fylo.mjs'

const { values: options } = parseArgs({
    options: {
        'binary-dir': { type: 'string' },
        'skip-build': { type: 'boolean', default: false },
        'filesystem-profile': { type: 'string', default: 'native' },
        // EFS profile only: cases run in parallel, optionally split across clients.
        concurrency: { type: 'string', default: '1' },
        shard: { type: 'string', default: '0/1' },
        // Hang guard per machine request; a network filesystem needs more.
        'request-timeout-ms': { type: 'string', default: '10000' }
    }
})
const requestTimeout = Number(options['request-timeout-ms'])
const concurrency = Number(options.concurrency)
const [shard, shards] = options.shard.split('/').map(Number)
if (!(concurrency >= 1) || !(shards >= 1) || !(shard >= 0 && shard < shards)) {
    throw new Error('--concurrency must be >= 1 and --shard must be i/n with 0 <= i < n')
}
if (options['skip-build'] && !options['binary-dir']) {
    throw new Error('--skip-build requires --binary-dir pointing to freshly built binaries')
}
if (options['binary-dir'] && !options['skip-build']) {
    throw new Error('--binary-dir requires --skip-build; build the selected binaries first')
}

const filesystemProfile = options['filesystem-profile']
if (!['native', 'efs-sidecar-v1'].includes(filesystemProfile)) {
    throw new Error('unknown filesystem profile: ' + filesystemProfile)
}

const workspace = await realpath(await mkdtemp(join(tmpdir(), 'fylo-rust-crash-')))
const template = join(workspace, 'template')
const collection = 'records'
const fileCollection = 'assets'
const identifier = '4VRNF52JPCO'
const secondIdentifier = '4VRNF52JPCP'
const fileIdentifier = '4VRNF52JPCQ'
const crashFileIdentifier = '4VRNF52JPCS'
const deletedIdentifier = '4VRNF52JPCR'
const COMPACTION_POINTS = [
    'before-file-write',
    'after-file-sync',
    'after-file-rename',
    'after-capture'
]
const COMPACTION_ID = '4VRNF52JPDZ'
const wideDocument = (seed) =>
    Object.fromEntries(
        Array.from({ length: 24 }, (_, field) => [`field${field}`, `${seed}-value-${field}`])
    )
const binary = join(
    options['binary-dir'] ?? CargoArtifacts.path('debug'),
    platform() === 'win32' ? 'fylo-write-preview.exe' : 'fylo-write-preview'
)
const machineBinary = join(
    options['binary-dir'] ?? CargoArtifacts.path('debug'),
    platform() === 'win32' ? 'fylo-rust.exe' : 'fylo-rust'
)

class EfsMachine {
    constructor(root, overrides = {}) {
        this.child = spawn(
            machineBinary,
            [
                'exec',
                '--loop',
                '--root',
                root,
                '--exclusive-root',
                '--filesystem-profile',
                'efs-sidecar-v1'
            ],
            { stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, ...overrides } }
        )
        this.waiters = []
        this.responses = []
        this.stderr = ''
        this.exited = once(this.child, 'exit')
        this.child.stderr.setEncoding('utf8')
        this.child.stderr.on('data', (chunk) => {
            this.stderr += chunk
        })
        const reader = createInterface({ input: this.child.stdout })
        reader.on('line', (line) => {
            try {
                const response = JSON.parse(line)
                const waiter = this.waiters.shift()
                if (waiter) waiter.resolve(response)
                else this.responses.push(response)
            } catch (error) {
                this.rejectPending(error)
            }
        })
        this.child.on('error', (error) => this.rejectPending(error))
        this.child.stdin.on('error', (error) => this.rejectPending(error))
        this.child.on('exit', () =>
            this.rejectPending(new Error('EFS machine exited: ' + this.stderr))
        )
    }

    rejectPending(error) {
        for (const waiter of this.waiters.splice(0)) waiter.reject(error)
    }

    async request(request) {
        assert(
            this.child.exitCode === null && this.child.signalCode === null,
            'EFS machine already exited: ' + this.stderr
        )
        const response = this.responses.length
            ? Promise.resolve(this.responses.shift())
            : new Promise((resolve, reject) => this.waiters.push({ resolve, reject }))
        this.child.stdin.write(JSON.stringify(request) + '\n')
        return await withDeadline(response, 'EFS request ' + request.op)
    }

    async close() {
        if (this.child.exitCode !== null || this.child.signalCode !== null) return
        this.child.stdin.end()
        try {
            await withDeadline(this.exited, 'EFS machine close')
        } catch (error) {
            this.child.kill('SIGKILL')
            await this.exited
            throw error
        }
    }
}

try {
    if (!options['skip-build'])
        await command([
            process.execPath,
            './scripts/run-rust.mjs',
            'cargo',
            'build',
            '--locked',
            '-p',
            'fylo-cli',
            '--bin',
            'fylo-write-preview',
            '--bin',
            'fylo-rust'
        ])

    if (filesystemProfile === 'efs-sidecar-v1') {
        await verifyEfsCrashScenarios(binary, workspace)
    } else {
        assertProcessIdentityAvailable()
        await verifyNativeCrashScenarios()
    }
} finally {
    await rm(workspace, { recursive: true, force: true })
}

async function verifyNativeCrashScenarios() {
    await mkdir(template, { recursive: true })
    const seedFile = join(workspace, 'seed.bin')
    await writeFile(seedFile, new Uint8Array([1, 2, 3, 4]))
    const seed = new Fylo(template, { binary: machineBinary, exclusiveRoot: true })
    await seed.ready
    await requestRequired(seed, { op: 'createCollection', collection, kind: 'document' })
    await requestRequired(seed, {
        op: 'createCollection',
        collection: fileCollection,
        kind: 'file'
    })
    await requestRequired(seed, {
        op: 'putData',
        collection,
        id: identifier,
        data: { name: 'Ada', score: 42 }
    })
    await requestRequired(seed, {
        op: 'putData',
        collection,
        id: secondIdentifier,
        data: { name: 'Grace', score: 50 }
    })
    await requestRequired(seed, {
        op: 'putData',
        collection,
        id: deletedIdentifier,
        data: { name: 'Linus', score: 1 }
    })
    await requestRequired(seed, { op: 'delDoc', collection, id: deletedIdentifier })
    const seededFile = await requestRequired(seed, {
        op: 'putData',
        collection: fileCollection,
        id: fileIdentifier,
        file: { path: seedFile, key: '/seed.bin' },
        meta: { source: 'seed' }
    })
    const seededMetadata = await requestRequired(seed, {
        op: 'getMeta',
        collection: fileCollection,
        id: seededFile
    })
    await seed.close()

    // A successful raw-file metadata read proves that this filesystem carries
    // the platform metadata used to recover the durable key.
    const extendedAttributes = seededMetadata.key === '/seed.bin'

    // A versioned root is a separate template: the repository failpoints are
    // only reachable where `.fylo-vcs` exists.
    const versionedTemplate = join(workspace, 'versioned-template')
    await mkdir(versionedTemplate, { recursive: true })
    const versionedSeed = new Fylo(versionedTemplate, {
        binary: machineBinary,
        exclusiveRoot: true
    })
    await versionedSeed.ready
    await requestRequired(versionedSeed, {
        op: 'createCollection',
        collection,
        kind: 'document'
    })
    await requestRequired(versionedSeed, {
        op: 'putData',
        collection,
        id: identifier,
        data: { name: 'Ada', score: 42 }
    })
    await requestRequired(versionedSeed, {
        op: 'putData',
        collection,
        id: secondIdentifier,
        data: { name: 'Grace', score: 50 }
    })
    await requestRequired(versionedSeed, { op: 'commit', message: 'crash matrix seed' })
    await versionedSeed.close()

    // A segment collection (ADR 0015) is a third template. Its writes append
    // to a document segment, so the log failpoints are reached through a
    // different apply, and compaction and layout migration have failpoints of
    // their own. The deleted record is restored and deleted again, so
    // compaction has dead versions to drop.
    const segmentTemplate = join(workspace, 'segment-template')
    await mkdir(segmentTemplate, { recursive: true })
    const segmentSeed = new Fylo(segmentTemplate, { binary: machineBinary, exclusiveRoot: true })
    await segmentSeed.ready
    await requestRequired(segmentSeed, {
        op: 'createCollection',
        collection,
        kind: 'document',
        layout: 'segments'
    })
    for (const [id, data] of [
        [identifier, { name: 'Ada', score: 42 }],
        [secondIdentifier, { name: 'Grace', score: 50 }],
        [deletedIdentifier, { name: 'Linus', score: 1 }]
    ]) {
        await requestRequired(segmentSeed, { op: 'putData', collection, id, data })
    }
    await requestRequired(segmentSeed, { op: 'delDoc', collection, id: deletedIdentifier })
    await requestRequired(segmentSeed, { op: 'restoreDoc', collection, id: deletedIdentifier })
    await requestRequired(segmentSeed, { op: 'delDoc', collection, id: deletedIdentifier })
    await segmentSeed.close()
    const onSegments = (scenario) => ({ ...scenario, template: () => segmentTemplate })

    const { failpoints } = JSON.parse((await run(binary, ['failpoints'])).stdout)
    assert(Array.isArray(failpoints) && failpoints.length > 0, 'Rust declared no failpoints')

    const scenarios = [
        {
            name: 'put-document',
            args: (root) => [
                'put-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                '4VRNF52JPD0',
                '--document',
                '{"name":"Hopper","score":7}'
            ]
        },
        {
            name: 'put-file',
            args: (root) => [
                'put-file',
                '--root',
                root,
                '--collection',
                fileCollection,
                '--id',
                crashFileIdentifier,
                '--bytes-hex',
                '0a0b0c',
                '--key',
                '/crash.bin',
                '--extension',
                '.bin',
                '--metadata',
                '{"source":"crash"}'
            ]
        },
        {
            name: 'patch-document',
            args: (root) => [
                'patch-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--document',
                '{"name":"Ada","score":99}'
            ]
        },
        {
            name: 'patch-fields',
            args: (root) => [
                'patch-fields',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--changes',
                '{"score":51}'
            ]
        },
        {
            name: 'delete-document',
            args: (root) => [
                'delete-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier
            ]
        },
        {
            name: 'set-metadata',
            args: (root) => [
                'set-metadata',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--record',
                '{"reviewer":"crash"}'
            ]
        },
        {
            name: 'set-access',
            args: (root) => [
                'set-access',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--mode',
                '640'
            ]
        },
        // Raw-file collections stay on the transaction journal (ADR 0010), so
        // their access, delete and restore transitions are exercised here.
        {
            name: 'set-access-file',
            args: (root) => [
                'set-access',
                '--root',
                root,
                '--collection',
                fileCollection,
                '--id',
                fileIdentifier,
                '--mode',
                '640'
            ]
        },
        {
            name: 'delete-file',
            args: (root) => [
                'delete-document',
                '--root',
                root,
                '--collection',
                fileCollection,
                '--id',
                fileIdentifier
            ]
        },
        {
            name: 'restore-file',
            prepare: async (binary, root) => {
                await run(binary, [
                    'delete-document',
                    '--root',
                    root,
                    '--collection',
                    fileCollection,
                    '--id',
                    fileIdentifier
                ])
            },
            args: (root) => [
                'restore-document',
                '--root',
                root,
                '--collection',
                fileCollection,
                '--id',
                fileIdentifier
            ]
        },
        {
            name: 'sql-update',
            args: (root) => [
                'sql',
                '--root',
                root,
                '--statement',
                `UPDATE ${collection} SET score = 77 WHERE name = 'Ada'`
            ]
        },
        {
            name: 'sql-delete',
            args: (root) => [
                'sql',
                '--root',
                root,
                '--statement',
                `DELETE FROM ${collection} WHERE name = 'Grace'`
            ]
        },
        {
            name: 'restore-document',
            args: (root) => [
                'restore-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                deletedIdentifier
            ]
        },
        {
            name: 'reshard',
            args: (root) => ['reshard', '--root', root, '--collection', collection, '--width', '3']
        },
        {
            // A schema registry commit writes the version file, then fences
            // the generation state, then moves the descriptor's head.
            name: 'push-schema',
            prepare: prepareSchemaFolder,
            args: pushSchemaArguments
        },
        {
            name: 'migrate-into-segments',
            args: (root) => [
                'migrate-layout',
                '--root',
                root,
                '--collection',
                collection,
                '--layout',
                'segments'
            ]
        },
        onSegments({
            name: 'put-document-segments',
            args: (root) => [
                'put-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                '4VRNF52JPD1',
                '--document',
                '{"name":"Hopper","score":7}'
            ]
        }),
        onSegments({
            name: 'patch-fields-segments',
            args: (root) => [
                'patch-fields',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--changes',
                '{"score":51}'
            ]
        }),
        onSegments({
            name: 'delete-document-segments',
            args: (root) => [
                'delete-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier
            ]
        }),
        onSegments({
            name: 'restore-document-segments',
            args: (root) => [
                'restore-document',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                deletedIdentifier
            ]
        }),
        onSegments({
            name: 'set-metadata-segments',
            args: (root) => [
                'set-metadata',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--record',
                '{"reviewer":"crash"}'
            ]
        }),
        onSegments({
            name: 'set-access-segments',
            args: (root) => [
                'set-access',
                '--root',
                root,
                '--collection',
                collection,
                '--id',
                identifier,
                '--mode',
                '640'
            ]
        }),
        onSegments({
            name: 'sql-update-segments',
            args: (root) => [
                'sql',
                '--root',
                root,
                '--statement',
                `UPDATE ${collection} SET score = 77 WHERE name = 'Ada'`
            ]
        }),
        onSegments({
            name: 'sql-delete-segments',
            args: (root) => [
                'sql',
                '--root',
                root,
                '--statement',
                `DELETE FROM ${collection} WHERE name = 'Grace'`
            ]
        }),
        onSegments({
            // A segment collection commits the version as a schema record
            // (ADR 0017 section 3): the record is appended and flushed, the
            // schemas snapshot rewritten, `stable` published, the descriptor
            // brought up to the head, and a checkpoint names the snapshot.
            name: 'push-schema-segments',
            prepare: prepareSchemaFolder,
            args: pushSchemaArguments
        }),
        onSegments({
            name: 'compact-segments',
            args: (root) => ['compact', '--root', root, '--collection', collection]
        }),
        onSegments({
            name: 'migrate-out-of-segments',
            args: (root) => [
                'migrate-layout',
                '--root',
                root,
                '--collection',
                collection,
                '--layout',
                'files'
            ]
        }),
        {
            name: 'commit',
            template: () => versionedTemplate,
            prepare: async (binary, root) => {
                await run(binary, [
                    'patch-fields',
                    '--root',
                    root,
                    '--collection',
                    collection,
                    '--id',
                    identifier,
                    '--changes',
                    '{"score":61}'
                ])
            },
            args: (root) => ['commit', '--root', root, '--message', 'crash matrix commit']
        }
    ].filter((scenario) => extendedAttributes || !scenario.name.includes('file'))

    // Each durable transition is interrupted three ways. `abort` loses the
    // process, so the next opener must recover the journal. `enospc` and
    // `edquot` are ordinary I/O errors, so the writer must roll back in place
    // and leave nothing for recovery to do — a distinction the crash case
    // cannot make.
    const ACTIONS = ['abort', 'enospc', 'edquot']
    const reached = new Set()
    const unsupported = new Set()
    let interrupted = 0
    for (const failpoint of failpoints) {
        for (const scenario of scenarios) {
            for (const action of ACTIONS) {
                const root = join(workspace, `${failpoint}--${scenario.name}--${action}`)
                await cloneRoot(scenario.template ? scenario.template() : template, root)
                if (scenario.prepare) await scenario.prepare(binary, root)
                const failed = await run(binary, scenario.args(root), {
                    FYLO_RUST_FAILPOINT: failpoint,
                    FYLO_RUST_FAILPOINT_ACTION: action
                })
                // A mutation the platform does not offer at all never reaches
                // the transition, so it is not evidence either way.
                if (failed.exitCode === 0 || failed.stderr.includes('ENATIVE_UNSUPPORTED')) {
                    if (failed.stderr.includes('ENATIVE_UNSUPPORTED')) {
                        unsupported.add(`${scenario.name} (${failed.stderr.trim().slice(0, 60)})`)
                    }
                    await rm(root, { recursive: true, force: true })
                    continue
                }
                reached.add(failpoint)
                interrupted++
                if (action === 'enospc') {
                    assert(
                        /disk|space|full/i.test(failed.stderr),
                        `${failpoint}/${scenario.name}: a full volume was not reported as one: ${failed.stderr.trim()}`
                    )
                }
                if (action === 'edquot') {
                    assert(
                        /quota/i.test(failed.stderr),
                        `${failpoint}/${scenario.name}: quota exhaustion was not reported as one: ${failed.stderr.trim()}`
                    )
                }
                await assertRecoverable(binary, root, failpoint, scenario, action)
                await rm(root, { recursive: true, force: true })
            }
        }
    }

    // A transition guarding a capability the platform lacks cannot be reached
    // here, and pretending otherwise would either fail honest runs or hide a
    // real gap. Each exemption names the capability it needs and is printed, so
    // a platform's reduced coverage is visible in the log rather than implied.
    const REQUIRES = {
        'before-file-write': 'extended attributes',
        'after-file-rename': 'extended attributes',
        'after-file-sync': 'extended attributes',
        'after-chown': 'POSIX ownership',
        'after-chmod': 'POSIX ownership',
        'after-access-marker': 'POSIX ownership',
        'after-record-payload-rename': 'EFS metadata profile',
        'after-record-sidecar-rename': 'EFS metadata profile'
    }
    const available = {
        'extended attributes': extendedAttributes,
        'POSIX ownership': platform() !== 'win32',
        'EFS metadata profile': false
    }
    if (!extendedAttributes) {
        console.error(
            'skipped raw-file scenarios: this filesystem does not carry extended attributes'
        )
    }
    for (const skipped of unsupported) console.error(`skipped unsupported here: ${skipped}`)

    const uncovered = failpoints.filter((name) => !reached.has(name))
    const exempt = uncovered.filter((name) => available[REQUIRES[name]] === false)
    for (const name of exempt) {
        console.error(`not reachable here: ${name} requires ${REQUIRES[name]}`)
    }
    const missing = uncovered.filter((name) => !exempt.includes(name))
    assert(
        missing.length === 0,
        `no scenario reaches these declared failpoints: ${missing.join(', ')}`
    )
    const queueInterruptions = await verifyQueueCrashScenarios(machineBinary, template, workspace)
    const compactions = await verifyNativeCompactionInterruptions()
    console.log(
        `Recovered ${interrupted} interrupted native mutations across ${failpoints.length} failpoints, ` +
            `${compactions} interrupted index compactions, ` +
            `and validated ${queueInterruptions} interrupted queue operations`
    )
}

// Index compaction rewrites the snapshot inside an ordinary write. Every other
// scenario stops at a transition's first hit and never fills the WAL, so these
// seed a WAL just under the compaction threshold and interrupt every hit of
// every file transition in the one write that crosses it.

async function seedNearCompaction(root, request) {
    const walPath = join(root, '.collections', collection, 'index', 'keys.wal')
    const walBytes = async () => (await stat(walPath).catch(() => ({ size: 0 }))).size
    await request({ op: 'createCollection', collection, kind: 'document' })
    await request({ op: 'createCollection', collection: fileCollection, kind: 'file' })
    await request({
        op: 'putData',
        collection,
        id: secondIdentifier,
        data: wideDocument('survivor')
    })
    for (let seed = 0; (await walBytes()) < 60 * 1024; seed++) {
        await request({ op: 'putData', collection, data: wideDocument(seed) })
    }
    assert((await walBytes()) < 64 * 1024, 'compaction seed overshot the WAL threshold')
    return walBytes
}

async function verifyNativeCompactionInterruptions() {
    const source = join(workspace, 'compaction-template')
    await mkdir(source, { recursive: true })
    const seed = new Fylo(source, { binary: machineBinary, exclusiveRoot: true })
    await seed.ready
    const seededWal = await seedNearCompaction(source, (request) =>
        requestRequired(seed, request)
    ).finally(() => seed.close())
    const walBefore = await seededWal()
    const scenario = {
        name: 'compacting-put',
        args: (root) => [
            'put-document',
            '--root',
            root,
            '--collection',
            collection,
            '--id',
            COMPACTION_ID,
            '--document',
            JSON.stringify(wideDocument('crossing'))
        ]
    }
    let interrupted = 0
    for (const failpoint of COMPACTION_POINTS) {
        for (let skip = 0; ; skip++) {
            const root = join(workspace, `compaction-${failpoint}-${skip}`)
            await cloneRoot(source, root)
            const failed = await run(binary, scenario.args(root), {
                FYLO_RUST_FAILPOINT: failpoint,
                FYLO_RUST_FAILPOINT_ACTION: 'abort',
                FYLO_RUST_FAILPOINT_SKIP: String(skip)
            })
            if (failed.exitCode === 0) {
                const walAfter = (
                    await stat(join(root, '.collections', collection, 'index', 'keys.wal'))
                ).size
                assert(
                    walAfter < walBefore,
                    `${failpoint}: the crossing write did not compact the index`
                )
                await rm(root, { recursive: true, force: true })
                break
            }
            await assertRecoverable(binary, root, `${failpoint}#${skip}`, scenario, 'abort')
            await rm(root, { recursive: true, force: true })
            interrupted++
        }
    }
    assert(interrupted > 0, 'no compaction transition was interrupted')
    return interrupted
}

async function verifyEfsCompactionInterruptions() {
    const source = join(workspace, 'efs-compaction-template')
    await mkdir(source)
    const walBefore = await efsDatabase(source, async (database) => {
        const walBytes = await seedNearCompaction(source, (request) =>
            requestRequired(
                database,
                request.op === 'putData' ? { ...request, access: { mode: 0o640 } } : request
            )
        )
        return walBytes()
    })
    const request = {
        op: 'putData',
        collection,
        id: COMPACTION_ID,
        data: wideDocument('crossing'),
        access: { mode: 0o640 }
    }
    let interrupted = 0
    for (const failpoint of COMPACTION_POINTS) {
        for (let skip = 0; ; skip++) {
            const label = 'compaction ' + failpoint + '#' + skip
            const root = join(workspace, 'efs-compaction-' + failpoint + '-' + skip)
            await cloneRoot(source, root)
            const before = await efsInventory(root)
            const writer = new EfsMachine(root, {
                FYLO_RUST_FAILPOINT: failpoint,
                FYLO_RUST_FAILPOINT_ACTION: 'abort',
                FYLO_RUST_FAILPOINT_SKIP: String(skip)
            })
            let crashed = false
            try {
                await requestRequired(writer, { op: 'handshake' })
                const response = await writer.request(request)
                assert(
                    response.ok,
                    label + ': uninterrupted write failed: ' + JSON.stringify(response.error)
                )
            } catch (error) {
                await withDeadline(writer.exited, label + ' aborted process')
                assert(
                    writer.child.signalCode !== null,
                    label + ': failed without an injected abort: ' + error
                )
                crashed = true
            } finally {
                await writer.close()
            }
            if (!crashed) {
                const walAfter = (
                    await stat(join(root, '.collections', collection, 'index', 'keys.wal'))
                ).size
                assert(
                    walAfter < walBefore,
                    label + ': the crossing write did not compact the index'
                )
                await rm(root, { recursive: true, force: true })
                break
            }
            await recoverEfs(root)
            const recovered = await efsInventory(root)
            const added = recovered.filter((entry) => entry.id === COMPACTION_ID)
            assert(
                sameInventory(
                    recovered.filter((entry) => entry.id !== COMPACTION_ID),
                    before
                ),
                label + ': recovery changed records the write did not touch'
            )
            assert(added.length <= 1, label + ': recovery duplicated the written record')
            await recoverEfs(root)
            assert(
                sameInventory(await efsInventory(root), recovered),
                label + ': repeated recovery changed state'
            )
            await rm(root, { recursive: true, force: true })
            interrupted++
        }
    }
    assert(interrupted > 0, 'no EFS compaction transition was interrupted')
    return interrupted
}

async function verifyQueueCrashScenarios(binary, source, workspace) {
    const failpoints = ['before-file-write', 'after-file-sync', 'after-file-rename']
    const actions = ['abort', 'enospc', 'edquot']
    const scenarios = [
        {
            name: 'queue-publish',
            prepare: async () => ({
                op: 'queuePublish',
                topic: 'crash-publish',
                payload: { value: 1 }
            }),
            inspect: { topic: 'crash-publish', group: 'workers' }
        },
        {
            name: 'queue-claim',
            prepare: async (database) => {
                await database.queuePublish('crash-claim', { value: 1 })
                return {
                    op: 'queueClaim',
                    topic: 'crash-claim',
                    group: 'workers',
                    visibilityTimeoutMs: 100
                }
            },
            inspect: { topic: 'crash-claim', group: 'workers' }
        },
        {
            name: 'queue-ack',
            prepare: async (database) => {
                await database.queuePublish('crash-ack', { value: 1 })
                const [delivery] = await database.queueClaim('crash-ack', 'workers')
                return {
                    op: 'queueAck',
                    topic: 'crash-ack',
                    group: 'workers',
                    id: delivery.id,
                    receipt: delivery.receipt
                }
            },
            inspect: { topic: 'crash-ack', group: 'workers' }
        },
        {
            name: 'queue-dead-letter',
            prepare: async (database) => {
                await database.queuePublish('crash-dlq', { value: 1 })
                const [delivery] = await database.queueClaim('crash-dlq', 'workers', {
                    maxAttempts: 1
                })
                return {
                    op: 'queueNack',
                    topic: 'crash-dlq',
                    group: 'workers',
                    id: delivery.id,
                    receipt: delivery.receipt,
                    reason: 'crash matrix'
                }
            },
            inspect: { topic: 'crash-dlq', group: 'workers' }
        }
    ]
    let interrupted = 0
    for (const failpoint of failpoints) {
        for (const scenario of scenarios) {
            for (const action of actions) {
                const root = join(workspace, `${failpoint}--${scenario.name}--${action}`)
                await cloneRoot(source, root)
                const database = new Fylo(root, { binary, exclusiveRoot: true })
                await database.ready
                const request = await scenario.prepare(database)
                await database.close()
                const failed = await run(
                    binary,
                    ['exec', '--request', JSON.stringify(request), '--root', root],
                    {
                        FYLO_RUST_FAILPOINT: failpoint,
                        FYLO_RUST_FAILPOINT_ACTION: action
                    }
                )
                assert(
                    failed.exitCode !== 0,
                    `${failpoint}/${scenario.name}/${action}: queue operation was not interrupted`
                )
                const reopened = new Fylo(root, { binary, exclusiveRoot: true })
                await reopened.ready
                try {
                    await requestRequired(reopened, {
                        op: 'queueStats',
                        ...scenario.inspect
                    })
                    await requestRequired(reopened, {
                        op: 'queueDeadLetters',
                        ...scenario.inspect,
                        limit: 10
                    })
                } finally {
                    await reopened.close()
                }
                interrupted++
                await rm(root, { recursive: true, force: true })
            }
        }
    }
    return interrupted
}

/**
 * A crash may leave the mutation applied or rolled back — both are valid — but
 * the root must always open, recover idempotently, and land on a stable even
 * generation with an index that matches its documents.
 */
async function prepareSchemaFolder(_binary, root) {
    const folder = join(`${root}-schemas`, collection)
    await mkdir(join(folder, 'history'), { recursive: true })
    await writeFile(
        join(folder, 'manifest.json'),
        JSON.stringify({ current: 'v1', versions: [{ v: 'v1' }] })
    )
    await writeFile(
        join(folder, 'history', 'v1.schema.json'),
        JSON.stringify({ name: '^.+$', score: '^[0-9]+$' })
    )
}

function pushSchemaArguments(root) {
    return [
        'push-schema',
        '--root',
        root,
        '--collection',
        collection,
        '--schema-dir',
        `${root}-schemas`
    ]
}

// A segment collection's schema record is durable from the moment its flush
// returns. A crash after that must recover the new head; an in-process
// failure there must roll back to the old one. Either way the registry is
// never half committed: the reader's head, the schemas snapshot and the
// descriptor agree.
async function assertSchemaRegistry(binary, root, failpoint, scenario, action, label) {
    const recordPoints = ['after-schema-record', 'after-schemas-snapshot']
    const reported = await run(binary, [
        'schema-head',
        '--root',
        root,
        '--collection',
        collection
    ])
    assert(reported.exitCode === 0, `${label}: the schema head is unreadable: ${reported.stderr}`)
    const head = JSON.parse(reported.stdout).result ?? 0
    assert(head === 0 || head === 1, `${label}: schema head ${head} is neither the old nor the new`)
    const descriptorPath = join(root, '.fylo-catalog', 'collections', `${collection}.json`)
    const descriptor = JSON.parse(await readFile(descriptorPath, 'utf8'))
    if (scenario.name === 'push-schema') {
        assert(
            (descriptor.schemaHead ?? 0) === head,
            `${label}: the descriptor's head ${descriptor.schemaHead} disagrees with ${head}`
        )
        return
    }
    const snapshot = JSON.parse(
        await readFile(join(root, '.collections', collection, 'segments', 'schemas.snapshot'), 'utf8')
    )
    assert(
        snapshot.versions.length === head,
        `${label}: the schemas snapshot lists ${snapshot.versions.length} versions, the head is ${head}`
    )
    assert(
        (descriptor.schemaHead ?? 0) === head,
        `${label}: recovery left the descriptor's head ${descriptor.schemaHead} behind the records' ${head}`
    )
    if (recordPoints.includes(failpoint)) {
        const expected = action === 'abort' ? 1 : 0
        assert(
            head === expected,
            `${label}: expected head ${expected} after the record was flushed, found ${head}`
        )
    }
}

async function assertRecoverable(binary, root, failpoint, scenario, action) {
    const label = `${failpoint}/${scenario.name}/${action}`
    const recoveryCollection = scenario.name.endsWith('-file') ? fileCollection : collection

    if (action === 'abort') {
        // Nobody runs `recover` after a real crash: the next client to read
        // must get the recovered collection, not a phantom concurrent write.
        // A clone keeps the explicit `recover` path below covered too.
        const probe = `${root}-read-first`
        await cloneRoot(root, probe)
        const reader = new Fylo(probe, { binary: machineBinary, exclusiveRoot: true })
        await reader.ready
        try {
            const inspection = await reader.inspectCollection(recoveryCollection)
            assert(
                Number(inspection.indexedDocs) === Number(inspection.docsStored),
                `${label}: the first read after a crash saw a disagreeing index: ${JSON.stringify(inspection)}`
            )
        } catch (error) {
            throw new Error(`${label}: the first read after a crash failed: ${error}`)
        } finally {
            await reader.close()
            await rm(probe, { recursive: true, force: true })
        }
    }

    const first = await run(binary, ['recover', '--root', root, '--collection', recoveryCollection])
    assert(first.exitCode === 0, `${label}: Rust recovery failed: ${first.stderr}`)
    if (action !== 'abort') {
        // The writer stayed alive, so it owed itself a rollback before
        // returning the error. Anything left for recovery means it did not.
        assert(
            JSON.parse(first.stdout).recovered === false,
            `${label}: a failed write left its journal behind instead of rolling back`
        )
    }
    const second = await run(binary, [
        'recover',
        '--root',
        root,
        '--collection',
        recoveryCollection
    ])
    assert(second.exitCode === 0, `${label}: repeated recovery failed: ${second.stderr}`)
    assert(
        JSON.parse(second.stdout).recovered === false,
        `${label}: recovery is not idempotent — the second pass still had work`
    )
    if (scenario.name.startsWith('push-schema')) {
        await assertSchemaRegistry(binary, root, failpoint, scenario, action, label)
    }

    const database = new Fylo(root, { binary: machineBinary, exclusiveRoot: true })
    await database.ready
    try {
        const expectedCollections = scenario.template ? [collection] : [collection, fileCollection]
        for (const name of expectedCollections) {
            const inspection = await database.inspectCollection(name)
            assert(
                Number.isFinite(Number(inspection.docsStored)),
                `${label}: ${name} is unreadable after recovery`
            )
            assert(
                Number(inspection.indexedDocs) === Number(inspection.docsStored),
                `${label}: ${name} index disagrees with its documents after recovery: ${JSON.stringify(inspection)}`
            )
        }
        const survivor = (await database.getDoc(collection, secondIdentifier))[secondIdentifier]
        assert(
            survivor !== undefined || scenario.name.startsWith('sql-delete'),
            `${label}: an untouched document disappeared`
        )
    } finally {
        await database.close()
    }
}

/**
 * Copy a seeded root, extended attributes included.
 *
 * `fs.cp` carries contents and mode but not extended attributes, so a cloned
 * raw file loses the durable key it is read by — silently, and only on the
 * platforms where the copy is not a filesystem-level clone. The fixture has to
 * survive being cloned or the gate tests a root the engine never wrote.
 */
async function cloneRoot(source, target) {
    await mkdir(target, { recursive: true })
    const [command, args, okExit] =
        platform() === 'win32'
            ? [
                  'robocopy',
                  [source, target, '/E', '/COPYALL', '/NFL', '/NDL', '/NJH', '/NJS', '/NP'],
                  8
              ]
            : ['cp', ['-a', `${source}/.`, target], 1]
    const subprocess = Bun.spawn([command, ...args], {
        cwd: process.cwd(),
        env: process.env,
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const [stderr, exitCode] = await Promise.all([
        new Response(subprocess.stderr).text(),
        subprocess.exited
    ])
    // robocopy reports copied/extra files with codes below 8; only 8 and above
    // are failures.
    if (exitCode >= okExit) throw new Error(`cloning ${source} failed (${exitCode}): ${stderr}`)
}

async function requestRequired(database, request) {
    const response = await database.request(request)
    if (!response.ok) {
        throw new Error(
            `${request.op} failed (${response.error?.code}): ${response.error?.message}`
        )
    }
    return response.result
}

async function run(binary, arguments_, overrides = {}) {
    const subprocess = Bun.spawn([binary, ...arguments_], {
        cwd: process.cwd(),
        env: { ...process.env, ...overrides },
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const [stdout, stderr, exitCode] = await Promise.all([
        new Response(subprocess.stdout).text(),
        new Response(subprocess.stderr).text(),
        subprocess.exited
    ])
    return { stdout, stderr, exitCode }
}

async function command(arguments_) {
    const subprocess = Bun.spawn(arguments_, {
        cwd: process.cwd(),
        env: process.env,
        stdout: 'inherit',
        stderr: 'inherit'
    })
    const exitCode = await subprocess.exited
    if (exitCode !== 0) throw new Error(`command failed (${exitCode}): ${arguments_.join(' ')}`)
}

/**
 * On macOS and BSD a native lock names its owner by start time, read through
 * `ps`. Where `ps` cannot run (sandbox-exec refuses the setuid binary) an
 * aborted writer's lock is unprovably stale for the lock TTL, so recovery
 * fails closed with ENATIVE_CONCURRENT_WRITE. That is correct engine
 * behaviour, not a crash-recovery result, so refuse to run instead.
 */
function assertProcessIdentityAvailable() {
    if (['linux', 'win32'].includes(platform())) return
    let identity = ''
    try {
        const probe = Bun.spawnSync(['ps', '-o', 'lstart=', '-p', String(process.pid)])
        if (probe.exitCode === 0) identity = probe.stdout.toString().trim()
    } catch {}
    assert(
        identity !== '',
        'process identity is unavailable here (`ps` cannot run, e.g. under sandbox-exec); ' +
            'abort-case lock recovery cannot be verified in this environment'
    )
}

function assert(condition, message) {
    if (!condition) throw new Error(message)
}

// Explicit EFS lane: the main CLI owns profile admission and the real machine
// API owns writes. The native preview CLI above remains native by default.

async function withDeadline(promise, label) {
    let timer
    try {
        return await Promise.race([
            promise,
            new Promise((_, reject) => {
                timer = setTimeout(() => reject(new Error(label + ' timed out')), requestTimeout)
            })
        ])
    } finally {
        clearTimeout(timer)
    }
}

async function efsDatabase(root, callback) {
    const database = new EfsMachine(root)
    try {
        await requestRequired(database, { op: 'handshake' })
        return await callback(database)
    } finally {
        await database.close()
    }
}

async function fixtureFiles(root) {
    const entries = await readdir(root, { withFileTypes: true }).catch((error) => {
        if (error.code === 'ENOENT') return []
        throw error
    })
    const files = []
    for (const entry of entries) {
        const file = join(root, entry.name)
        if (entry.isDirectory()) files.push(...(await fixtureFiles(file)))
        else {
            assert(entry.isFile(), 'non-regular EFS fixture entry: ' + file)
            files.push(file)
        }
    }
    return files.sort()
}

async function efsInventory(root) {
    const files = [
        ...(await fixtureFiles(join(root, '.collections'))),
        ...(await fixtureFiles(join(root, '.buckets')))
    ].sort()
    const records = files.filter(
        (file) =>
            file.split(sep).some((part) => part === 'docs' || part === '.deleted') &&
            !file.endsWith('.fylo-attrs') &&
            !file.endsWith('.tmp')
    )
    const recordSet = new Set(records)
    for (const manifest of files.filter((file) => file.endsWith('.fylo-attrs'))) {
        assert(
            recordSet.has(manifest.slice(0, -'.fylo-attrs'.length)),
            'orphan EFS metadata: ' + manifest
        )
    }
    const inventory = []
    for (const file of records) {
        const payload = await stat(file)
        const manifest = await stat(file + '.fylo-attrs')
        assert((manifest.mode & 0o777) === 0o600, 'nonprivate EFS manifest: ' + file)
        assert(
            manifest.uid === payload.uid && manifest.gid === payload.gid,
            'EFS metadata owner differs from payload: ' + file
        )
        const attributes = JSON.parse(await readFile(file + '.fylo-attrs', 'utf8'))
        assert(
            attributes && typeof attributes === 'object' && !Array.isArray(attributes),
            'malformed EFS metadata map: ' + file
        )
        inventory.push({
            path: relative(root, file),
            id: basename(file).split('.')[0],
            collection: relative(root, file).split(sep).slice(0, 2).join('/'),
            hash: createHash('sha256')
                .update(await readFile(file))
                .digest('hex'),
            attributes,
            uid: payload.uid,
            gid: payload.gid,
            mode: payload.mode & 0o777,
            mtimeMs: Math.trunc(payload.mtimeMs),
            manifestMode: manifest.mode & 0o777
        })
    }
    return inventory
}

function comparableInventory(inventory) {
    return inventory.map(({ mtimeMs, attributes, ...record }) => {
        // Successful reference mutations use another wall-clock instant.
        // Rollback must retain exact originals; committed timestamps are
        // checked against originals and the interrupted mutation interval.
        const stable = { ...attributes }
        delete stable['user.fylo.meta-updated-at']
        delete stable['user.fylo.checksum']
        return { ...record, attributes: stable }
    })
}

function sameInventory(left, right) {
    return JSON.stringify(left) === JSON.stringify(right)
}

function assertCommittedTimes(before, after, started, finished, label) {
    for (const record of after) {
        const previous = before.find(
            (item) => item.collection === record.collection && item.id === record.id
        )
        if (previous && previous.hash === record.hash) {
            assert(
                record.mtimeMs === previous.mtimeMs,
                label + ': metadata/move changed unchanged payload mtime'
            )
        } else {
            assert(
                record.mtimeMs >= started - 1 && record.mtimeMs <= finished + 1,
                label + ': changed payload time is outside the mutation interval'
            )
        }
        const stamp = record.attributes['user.fylo.meta-updated-at']
        const oldStamp = previous?.attributes['user.fylo.meta-updated-at']
        if (stamp && oldStamp && stamp !== oldStamp) {
            assert(
                Number(Buffer.from(stamp, 'base64').toString()) >
                    Number(Buffer.from(oldStamp, 'base64').toString()),
                label + ': metadata update timestamp did not advance'
            )
        }
    }
}

async function efsJournals(root) {
    const manifests = (await fixtureFiles(join(root, '.fylo-transactions'))).filter(
        (file) => basename(file) === 'transaction.json'
    )
    // The manifest is written once at begin; each capture is its own durable
    // file under `captures/`, which is what recovery reads.
    return await Promise.all(
        manifests.map(async (file) => {
            const captures = (await fixtureFiles(join(file, '..', 'captures'))).filter((capture) =>
                capture.endsWith('.json')
            )
            return {
                ...JSON.parse(await readFile(file, 'utf8')),
                captures: await Promise.all(
                    captures.map(async (capture) => JSON.parse(await readFile(capture, 'utf8')))
                )
            }
        })
    )
}

async function efsUnstableGenerations(root) {
    const states = (await fixtureFiles(join(root, '.fylo-transactions'))).filter(
        (file) => basename(file) === 'state.json'
    )
    const unstable = []
    for (const file of states) {
        if (JSON.parse(await readFile(file, 'utf8')).state !== 'stable') unstable.push(file)
    }
    return unstable
}
async function recoverEfs(root) {
    await efsDatabase(root, async (database) => {
        for (const name of [collection, fileCollection]) {
            // Existing public rebuild performs collection recovery first.
            await requestRequired(database, { op: 'rebuildCollection', collection: name })
            const inspected = await requestRequired(database, {
                op: 'inspectCollection',
                collection: name
            })
            assert(
                Number(inspected.indexedDocs) === Number(inspected.docsStored),
                name + ': EFS recovered index differs from records'
            )
        }
    })
    assert((await efsJournals(root)).length === 0, 'EFS recovery left a journal')
}

async function verifyEfsCrashScenarios(preview, workspace) {
    assert(platform() !== 'win32', 'EFS profile qualification needs POSIX metadata')
    const source = join(workspace, 'efs-template')
    await mkdir(source)
    // Records carry owner-only access descriptors, so mutating one needs the
    // owner. That is the filesystem's owner, not this process: an EFS access
    // point maps every client to its own POSIX identity.
    const { uid, gid } = await stat(source)
    const owner = { uid, groups: [gid] }
    const input = join(workspace, 'efs-input.bin')
    await writeFile(input, new Uint8Array([1, 2, 3, 4]))
    await efsDatabase(source, async (database) => {
        await requestRequired(database, { op: 'createCollection', collection, kind: 'document' })
        await requestRequired(database, {
            op: 'createCollection',
            collection: fileCollection,
            kind: 'file'
        })
        for (const [id, name] of [
            [identifier, 'Ada'],
            [secondIdentifier, 'Grace'],
            [deletedIdentifier, 'Linus']
        ]) {
            await requestRequired(database, {
                op: 'putData',
                collection,
                id,
                data: { name, score: 42 },
                meta: { reviewer: 'seed', typed: { nested: [1, true] } },
                access: { mode: 0o640 }
            })
        }
        await requestRequired(database, {
            op: 'delDoc',
            collection,
            id: deletedIdentifier,
            access: owner
        })
        await requestRequired(database, {
            op: 'putData',
            collection: fileCollection,
            id: fileIdentifier,
            file: { path: input, key: '/seed.bin' },
            meta: { source: 'seed' },
            access: { mode: 0o640 }
        })
        await requestRequired(database, { op: 'commit', message: 'EFS crash seed' })
    })
    await efsInventory(source)
    const { failpoints } = JSON.parse((await run(preview, ['failpoints'])).stdout)
    const pairPoints = ['after-record-payload-rename', 'after-record-sidecar-rename']
    for (const name of pairPoints) {
        assert(failpoints.includes(name), 'candidate did not declare EFS pair failpoint: ' + name)
    }
    const scenarios = [
        {
            name: 'put-document',
            request: {
                op: 'putData',
                collection,
                id: '4VRNF52JPD0',
                data: { name: 'Hopper', score: 7 },
                meta: { reviewer: 'created' },
                access: { mode: 0o640 }
            }
        },
        {
            name: 'put-file',
            request: {
                op: 'putData',
                collection: fileCollection,
                id: crashFileIdentifier,
                file: { path: input, key: '/crash.bin' },
                meta: { source: 'created' },
                access: { mode: 0o640 }
            }
        },
        {
            name: 'patch-document',
            request: {
                op: 'patchDoc',
                collection,
                id: identifier,
                newDoc: { score: 99 },
                access: owner
            }
        },
        {
            name: 'metadata',
            request: {
                op: 'setMeta',
                collection,
                id: identifier,
                meta: { reviewer: 'after', typed: null, added: [1, true] },
                access: owner
            }
        },
        {
            name: 'delete-document',
            pairMove: true,
            request: {
                op: 'delDoc',
                collection,
                id: identifier,
                access: owner
            }
        },
        {
            name: 'delete-file',
            pairMove: true,
            request: {
                op: 'delDoc',
                collection: fileCollection,
                id: fileIdentifier,
                access: owner
            }
        },
        {
            name: 'restore',
            pairMove: true,
            request: {
                op: 'restoreDoc',
                collection,
                id: deletedIdentifier,
                access: owner
            }
        },
        {
            name: 'sql-delete',
            pairMove: true,
            request: {
                op: 'executeSQL',
                sql: "DELETE FROM records WHERE name = 'Grace'",
                access: owner
            }
        },
        {
            name: 'reshard',
            pairMove: true,
            request: {
                op: 'reshardCollection',
                collection,
                width: 3
            }
        },
        {
            name: 'commit',
            prepare: {
                op: 'setMeta',
                collection,
                id: identifier,
                meta: { reviewer: 'pending commit' },
                access: owner
            },
            request: { op: 'commit', message: 'EFS interrupted commit' }
        }
    ]
    const reached = new Set()
    let interrupted = 0
    const runCase = async ({ failpoint, scenario, action }) => {
        const label = failpoint + '/' + scenario.name + '/' + action
        const root = join(workspace, 'efs-' + failpoint + '-' + scenario.name + '-' + action)
        const expectedRoot = root + '-reference'
        await cloneRoot(source, root)
        if (scenario.prepare) {
            await efsDatabase(root, (database) => requestRequired(database, scenario.prepare))
        }
        const before = await efsInventory(root)
        await cloneRoot(root, expectedRoot)
        await efsDatabase(expectedRoot, (database) => requestRequired(database, scenario.request))
        const expectedAfter = await efsInventory(expectedRoot)
        const writer = new EfsMachine(root, {
            FYLO_RUST_FAILPOINT: failpoint,
            FYLO_RUST_FAILPOINT_ACTION: action
        })
        const started = Date.now()
        let response
        let crashed = false
        try {
            await requestRequired(writer, { op: 'handshake' })
            try {
                response = await writer.request(scenario.request)
            } catch (error) {
                await withDeadline(writer.exited, label + ' aborted process')
                assert(
                    action === 'abort' && writer.child.signalCode !== null,
                    label + ': request failed without an injected process abort: ' + error
                )
                crashed = true
            }
            if (!crashed && !response.ok) {
                assert(
                    response.error?.code !== 'ENATIVE_UNSUPPORTED',
                    label + ': EFS required mutation was unsupported'
                )
                assert(action !== 'abort', label + ': abort returned an ordinary error')
                if (action === 'enospc') {
                    assert(
                        /disk|space|full/i.test(response.error?.message),
                        label + ': full-volume error lost its identity'
                    )
                }
                if (action === 'edquot') {
                    assert(
                        /quota/i.test(response.error?.message),
                        label + ': quota error lost its identity'
                    )
                }
                // An orphan journal under a stable generation is not
                // recovery work: the next opener deletes it without
                // restoring anything, as the native gate's
                // `recovered === false` check also accepts.
                assert(
                    (await efsUnstableGenerations(root)).length === 0,
                    label + ': failed live writer left recovery work: ' + response.error?.message
                )
                assert(
                    sameInventory(await efsInventory(root), before),
                    label + ': live rollback changed payload/metadata/native mode/time'
                )
            }
        } finally {
            await writer.close()
        }
        const finished = Date.now()
        if (!crashed && response.ok) {
            await rm(root, { recursive: true, force: true })
            await rm(expectedRoot, { recursive: true, force: true })
            return
        }
        reached.add(failpoint)
        interrupted++
        const journals = await efsJournals(root)
        if (crashed && ['before-commit-marker', 'after-commit-marker'].includes(failpoint)) {
            const phase = failpoint === 'after-commit-marker' ? 'committed' : 'active'
            assert(
                journals.some((journal) => journal.phase === phase),
                label + ': interrupted journal did not retain ' + phase + ' phase'
            )
        }
        if (crashed && scenario.pairMove && pairPoints.includes(failpoint)) {
            assert(
                journals.some(
                    (journal) =>
                        journal.captures.filter((capture) => capture.path.endsWith('.fylo-attrs'))
                            .length >= 2
                ),
                label + ': source and destination sidecars were not both captured'
            )
        }
        await recoverEfs(root)
        const recovered = await efsInventory(root)
        const isBefore = sameInventory(recovered, before)
        const isAfter = sameInventory(
            comparableInventory(recovered),
            comparableInventory(expectedAfter)
        )
        assert(isBefore || isAfter, label + ': recovery produced a mixed payload/metadata pair')
        if (crashed && failpoint === 'before-commit-marker') {
            assert(isBefore, label + ': active journal did not restore complete before-images')
        }
        if (crashed && failpoint === 'after-commit-marker') {
            assert(isAfter, label + ': committed journal lost durable pair state')
        }
        if (!isBefore) assertCommittedTimes(before, recovered, started, finished, label)
        await recoverEfs(root)
        assert(
            sameInventory(await efsInventory(root), recovered),
            label + ': repeated recovery changed metadata, permissions or timestamps'
        )
        await rm(root, { recursive: true, force: true })
        await rm(expectedRoot, { recursive: true, force: true })
    }
    // Cases share nothing but the read-only template, so they run concurrently:
    // on a network filesystem each one waits on round trips, not the CPU.
    const jobs = failpoints
        .flatMap((failpoint) =>
            scenarios.flatMap((scenario) =>
                ['abort', 'enospc', 'edquot'].map((action) => ({ failpoint, scenario, action }))
            )
        )
        .filter((_, index) => index % shards === shard)
    let next = 0
    let failure
    await Promise.all(
        Array.from({ length: concurrency }, async () => {
            while (failure === undefined && next < jobs.length) {
                try {
                    await runCase(jobs[next++])
                } catch (error) {
                    failure ??= error
                }
            }
        })
    )
    if (failure !== undefined) throw failure
    if (shards > 1) {
        // A shard reaches only some transitions; the union is checked across shards.
        console.log(
            'Shard ' + shard + '/' + shards + ' reached: ' + JSON.stringify([...reached].sort())
        )
        console.log('Validated ' + interrupted + ' EFS-profile interrupted mutations in this shard')
        return
    }
    const missing = failpoints.filter((name) => !reached.has(name))
    assert(
        missing.length === 0,
        'EFS crash scenarios missed declared transitions: ' + missing.join(', ')
    )
    const compactions = await verifyEfsCompactionInterruptions()
    console.log(
        'Validated ' +
            interrupted +
            ' EFS-profile interrupted mutations and ' +
            compactions +
            ' interrupted index compactions; remote qualification pending'
    )
}
