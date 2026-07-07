// Fylo client — drives the `fylo` binary's persistent NDJSON loop.
//
// For JS/TS apps that consume the compiled binary instead of importing the npm
// package. No dependencies (node:child_process only). Requires the `fylo`
// binary on PATH (brew/scoop) or an explicit path. One long-lived subprocess
// keeps the engine warm across calls.
//
//   import { Fylo } from './fylo.mjs'
//   const db = new Fylo('/path/to/db')
//   await db.createCollection('users')
//   const id = await db.putData('users', { name: 'Ada', role: 'admin' })
//   const doc = await db.getLatest('users', id)
//   const admins = await db.findDocs('users', { $ops: [{ role: { $eq: 'admin' } }] })
//   await db.close()
//
// Each operation method builds the request and resolves with the op's `result`
// (rejecting on failure). Method names mirror the machine-protocol op names.
// `request(op)` remains a raw escape hatch resolving with the full response
// object — use it for ops without a dedicated method (branching, schema, ...).
// Requests are queued: each resolves with its own response line, in order.

import { spawn } from 'node:child_process'

// Property access that isn't a real method/field falls through to a collection
// facade, so `db.users.put(...)` works alongside `db.putData('users', ...)`.
const RESERVED = new Set(['then'])
const FACADE = {
    get(target, prop, receiver) {
        if (typeof prop === 'symbol' || RESERVED.has(prop) || prop in target) {
            return Reflect.get(target, prop, receiver)
        }
        return target.collection(String(prop))
    }
}

export class Fylo {
    /** @param {string} root @param {{ binary?: string, worm?: boolean }} [opts] */
    constructor(root, opts = {}) {
        const args = ['exec', '--loop', '--root', root]
        if (opts.worm) args.push('--worm')
        this._proc = spawn(opts.binary ?? 'fylo', args, { stdio: ['pipe', 'pipe', 'inherit'] })
        this._queue = [] // pending { resolve, reject } in request order
        this._buffer = ''
        this._proc.stdout.setEncoding('utf8')
        this._proc.stdout.on('data', (chunk) => this._onData(chunk))
        this._proc.on('exit', () => {
            const err = new Error('fylo process exited')
            for (const p of this._queue.splice(0)) p.reject(err)
        })
        // Surface spawn failures (e.g. binary missing) instead of crashing on an
        // unhandled 'error' event.
        this._proc.on('error', (err) => {
            for (const p of this._queue.splice(0)) p.reject(err)
        })
        return new Proxy(this, FACADE)
    }

    /**
     * Collection-scoped facade with short method names, so
     * `db.collection('users').put(data)` reads like the browser client. The
     * dynamic sugar `db.users.put(data)` resolves here too.
     * @param {string} name
     */
    collection(name) {
        return {
            create: (kind) => this.createCollection(name, kind),
            drop: () => this.dropCollection(name),
            inspect: () => this.inspectCollection(name),
            rebuild: () => this.rebuildCollection(name),
            put: (data) => this.putData(name, data),
            get: (id) => this.getDoc(name, id),
            latest: (id, onlyId) => this.getLatest(name, id, onlyId),
            patch: (id, newDoc, oldDoc) => this.patchDoc(name, id, newDoc, oldDoc),
            delete: (id) => this.delDoc(name, id),
            restore: (id) => this.restoreDoc(name, id),
            find: (query) => this.findDocs(name, query)
        }
    }

    _onData(chunk) {
        this._buffer += chunk
        let nl
        while ((nl = this._buffer.indexOf('\n')) !== -1) {
            const line = this._buffer.slice(0, nl).trim()
            this._buffer = this._buffer.slice(nl + 1)
            if (!line) continue
            const pending = this._queue.shift()
            if (pending) pending.resolve(JSON.parse(line))
        }
    }

    /** Send one raw machine-protocol op; resolves with the full response object. */
    request(op) {
        return new Promise((resolve, reject) => {
            if (this._proc.exitCode !== null) return reject(new Error('fylo process exited'))
            this._queue.push({ resolve, reject })
            this._proc.stdin.write(JSON.stringify(op) + '\n')
        })
    }

    /** Build an op, send it, and resolve with `result` (rejects on failure). */
    async _op(op, fields) {
        const payload = { op }
        for (const [key, value] of Object.entries(fields)) {
            if (value !== undefined) payload[key] = value
        }
        const response = await this.request(payload)
        if (!response.ok) throw new Error(response.error?.message ?? 'fylo error')
        return response.result
    }

    // --- Collections ---
    createCollection(collection, kind = 'document') {
        return this._op('createCollection', { collection, kind })
    }
    dropCollection(collection) {
        return this._op('dropCollection', { collection })
    }
    inspectCollection(collection) {
        return this._op('inspectCollection', { collection })
    }
    rebuildCollection(collection) {
        return this._op('rebuildCollection', { collection })
    }

    // --- Documents ---
    putData(collection, data) {
        return this._op('putData', { collection, data })
    }
    batchPutData(collection, batch) {
        return this._op('batchPutData', { collection, batch })
    }
    getDoc(collection, id) {
        return this._op('getDoc', { collection, id })
    }
    getLatest(collection, id, onlyId = false) {
        return this._op('getLatest', { collection, id, onlyId })
    }
    patchDoc(collection, id, newDoc, oldDoc) {
        return this._op('patchDoc', { collection, id, newDoc, oldDoc })
    }
    patchDocs(collection, update) {
        return this._op('patchDocs', { collection, update })
    }
    delDoc(collection, id) {
        return this._op('delDoc', { collection, id })
    }
    delDocs(collection, criteria) {
        return this._op('delDocs', { collection, delete: criteria })
    }
    restoreDoc(collection, id) {
        return this._op('restoreDoc', { collection, id })
    }

    // --- Query ---
    findDocs(collection, query) {
        return this._op('findDocs', { collection, query })
    }
    findDeletedDocs(collection, query = {}) {
        return this._op('findDeletedDocs', { collection, query })
    }
    joinDocs(join) {
        return this._op('joinDocs', { join })
    }
    executeSQL(sql) {
        return this._op('executeSQL', { sql })
    }
    /**
     * Tagged-template SQL — interpolated values are escaped, so
     * ``db.sql`SELECT * FROM users WHERE name = ${name}` `` is injection-safe.
     * @param {TemplateStringsArray} strings
     * @param {...unknown} values
     */
    sql(strings, ...values) {
        let statement = strings[0]
        for (let i = 0; i < values.length; i++) {
            statement += Fylo._sqlValue(values[i]) + strings[i + 1]
        }
        return this.executeSQL(statement)
    }
    /** Escape one scalar into a SQL literal. */
    static _sqlValue(value) {
        if (value === null || value === undefined) return 'NULL'
        if (typeof value === 'number') {
            if (!Number.isFinite(value)) throw new Error('SQL value must be a finite number')
            return String(value)
        }
        if (typeof value === 'boolean') return value ? 'true' : 'false'
        if (typeof value === 'bigint') return value.toString()
        if (value instanceof Date) return `'${value.toISOString().replaceAll("'", "''")}'`
        if (typeof value === 'object') throw new Error('SQL values must be scalar')
        return `'${String(value).replaceAll("'", "''")}'`
    }
    importBulkData(collection, url, limitOrOptions) {
        return this._op('importBulkData', { collection, url, limitOrOptions })
    }

    /** Close stdin so the loop ends, and wait for the process to exit. */
    close() {
        return new Promise((resolve) => {
            if (this._proc.exitCode !== null) return resolve()
            this._proc.on('exit', () => resolve())
            this._proc.stdin.end()
        })
    }
}
