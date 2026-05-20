#!/usr/bin/env bun
import path from 'node:path'
import Fylo from '../index.js'
import { runMachineRequestSource } from './machine.js'
import { renderTableOutput, writeCliText } from './output.js'
import {
    doctorSchema,
    inspectSchema,
    materializeSchemaDocument,
    validateSchemaDocument
} from '../schema/admin.js'

/**
 * @typedef {import('../types/vendor.js').TTID} TTID
 * @typedef {import('./format.js').FormatTableOptions} FormatTableOptions
 * @typedef {import('./output.js').PagerMode} PagerMode
 */

/**
 * @typedef {object} ParsedArgs
 * @property {string[]} positionals
 * @property {string | undefined} root
 * @property {boolean} worm
 * @property {boolean} json
 * @property {boolean} idOnly
 * @property {string | undefined} schemaDir
 * @property {number | undefined} pageSize
 * @property {'left' | 'center' | 'right' | 'auto'} align
 * @property {PagerMode} pager
 * @property {string | undefined} request
 * @property {boolean} help
 * @returns {string}
 */
function usage() {
    return [
        'Usage:',
        '  fylo.query "<SQL>"',
        '  fylo.query sql "<SQL>"',
        '  fylo.exec exec --request <json|@path|-> [--root <path>] [--worm]',
        '  fylo.query inspect <collection> [--root <path>] [--worm] [--json]',
        '  fylo.query get <collection> <doc-id> [--root <path>] [--worm] [--json]',
        '  fylo.query latest <collection> <doc-or-lineage-id> [--root <path>] [--worm] [--json] [--id-only]',
        '  fylo.query history <collection> <doc-or-lineage-id> [--root <path>] [--worm] [--json]',
        '  fylo.query rebuild <collection> [--root <path>] [--worm] [--json]',
        '  fylo.query schema inspect <collection> [--schema-dir <path>] [--json]',
        '  fylo.query schema current <collection> [--schema-dir <path>] [--json]',
        '  fylo.query schema history <collection> [--schema-dir <path>] [--json]',
        '  fylo.query schema doctor <collection> [--schema-dir <path>] [--json]',
        '  fylo.query schema validate <collection> <json|@path|-> [--schema-dir <path>] [--json]',
        '  fylo.query schema materialize <collection> <json|@path|-> [--schema-dir <path>] [--json]',
        '  fylo.admin inspect <collection> [--root <path>] [--worm] [--json]',
        '  fylo.admin get <collection> <doc-id> [--root <path>] [--worm] [--json]',
        '  fylo.admin latest <collection> <doc-or-lineage-id> [--root <path>] [--worm] [--json] [--id-only]',
        '  fylo.admin history <collection> <doc-or-lineage-id> [--root <path>] [--worm] [--json]',
        '  fylo.admin rebuild <collection> [--root <path>] [--worm] [--json]',
        '  fylo.admin schema inspect <collection> [--schema-dir <path>] [--json]',
        '  fylo.admin schema current <collection> [--schema-dir <path>] [--json]',
        '  fylo.admin schema history <collection> [--schema-dir <path>] [--json]',
        '  fylo.admin schema doctor <collection> [--schema-dir <path>] [--json]',
        '  fylo.admin schema validate <collection> <json|@path|-> [--schema-dir <path>] [--json]',
        '  fylo.admin schema materialize <collection> <json|@path|-> [--schema-dir <path>] [--json]',
        '',
        'Options:',
        '  --root <path>   Override FYLO_ROOT for this command',
        '  --schema-dir <path> Override FYLO_SCHEMA for schema admin commands',
        '  --worm          Enable WORM-aware admin behavior for this command',
        '  --json          Emit machine-readable JSON output',
        '  --id-only       Return only the resolved document id for latest',
        '  --page-size <n> Repeat headers every n rows in text output',
        '  --align <mode>  Cell alignment: left, center, right, or auto',
        '  --request <v>   Machine request payload, @file path, or - for stdin',
        '  --no-pager      Disable interactive paging even on large text output',
        '  --help          Show this message'
    ].join('\n')
}

