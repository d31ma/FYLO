import { writeFile } from 'node:fs/promises'
import { tryAcquireProcessFileLock } from '../../src/storage/secure-open.js'

const [, , claimPath, readyPath] = process.argv
const release = tryAcquireProcessFileLock(claimPath)
if (!release) throw new Error('Unable to acquire takeover claim')
await writeFile(readyPath, '')
await new Promise(() => {})
