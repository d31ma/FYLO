// @ts-nocheck
// CHEX client — drives the `chex` binary's persistent NDJSON loop.
//
// For JS/TS apps that consume the compiled binary instead of importing a
// package. No dependencies (node:child_process only). Requires the `chex`
// binary on PATH or an explicit path. One long-lived subprocess.
//
//   import { CHEX } from './chex.mjs'
//   const c = new CHEX()
//   const data = await c.validate('./schemas/person.schema.json', { name: 'Ada' })
//   const data2 = await c.validate('person', { name: 'Ada' }, './schemas')
//   await c.close()
//
// `validate` resolves with the validated data and rejects when it does not match
// the schema. `request(op)` is a raw escape hatch resolving the full response.
// Requests are queued: each resolves with its own response line, in order.

import { spawn } from 'node:child_process'

export class CHEX {
    /** @param {{ binary?: string }} [opts] */
    constructor(opts = {}) {
        this._proc = spawn(opts.binary ?? 'chex', ['exec', '--loop'], {
            stdio: ['pipe', 'pipe', 'inherit']
        })
        this._queue = [] // pending { resolve, reject } in request order
        this._buffer = ''
        this._proc.stdout.setEncoding('utf8')
        this._proc.stdout.on('data', (chunk) => this._onData(chunk))
        this._proc.on('exit', () => {
            const err = new Error('chex process exited')
            for (const p of this._queue.splice(0)) p.reject(err)
        })
        // Surface spawn failures (e.g. binary missing) instead of crashing on an
        // unhandled 'error' event.
        this._proc.on('error', (err) => {
            for (const p of this._queue.splice(0)) p.reject(err)
        })
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
            if (this._proc.exitCode !== null) return reject(new Error('chex process exited'))
            this._queue.push({ resolve, reject })
            this._proc.stdin.write(JSON.stringify(op) + '\n')
        })
    }

    async _op(op, fields) {
        const payload = { op }
        for (const [key, value] of Object.entries(fields)) {
            if (value !== undefined) payload[key] = value
        }
        const response = await this.request(payload)
        if (!response.ok) throw new Error(response.error?.message ?? 'chex error')
        return response.result
    }

    /** Validate data against a schema (name or .schema.json path). Returns the validated data. */
    validate(schema, data, schemaDir) {
        return this._op('validate', { schema, data, schemaDir })
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