/**
 * @param {string[]} argv
 * @returns {ParsedArgs}
 */
function parseArgs(argv) {
    const positionals = []
    let root
    let worm = false
    let json = false
    let idOnly = false
    let schemaDir
    let pageSize
    /** @type {'left' | 'center' | 'right' | 'auto'} */
    let align = 'auto'
    /** @type {PagerMode} */
    let pager = 'auto'
    let request
    let help = false
    for (let index = 0; index < argv.length; index++) {
        const arg = argv[index]
        if (arg === '--root') {
            const value = argv[index + 1]
            if (!value) throw new Error('Missing value for --root')
            root = path.resolve(value)
            index++
            continue
        }
        if (arg === '--schema-dir') {
            const value = argv[index + 1]
            if (!value) throw new Error('Missing value for --schema-dir')
            schemaDir = path.resolve(value)
            index++
            continue
        }
        if (arg === '--json') {
            json = true
            continue
        }
        if (arg === '--worm') {
            worm = true
            continue
        }
        if (arg === '--id-only') {
            idOnly = true
            continue
        }
        if (arg === '--page-size') {
            const value = Number(argv[index + 1])
            if (!Number.isInteger(value) || value <= 0)
                throw new Error('Missing or invalid value for --page-size')
            pageSize = value
            index++
            continue
        }
        if (arg === '--align') {
            const value = argv[index + 1]
            if (!value || !['left', 'center', 'right', 'auto'].includes(value))
                throw new Error('Missing or invalid value for --align')
            align = /** @type {'left' | 'center' | 'right' | 'auto'} */ (value)
            index++
            continue
        }
        if (arg === '--no-pager') {
            pager = 'never'
            continue
        }
        if (arg === '--request') {
            const value = argv[index + 1]
            if (!value) throw new Error('Missing value for --request')
            request = value
            index++
            continue
        }
        if (arg === '--help' || arg === '-h') {
            help = true
            continue
        }
        positionals.push(arg)
    }
    return {
        positionals,
        root,
        worm,
        json,
        idOnly,
        schemaDir,
        pageSize,
        align,
        pager,
        request,
        help
    }
}

/**
 * @param {string} input
 * @returns {boolean}
 */
function isSqlCommand(input) {
    return /^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)\b/i.test(input.trim())
}

/**
 * @param {string} command
 * @param {unknown} result
 */
function renderSqlResult(command, result) {
    switch (command.toUpperCase()) {
        case 'CREATE':
            return 'Successfully created schema'
        case 'DROP':
            return 'Successfully dropped schema'
        case 'SELECT':
            if (typeof result === 'object' && result !== null && !Array.isArray(result))
                return renderTableOutput(result, cliRuntimeOptions.tableOptions)
            return String(result)
        case 'INSERT':
            return String(result)
        case 'UPDATE':
            return `Successfully updated ${result} document(s)`
        case 'DELETE':
            return `Successfully deleted ${result} document(s)`
        default:
            throw new Error(`Invalid SQL operation: ${command}`)
    }
}

/**
 * Mutable runtime formatting options shared by command handlers for one CLI
 * invocation.
 */
class CliRuntimeOptions {
    /** @type {FormatTableOptions} */
    tableOptions = {}
    /** @type {PagerMode} */
    pagerMode = 'auto'

    /** @param {ParsedArgs} args */
    apply(args) {
        this.tableOptions = {
            cellAlign: args.align,
            pageSize: args.pageSize,
            terminalWidth: 'auto',
            wrap: true
        }
        this.pagerMode = args.pager
    }
}

const cliRuntimeOptions = new CliRuntimeOptions()

/**
 * @param {ParsedArgs} args
 */
function setTableOptions(args) {
    cliRuntimeOptions.apply(args)
}

