import { mkdir, mkdtemp } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

export async function createTestRoot(prefix = 'fylo-test-') {
    const root = await mkdtemp(path.join(os.tmpdir(), prefix))
    await mkdir(path.join(root, '.collections'), { recursive: true })
    return root
}
