import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { createInterface } from 'node:readline'

class SoakProcess {
    constructor() {
        this.mode = process.env.TEST_SOAK_MODE
        this.root = this.argument('--root')
        this.statePath = join(this.root, 'test-documents.json')
        this.documents = existsSync(this.statePath)
            ? JSON.parse(readFileSync(this.statePath, 'utf8'))
            : {}
        this.patched = false
        this.record('started')
        process.on('SIGTERM', () => {})
        this.reader = createInterface({ input: process.stdin })
        this.reader.on('line', (line) => this.receive(JSON.parse(line)))
        this.reader.on('close', () => {
            if (this.mode === 'hang-close') this.hang('close')
            else if (
                this.mode === 'delayed-final-close' &&
                this.documents['4VRNF52JPK1']?.sequence > 0
            ) {
                this.record('hung', { stage: 'final-close' })
                setTimeout(() => process.exit(0), 300)
            } else process.exit(0)
        })
    }

    argument(name) {
        return process.argv[process.argv.indexOf(name) + 1]
    }

    record(event, fields = {}) {
        appendFileSync(
            process.env.TEST_SOAK_EVENTS,
            `${JSON.stringify({ event, pid: process.pid, root: this.root, ...fields })}\n`
        )
    }

    hang(stage) {
        this.record('hung', { stage })
        setInterval(() => {}, 1000)
    }

    receive(request) {
        this.record('request', { op: request.op })
        if (request.op === 'handshake' && this.mode === 'hang-handshake') {
            this.hang('handshake')
            return
        }
        if (request.op === 'createCollection' && this.mode === 'hang-request') {
            this.hang('request')
            return
        }
        try {
            const result = this.execute(request)
            process.stdout.write(`${JSON.stringify({ ok: true, result })}\n`)
        } catch (error) {
            process.stdout.write(
                `${JSON.stringify({ ok: false, error: { code: 'ETEST', message: error.message } })}\n`
            )
        }
    }

    execute(request) {
        switch (request.op) {
            case 'handshake':
                return {
                    machine: {
                        maxRequestBytes: Number(this.argument('--max-request-bytes')),
                        maxResponseBytes: Number(this.argument('--max-response-bytes'))
                    }
                }
            case 'createCollection':
                return true
            case 'putData':
                this.documents[request.id] = request.data
                this.save()
                return request.id
            case 'patchDoc':
                Object.assign(this.documents[request.id], request.newDoc)
                this.patched = true
                this.save()
                return true
            case 'setMeta':
                writeFileSync(join(this.root, 'test-metadata.json'), JSON.stringify(request.meta))
                return { id: request.id, ...request.meta }
            case 'getMeta':
                return {
                    id: request.id,
                    ...JSON.parse(readFileSync(join(this.root, 'test-metadata.json'), 'utf8'))
                }
            case 'executeSQL':
                this.documents['4VRNF52JPK2'].active = request.sql.includes('= true')
                this.save()
                return 1
            case 'getDoc':
                return this.document(request.id)
            case 'findDocs':
                return Object.fromEntries(
                    Object.entries(this.documents).filter(
                        ([, document]) => document.sequence === request.query.$ops[0].sequence.$eq
                    )
                )
            case 'inspectCollection':
                return { docsStored: Object.keys(this.documents).length }
            default:
                throw new Error(`unsupported fixture operation ${request.op}`)
        }
    }

    document(id) {
        const document = { ...this.documents[id] }
        if (this.mode === 'stale-restart' && !this.patched && document.sequence > 0) {
            document.sequence--
        }
        return { [id]: document }
    }

    save() {
        writeFileSync(this.statePath, JSON.stringify(this.documents))
    }
}

new SoakProcess()