/**
 * @param {string} sql
 * @param {string | undefined} root
 */
async function runSql(sql, root) {
    const result = await new Fylo(root ? { root } : {}).executeSQL(sql)
    const operation = sql.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)/i)?.[0]
    if (!operation) throw new Error('Missing SQL operation')
    return renderSqlResult(operation, result)
}

/**
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @returns {Fylo}
 */
function createFylo(root, worm = false) {
    return new Fylo({
        ...(root ? { root } : {}),
        ...(worm ? { worm: { mode: 'append-only' } } : {})
    })
}

/**
 * @param {unknown} value
 */
function printJson(value) {
    console.log(JSON.stringify(value, null, 2))
}

/**
 * @param {string | undefined} source
 * @returns {Promise<Record<string, any>>}
 */
async function loadJsonObject(source) {
    if (!source) throw new Error('Missing JSON document input')
    let text
    if (source === '-') {
        if (process.stdin.isTTY) throw new Error('JSON document requires <json|@path|-> input')
        const chunks = []
        for await (const chunk of process.stdin) {
            chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : Buffer.from(chunk))
        }
        text = Buffer.concat(chunks).toString('utf8')
    } else if (source.startsWith('@')) {
        text = await Bun.file(path.resolve(source.slice(1))).text()
    } else {
        text = source
    }
    const value = JSON.parse(text)
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error('JSON document input must be an object')
    }
    return /** @type {Record<string, any>} */ (value)
}

/**
 * @param {string | undefined} request
 * @param {{ root?: string, worm?: boolean }} overrides
 * @returns {Promise<boolean>}
 */
async function runMachineExec(request, overrides) {
    const response = await runMachineRequestSource(request, overrides)
    process.stdout.write(`${JSON.stringify(response)}\n`)
    return response.ok
}

/**
 * @param {string} collection
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @param {boolean=} json
 */
async function runInspect(collection, root, worm = false, json = false) {
    const result = await createFylo(root, worm).inspectCollection(collection)
    if (json) {
        printJson(result)
        return undefined
    }
    return [
        `Collection ${result.collection}`,
        `Exists: ${result.exists ? 'yes' : 'no'}`,
        `WORM mode: ${result.worm ? 'enabled' : 'disabled'}`,
        `Stored documents: ${result.docsStored}`,
        `Indexed documents: ${result.indexedDocs}`,
        `Head files: ${result.headFiles}`,
        `Active heads: ${result.activeHeads}`,
        `Deleted heads: ${result.deletedHeads}`,
        `Version metadata files: ${result.versionMetas}`
    ].join('\n')
}

/**
 * @param {string} collection
 * @param {TTID} docId
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @param {boolean=} json
 */
async function runGet(collection, docId, root, worm = false, json = false) {
    const result = await createFylo(root, worm).getDoc(collection, docId).once()
    if (Object.keys(result).length === 0) throw new Error(`Document not found: ${docId}`)
    if (json) {
        printJson(result)
        return undefined
    }
    return renderTableOutput(result, cliRuntimeOptions.tableOptions)
}

/**
 * @param {string} collection
 * @param {TTID} docId
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @param {boolean=} json
 * @param {boolean=} idOnly
 * @returns {Promise<string | undefined>}
 */
async function runLatest(collection, docId, root, worm = false, json = false, idOnly = false) {
    const fylo = createFylo(root, worm)
    if (idOnly) {
        const latestId = await fylo.getLatest(collection, docId, true)
        if (!latestId) throw new Error(`No active head found for ${docId}`)
        if (json) {
            printJson({ id: latestId })
            return undefined
        }
        return String(latestId)
    }
    const result = /** @type {Record<string, any>} */ (await fylo.getLatest(collection, docId))
    if (Object.keys(result).length === 0) throw new Error(`No active head found for ${docId}`)
    if (json) {
        printJson(result)
        return undefined
    }
    return renderTableOutput(result, cliRuntimeOptions.tableOptions)
}

