import { chmod, chown, lstat } from 'node:fs/promises'
import { getXattr, removeXattr, setXattr } from '../storage/xattr.js'

/** Internal marker that distinguishes protected records from open records. */
export const ACCESS_XATTR = 'user.fylo.access'
export const DEFAULT_ACCESS_MODE = 0o600
const MAX_POSIX_UID = 0xffff_fffe

/**
 * A stable permission failure for direct document/file operations.
 */
export class FyloPermissionError extends Error {
    /** @type {'EACCES'} */
    code = 'EACCES'
    /** @type {string} */
    collection
    /** @type {string} */
    docId
    /** @type {'read' | 'write'} */
    operation

    /**
     * @param {{ collection: string, docId: string, operation: 'read' | 'write' }} input
     */
    constructor(input) {
        super(`Permission denied: ${input.operation} ${input.collection}/${input.docId}`)
        this.name = 'FyloPermissionError'
        this.collection = input.collection
        this.docId = input.docId
        this.operation = input.operation
    }
}

/**
 * @typedef {object} FyloAccessInput
 * @property {number} uid
 * @property {number=} mode
 *
 * @typedef {object} FyloAccessDescriptor
 * @property {1} version
 * @property {number} uid
 * @property {number} gid
 * @property {number} mode
 *
 * @typedef {object} FyloNativeAccessState
 * @property {number} uid
 * @property {number} gid
 * @property {number} mode
 * @property {FyloAccessDescriptor | null} descriptor
 */

/**
 * @param {unknown} value
 * @param {{ allowMode: boolean }} options
 * @returns {{ uid: number, mode?: number }}
 */
export function normalizeAccessInput(value, options) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new TypeError('as() requires an object containing a numeric uid')
    }
    const input = /** @type {{ uid?: unknown, mode?: unknown }} */ (value)
    const unexpected = Object.keys(input).filter((key) => key !== 'uid' && key !== 'mode')
    if (unexpected.length > 0) {
        throw new TypeError(`as() received unsupported field: ${unexpected[0]}`)
    }
    if (
        !Number.isSafeInteger(input.uid) ||
        Number(input.uid) < 0 ||
        Number(input.uid) > MAX_POSIX_UID
    ) {
        throw new TypeError(`as().uid must be an integer between 0 and ${MAX_POSIX_UID}`)
    }
    const hasMode = Object.hasOwn(input, 'mode')
    if (hasMode && !options.allowMode) {
        throw new TypeError('mode is only supported by put(...).as({ uid, mode })')
    }
    if (
        hasMode &&
        (!Number.isSafeInteger(input.mode) || Number(input.mode) < 0 || Number(input.mode) > 0o777)
    ) {
        throw new TypeError('as().mode must be an integer between 0o000 and 0o777')
    }
    return {
        uid: Number(input.uid),
        ...(hasMode ? { mode: Number(input.mode) } : {})
    }
}

/** @param {FyloAccessDescriptor} descriptor */
function encodeDescriptor(descriptor) {
    return JSON.stringify(descriptor)
}

/**
 * @param {string} target
 * @returns {Promise<FyloAccessDescriptor | null>}
 */
export async function readAccessDescriptor(target) {
    const encoded = getXattr(target, ACCESS_XATTR)
    if (encoded === null) return null
    let parsed
    try {
        parsed = JSON.parse(new TextDecoder().decode(encoded))
    } catch {
        throw new Error(`Invalid FYLO access descriptor: ${target}`)
    }
    if (
        !parsed ||
        parsed.version !== 1 ||
        !Number.isSafeInteger(parsed.uid) ||
        parsed.uid < 0 ||
        !Number.isSafeInteger(parsed.gid) ||
        parsed.gid < 0 ||
        !Number.isSafeInteger(parsed.mode) ||
        parsed.mode < 0 ||
        parsed.mode > 0o777
    ) {
        throw new Error(`Invalid FYLO access descriptor: ${target}`)
    }
    return /** @type {FyloAccessDescriptor} */ (parsed)
}

/**
 * Captures both the Fylo descriptor and the native inode projection so an
 * atomic replacement or rollback preserves an open record's owner and mode.
 * @param {string} target
 * @returns {Promise<FyloNativeAccessState>}
 */
export async function snapshotAccessState(target) {
    const info = await lstat(target)
    return {
        uid: info.uid,
        gid: info.gid,
        mode: info.mode & 0o777,
        descriptor: await readAccessDescriptor(target)
    }
}

/**
 * Applies the portable descriptor and its native POSIX projection. The marker
 * is written before chmod so a restrictive mode cannot prevent its creation.
 *
 * @param {string} target
 * @param {{ uid: number, mode?: number, gid?: number }} access
 * @returns {Promise<FyloAccessDescriptor>}
 */
export async function applyAccessDescriptor(target, access) {
    if (process.platform === 'win32') {
        throw new Error('UID/mode access control is only supported on POSIX platforms')
    }
    const before = await lstat(target)
    if (before.isSymbolicLink() || !before.isFile()) {
        throw new Error(`Access target must be a regular, non-link file: ${target}`)
    }
    const descriptor = /** @type {FyloAccessDescriptor} */ ({
        version: 1,
        uid: access.uid,
        gid: access.gid ?? before.gid,
        mode: access.mode ?? DEFAULT_ACCESS_MODE
    })
    const previousMarker = getXattr(target, ACCESS_XATTR)
    try {
        setXattr(target, ACCESS_XATTR, encodeDescriptor(descriptor))
        await chown(target, descriptor.uid, descriptor.gid)
        await chmod(target, descriptor.mode)
        return descriptor
    } catch (error) {
        try {
            await chown(target, before.uid, before.gid)
            await chmod(target, before.mode & 0o777)
            if (previousMarker === null) removeXattr(target, ACCESS_XATTR)
            else setXattr(target, ACCESS_XATTR, previousMarker)
        } catch (rollbackError) {
            throw new AggregateError(
                [error, rollbackError],
                'Applying UID/mode failed and rollback was incomplete'
            )
        }
        throw error
    }
}

/**
 * Reapplies an existing descriptor after an atomic inode replacement.
 * @param {string} target
 * @param {FyloAccessDescriptor | null} descriptor
 */
export async function restoreAccessDescriptor(target, descriptor) {
    if (!descriptor) return
    await applyAccessDescriptor(target, descriptor)
}

/**
 * Restores a captured inode projection. Protected states use their portable
 * descriptor; open states retain native ownership without gaining a marker.
 * @param {string} target
 * @param {FyloNativeAccessState | null} state
 */
export async function restoreAccessState(target, state) {
    if (!state) return
    if (state.descriptor) {
        await applyAccessDescriptor(target, state.descriptor)
        return
    }
    removeXattr(target, ACCESS_XATTR)
    await chown(target, state.uid, state.gid)
    await chmod(target, state.mode)
}

/**
 * @param {FyloAccessDescriptor} descriptor
 * @param {number | undefined} actorUid
 * @param {'read' | 'write'} operation
 */
export function descriptorAllows(descriptor, actorUid, operation) {
    const bits = actorUid === descriptor.uid ? (descriptor.mode >> 6) & 0o7 : descriptor.mode & 0o7
    return (bits & (operation === 'read' ? 0o4 : 0o2)) !== 0
}
