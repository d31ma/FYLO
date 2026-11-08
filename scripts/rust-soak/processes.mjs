import { spawn } from 'node:child_process'

export class SoakProcesses {
    constructor(options, signal) {
        this.options = options
        this.signal = signal
        this.children = new Map()
    }

    own(child, group = false) {
        const state = { group, exited: false, error: null, closed: null }
        state.closed = new Promise((resolve) => {
            child.once('error', (error) => {
                state.error = error
            })
            child.once('close', (code, signal) => {
                state.exited = true
                resolve({ code, signal })
            })
        })
        this.children.set(child, state)
        return child
    }

    async bounded(promise, milliseconds, label, interruptible = true) {
        let timer
        let onAbort
        try {
            return await Promise.race([
                promise,
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error(`${label} timed out`)), milliseconds)
                    if (!interruptible) return
                    onAbort = () => reject(new Error(`soak interrupted by ${this.signal.reason}`))
                    if (this.signal.aborted) onAbort()
                    else this.signal.addEventListener('abort', onAbort, { once: true })
                })
            ])
        } finally {
            clearTimeout(timer)
            if (onAbort) this.signal.removeEventListener('abort', onAbort)
        }
    }

    async request(database, request) {
        return await this.bounded(
            database.request(request),
            this.options.operationTimeoutMs,
            `${request.op} request`
        )
    }

    async close(database) {
        const child = database._proc
        const state = this.children.get(child)
        if (!state) return
        let failure
        try {
            if (!state.exited) {
                child.stdin.end()
            }
            const result = await this.bounded(
                state.closed,
                this.options.shutdownTimeoutMs,
                'native shutdown',
                false
            )
            if (state.error) throw state.error
            if (result.code !== 0 || result.signal) {
                throw new Error(`native shutdown failed (${result.code ?? result.signal})`)
            }
        } catch (error) {
            failure = error
        } finally {
            await this.stop(child)
        }
        if (failure) throw failure
    }

    async stop(child) {
        const state = this.children.get(child)
        if (!state) return
        if (state.group && child.pid) await this.stopGroup(child, state)
        if (!state.exited) {
            if (!state.group) {
                child.kill('SIGKILL')
            }
            await this.bounded(state.closed, this.options.shutdownTimeoutMs, 'child reap', false)
        }
        this.children.delete(child)
    }

    async stopGroup(child, state) {
        try {
            if (process.platform !== 'win32') {
                process.kill(-child.pid, 'SIGKILL')
                return
            }
            if (state.exited) return
            await this.command(['taskkill', '/PID', String(child.pid), '/T', '/F'], {
                timeoutMs: this.options.shutdownTimeoutMs,
                label: 'owned Windows process-tree termination',
                interruptible: false,
                group: false
            })
        } catch (error) {
            if (error.code !== 'ESRCH' && !state.exited) throw error
        }
    }

    async stopAll() {
        const errors = []
        for (const child of this.children.keys()) {
            try {
                await this.stop(child)
            } catch (error) {
                errors.push(error)
            }
        }
        if (errors.length) throw new AggregateError(errors, 'soak child cleanup failed')
    }

    async command(
        arguments_,
        { capture = false, timeoutMs, label, interruptible = true, group = true }
    ) {
        const child = this.own(
            spawn(arguments_[0], arguments_.slice(1), {
                cwd: process.cwd(),
                env: process.env,
                stdio: ['ignore', capture ? 'pipe' : 'inherit', 'inherit'],
                detached: group && process.platform !== 'win32'
            }),
            group
        )
        let output = ''
        if (capture) {
            child.stdout.on('data', (chunk) => {
                output += chunk
            })
        }
        try {
            const state = this.children.get(child)
            const result = await this.bounded(state.closed, timeoutMs, label, interruptible)
            if (state.error) throw state.error
            if (result.code !== 0) throw new Error(`${label} failed (${result.code})`)
            return output
        } finally {
            await this.stop(child)
        }
    }
}
