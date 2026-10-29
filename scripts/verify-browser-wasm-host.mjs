// Drive the browser module through a real host table.
//
// `fylo-wasm` has no filesystem of its own, so nothing about it is exercised
// until an embedder fills the twelve-function table. A browser cannot run in
// `bun test`, but the boundary being tested is not browser-specific: it is
// pointer handoff, errno mapping, the directory-listing retry, and the packed
// response. Backing the table with `node:fs` runs exactly the code a Worker
// will run over OPFS, against storage this script can inspect afterwards.
//
// Usage: node scripts/verify-browser-wasm-host.mjs [--module <path>] [--binary <native fylo-rust>]
//          [--output <path>]

import { CargoArtifacts } from './cargo-artifacts.mjs'
import {
    closeSync,
    openSync,
    readSync,
    writeSync,
    fstatSync,
    ftruncateSync,
    fsyncSync
} from 'node:fs'
import {
    mkdirSync,
    mkdtempSync,
    readdirSync,
    renameSync,
    rmSync,
    statSync,
    unlinkSync
} from 'node:fs'
import { readFileSync, writeFileSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { webcrypto } from 'node:crypto'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'

import {
    KIND_DIRECTORY,
    KIND_FILE,
    KIND_MISSING,
    OPEN_APPEND,
    OPEN_CREATE,
    OPEN_EXCLUSIVE,
    OPEN_TRUNCATE,
    OPEN_WRITE,
    createHostImports,
    exec
} from '../src/browser/host-vfs.mjs'

const modulePath = resolve(
    option('--module') ?? CargoArtifacts.path('wasm32-unknown-unknown/release/fylo_wasm.wasm')
)
const output = resolve(option('--output') ?? CargoArtifacts.path('browser/browser-wasm-host.json'))
// The native binary pushes the schemas the module validates with: a browser
// build has no regex compiler (ADR 0017 section 4).
const nativeBinary = resolve(
    option('--binary') ??
        CargoArtifacts.path('release', process.platform === 'win32' ? 'fylo-rust.exe' : 'fylo-rust')
)
const root = mkdtempSync(join(tmpdir(), 'fylo-browser-'))

try {
    const instance = await instantiate(modulePath, nodeBackend())
    const frames = run(instance, [
        { op: 'handshake' },
        { op: 'createCollection', collection: 'notes', root },
        { op: 'putData', collection: 'notes', data: { name: 'Ada', score: 42 }, root },
        { op: 'putData', collection: 'notes', data: { name: 'Grace', score: 7 }, root },
        { op: 'findDocs', collection: 'notes', query: {}, root },
        { op: 'findDocs', collection: 'notes', query: { $ops: [{ name: { $eq: 'Ada' } }] }, root },
        { op: 'inspectCollection', collection: 'notes', root },
        // Enough records to push the directory listing past the module's first
        // 4 KiB buffer, which is the retry path a small fixture never reaches.
        ...Array.from({ length: 120 }, (_, index) => ({
            op: 'putData',
            collection: 'notes',
            data: { name: `row-${index}`, score: index },
            root
        })),
        { op: 'inspectCollection', collection: 'notes', root },
        { op: 'executeSQL', sql: "SELECT * FROM notes WHERE name = 'Grace'", root },
        { op: 'rebuildCollection', collection: 'notes', root }
    ])

    for (const [index, frame] of frames.entries()) {
        assert(frame.ok, `frame ${index} (${frame.op}) failed: ${frame.error?.message}`)
    }

    const handshake = frames[0].result
    assert(handshake.capabilities.exclusiveRoot === false, 'browser must not claim a kernel lease')
    assert(
        handshake.capabilities.machineAccess === undefined,
        'browser must not advertise POSIX access'
    )

    const found = frames[4].result
    assert(Object.keys(found).length === 2, 'findDocs did not return both documents')
    const ordered = Object.keys(found)
    assert(
        ordered.join() === [...ordered].sort().join(),
        `results are not ascending by identifier: ${ordered.join()}`
    )

    const inspected = frames.at(-3).result
    assert(inspected.docsStored === 122, `expected 122 documents, got ${inspected.docsStored}`)
    assert(inspected.indexedDocs === 122, `index lost documents: ${inspected.indexedDocs}`)

    // The module wrote a real FYLO root through the host, not a private format.
    const documents = readdirSync(join(root, '.collections', 'notes', 'docs'))
    assert(documents.length > 0, 'no shard directories were created')

    // Attributes belong to the host now: nothing may be written beside a record.
    const stray = everyFile(root).filter((path) => path.endsWith('.fylo-attrs'))
    assert(stray.length === 0, `attribute sidecars were written: ${stray.join(', ')}`)

    // ...and they must still round-trip, which is what the sidecar used to do.
    writeFileSync(join(root, 'source.bin'), 'hello bucket')
    const seeded = run(instance, [
        { op: 'createCollection', collection: 'files', kind: 'file', root },
        {
            op: 'putData',
            collection: 'files',
            file: { path: join(root, 'source.bin') },
            meta: { reviewed: true },
            root
        }
    ]).slice(1)
    assert(seeded[0].ok, `bucket write failed: ${seeded[0].error?.message}`)
    const stored = run(instance, [
        { op: 'getDoc', collection: 'files', id: seeded[0].result, root }
    ])[0].result[seeded[0].result]
    assert(stored.key === '/source.bin', `logical key lost: ${stored.key}`)
    assert(stored.meta?.reviewed === true, 'developer metadata lost without a sidecar')
    assert(stored.contentLength === 12, `content length wrong: ${stored.contentLength}`)

    const schemaFrames = verifySchemaValidation(instance, handshake)

    const report = {
        format: 'fylo.browser-wasm-host.v1',
        module: modulePath,
        frames: frames.length + schemaFrames,
        documents: inspected.docsStored,
        shards: documents.length,
        schemaValidation: true,
        passed: true
    }
    await mkdir(dirname(output), { recursive: true })
    await writeFile(output, `${JSON.stringify(report, null, 2)}\n`)
    console.log(
        `Verified the browser module answered ${frames.length} frames through a host table and stored ${inspected.docsStored} documents: ${output}`
    )
} catch (error) {
    console.error(error instanceof Error ? error.message : error)
    process.exitCode = 1
} finally {
    rmSync(root, { recursive: true, force: true })
}

/**
 * ADR 0017 section 4: the browser validates with the DFAs a native push
 * stored in the schema registry, and fails closed where it cannot.
 *
 * The native binary pushes the schemas, because the module has no regex
 * compiler; every write after that goes through the module, and each refusal
 * must carry the code and message the native binary gives for the same
 * document.
 */
function verifySchemaValidation(instance, handshake) {
    assert(
        handshake.capabilities.schemaValidation?.compiledOnly === true &&
            handshake.capabilities.schemaValidation?.validator === 'chex',
        `browser must report its compiled-only validator: ${JSON.stringify(handshake.capabilities.schemaValidation)}`
    )
    const schemaDir = join(root, '..', `${basename(root)}-schemas`)
    const author = (collection, chex) => {
        mkdirSync(join(schemaDir, collection, 'history'), { recursive: true })
        writeFileSync(
            join(schemaDir, collection, 'manifest.json'),
            JSON.stringify({ current: 'v1', versions: [{ v: 'v1' }] })
        )
        writeFileSync(join(schemaDir, collection, 'history', 'v1.schema.json'), JSON.stringify(chex))
    }
    author('people', { name: '^[A-Z]\\w+$', 'age?': '^[0-9]+$', city: '(?i)^[a-zé ]+$' })
    // A Unicode word boundary has no DFA form: native-only (not portable).
    author('words', { word: '\\bword\\b' })
    // Pushed by nobody: its schema exists only in the folder, which a browser
    // cannot read, so the module knows it only by its recorded strict mode.
    author('legacy', { name: '^[a-z]+$' })

    const invalid = { name: 'ada', city: 'Zürich' }
    const native = runNative([
        { op: 'createCollection', collection: 'people', root },
        { op: 'createCollection', collection: 'words', root },
        { op: 'schemaPush', collection: 'people', schemaDir, root },
        { op: 'schemaPush', collection: 'words', schemaDir, root },
        // Created after the one-time import, so it stays on the folder.
        { op: 'createCollection', collection: 'legacy', root },
        ...['people', 'words', 'legacy'].map((collection) => ({
            op: 'schemaMode',
            collection,
            mode: 'strict',
            root
        })),
        // The refusal the browser must reproduce, code and message.
        { op: 'putData', collection: 'people', data: invalid, root }
    ])
    for (const [index, frame] of native.slice(0, -1).entries()) {
        assert(frame.ok, `native schema setup frame ${index} failed: ${frame.error?.message}`)
    }
    const nativeRefusal = native.at(-1)
    assert(nativeRefusal.error?.code === 'ESCHEMA', 'native accepted an invalid document')
    for (const [collection, portable] of [
        ['people', true],
        ['words', false]
    ]) {
        const record = JSON.parse(
            readFileSync(join(root, '.fylo-schemas', collection, '1.json'), 'utf8')
        )
        assert(record.portable === portable, `${collection} portable should be ${portable}`)
    }

    const requests = [
        { op: 'putData', collection: 'people', data: { name: 'Ada', age: 36, city: 'Montréal' }, root },
        { op: 'putData', collection: 'people', data: invalid, root },
        { op: 'putData', collection: 'people', data: { name: 'Ada', extra: 1, city: 'x' }, root },
        { op: 'executeSQL', sql: "INSERT INTO people (name, city) VALUES ('grace', 'Paris')", root },
        { op: 'schemaValidate', collection: 'people', document: { name: 'Grace', city: 'Été' }, root },
        { op: 'schemaMode', collection: 'people', mode: 'warn', root },
        { op: 'putData', collection: 'people', data: invalid, root },
        { op: 'schemaMode', collection: 'people', mode: 'off', root },
        { op: 'putData', collection: 'people', data: { anything: true }, root },
        { op: 'putData', collection: 'words', data: { word: 'a word' }, root },
        { op: 'schemaMode', collection: 'words', mode: 'warn', root },
        { op: 'putData', collection: 'words', data: { word: 'a word' }, root },
        { op: 'schemaMode', collection: 'words', mode: 'off', root },
        { op: 'putData', collection: 'words', data: { word: 'a word' }, root },
        { op: 'putData', collection: 'legacy', data: { name: 'ok' }, root },
        { op: 'findDocs', collection: 'people', query: {}, root }
    ]
    const frames = run(instance, requests)
    const [
        valid,
        refused,
        unknownField,
        sqlInsert,
        validated,
        warnMode,
        warned,
        ,
        unchecked,
        unportableStrict,
        ,
        unportableWarn,
        ,
        unportableOff,
        folderOnly,
        found
    ] = frames
    assert(valid.ok, `a valid document was refused: ${valid.error?.message}`)
    assert(
        refused.error?.code === nativeRefusal.error.code &&
            refused.error?.message === nativeRefusal.error.message,
        `browser refusal differs from native: ${JSON.stringify(refused.error)} vs ${JSON.stringify(nativeRefusal.error)}`
    )
    assert(unknownField.error?.code === 'ESCHEMA', 'an undeclared field was stored')
    assert(sqlInsert.error?.code === 'ESCHEMA', 'SQL INSERT skipped validation')
    assert(validated.ok && validated.result.document._v === 'v1', 'schemaValidate failed')
    assert(warnMode.ok && warned.ok, `warn mode refused the write: ${warned.error?.message}`)
    assert(unchecked.ok, `off mode refused the write: ${unchecked.error?.message}`)
    for (const [name, frame] of [
        ['strict', unportableStrict],
        ['warn', unportableWarn]
    ]) {
        assert(
            frame.error?.code === 'ESCHEMAUNAVAILABLE',
            `a non-portable schema in ${name} mode did not fail closed: ${JSON.stringify(frame.error ?? frame.result)}`
        )
    }
    assert(unportableOff.ok, `off mode refused a non-portable collection: ${unportableOff.error?.message}`)
    assert(
        folderOnly.error?.code === 'ESCHEMAUNAVAILABLE',
        `a folder-only schema did not fail closed: ${JSON.stringify(folderOnly.error ?? folderOnly.result)}`
    )
    const people = Object.values(found.result)
    assert(people.length === 3, `expected 3 people (valid, warned, unchecked), got ${people.length}`)
    assert(
        people.some((person) => person.name === 'Ada' && person._v === 'v1'),
        'the valid document was not stamped with the head version'
    )
    rmSync(schemaDir, { recursive: true, force: true })
    return frames.length
}

/** Run `requests` through the native binary, one process, one frame each. */
function runNative(requests) {
    const outcome = spawnSync(nativeBinary, ['exec', '--loop', '--root', root], {
        input: `${requests.map((request) => JSON.stringify(request)).join('\n')}\n`,
        encoding: 'utf8',
        env: Object.fromEntries(
            Object.entries(process.env).filter(([name]) => !name.startsWith('FYLO_'))
        ),
        cwd: root
    })
    if (outcome.error) {
        throw new Error(`cannot run the native binary ${nativeBinary}: ${outcome.error.message}`)
    }
    const frames = outcome.stdout
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
    assert(
        frames.length === requests.length,
        `native binary answered ${frames.length} of ${requests.length} frames: ${outcome.stderr}`
    )
    return frames
}

async function instantiate(path, backend) {
    let instance
    const imports = createHostImports({ memory: () => instance.exports.memory, backend })
    const module_ = await WebAssembly.compile(await readFile(path))
    instance = await WebAssembly.instantiate(module_, imports)
    return instance
}

function run(instance, requests) {
    const ndjson = `${requests.map((request) => JSON.stringify(request)).join('\n')}\n`
    return exec(instance, ndjson)
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
}

/** The same shape a Worker implements over OPFS sync access handles. */
function nodeBackend() {
    const handles = new Map()
    // One manifest for the whole root rather than a file per record, which is
    // the point of moving attributes into the table: OPFS pays per file.
    const attributes = new Map()
    let next = 1
    return {
        open(path, flags) {
            let mode = 'r'
            if (flags & OPEN_EXCLUSIVE) mode = 'wx+'
            else if (flags & OPEN_TRUNCATE && flags & OPEN_CREATE) mode = 'w+'
            else if (flags & OPEN_CREATE) mode = 'a+'
            else if (flags & OPEN_WRITE) mode = 'r+'
            if (flags & OPEN_APPEND && !(flags & OPEN_EXCLUSIVE)) mode = 'a+'
            const descriptor = openSync(path, mode)
            const handle = next++
            handles.set(handle, descriptor)
            return handle
        },
        close(handle) {
            const descriptor = handles.get(handle)
            if (descriptor === undefined) return
            closeSync(descriptor)
            handles.delete(handle)
        },
        readAt(handle, offset, into) {
            const descriptor = required(handles, handle)
            return readSync(descriptor, into, 0, into.length, offset)
        },
        writeAt(handle, offset, from) {
            const descriptor = required(handles, handle)
            return writeSync(descriptor, from, 0, from.length, offset)
        },
        truncate(handle, length) {
            ftruncateSync(required(handles, handle), length)
        },
        flush(handle) {
            fsyncSync(required(handles, handle))
        },
        stat(path) {
            try {
                const entry = statSync(path)
                return {
                    kind: entry.isDirectory() ? KIND_DIRECTORY : KIND_FILE,
                    len: entry.size,
                    modifiedMs: Math.trunc(entry.mtimeMs)
                }
            } catch {
                return { kind: KIND_MISSING, len: 0, modifiedMs: 0 }
            }
        },
        mkdir(path, recursive) {
            mkdirSync(path, { recursive })
        },
        unlink(path) {
            unlinkSync(path)
        },
        rmdir(path, recursive) {
            rmSync(path, { recursive, force: false })
        },
        rename(from, to) {
            renameSync(from, to)
        },
        readDir(path) {
            return readdirSync(path)
        },
        random(into) {
            // A browser worker calls `crypto.getRandomValues` here.
            webcrypto.getRandomValues(into)
        },
        nowUnixMs() {
            return Date.now()
        },
        log(message) {
            console.error(message)
        },
        readAttrs(path) {
            return attributes.get(path) ?? new Uint8Array()
        },
        writeAttrs(path, manifest) {
            if (manifest.length === 0) attributes.delete(path)
            else attributes.set(path, manifest.slice())
        }
    }
}

function everyFile(directory) {
    return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
        const path = join(directory, entry.name)
        return entry.isDirectory() ? everyFile(path) : [path]
    })
}

function required(handles, handle) {
    const descriptor = handles.get(handle)
    if (descriptor === undefined) {
        const error = new Error(`unknown handle ${handle}`)
        error.code = 'EBADF'
        throw error
    }
    return descriptor
}

function assert(value, message) {
    if (!value) throw new Error(message)
}

function option(name) {
    const index = process.argv.indexOf(name)
    if (index === -1) return undefined
    const value = process.argv[index + 1]
    if (!value || value.startsWith('--')) throw new Error(`missing value for ${name}`)
    return value
}

function fstat(descriptor) {
    return fstatSync(descriptor)
}
void fstat
