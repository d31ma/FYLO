import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import { VersionRepository } from '../../src/versioning/repository.js'

const [, , root, commitId, killPhase] = process.argv
const repository = new VersionRepository(root, {
    async onMaterializationPhase(phase) {
        if (phase !== killPhase) return
        await writeFile(path.join(root, '.vcs-kill-ready'), phase)
        await new Promise(() => {})
    }
})

await repository.restoreCommit(commitId, { force: true })
