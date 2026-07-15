// @ts-nocheck
// TTID client — drives the `ttid` binary's persistent NDJSON loop.
//
// For JS/TS apps that consume the compiled binary instead of importing the npm
// package. No dependencies (node:child_process only). Requires the `ttid`
// binary on PATH or an explicit path. One long-lived subprocess.
//
//   import { TTID } from './ttid.mjs'
//   const t = new TTID()
//   const id = await t.generate()                 // new id
//   const updated = await t.generate(id)          // advance it
//   const times = await t.decodeTime(updated)     // { createdAt, updatedAt }
//   const ttid = await t.isTTID(id)               // { valid, createdAt }
//   const uuid = await t.isUUID('...')            // { valid }
//   await t.close()
//
// Each method builds the request and resolves with the op's `result` (rejecting
// on failure). `request(op)` is a raw escape hatch resolving the full response.
// Requests are queued: each resolves with its own response line, in order.

import { spawn } from 'node:child_process'

export class TTID {
    /** @param {{ binary?: string }} [opts] */
    constructor(opts = {}) {
        this._proc = spawn(opts.binary ?? 'ttid', ['exec', '--loop'], {
            stdio: ['pipe', 'pipe', 'inherit']
        })
        this._queue = [] // pending { resolve, reject } in request order
        this._buffer = ''
        this._proc.stdout.setEncoding('utf8')
        this._proc.stdout.on('data', (chunk) => this._onData(chunk))
        this._proc.on('exit', () => {
            const err = new Error('ttid process exited')
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
            if (this._proc.exitCode !== null) return reject(new Error('ttid process exited'))
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
        if (!response.ok) throw new Error(response.error?.message ?? 'ttid error')
        return response.result
    }

    /** Generate a new TTID, or advance an existing one (optionally marking it deleted). */
    generate(id, del) {
        return this._op('generate', { id, delete: del })
    }
    /** Decode embedded timestamps → { createdAt, updatedAt?, deletedAt? }. */
    decodeTime(id) {
        return this._op('decodeTime', { id })
    }
    /** Validate a TTID → { valid, createdAt }. */
    isTTID(id) {
        return this._op('isTTID', { id })
    }
    /** Validate a UUID → { valid }. */
    isUUID(id) {
        return this._op('isUUID', { id })
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
