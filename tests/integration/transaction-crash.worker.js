import Fylo from '../../src/index.js'

const [, , root, collection, killAfterArg] = process.argv
const killAfter = Number(killAfterArg ?? 1)
const fylo = new Fylo(root, { versioning: { autoCommit: false } })
await fylo.ready()

const updateDocument = fylo.engine.updateDocument.bind(fylo.engine)
let writes = 0
fylo.engine.updateDocument = async (...args) => {
    const result = await updateDocument(...args)
    writes++
    if (writes === killAfter) process.kill(process.pid, 'SIGKILL')
    return result
}

await fylo._sql(`UPDATE ${collection} SET state = 'after' WHERE state = 'before'`)
