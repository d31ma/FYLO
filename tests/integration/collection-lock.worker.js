import Fylo from '../../src/index.js'

const [, , root, collection, prefix, countStr] = process.argv
const count = Number(countStr)
const fylo = new Fylo(root)
await fylo.createCollection(collection)
for (let i = 0; i < count; i++) {
    await fylo.putData(collection, { title: `${prefix}-${i}`, prefix, i })
}
