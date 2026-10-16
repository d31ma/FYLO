import { expect, test } from './browser-test.mjs'
import { writeFile, mkdir } from 'node:fs/promises'
import { dirname } from 'node:path'

test('runs the FYLO engine over real OPFS in a worker', async ({ page }, testInfo) => {
    await page.goto('/tests/browser/fixtures/wasm-engine.html', { waitUntil: 'domcontentloaded' })
    await expect(
        page.locator('[data-status="passed"], [data-status="failed"], [data-status="unsupported"]')
    ).toBeVisible()
    const evidence = await page.evaluate(() => globalThis.__FYLO_ENGINE_EVIDENCE__)
    const path = testInfo.outputPath('wasm-engine-evidence.json')
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, `${JSON.stringify(evidence, null, 2)}\n`)
    await testInfo.attach('evidence', { path, contentType: 'application/json' })
    console.log(JSON.stringify(evidence.surface, null, 2))
    expect(evidence.supported, evidence.reason).toBe(true)
    expect(evidence.status, evidence.reason).toBe('passed')
    expect(evidence.frames[0]?.ok, evidence.reason).toBe(true)
    expect(evidence.frames[0]?.result?.buildTarget).toBe('browser-wasm32')
    expect(evidence.frames[0]?.result?.capabilities?.exclusiveRoot).toBe(false)
    const found = evidence.frames[4].result
    expect(Object.keys(found)).toHaveLength(2)
    expect(Object.keys(found)).toEqual([...Object.keys(found)].sort())
    expect(
        Object.values(found)
            .map((document) => document.name)
            .sort()
    ).toEqual(['Ada', 'Grace'])
    expect(evidence.frames[5].result.docsStored).toBe(2)
    expect(evidence.frames[5].result.indexedDocs).toBe(2)
    const equality = evidence.frames.slice(-4).map((frame) =>
        Object.values(frame.result)
            .map((document) => document.name)
            .sort()
    )
    expect(equality).toEqual([
        ['boolean', 'number', 'padded', 'string'],
        ['missing', 'null'],
        ['boolean', 'number', 'padded', 'string'],
        ['boolean', 'number', 'padded', 'string']
    ])
})