/**
 * @param {string} collection
 * @param {TTID} docId
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @param {boolean=} json
 */
async function runHistory(collection, docId, root, worm = false, json = false) {
    const history = await createFylo(root, worm).getHistory(collection, docId)
    if (json) {
        printJson(history)
        return undefined
    }
    if (history.length === 0) {
        return `No history found for ${docId}`
    }
    const blocks = []
    for (const entry of history) {
        blocks.push(
            [
                `${entry.id}${entry.isHead ? ' [head]' : ''}${entry.deleted ? ' [deleted]' : ''}`,
                `  lineage: ${entry.lineageId}`,
                `  previous: ${entry.previousVersionId ?? 'none'}`,
                `  updatedAt: ${entry.updatedAt}`,
                entry.deletedAt ? `  deletedAt: ${entry.deletedAt}` : undefined,
                renderTableOutput({ [entry.id]: entry.data }, cliRuntimeOptions.tableOptions)
            ]
                .filter(Boolean)
                .join('\n')
        )
    }
    return blocks.join('\n\n')
}

/**
 * @param {string} collection
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @param {boolean=} json
 */
async function runRebuild(collection, root, worm = false, json = false) {
    const result = await createFylo(root, worm).rebuildCollection(collection)
    if (json) {
        printJson(result)
        return undefined
    }
    return [
        `Rebuilt collection ${result.collection}`,
        `WORM mode: ${result.worm ? 'enabled' : 'disabled'}`,
        `Documents scanned: ${result.docsScanned}`,
        `Indexed documents: ${result.indexedDocs}`,
        result.worm ? `Heads rebuilt: ${result.headsRebuilt}` : undefined,
        result.worm ? `Version metadata rebuilt: ${result.versionMetasRebuilt}` : undefined,
        result.worm ? `Stale heads removed: ${result.staleHeadsRemoved}` : undefined,
        result.worm
            ? `Stale version metadata removed: ${result.staleVersionMetasRemoved}`
            : undefined
    ]
        .filter(Boolean)
        .join('\n')
}

/**
 * @param {'inspect' | 'current' | 'history' | 'doctor' | 'validate' | 'materialize'} action
 * @param {string} collection
 * @param {string | undefined} input
 * @param {string | undefined} schemaDir
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runSchema(action, collection, input, schemaDir, json = false) {
    if (action === 'inspect') {
        const result = await inspectSchema(collection, schemaDir)
        if (json) {
            printJson(result)
            return undefined
        }
        return [
            `Schema ${result.collection}`,
            `Schema dir: ${result.schemaDir}`,
            `Versioned: ${result.versioned ? 'yes' : 'no'}`,
            `Current: ${result.current ?? 'none'}`,
            `Manifest: ${result.manifestPath}`,
            `Versions: ${result.versions.length}`
        ].join('\n')
    }
    if (action === 'current') {
        const result = await inspectSchema(collection, schemaDir)
        const current = result.current ?? ''
        if (json) {
            printJson({ collection: result.collection, schemaDir: result.schemaDir, current })
            return undefined
        }
        return current || `No current schema version for ${collection}`
    }
    if (action === 'history') {
        const result = await inspectSchema(collection, schemaDir)
        if (json) {
            printJson(result.versions)
            return undefined
        }
        if (result.versions.length === 0) return `No schema history found for ${collection}`
        return result.versions
            .map((version) =>
                [
                    `${version.version}${version.current ? ' [current]' : ''}`,
                    `  addedAt: ${version.addedAt ?? 'unknown'}`,
                    `  file: ${version.path}`,
                    `  exists: ${version.exists ? 'yes' : 'no'}`,
                    version.nextVersion
                        ? `  upgrader to ${version.nextVersion}: ${
                              version.upgraderExists ? 'yes' : 'missing'
                          }`
                        : undefined
                ]
                    .filter(Boolean)
                    .join('\n')
            )
            .join('\n\n')
    }
    if (action === 'doctor') {
        const result = await doctorSchema(collection, schemaDir)
        if (json) {
            printJson(result)
            return undefined
        }
        const lines = [
            `Schema doctor ${result.collection}: ${result.ok ? 'ok' : 'failed'}`,
            `Schema dir: ${result.schemaDir}`
        ]
        if (result.issues.length) {
            lines.push('Issues:')
            for (const issue of result.issues) lines.push(`  - ${issue}`)
        }
        if (result.warnings.length) {
            lines.push('Warnings:')
            for (const warning of result.warnings) lines.push(`  - ${warning}`)
        }
        return lines.join('\n')
    }
    const document = await loadJsonObject(input)
    if (action === 'validate') {
        const result = await validateSchemaDocument(collection, document, schemaDir)
        if (json) {
            printJson(result)
            return undefined
        }
        return `Schema validation passed for ${collection} at ${result.current ?? 'unversioned'}`
    }
    const result = await materializeSchemaDocument(collection, document, schemaDir)
    if (json) {
        printJson(result)
        return undefined
    }
    return renderTableOutput({ document: result.document }, cliRuntimeOptions.tableOptions)
}
/**
 * @param {ParsedArgs} args
 * @returns {Promise<void>}
 */
