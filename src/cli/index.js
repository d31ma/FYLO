#!/usr/bin/env bun
import path from 'node:path'
import os from 'node:os'
import Fylo from '../index.js'
import { runMachineRequestSource, serveStdioLoop } from './machine.js'
import { renderTableOutput, writeCliText } from './output.js'
import {
    doctorSchema,
    inspectSchema,
    materializeSchemaDocument,
    validateSchemaDocument
} from '../schema/admin.js'
import { VersionRepository } from '../versioning/repository.js'
import { runtimeIdentity } from './runtime-identity.js'
import { FyloS3Restore } from '../replication/s3-restore.js'

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
 * @property {boolean} createBranch
 * @property {boolean} force
 * @property {string | undefined} schemaDir
 * @property {number | undefined} pageSize
 * @property {'left' | 'center' | 'right' | 'auto'} align
 * @property {PagerMode} pager
 * @property {string | undefined} request
 * @property {string | undefined} message
 * @property {boolean} help
 * @property {boolean} loop
 * @property {boolean} version
 * @property {boolean} exclusiveRoot
 * @property {'text' | 'json' | undefined} output
 * @property {number | undefined} maxRequestBytes
 * @property {number | undefined} maxResponseBytes
 * @property {string | undefined} backupBucket
 * @property {string | undefined} backupPrefix
 * @property {string | undefined} backupEndpoint
 * @property {string | undefined} backupRegion
 * @property {boolean} backupAllowBucketRoot
 * @property {number | undefined} backupConcurrency
 * @property {number | undefined} backupReconcileIntervalMs
 * @property {number | undefined} backupMaxObjectBytes
 * @property {string | undefined} destination
 * @returns {string}
 */
