import { spawn } from 'node:child_process'

export class SmbObservationProcess {
    static sourceUrl = import.meta.url

    constructor(journal, config, startedAt) {
        this.journal = journal
        this.config = config
        this.startedAt = startedAt
        this.startedMonotonic = process.hrtime.bigint()
        this.uncertain = false
    }

    async execute(argv, options = {}) {
        return this.#execute(argv, options)
    }

    start(argv, options = {}) {
        return this.#start(argv, options)
    }

    async #execute(argv, options) {
        const handle = this.start(argv, options)
        return handle.completion
    }

    #start(
        argv,
        {
            input = '',
            timeoutMs = this.config.limits.subprocessMs,
            holdInput = false,
            exportPhase = false
        } = {}
    ) {
        if (this.uncertain)
            throw new Error('Daemon/process outcome is uncertain; no further operations')
        const ceiling = exportPhase ? this.config.limits.totalMs : this.config.limits.laneMs
        const remaining = ceiling - Number(process.hrtime.bigint() - this.startedMonotonic) / 1e6
        const budget = Math.min(timeoutMs, remaining)
        if (budget <= 2000) throw new Error('Observation deadline lacks owned-CLI shutdown reserve')
        const state = {
            argv,
            input,
            startedAt: new Date().toISOString(),
            completedAt: null,
            exitCode: null,
            signal: null,
            timedOut: false,
            evidenceIncomplete: false,
            cliProcessClosed: false,
            cliExitUnverified: false,
            startedMonotonicNs: process.hrtime.bigint().toString(),
            stdout: '',
            stderr: '',
            error: null
        }
        const stdout = []
        const stderr = []
        let captured = 0
        let closed = false
        let child
        let timer
        const terminationTimers = []
        const completion = new Promise((resolve, reject) => {
            const finish = async () => {
                if (closed) return
                closed = true
                clearTimeout(timer)
                for (const pending of terminationTimers) clearTimeout(pending)
                state.settledAt = new Date().toISOString()
                state.settledMonotonicNs = process.hrtime.bigint().toString()
                state.completedAt = state.cliProcessClosed ? state.settledAt : null
                state.cliExitUnverified = Boolean(state.cliPid) && !state.cliProcessClosed
                state.stdout = Buffer.concat(stdout).toString('utf8')
                state.stderr = Buffer.concat(stderr).toString('utf8')
                state.stdoutBase64 = Buffer.concat(stdout).toString('base64')
                state.stderrBase64 = Buffer.concat(stderr).toString('base64')
                try {
                    await this.journal.record({ type: 'transport', transport: state })
                    resolve(state)
                } catch (error) {
                    this.uncertain = true
                    reject(error)
                }
            }
            const capture = (destination, chunk, stream) => {
                if (closed) return
                captured += chunk.length
                if (captured > Math.min(8 * 1024 * 1024, this.journal.maximumBytes / 8)) {
                    state.evidenceIncomplete = true
                    this.journal.incomplete = true
                    this.uncertain = true
                    state.error = {
                        message: 'Output ceiling exceeded; captured prefix is incomplete'
                    }
                    state.cliExitUnverified = true
                    this.#terminate(child, state, finish, () => closed, terminationTimers)
                    return
                }
                destination.push(chunk)
                if (holdInput) state[stream] += chunk.toString('utf8')
            }
            const env = { ...process.env }
            delete env.DOCKER_CONTEXT
            delete env.DOCKER_HOST
            delete env.DOCKER_TLS_VERIFY
            delete env.DOCKER_CERT_PATH
            env.GIT_OPTIONAL_LOCKS = '0'
            child = spawn(argv[0], argv.slice(1), { env, stdio: ['pipe', 'pipe', 'pipe'] })
            state.cliPid = child.pid ?? null
            timer = setTimeout(() => {
                state.timedOut = true
                state.evidenceIncomplete = true
                state.cliExitUnverified = true
                this.journal.incomplete = true
                state.error = {
                    message: 'Docker CLI deadline; daemon operation may still be running'
                }
                this.uncertain = true
                this.#terminate(child, state, finish, () => closed, terminationTimers)
            }, budget - 2000)
            child.stdout.on('data', (chunk) => capture(stdout, chunk, 'stdout'))
            child.stderr.on('data', (chunk) => capture(stderr, chunk, 'stderr'))
            child.stdin.on('error', (error) => {
                state.inputError = { name: error.name, message: error.message }
            })
            child.on('error', (error) => {
                state.error = { name: error.name, message: error.message, stack: error.stack }
                finish()
            })
            child.on('exit', (exitCode, signal) => {
                state.exitObservedAt = new Date().toISOString()
                state.exitCode = exitCode
                state.signal = signal
                if (state.localTerminationStartedAt) {
                    child.stdout.destroy()
                    child.stderr.destroy()
                }
            })
            child.on('close', (exitCode, signal) => {
                if (closed) return
                state.cliProcessClosed = true
                state.exitCode = exitCode
                state.signal = signal
                finish()
            })
            child.stdin.write(input)
            if (!holdInput) child.stdin.end()
        })
        completion.catch(() => {})
        return { state, completion, closeInput: () => child.stdin.end() }
    }

    #terminate(child, state, finish, isFinished, timers) {
        if (state.localTerminationStartedAt) return
        state.localTerminationStartedAt = new Date().toISOString()
        state.localSignals = [{ signal: 'SIGTERM', at: state.localTerminationStartedAt }]
        child.stdin.destroy()
        child.stdout.pause()
        child.stderr.pause()
        child.kill('SIGTERM')
        timers.push(
            setTimeout(() => {
                if (isFinished()) return
                state.localSignals.push({ signal: 'SIGKILL', at: new Date().toISOString() })
                child.kill('SIGKILL')
                timers.push(
                    setTimeout(() => {
                        if (isFinished()) return
                        state.cliExitUnverified = true
                        state.supervisorRequired = true
                        child.stdout.destroy()
                        child.stderr.destroy()
                        child.unref()
                        finish()
                    }, 1000)
                )
            }, 1000)
        )
    }
}
