import { createHash, randomUUID } from 'node:crypto'
import { mkdtempSync, rmSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { Database } from 'bun:sqlite'
import { encodedJsonBytes } from './protocol.js'

export const DEFAULT_QUERY_PAGE_ITEMS = 256
export const MAX_QUERY_PAGE_ITEMS = 4096
export const QUERY_CURSOR_TTL_MS = 15 * 60 * 1000
export const MAX_QUERY_SNAPSHOT_BYTES = 1024 * 1024 * 1024

export class MachineCursorError extends Error {
    /** @type {'EINVALIDCURSOR'} */
    code = 'EINVALIDCURSOR'

    /** @param {string} message */
    constructor(message) {
        super(message)
        this.name = 'MachineCursorError'
    }
}

/** @param {unknown} value @returns {unknown} */
function canonicalValue(value) {
    if (Array.isArray(value)) return value.map(canonicalValue)
    if (!value || typeof value !== 'object') return value
    return Object.fromEntries(
        Object.entries(/** @type {Record<string, unknown>} */ (value))
            .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
            .map(([key, item]) => [key, canonicalValue(item)])
    )
}

/** @param {unknown} value */
function digest(value) {
    return createHash('sha256')
        .update(JSON.stringify(canonicalValue(value)))
        .digest('base64url')
}

/**
 * @param {{ op: string, collection: string, query: Record<string, any>, access?: Record<string, any> }} input
 */
export function queryCursorScope(input) {
    const access = input.access
        ? {
              ...input.access,
              ...(Array.isArray(input.access.groups)
                  ? { groups: [...input.access.groups].sort((left, right) => left - right) }
                  : {})
          }
        : null
    return digest({
        version: 1,
        op: input.op,
        collection: input.collection,
        query: input.query,
        access
    })
}

/**
 * @param {unknown} value
 * @returns {Array<[string, unknown]>}
 */
function entries(value) {
    if (Array.isArray(value)) return value.map((id) => [String(id), String(id)])
    if (value && typeof value === 'object') {
        return Object.entries(/** @type {Record<string, unknown>} */ (value))
    }
    if (value === undefined || value === null) return []
    const id = String(value)
    return [[id, id]]
}

/**
 * @typedef {object} QueryCursorState
 * @property {string} scope
 * @property {number} expiresAt
 * @property {Database} database
 * @property {string} directory
 * @property {string} filename
 * @property {string} lastId
 */

/** @param {QueryCursorState} state */
function closeState(state) {
    try {
        state.database.close(false)
    } finally {
        rmSync(state.directory, { recursive: true, force: true })
    }
}

/** @param {Map<string, QueryCursorState>} cursors */
export function closeQueryCursorStates(cursors) {
    for (const state of cursors.values()) closeState(state)
    cursors.clear()
}

/**
 * Materialize a stable query snapshot into a private SQLite spool. This keeps
 * heap use bounded while providing deterministic TTID ordering and continuation
 * semantics even when the engine evaluates source files concurrently.
 *
 * @param {AsyncIterable<unknown>} values
 * @param {string} scope
 * @param {number} expiresAt
 */
async function createState(values, scope, expiresAt) {
    const directory = mkdtempSync(path.join(os.tmpdir(), `fylo-query-${process.pid}-`))
    const filename = path.join(directory, 'snapshot.sqlite')
    const database = new Database(filename, { create: true, strict: true })
    database.run('PRAGMA journal_mode = WAL')
    database.run('PRAGMA synchronous = NORMAL')
    database.run('CREATE TABLE result (id TEXT PRIMARY KEY, payload TEXT NOT NULL) WITHOUT ROWID')
    const insert = database.prepare(
        'INSERT INTO result (id, payload) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET payload = excluded.payload'
    )
    let materializedBytes = 0
    try {
        for await (const value of values) {
            for (const [id, item] of entries(value)) {
                const payload = JSON.stringify(item)
                materializedBytes += Buffer.byteLength(id) + Buffer.byteLength(payload)
                if (materializedBytes > MAX_QUERY_SNAPSHOT_BYTES) {
                    const error = /** @type {RangeError & { code?: string }} */ (
                        new RangeError(
                            `Machine query snapshot exceeds ${MAX_QUERY_SNAPSHOT_BYTES} bytes`
                        )
                    )
                    error.code = 'EQUERYSNAPSHOTTOOLARGE'
                    throw error
                }
                insert.run(id, payload)
            }
        }
        database.run('PRAGMA wal_checkpoint(TRUNCATE)')
        insert.finalize()
        return /** @type {QueryCursorState} */ ({
            scope,
            expiresAt,
            database,
            directory,
            filename,
            lastId: ''
        })
    } catch (error) {
        insert.finalize()
        closeState({ scope, expiresAt, database, directory, filename, lastId: '' })
        throw error
    }
}

/**
 * Process-bound cursors retain a disk-backed immutable query snapshot. A
 * process restart invalidates the opaque token; clients then restart from page
 * one. Expiration eagerly removes the private spool.
 *
 * @param {Map<string, QueryCursorState>} cursors
 * @param {AsyncIterable<unknown> | undefined} values supplied only for a first page
 * @param {{
 *   onlyIds: boolean,
 *   scope: string,
 *   cursor?: string,
 *   limit?: number,
 *   maxResponseBytes: number,
 *   now?: number
 * }} options
 */
export async function collectQueryPage(cursors, values, options) {
    const limit = options.limit ?? DEFAULT_QUERY_PAGE_ITEMS
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > MAX_QUERY_PAGE_ITEMS) {
        const error = /** @type {RangeError & { code: string }} */ (
            new RangeError(
                `Machine query page.limit must be an integer from 1 to ${MAX_QUERY_PAGE_ITEMS}`
            )
        )
        error.code = 'EBADREQUEST'
        throw error
    }
    const now = options.now ?? Date.now()
    for (const [candidateToken, candidate] of cursors) {
        if (candidate.expiresAt > now) continue
        cursors.delete(candidateToken)
        closeState(candidate)
    }

    let token = options.cursor
    /** @type {QueryCursorState} */
    let state
    if (token) {
        const existing = cursors.get(token)
        if (!existing) {
            throw new MachineCursorError(
                'Machine query cursor is invalid, expired, or belongs to another process'
            )
        }
        if (existing.scope !== options.scope) {
            throw new MachineCursorError('Machine query cursor does not match this query scope')
        }
        state = existing
    } else {
        if (!values) throw new MachineCursorError('Machine query cursor is invalid')
        token = randomUUID()
        state = await createState(values, options.scope, now + QUERY_CURSOR_TTL_MS)
        cursors.set(token, state)
    }

    const rows = state.database
        .query('SELECT id, payload FROM result WHERE id > ? ORDER BY id LIMIT ?')
        .all(state.lastId, limit + 1)
    /** @type {string[] | Record<string, unknown>} */
    const items = options.onlyIds ? [] : {}
    let count = 0
    const responseBudget = Math.max(256, options.maxResponseBytes - 768)
    for (const row of rows.slice(0, limit)) {
        const id = String(row.id)
        const item = JSON.parse(String(row.payload))
        if (options.onlyIds) /** @type {string[]} */ (items).push(id)
        else /** @type {Record<string, unknown>} */ (items)[id] = item
        if (
            encodedJsonBytes({
                items,
                nextCursor: token,
                page: { count: count + 1, limit }
            }).byteLength > responseBudget
        ) {
            if (options.onlyIds) /** @type {string[]} */ (items).pop()
            else delete (/** @type {Record<string, unknown>} */ (items)[id])
            if (count === 0) {
                cursors.delete(token)
                closeState(state)
                const error = /** @type {RangeError & { code?: string }} */ (
                    new RangeError(
                        `Machine query item ${id} cannot fit within the negotiated response frame`
                    )
                )
                error.code = 'EQUERYITEMTOOLARGE'
                throw error
            }
            break
        }
        state.lastId = id
        count++
    }

    const hasMore =
        rows.length > count &&
        state.database
            .query('SELECT 1 AS present FROM result WHERE id > ? ORDER BY id LIMIT 1')
            .get(state.lastId) != null
    if (!hasMore) {
        cursors.delete(token)
        closeState(state)
    }
    return {
        items,
        nextCursor: hasMore ? token : null,
        page: { count, limit }
    }
}
