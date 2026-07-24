import { describe, expect, test } from 'bun:test'
import { readFile } from 'node:fs/promises'
import path from 'node:path'

const root = path.resolve(import.meta.dir, '../..')

describe('release recovery and supply-chain gates', () => {
    test('blocks release assets on the reusable compiled-binary live S3 gate', async () => {
        const publish = await readFile(path.join(root, '.github/workflows/publish.yml'), 'utf8')
        const live = await readFile(path.join(root, '.github/workflows/s3-live.yml'), 'utf8')

        expect(publish).toContain('uses: ./.github/workflows/s3-live.yml')
        expect(publish).toContain('needs.live-s3.result ==')
        expect(publish).toContain('needs.macos-storage.result ==')
        expect(live).toContain('workflow_dispatch:')
        expect(live).toContain('FYLO_REQUIRE_LIVE_S3')
        expect(live).toContain('tests/interop/s3-live-binary.test.js')
        expect(live).toContain('RELEASE.2025-10-15T17-29-55Z')
        expect(live).toContain('retention-days: 90')
    })

    test('generates, attests, and verifies an SPDX SBOM before assets are uploaded', async () => {
        const workflow = await readFile(path.join(root, '.github/workflows/publish.yml'), 'utf8')
        const sbom = workflow.indexOf(
            'anchore/sbom-action@e22c389904149dbc22b58101806040fa8d37a610'
        )
        const provenance = workflow.indexOf(
            'actions/attest@f7c74d28b9d84cb8768d0b8ca14a4bac6ef463e6'
        )
        const verify = workflow.indexOf('gh attestation verify')
        const upload = workflow.indexOf('name: Upload verified release assets')

        expect(workflow).toContain('id-token: write')
        expect(workflow).toContain('attestations: write')
        expect(workflow).toContain('artifact-metadata: write')
        expect(workflow).toContain('syft-version: v1.49.0')
        expect(workflow).toContain('fylo-${VERSION}.spdx.json')
        expect(workflow).toContain('Verify exact native Linux release identity')
        expect(workflow).toContain('native-release-root-lease.test.js')
        expect(workflow).toContain('macos-15-intel')
        expect(workflow).toContain('.buildKind == "release"')
        expect(sbom).toBeGreaterThan(0)
        expect(provenance).toBeGreaterThan(sbom)
        expect(verify).toBeGreaterThan(provenance)
        expect(upload).toBeGreaterThan(verify)
    })

    test('keeps the Windows backup capability behind native NTFS tests', async () => {
        for (const name of ['ci.yml', 'publish.yml']) {
            const workflow = await readFile(path.join(root, '.github/workflows', name), 'utf8')
            expect(workflow).toContain('tests/integration/s3-backup.test.js')
            expect(workflow).toContain('tests/integration/s3-restore.test.js')
            expect(workflow).toContain('tests/interop/windows-native-binary.test.js')
        }
    })
})
