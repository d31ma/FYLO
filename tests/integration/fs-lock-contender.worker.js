import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tryAcquireFileLock } from '../../src/storage/fs-lock.js'

const [, , lockPath, owner, resultDirectory, releasePath] = process.argv
const acquired = await tryAcquireFileLock(lockPath, owner, 1)
await writeFile(path.join(resultDirectory, `${owner}.result`), JSON.stringify(acquired))
while (!(await Bun.file(releasePath).exists())) await Bun.sleep(5)