async function main(args) {
    setTableOptions(args)
    if (args.help || args.positionals.length === 0) {
        console.log(usage())
        return
    }
    const [command, ...rest] = args.positionals
    if (command === 'inspect') {
        const collection = rest[0]
        if (!collection) throw new Error('Missing collection name for inspect')
        const output = await runInspect(collection, args.root, args.worm, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'get') {
        const collection = rest[0]
        const docId = rest[1]
        if (!collection) throw new Error('Missing collection name for get')
        if (!docId) throw new Error('Missing document id for get')
        const output = await runGet(collection, docId, args.root, args.worm, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'latest') {
        const collection = rest[0]
        const docId = rest[1]
        if (!collection) throw new Error('Missing collection name for latest')
        if (!docId) throw new Error('Missing document id for latest')
        const output = await runLatest(
            collection,
            docId,
            args.root,
            args.worm,
            args.json,
            args.idOnly
        )
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'history') {
        const collection = rest[0]
        const docId = rest[1]
        if (!collection) throw new Error('Missing collection name for history')
        if (!docId) throw new Error('Missing document id for history')
        const output = await runHistory(collection, docId, args.root, args.worm, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'rebuild') {
        const collection = rest[0]
        if (!collection) throw new Error('Missing collection name for rebuild')
        const output = await runRebuild(collection, args.root, args.worm, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'schema') {
        const action = rest[0]
        const collection = rest[1]
        const input = rest[2]
        if (
            !['inspect', 'current', 'history', 'doctor', 'validate', 'materialize'].includes(
                action ?? ''
            )
        ) {
            throw new Error('Missing or invalid schema command')
        }
        if (!collection) throw new Error('Missing collection name for schema command')
        const output = await runSchema(
            /** @type {'inspect' | 'current' | 'history' | 'doctor' | 'validate' | 'materialize'} */ (
                action
            ),
            collection,
            input,
            args.schemaDir,
            args.json
        )
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'exec') {
        const ok = await runMachineExec(args.request, { root: args.root, worm: args.worm })
        if (!ok) process.exitCode = 1
        return
    }
    if (command === 'sql') {
        const sql = rest.join(' ').trim()
        if (!sql) throw new Error('Missing SQL statement')
        const output = await runSql(sql, args.root)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    const sql = args.positionals.join(' ').trim()
    if (!isSqlCommand(sql)) {
        console.error(usage())
        throw new Error(`Unknown command: ${command}`)
    }
    const output = await runSql(sql, args.root)
    if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
}
const cliArgs = parseArgs(process.argv.slice(2))
try {
    await main(cliArgs)
} catch (error) {
    console.error(/** @type {Error} */ (error).message)
    process.exit(1)
}
