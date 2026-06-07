import Fylo from '../../src/index.js'

const [, , root, collection, countStr] = process.argv
const count = Number(countStr)
const fylo = new Fylo(root)
await fylo[collection].create()
for (let i = 0; i < count; i++) {
    await fylo[collection].put({ title: `k-${i}`, i })
}
