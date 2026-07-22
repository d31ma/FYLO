import Fylo from '../../src/index.js'
import { writeFile } from 'node:fs/promises'

const [, , root, collection, countStr, readyPath] = process.argv
const count = Number(countStr)
const fylo = new Fylo(root)
await fylo[collection].create()
for (let i = 0; i < count; i++) {
    await fylo[collection].put({ title: `k-${i}`, i })
    if (i === 0 && readyPath) await writeFile(readyPath, 'ready')
}
