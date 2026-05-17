import { cp, mkdtemp } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

export const EXAMPLE_ROOT = path.join(process.cwd(), 'examples', 'db')

export async function createTestRoot(prefix = 'fylo-test-') {
    const root = await mkdtemp(path.join(os.tmpdir(), prefix))
    await cp(EXAMPLE_ROOT, root, { recursive: true })
    return root
}