function usage() {
    return [
        'Usage:',
        '  fylo checkout [-b] <branch> [--root <path>] [--json]',
        '  fylo branch [--root <path>] [--json]',
        '  fylo commit -m <message> [--root <path>] [--json]',
        '  fylo log [--root <path>] [--json]',
        '  fylo status [--root <path>] [--json]',
        '  fylo diff [<from>] [<to>] [--root <path>] [--json]',
        '  fylo restore-commit <commit-id> [--root <path>] [--force] [--json]',
        '  fylo merge <ref> [-m <message>] [--root <path>] [--json]',
        '  fylo version [--output json]',
        '  fylo "<SQL>"',
        '  fylo sql "<SQL>"',
        '  fylo exec --request <json|@path|-> [--root <path>] [--worm]',
        '  fylo exec --loop [--root <path>] [--worm] [--exclusive-root] [--backup-bucket <name> --backup-prefix <prefix>]',
        '  fylo backup verify --backup-bucket <name> --backup-prefix <prefix> [--json]',
        '  fylo backup restore --backup-bucket <name> --backup-prefix <prefix> --destination <new-path> [--json]',
        '  fylo inspect <collection> [--root <path>] [--worm] [--json]',
        '  fylo get <collection> <doc-id> [--root <path>] [--worm] [--json]',
        '  fylo latest <collection> <doc-id> [--root <path>] [--worm] [--json] [--id-only]',
        '  fylo rebuild <collection> [--root <path>] [--worm] [--json]',
        '  fylo verify <collection> [--root <path>] [--json]',
        '  fylo deleted <collection> [--root <path>] [--json]',
        '  fylo restore <collection> <doc-id> [--root <path>] [--json]',
        '  fylo schema inspect <collection> [--schema-dir <path>] [--json]',
        '  fylo schema current <collection> [--schema-dir <path>] [--json]',
        '  fylo schema history <collection> [--schema-dir <path>] [--json]',
        '  fylo schema doctor <collection> [--schema-dir <path>] [--json]',
        '  fylo schema validate <collection> <json|@path|-> [--schema-dir <path>] [--json]',
        '  fylo schema materialize <collection> <json|@path|-> [--schema-dir <path>] [--json]',
        '',
        'Options:',
        '  --root <path>   Override FYLO_ROOT for this command',
        '  --schema-dir <path> Override FYLO_SCHEMA for schema admin commands',
        '  --worm          Enable WORM-aware admin behavior for this command',
        '  --json          Emit machine-readable JSON output',
        '  --id-only       Return only the resolved document id for latest',
        '  -b              Create a new branch during checkout',
        '  -m, --message <v> Commit message',
        '  --force         Allow restore-commit to overwrite uncommitted changes',
        '  --page-size <n> Repeat headers every n rows in text output',
        '  --align <mode>  Cell alignment: left, center, right, or auto',
        '  --request <v>   Machine request payload, @file path, or - for stdin',
        '  --exclusive-root Acquire an exclusive crash-safe root lease for exec --loop',
        '  --max-request-bytes <n> Maximum NDJSON request bytes, excluding LF',
        '  --max-response-bytes <n> Maximum NDJSON response bytes, excluding LF',
        '  --backup-bucket <name> S3-compatible bucket for whole-root backup',
        '  --backup-prefix <prefix> Required isolated object-key prefix',
        '  --backup-endpoint <url> S3-compatible endpoint; credentials stay in AWS_*/FYLO_S3_* env',
        '  --backup-region <name> S3-compatible region',
        '  --backup-allow-bucket-root Permit destructive reconciliation at bucket root',
        '  --backup-concurrency <n> Maximum concurrent backup/restore requests',
        '  --backup-reconcile-interval-ms <n> Periodic whole-root reconcile interval',
        '  --backup-max-object-bytes <n> Maximum object accepted by verify/restore',
        '  --destination <path> New, non-existent whole-root restore destination',
        '  --output <mode> Output mode for version: text or json',
        '  --version       Print the FYLO runtime version',
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
    let createBranch = false
    let force = false
    let schemaDir
    let pageSize
    /** @type {'left' | 'center' | 'right' | 'auto'} */
    let align = 'auto'
    /** @type {PagerMode} */
    let pager = 'auto'
    let request
    let message
    let help = false
    let loop = false
    let version = false
    let exclusiveRoot = false
    /** @type {'text' | 'json' | undefined} */
    let output
    let maxRequestBytes
    let maxResponseBytes
    let backupBucket
    let backupPrefix
    let backupEndpoint
    let backupRegion
    let backupAllowBucketRoot = false
    let backupConcurrency
    let backupReconcileIntervalMs
    let backupMaxObjectBytes
    let destination
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
        if (arg === '-b') {
            createBranch = true
            continue
        }
        if (arg === '-m' || arg === '--message') {
            const value = argv[index + 1]
            if (!value) throw new Error(`Missing value for ${arg}`)
            message = value
            index++
            continue
        }
        if (arg === '--force') {
            force = true
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
        if (arg === '--loop' || arg === '--serve-stdio') {
            loop = true
            continue
        }
        if (arg === '--exclusive-root') {
            exclusiveRoot = true
            continue
        }
        if (arg === '--max-request-bytes' || arg === '--max-response-bytes') {
            const value = Number(argv[index + 1])
            if (!Number.isSafeInteger(value) || value <= 0) {
                throw new Error(`Missing or invalid value for ${arg}`)
            }
            if (arg === '--max-request-bytes') maxRequestBytes = value
            else maxResponseBytes = value
            index++
            continue
        }
        if (
            arg === '--backup-bucket' ||
            arg === '--backup-prefix' ||
            arg === '--backup-endpoint' ||
            arg === '--backup-region' ||
            arg === '--destination'
        ) {
            const value = argv[index + 1]
            if (!value) throw new Error(`Missing value for ${arg}`)
            if (arg === '--backup-bucket') backupBucket = value
            else if (arg === '--backup-prefix') backupPrefix = value
            else if (arg === '--backup-endpoint') backupEndpoint = value
            else if (arg === '--backup-region') backupRegion = value
            else destination = path.resolve(value)
            index++
            continue
        }
        if (arg === '--backup-allow-bucket-root') {
            backupAllowBucketRoot = true
            continue
        }
        if (
            arg === '--backup-concurrency' ||
            arg === '--backup-reconcile-interval-ms' ||
            arg === '--backup-max-object-bytes'
        ) {
            const value = Number(argv[index + 1])
            if (!Number.isSafeInteger(value) || value <= 0) {
                throw new Error(`Missing or invalid value for ${arg}`)
            }
            if (arg === '--backup-concurrency') backupConcurrency = value
            else if (arg === '--backup-reconcile-interval-ms') backupReconcileIntervalMs = value
            else backupMaxObjectBytes = value
            index++
            continue
        }
        if (arg === '--output') {
            const value = argv[index + 1]
            if (value !== 'text' && value !== 'json') {
                throw new Error('Missing or invalid value for --output')
            }
            output = value
            index++
            continue
        }
        if (arg === '--version') {
            version = true
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
        createBranch,
        force,
        schemaDir,
        pageSize,
        align,
        pager,
        request,
        message,
        help,
        loop,
        version,
        exclusiveRoot,
        output,
        maxRequestBytes,
        maxResponseBytes,
        backupBucket,
        backupPrefix,
        backupEndpoint,
        backupRegion,
        backupAllowBucketRoot,
        backupConcurrency,
        backupReconcileIntervalMs,
        backupMaxObjectBytes,
        destination
    }
}

/** @param {ParsedArgs} args */
function s3Options(args) {
    const configured =
        args.backupBucket !== undefined ||
        args.backupPrefix !== undefined ||
        args.backupEndpoint !== undefined ||
        args.backupRegion !== undefined ||
        args.backupAllowBucketRoot ||
        args.backupConcurrency !== undefined ||
        args.backupReconcileIntervalMs !== undefined
    if (!configured) return undefined
    if (!args.backupBucket) throw new Error('--backup-bucket is required')
    if (!args.backupPrefix && !args.backupAllowBucketRoot) {
        throw new Error(
            '--backup-prefix is required unless --backup-allow-bucket-root is explicitly set'
        )
    }
    return {
        bucket: args.backupBucket,
        prefix: args.backupPrefix,
        endpoint: args.backupEndpoint,
        region: args.backupRegion,
        allowBucketRoot: args.backupAllowBucketRoot || undefined,
        concurrency: args.backupConcurrency,
        reconcileIntervalMs: args.backupReconcileIntervalMs
    }
}

/**
 * @param {string} input
 * @returns {boolean}
 */
function isSqlCommand(input) {
    return /^(?:EXPLAIN(?:\s+ANALYZE)?\s+)?(?:SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)\b/i.test(
        input.trim()
    )
}

/**
 * @param {string} command
 * @param {unknown} result
 */
async function renderSqlResult(command, result) {
    switch (command.toUpperCase()) {
        case 'EXPLAIN':
            return JSON.stringify(result, null, 2)
        case 'CREATE':
            return 'Successfully created schema'
        case 'DROP':
            return 'Successfully dropped schema'
        case 'SELECT':
            if (typeof result === 'object' && result !== null && !Array.isArray(result))
                return await renderTableOutput(result, cliRuntimeOptions.tableOptions)
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
    const result = await createFylo(root)._sql(sql)
    const operation =
        sql.match(/^EXPLAIN\b/i)?.[0] ??
        sql.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)/i)?.[0]
    if (!operation) throw new Error('Missing SQL operation')
    return await renderSqlResult(operation, result)
}

/**
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @returns {import('../api/fylo.js').FyloCollections}
 */
function createFylo(root, worm = false) {
    return /** @type {import('../api/fylo.js').FyloCollections} */ (
        /** @type {unknown} */ (
            new Fylo(root ?? Fylo.defaultRoot(), {
                ...(worm ? { worm: { mode: 'strict' } } : {})
            })
        )
    )
}

/**
 * @param {string | undefined} root
 * @returns {VersionRepository}
 */
function createVersionRepository(root) {
    return new VersionRepository(root ?? Fylo.defaultRoot())
}

/**
 * @param {string} branch
 * @param {string | undefined} root
 * @param {boolean} createBranch
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runCheckout(branch, root, createBranch, json = false) {
    const result = await createVersionRepository(root).checkout(branch, { create: createBranch })
    if (json) {
        printJson(result)
        return undefined
    }
    return `${createBranch ? 'Created and switched to' : 'Switched to'} branch ${result.branch}`
}

/**
 * @param {string | undefined} root
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runBranch(root, json = false) {
    const result = await createVersionRepository(root).listBranches()
    if (json) {
        printJson(result)
        return undefined
    }
    return result.branches
        .map((branch) => `${branch.name === result.current ? '*' : ' '} ${branch.name}`)
        .join('\n')
}

/**
 * @param {string | undefined} root
 * @param {string | undefined} message
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runCommit(root, message, json = false) {
    if (!message) throw new Error('Missing commit message; pass -m <message>')
    const result = await createVersionRepository(root).commit(message)
    if (json) {
        printJson(result)
        return undefined
    }
    return `[${result.branch} ${result.id}] ${result.message}`
}

/**
 * @param {string | undefined} root
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runLog(root, json = false) {
    const result = await createVersionRepository(root).log()
    if (json) {
        printJson(result)
        return undefined
    }
    if (result.length === 0) return 'No commits yet'
    return result
        .map((commit) =>
            [
                `commit ${commit.id}`,
                `Branch: ${commit.branch}`,
                `Date: ${commit.createdAt}`,
                '',
                `    ${commit.message}`
            ].join('\n')
        )
        .join('\n\n')
}

/**
 * @param {string | undefined} root
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runStatus(root, json = false) {
    const result = await createVersionRepository(root).status()
    if (json) {
        printJson(result)
        return undefined
    }
    return [
        `On branch ${result.branch}`,
        `HEAD ${result.head ?? 'none'}`,
        result.clean
            ? 'Working tree clean'
            : `Working tree has ${result.diff.counts.total} change(s)`,
        renderDiffChanges(result.diff)
    ]
        .filter(Boolean)
        .join('\n')
}

/**
 * @param {string | undefined} root
 * @param {string | undefined} from
 * @param {string | undefined} to
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runDiff(root, from, to, json = false) {
    const result = await createVersionRepository(root).diff(from ?? 'HEAD', to ?? 'WORKTREE')
    if (json) {
        printJson(result)
        return undefined
    }
    return [`Diff ${result.from} -> ${result.to}`, renderDiffChanges(result) || 'No changes'].join(
        '\n'
    )
}

/**
 * @param {string | undefined} root
 * @param {string} commitId
 * @param {boolean} force
 * @param {boolean=} json
 * @returns {Promise<string | undefined>}
 */
async function runRestoreCommit(root, commitId, force, json = false) {
    const result = await createVersionRepository(root).restoreCommit(commitId, { force })
    if (json) {
        printJson(result)
        return undefined
    }
    return `Restored branch ${result.branch} to commit ${result.restored}`
}

/**
 * @param {string | undefined} root
 * @param {string} source
 * @param {string | undefined} message
 * @param {boolean=} json
 * @returns {Promise<{ output: string | undefined, merged: boolean }>}
 */
async function runMerge(root, source, message, json = false) {
    const result = await createVersionRepository(root).merge(source, { message })
    if (json) {
        printJson(result)
        return { output: undefined, merged: result.merged }
    }
    if (!result.merged) {
        return {
            output: [
                `Merge conflict while merging ${source} into ${result.branch}`,
                renderMergeConflicts(result.conflicts)
            ]
                .filter(Boolean)
                .join('\n'),
            merged: false
        }
    }
    if (result.mode === 'already-up-to-date') {
        return { output: `Already up to date with ${source}`, merged: true }
    }
    if (result.mode === 'fast-forward') {
        return { output: `Fast-forwarded ${result.branch} to ${result.head}`, merged: true }
    }
    return {
        output: `Merged ${source} into ${result.branch} as ${result.commit}`,
        merged: true
    }
}

/**
 * @param {import('../versioning/repository.js').FyloDiffResult} diff
 * @returns {string}
 */
function renderDiffChanges(diff) {
    return diff.changes
        .map(
            (change) =>
                `${change.status.padEnd(8)} ${change.collection}/${change.kind}/${change.id}`
        )
        .join('\n')
}

/**
 * @param {import('../versioning/repository.js').FyloMergeConflict[]} conflicts
 * @returns {string}
 */
function renderMergeConflicts(conflicts) {
    return conflicts
        .map((conflict) => `conflict ${conflict.collection}/${conflict.kind}/${conflict.id}`)
        .join('\n')
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
    const result = await createFylo(root, worm)[collection].inspect()
    if (json) {
        printJson(result)
        return undefined
    }
    return [
        `Collection ${result.collection}`,
        `Exists: ${result.exists ? 'yes' : 'no'}`,
        `WORM mode: ${result.worm ? 'enabled' : 'disabled'}`,
        `Stored documents: ${result.docsStored}`,
        `Deleted documents: ${result.deletedDocs}`,
        `Indexed documents: ${result.indexedDocs}`
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
    const result = await createFylo(root, worm)[collection].get(docId).once()
    if (Object.keys(result).length === 0) throw new Error(`Document not found: ${docId}`)
    if (json) {
        printJson(result)
        return undefined
    }
    return await renderTableOutput(result, cliRuntimeOptions.tableOptions)
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
        const latestId = await fylo[collection].latest(docId, true)
        if (!latestId) throw new Error(`No document found for ${docId}`)
        if (json) {
            printJson({ id: latestId })
            return undefined
        }
        return String(latestId)
    }
    const result = /** @type {Record<string, any>} */ (await fylo[collection].latest(docId))
    if (Object.keys(result).length === 0) throw new Error(`No document found for ${docId}`)
    if (json) {
        printJson(result)
        return undefined
    }
    return await renderTableOutput(result, cliRuntimeOptions.tableOptions)
}

/**
 * @param {string} collection
 * @param {string | undefined} root
 * @param {boolean=} worm
 * @param {boolean=} json
 */
async function runRebuild(collection, root, worm = false, json = false) {
    const result = await createFylo(root, worm)[collection].rebuild()
    if (json) {
        printJson(result)
        return undefined
    }
    return [
        `Rebuilt collection ${result.collection}`,
        `WORM mode: ${result.worm ? 'enabled' : 'disabled'}`,
        `Documents scanned: ${result.docsScanned}`,
        `Indexed documents: ${result.indexedDocs}`
    ]
        .filter(Boolean)
        .join('\n')
}

/**
 * Stamp-ignoring integrity audit for a file collection. Exits non-zero when
 * corruption is found so cron/CI wrappers can alert on the exit code alone.
 * @param {string} collection
 * @param {string | undefined} root
 * @param {boolean=} json
 */
async function runVerify(collection, root, json = false) {
    const result = await createFylo(root)[collection].verify()
    if (result.corrupt.length > 0) process.exitCode = 1
    if (json) {
        printJson(result)
        return undefined
    }
    const lines = [
        `Verified collection ${result.collection}`,
        `Files scanned: ${result.filesScanned}`,
        `Checksums verified: ${result.verified}`,
        `Freshly stamped: ${result.stamped}`,
        `Corrupt: ${result.corrupt.length}`
    ]
    for (const failure of result.corrupt) {
        lines.push(
            `  ${failure.id} (${failure.namespace}) expected ${failure.expected} got ${failure.actual}`
        )
    }
    return lines.join('\n')
}

/**
 * @param {string} collection
 * @param {string | undefined} root
 * @param {boolean=} json
 */
async function runDeleted(collection, root, json = false) {
    const results = {}
    for await (const result of createFylo(root)
        [collection].find.deleted({ $deleted: { $gte: 0 } })
        .collect()) {
        if (result && typeof result === 'object') Object.assign(results, result)
    }
    if (json) {
        printJson(results)
        return undefined
    }
    if (Object.keys(results).length === 0) return `No deleted documents found for ${collection}`
    return await renderTableOutput(results, cliRuntimeOptions.tableOptions)
}

/**
 * @param {string} collection
 * @param {TTID} docId
 * @param {string | undefined} root
 * @param {boolean=} json
 */
async function runRestore(collection, docId, root, json = false) {
    const id = await createFylo(root)[collection].restore(docId)
    const result = { restored: true, id }
    if (json) {
        printJson(result)
        return undefined
    }
    return `Restored document ${id}`
}

/**
 * Offline whole-root verification and restore. Credentials are resolved by
 * FyloS3Restore from the standard AWS/FYLO environment chain and are never
 * accepted in request frames or printed.
 *
 * @param {'verify' | 'restore'} action
 * @param {ParsedArgs} args
 */
async function runBackupRecovery(action, args) {
    const options = s3Options(args)
    if (!options) {
        throw new Error('Whole-root backup recovery requires --backup-bucket and --backup-prefix')
    }
    if (action === 'restore' && !args.destination) {
        throw new Error('--destination is required for backup restore')
    }
    const destination =
        args.destination ??
        path.join(os.tmpdir(), `fylo-verify-${process.pid}-${Bun.randomUUIDv7()}`)
    const recovery = new FyloS3Restore(options, destination)
    const result = await recovery[action]({
        concurrency: args.backupConcurrency,
        maxObjectBytes: args.backupMaxObjectBytes,
        onStatus(status) {
            if (args.json) return
            process.stderr.write(`${JSON.stringify(status)}\n`)
        }
    })
    if (args.json) {
        printJson(result)
        return undefined
    }
    return action === 'verify'
        ? `Verified ${result.files} backup objects (${result.bytes} bytes)`
        : `Restored ${result.files} backup objects (${result.bytes} bytes) to ${destination}`
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
    return await renderTableOutput({ document: result.document }, cliRuntimeOptions.tableOptions)
}
/**
 * @param {ParsedArgs} args
 * @returns {Promise<void>}
 */
async function main(args) {
    setTableOptions(args)
    if (args.version || args.positionals[0] === 'version') {
        const identity = runtimeIdentity({
            maxRequestBytes: args.maxRequestBytes,
            maxResponseBytes: args.maxResponseBytes
        })
        if (args.json || args.output === 'json') printJson(identity)
        else console.log(identity.runtimeVersion)
        return
    }
    if (args.help || args.positionals.length === 0) {
        console.log(usage())
        return
    }
    const [command, ...rest] = args.positionals
    if (command === 'checkout') {
        const branch = rest[0]
        if (!branch) throw new Error('Missing branch name for checkout')
        const output = await runCheckout(branch, args.root, args.createBranch, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'branch') {
        const output = await runBranch(args.root, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'commit') {
        const output = await runCommit(args.root, args.message, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'log') {
        const output = await runLog(args.root, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'status') {
        const output = await runStatus(args.root, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'diff') {
        const output = await runDiff(args.root, rest[0], rest[1], args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'restore-commit') {
        const commitId = rest[0]
        if (!commitId) throw new Error('Missing commit id for restore-commit')
        const output = await runRestoreCommit(args.root, commitId, args.force, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'merge') {
        const source = rest[0]
        if (!source) throw new Error('Missing ref for merge')
        const result = await runMerge(args.root, source, args.message, args.json)
        if (result.output)
            await writeCliText(result.output, { pagerMode: cliRuntimeOptions.pagerMode })
        if (!result.merged) process.exitCode = 1
        return
    }
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
    if (command === 'rebuild') {
        const collection = rest[0]
        if (!collection) throw new Error('Missing collection name for rebuild')
        const output = await runRebuild(collection, args.root, args.worm, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'verify') {
        const collection = rest[0]
        if (!collection) throw new Error('Missing collection name for verify')
        const output = await runVerify(collection, args.root, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'deleted') {
        const collection = rest[0]
        if (!collection) throw new Error('Missing collection name for deleted')
        const output = await runDeleted(collection, args.root, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'restore') {
        const collection = rest[0]
        const docId = rest[1]
        if (!collection) throw new Error('Missing collection name for restore')
        if (!docId) throw new Error('Missing document id for restore')
        const output = await runRestore(collection, docId, args.root, args.json)
        if (output) await writeCliText(output, { pagerMode: cliRuntimeOptions.pagerMode })
        return
    }
    if (command === 'backup') {
        const action = rest[0]
        if (action !== 'verify' && action !== 'restore') {
            throw new Error('Missing or invalid backup command; expected verify or restore')
        }
        const output = await runBackupRecovery(action, args)
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
        if (args.loop) {
            const backup = s3Options(args)
            const served = await serveStdioLoop({
                overrides: {
                    root: args.root,
                    worm: args.worm,
                    ...(backup ? { sync: { s3: backup } } : {})
                },
                frameLimits: {
                    maxRequestBytes: args.maxRequestBytes,
                    maxResponseBytes: args.maxResponseBytes
                },
                exclusiveRoot: args.exclusiveRoot
            })
            if (!served) process.exitCode = 1
            return
        }
        if (args.exclusiveRoot) {
            throw new Error('--exclusive-root requires exec --loop')
        }
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
    const failure = /** @type {Error & { code?: string }} */ (error)
    console.error(failure.code ? `${failure.code}: ${failure.message}` : failure.message)
    process.exit(1)
}
