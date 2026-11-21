import { statSync } from 'node:fs'
import { mkdir, mkdtemp } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

export function testFolder() {
    const folder = process.env.FYLO_TEST_ROOT
    if (!folder) return os.tmpdir()
    if (!statSync(folder, { throwIfNoEntry: false })?.isDirectory()) {
        throw new Error(
            `FYLO test image not attached: FYLO_TEST_ROOT names ${folder}, which is not a folder; run tests through scripts/ci/test-image.sh run`
        )
    }
    return folder
}

export async function createTestRoot(prefix = 'fylo-test-') {
    const root = await mkdtemp(path.join(testFolder(), prefix))
    await mkdir(path.join(root, '.collections'), { recursive: true })
    return root
}
