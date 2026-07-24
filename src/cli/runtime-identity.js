import packageManifest from '../../package.json' with { type: 'json' }
import { MACHINE_PROTOCOL_VERSION, normalizeMachineFrameLimits } from './protocol.js'
import {
    DEFAULT_QUERY_PAGE_ITEMS,
    MAX_QUERY_PAGE_ITEMS,
    MAX_QUERY_SNAPSHOT_BYTES,
    QUERY_CURSOR_TTL_MS
} from './query-page.js'

export const REQUIRED_VENDOR_VERSIONS = Object.freeze({
    chex: '26.28.02',
    ttid: '26.28.02'
})

const compiledCommit = typeof FYLO_BUILD_COMMIT === 'string' ? FYLO_BUILD_COMMIT : 'unknown'
const compiledTarget = typeof FYLO_BUILD_TARGET === 'string' ? FYLO_BUILD_TARGET : null
const compiledKind = typeof FYLO_BUILD_KIND === 'string' ? FYLO_BUILD_KIND : 'development'

function runtimeTarget() {
    const platform =
        process.platform === 'darwin'
            ? 'macos'
            : process.platform === 'win32'
              ? 'windows'
              : process.platform
    return `${platform}-${process.arch}`
}

/** @param {string} command @param {string} requiredVersion */
function dependencyIdentity(command, requiredVersion) {
    return {
        requiredVersion,
        available: Boolean(Bun.which(command))
    }
}

/**
 * Build identity plus the effective machine-protocol contract. Release builds
 * replace the three FYLO_BUILD_* expressions at compile time. Source and
 * locally compiled development executions cannot silently claim a revision.
 *
 * @param {{ maxRequestBytes?: number, maxResponseBytes?: number }=} limits
 * @param {{ backupConfigured?: boolean }=} runtime
 */
export function runtimeIdentity(limits = {}, runtime = {}) {
    const frames = normalizeMachineFrameLimits(limits)
    return {
        runtimeVersion: packageManifest.version,
        protocolVersion: MACHINE_PROTOCOL_VERSION,
        commit: compiledCommit,
        buildTarget: compiledTarget ?? runtimeTarget(),
        buildKind: compiledKind,
        dependencies: {
            chex: dependencyIdentity('chex', REQUIRED_VENDOR_VERSIONS.chex),
            ttid: dependencyIdentity('ttid', REQUIRED_VENDOR_VERSIONS.ttid)
        },
        machine: {
            framing: 'ndjson',
            encoding: 'utf-8',
            delimiter: 'LF',
            delimiterCountsTowardLimit: false,
            maxRequestBytes: frames.maxRequestBytes,
            maxResponseBytes: frames.maxResponseBytes,
            duplicateKeys: 'rejected',
            truncatedFrame: 'error-and-terminate',
            malformedFrame: 'error-and-resume-at-next-LF'
        },
        capabilities: {
            handshake: true,
            exclusiveRoot: true,
            queryPagination: {
                version: 1,
                operations: ['findDocs', 'findDeletedDocs'],
                defaultItems: DEFAULT_QUERY_PAGE_ITEMS,
                maxItems: MAX_QUERY_PAGE_ITEMS,
                maxSnapshotBytes: MAX_QUERY_SNAPSHOT_BYTES,
                cursorTtlMs: QUERY_CURSOR_TTL_MS,
                ordering: 'ttid-binary-ascending',
                scope: 'persistent-process',
                restartPolicy: 'restart-from-first-page',
                mutationPolicy: 'snapshot-at-first-page'
            },
            wholeRootBackup: {
                version: 1,
                available: true,
                configured: runtime.backupConfigured === true,
                machineOperations: ['backupStatus', 'backupReconcile'],
                offlineOperations: ['backup verify', 'backup restore'],
                metadataFormat: process.platform === 'win32' ? 'fylo.ntfs.v2' : 'fylo.posix.v2'
            }
        }
    }
}
