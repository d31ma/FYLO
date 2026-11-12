import { spawn } from 'node:child_process'

/**
 * One `xcrun simctl` call with a time limit. A call that passes its limit, or
 * that `cancel` ends, stops its own process by its saved id and fails: on
 * 2026-11-12 a `bootstatus -b` wait never returned after its device was shut
 * down, and the test run hung with it.
 */
export class SimctlCall {
    static DEFAULT_LIMIT_MS = 2 * 60_000
    static MILLISECONDS_PER_SECOND = 1_000

    /**
     * @param {string[]} args
     * @param {number} [limitMs]
     */
    constructor(args, limitMs = SimctlCall.DEFAULT_LIMIT_MS) {
        /** @type {string[]} */
        this.args = args
        /** @type {number} */
        this.limitMs = limitMs
        /** @type {import('node:child_process').ChildProcess | undefined} */
        this.child = undefined
        /** @type {(error: Error) => void} */
        this.fail = () => undefined
    }

    /** @returns {Promise<string>} The call's standard output. */
    run() {
        return new Promise((resolve, reject) => {
            this.fail = reject
            this.child = spawn('xcrun', ['simctl', ...this.args], {
                stdio: ['ignore', 'pipe', 'inherit']
            })
            const timer = setTimeout(() => this.cancel(this.limitText()), this.limitMs)
            timer.unref()
            let output = ''
            this.child.stdout.on('data', (chunk) => (output += chunk))
            this.child.once('error', reject)
            this.child.once('close', (code) => {
                clearTimeout(timer)
                this.settle(code, output, resolve)
            })
        })
    }

    /**
     * Stop the call's process and fail the call; does nothing to a call that
     * has already finished.
     * @param {string} reason
     */
    cancel(reason) {
        this.stopChild()
        this.fail(new Error(`simctl ${this.args[0]} ${reason}`))
    }

    /** @returns {string} */
    limitText() {
        return `did not finish within ${this.limitMs / SimctlCall.MILLISECONDS_PER_SECOND} s`
    }

    stopChild() {
        if (this.child === undefined || this.child.exitCode !== null) return
        this.child.kill('SIGTERM')
    }

    /**
     * @param {number | null} code
     * @param {string} output
     * @param {(output: string) => void} resolve
     */
    settle(code, output, resolve) {
        if (code === 0) return resolve(output)
        this.fail(new Error(`simctl ${this.args[0]} exited with ${code}`))
    }
}
