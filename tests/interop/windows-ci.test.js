import { describe, expect, test } from 'bun:test'

const workflows = ['.github/workflows/ci.yml', '.github/workflows/publish.yml']

describe('native Windows release gate', () => {
    for (const workflowPath of workflows) {
        test(`${workflowPath} requires the complete Windows x64 storage contract`, async () => {
            const workflow = await Bun.file(workflowPath).text()

            expect(workflow).toContain('os: [windows-2022, windows-2025]')
            expect(workflow).toContain('bun-version-file: .bun-version')
            expect(workflow).toContain('./scripts/install-vendor-bins.ps1')
            expect(workflow).toContain("FYLO_REQUIRE_WINDOWS_NATIVE: '1'")
            expect(workflow).toContain('tests/integration/fs-lock.test.js')
            expect(workflow).toContain('tests/integration/transactions.test.js')
            expect(workflow).toContain('tests/integration/crash-recovery.test.js')
            expect(workflow).toContain('tests/integration/document-path-security.test.js')
            expect(workflow).toContain('tests/integration/secure-open.test.js')
            expect(workflow).toContain('tests/interop/windows-native-binary.test.js')
        })
    }
})

describe('release supply-chain pinning', () => {
    for (const workflowPath of [...workflows, '.github/workflows/pages.yml']) {
        test(`${workflowPath} pins every external action to a commit`, async () => {
            const workflow = await Bun.file(workflowPath).text()
            const actions = [...workflow.matchAll(/uses:\s+([^\s#]+)@([^\s#]+)/g)]

            expect(actions.length).toBeGreaterThan(0)
            for (const [, action, reference] of actions) {
                expect(reference, `${action} must use a full commit SHA`).toMatch(/^[0-9a-f]{40}$/)
            }
        })
    }

    for (const workflowPath of workflows) {
        test(`${workflowPath} uses repository-verified toolchain installers`, async () => {
            const workflow = await Bun.file(workflowPath).text()

            expect(workflow).toContain('sh ./scripts/install-vendor-bins.sh')
            expect(workflow).toContain('sh ./scripts/install-kotlin-compiler.sh')
            expect(workflow).not.toContain('releases/latest')
            expect(workflow).not.toMatch(/curl[^\n]*\|\s*(?:ba)?sh/)
        })
    }

    test('vendor installers anchor versions and digests in the repository', async () => {
        const [shell, powershell, kotlin] = await Promise.all([
            Bun.file('scripts/install-vendor-bins.sh').text(),
            Bun.file('scripts/install-vendor-bins.ps1').text(),
            Bun.file('scripts/install-kotlin-compiler.sh').text()
        ])

        for (const installer of [shell, powershell]) {
            expect(installer).toContain('v26.28.02')
            expect(installer).not.toContain('releases/latest')
            expect(installer).not.toContain('SHA256SUMS')
        }
        expect(shell).toContain('93a1bf501eb8e8ad41c19904ae1424be13b7aa6a2a5d8de12767f681a70a62f4')
        expect(powershell).toContain(
            'b4beab399741b46a82d037cfef2b298418e7596245684aed91154aee8d6771aa'
        )
        expect(kotlin).toContain("KOTLIN_VERSION='2.1.10'")
        expect(kotlin).toContain(
            "KOTLIN_SHA256='c6e9e2636889828e19c8811d5ab890862538c89dc2a3101956dfee3c2a8ba6b1'"
        )
        expect(kotlin).not.toContain('.zip.sha256')
    })
})
