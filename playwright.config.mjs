import { defineConfig } from '@playwright/test'

const ISOLATED_PORT = process.env.FYLO_E2E_PORT ?? '4173'
const PLAIN_PORT = process.env.FYLO_E2E_PLAIN_PORT ?? '4174'

export default defineConfig({
    testDir: './tests/browser/e2e',
    timeout: 120_000,
    expect: { timeout: 120_000 },
    fullyParallel: false,
    workers: 1,
    preserveOutput: 'always',
    reporter: [['line']],
    use: {
        baseURL: `http://127.0.0.1:${ISOLATED_PORT}`,
        trace: 'retain-on-failure'
    },
    webServer: [
        {
            // Cross-origin isolation, without which SharedArrayBuffer — and the
            // Atomics bridge OPFS needs — does not exist.
            command: `node scripts/serve-isolated.mjs ${ISOLATED_PORT} .`,
            url: `http://127.0.0.1:${ISOLATED_PORT}/tests/browser/fixtures/wasm-opfs.html`,
            reuseExistingServer: true,
            timeout: 30_000
        },
        {
            // No isolation, as a static host serves a page: the single-file
            // hosts must work without SharedArrayBuffer.
            command: `node scripts/serve-isolated.mjs ${PLAIN_PORT} . --no-isolation`,
            url: `http://127.0.0.1:${PLAIN_PORT}/tests/browser/fixtures/vdbx-single-file.html`,
            reuseExistingServer: true,
            timeout: 30_000
        }
    ],
    projects: [
        { name: 'chromium', use: { browserName: 'chromium' } },
        { name: 'firefox', use: { browserName: 'firefox' } },
        { name: 'webkit', use: { browserName: 'webkit' } }
    ]
})
