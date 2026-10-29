// The single-file hosts of ADR 0016 section 10, on a page served the way
// GitHub Pages serves one: no COOP, no COEP, so no `SharedArrayBuffer`.

import { expect, test } from './browser-test.mjs'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'
import { STUB_SIZE, stubBytes } from '../fixtures/vdbx-stub.mjs'

const PLAIN_ORIGIN = 'http://127.0.0.1:4174'

test.beforeAll(async () => {
    test.setTimeout(600_000)
    const path = 'target/browser-fixtures/stub.vdbx'
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, stubBytes())
    // A real container, and the report the native engine gives for it. The
    // example refuses to overwrite, so a previous run's file goes first.
    await rm('target/browser-fixtures/fixture.vdbx', { force: true })
    const report = execFileSync(
        'cargo',
        [
            'run',
            '--quiet',
            '--locked',
            '-p',
            'fylo-storage-native',
            '--example',
            'vdbx_fixture',
            '--',
            'target/browser-fixtures/fixture.vdbx'
        ],
        { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }
    )
    await writeFile('target/browser-fixtures/fixture.json', report)
    // A database with documents, written through the native CLI.
    await rm('target/browser-fixtures/documents.vdbx', { force: true })
    const requests = [
        { op: 'createCollection', collection: 'users' },
        ...['ada', 'bo', 'cy', 'di'].map((name, index) => ({
            op: 'putData',
            collection: 'users',
            data: { name, team: index % 2 === 0 ? 'red' : 'blue' }
        }))
    ]
    execFileSync(
        'cargo',
        [
            'run',
            '--quiet',
            '--locked',
            '-p',
            'fylo-cli',
            '--bin',
            'fylo-rust',
            '--',
            'exec',
            '--loop',
            '--root',
            'target/browser-fixtures/documents.vdbx'
        ],
        {
            input: requests.map((request) => JSON.stringify(request)).join('\n') + '\n',
            stdio: ['pipe', 'ignore', 'inherit']
        }
    )
})

async function run(page, testInfo, scenario) {
    await page.goto(
        `${PLAIN_ORIGIN}/tests/browser/fixtures/vdbx-single-file.html?scenario=${scenario}`,
        {
            waitUntil: 'domcontentloaded'
        }
    )
    await expect(
        page.locator('[data-status="passed"], [data-status="failed"], [data-status="unsupported"]')
    ).toBeVisible()
    const evidence = await page.evaluate(() => globalThis.__FYLO_VDBX_EVIDENCE__)
    const path = testInfo.outputPath(`vdbx-${scenario}-evidence.json`)
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, `${JSON.stringify(evidence, null, 2)}\n`)
    await testInfo.attach(`vdbx-${scenario}`, { path, contentType: 'application/json' })
    expect(evidence.environment.crossOriginIsolated).toBe(false)
    test.skip(evidence.status === 'unsupported', evidence.reason)
    expect(evidence.status, evidence.reason).toBe('passed')
    return evidence
}

function expectReadsMatch(result) {
    expect(result.mismatches).toEqual([])
    expect(result.reads).toBeGreaterThan(50)
    expect(result.bytes).toBeGreaterThan(0)
}

test('reads a picked file at random offsets and refuses writes', async ({ page }, testInfo) => {
    const evidence = await run(page, testInfo, 'file')
    expect(evidence.info).toMatchObject({ writable: false, size: STUB_SIZE, engine: false })
    expect(evidence.stat).toEqual({ kind: 1, len: STUB_SIZE, modifiedMs: 1_700_000_000_000 })
    expect(evidence.magic).toBe(true)
    expectReadsMatch(evidence.reads)
    expect(evidence.refusals.write.refused).toBe(true)
    expect(evidence.refusals.truncate.refused).toBe(true)
    expect(evidence.refusals.openWrite.refused).toBe(true)
    expect(evidence.snapshotMatches).toBe(true)
})

test('reads a hosted file with ranged requests', async ({ page }, testInfo) => {
    const evidence = await run(page, testInfo, 'url')
    expect(evidence.info).toMatchObject({ writable: false, size: STUB_SIZE })
    expect(evidence.stat.len).toBe(STUB_SIZE)
    expect(evidence.magic).toBe(true)
    expectReadsMatch(evidence.reads)
    expect(evidence.refusals.write.refused).toBe(true)
    expect(evidence.missing.refused).toBe(true)
})

test('edits an OPFS working copy and writes it back', async ({ page }, testInfo) => {
    const evidence = await run(page, testInfo, 'opfs')
    expect(evidence.info).toMatchObject({ writable: true, size: STUB_SIZE })
    expect(evidence.progress.events).toBeGreaterThan(0)
    expect(evidence.progress.last).toEqual({ copied: STUB_SIZE, total: STUB_SIZE })
    expect(evidence.copyMatches).toBe(true)
    expectReadsMatch(evidence.readsBefore)
    expect(evidence.secondOpen).toMatchObject({ refused: true, code: 'EBUSY' })
    expect(evidence.writes).toBe(48)
    expect(evidence.statAfterWrites.len).toBe(evidence.expectedSize)
    expectReadsMatch(evidence.readsAfter)
    expect(evidence.editedMatches).toBe(true)
    expect(evidence.reopenedSize).toBe(evidence.expectedSize)
    expect(evidence.reopenedMatches).toBe(true)
    if (evidence.saved !== 'unsupported') {
        expect(evidence.saved).toBe('written')
        expect(evidence.savedMatches).toBe(true)
    }
    expect(evidence.listed).toBe(true)
    expect(evidence.removed).toBe(true)
})

test('saves a working copy as a download where it cannot write back', async ({
    page
}, testInfo) => {
    const downloaded = page.waitForEvent('download')
    // Left pending when the browser cannot run the scenario and the test skips.
    downloaded.catch(() => {})
    const evidence = await run(page, testInfo, 'download')
    expect(evidence.saved).toBe('downloaded')
    const file = await downloaded
    expect(file.suggestedFilename()).toBe('download.vdbx')
    const bytes = await readFile(await file.path())
    expect(bytes.length).toBe(evidence.size)
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(evidence.digest)
})

test('runs fylo-browser.wasm over a single file without isolation', async ({ page }, testInfo) => {
    const evidence = await run(page, testInfo, 'engine')
    expect(evidence.info.engine).toBe(true)
    expect(evidence.frames[0]?.ok, JSON.stringify(evidence.frames)).toBe(true)
    expect(evidence.frames[0]?.result?.buildTarget).toBe('browser-wasm32')
})

test('inspects a real .vdbx through every single-file host', async ({ page }, testInfo) => {
    const evidence = await run(page, testInfo, 'inspect')
    const labels = Object.keys(evidence.reports)
    expect(labels).toEqual(expect.arrayContaining(['file', 'url']))
    for (const label of labels) {
        expect(evidence.reports[label], label).toEqual(evidence.expected)
    }
    expect(evidence.expected.collections.map((collection) => collection.name)).toEqual([
        'users',
        'orders'
    ])
    expect(evidence.stubFrame.ok).toBe(false)
})

test('reads and edits the documents of a real .vdbx database', async ({ page }, testInfo) => {
    const evidence = await run(page, testInfo, 'documents')
    expect(evidence.red).toEqual(['ada', 'cy'])
    expect(evidence.reread).toBe('browser')
    expect(evidence.total).toBe(5)
})
