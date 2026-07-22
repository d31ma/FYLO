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
