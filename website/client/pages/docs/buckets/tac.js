const TITLE = 'Buckets & raw files — FYLO'
document.title = TITLE

export default class {
    basicCode = `import { Fylo } from './fylo.mjs'

const db = new Fylo('/mnt/fylo')

await db.assets.create('file')

const id = await db.assets.putFile(
    { path: '/uploads/summary.pdf' },
    { key: '/reports/2026/summary.pdf' },
    { rating: 5, starred: true }
)

const manifest = await db.getDoc('assets', id) // { [id]: manifest }, no bytes
const { result } = await db.request({ op: 'getFileData', collection: 'assets', id })
const bytes = Buffer.from(result.data, 'base64')`

    keysCode = `const exact = await db.assets.find({
    $ops: [{ key: { $eq: '/reports/2026/summary.pdf' } }]
})

const reports = await db.assets.find({
    $ops: [{ key: { $like: '/reports/%' } }]
})`

    verifyCode = `const { result: report } = await db.request({ op: 'verifyCollection', collection: 'assets' })
// { collection, filesScanned, verified, stamped,
//   corrupt: [{ id, namespace, expected, actual }] }`

    cronCode = `# Weekly integrity audit, Sunday 03:00 — the alert fires on a non-zero exit
0 3 * * 0  fylo verify assets --root /mnt/fylo --json || notify "fylo: corruption detected"`

    metaCode = `await db.setMeta('assets', id, { rating: 4, starred: null }) // update + remove
await db.getMeta('assets', id)
// { id, name, key, extension, contentType, contentLength, etag, checksumSHA256,
//   lastModified, mtime, updatedAt, createdAt, rating: 4 }`

    metaQueryCode = `await db.assets.find({ $ops: [{ ['meta/starred']: { $eq: true } }] })
await db.assets.find({ $ops: [{ ['meta/rating']: { $gte: 4 } }] })`

    pathCode = `.buckets/assets/docs/<shard>/<TTID>.<original-extension>`

    machineCode = `{
    "op": "putData",
    "root": "/mnt/fylo",
    "collection": "assets",
    "file": {
        "path": "/uploads/greeting.txt",
        "key": "/incoming/greeting.txt"
    }
}`
}
