import { mkdir, open } from 'node:fs/promises'
import { join } from 'node:path'

export class SmbObservationJournal {
    static sourceUrl = import.meta.url

    constructor(directory, maximumBytes, deadline = Date.now() + 420000) {
        this.directory = directory
        this.maximumBytes = maximumBytes
        this.bytes = 0
        this.incomplete = false
        this.queue = Promise.resolve()
        this.deadline = deadline
        this.budgetMs = Math.max(0, deadline - Date.now())
        this.startedMonotonic = process.hrtime.bigint()
    }

    async initialize(manifest) {
        return this.#initialize(manifest)
    }

    record(event) {
        return this.#record(event)
    }

    async export() {
        return this.#export()
    }

    async #initialize(manifest) {
        await this.#io(async () => {
            await mkdir(this.directory, { mode: 0o700 })
            this.file = await open(join(this.directory, 'events.jsonl'), 'wx', 0o600)
        })
        await this.record({ type: 'manifest', observedAt: new Date().toISOString(), manifest })
    }

    #record(event) {
        const line = JSON.stringify(event) + '\n'
        const bytes = Buffer.byteLength(line)
        if (this.bytes + bytes > this.maximumBytes - 4096) {
            this.incomplete = true
            throw new Error('Evidence ceiling exceeded; bundle incomplete, retain fixture')
        }
        this.bytes += bytes
        this.queue = this.queue
            .then(() =>
                this.#io(async () => {
                    await this.file.writeFile(line)
                    await this.file.sync()
                })
            )
            .catch((error) => {
                this.incomplete = true
                throw error
            })
        return this.queue
    }

    async #export() {
        await this.queue
        const complete = !this.incomplete
        const receipt =
            JSON.stringify({ complete, bytes: this.bytes, exportedAt: new Date().toISOString() }) +
            '\n'
        await this.#io(async () => {
            await this.file.writeFile(receipt)
            await this.file.sync()
            const exportFile = await open(join(this.directory, 'export.json'), 'wx', 0o600)
            try {
                await exportFile.writeFile(receipt)
                await exportFile.sync()
            } finally {
                await exportFile.close()
            }
            const directory = await open(this.directory, 'r')
            try {
                await directory.sync()
            } finally {
                await directory.close()
            }
        })
        if (!complete) throw new Error('Incomplete export; cleanup is prohibited')
        return { complete, directory: this.directory }
    }

    async #io(operation) {
        const remaining = Math.min(
            5000,
            this.budgetMs - Number(process.hrtime.bigint() - this.startedMonotonic) / 1e6
        )
        if (remaining <= 0) throw new Error('Local evidence I/O deadline exhausted; retain fixture')
        let timer
        try {
            return await Promise.race([
                operation(),
                new Promise((_, reject) => {
                    timer = setTimeout(
                        () =>
                            reject(
                                new Error(
                                    'Local evidence I/O timeout; completion unverified, retain fixture'
                                )
                            ),
                        remaining
                    )
                })
            ])
        } catch (error) {
            this.incomplete = true
            throw error
        } finally {
            clearTimeout(timer)
        }
    }
}
