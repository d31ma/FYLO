// The engine on a small stack: every operation on the deepest document the
// format accepts.
//
// A browser Worker gets a much smaller stack than a native thread, and Safari
// gives the least: a 60-level document overflowed there when the engine
// copied whole documents with `Map::clone`, nine frames per level. Each
// browser is launched here with its stack capped below what Safari leaves a
// Worker, so a recursion that grows back fails on every platform, not only on
// a Mac. The engine runs over memory, so this needs no OPFS (Linux WebKit has
// none).

import { expect, test } from '@playwright/test'

/** Below the stack a Safari Worker leaves the engine (Safari 26 on macOS). */
const STACK_BYTES = 320 * 1024

/** The nesting `DocumentLimits` allows: the root is level 1, an array a level too. */
const MAX_DEPTH = 64

/** A document `depth` levels deep: objects all the way down to an array. */
function nested(depth, tag) {
    let node = { leaf: tag, values: [1, 2.5, -3, true, null, 'x'] }
    for (let level = 3; level < depth; level++) node = { level, child: node, list: [level] }
    return { kind: tag, child: node }
}

function capped(browserName) {
    // Each engine's own knob for the stack it gives a thread.
    if (browserName === 'webkit') {
        return { env: { ...process.env, JSC_maxPerThreadStackUsage: String(STACK_BYTES) } }
    }
    if (browserName === 'chromium') {
        return { args: [`--js-flags=--stack-size=${Math.floor(STACK_BYTES / 1024)}`] }
    }
    return {}
}

test('every operation handles the deepest document on a small stack', async ({
    playwright,
    browserName,
    baseURL
}, testInfo) => {
    testInfo.annotations.push({
        type: 'stack',
        description: browserName === 'firefox' ? 'browser default' : `${STACK_BYTES} bytes`
    })
    const browser = await playwright[browserName].launch(capped(browserName))
    try {
        const page = await browser.newPage({ baseURL })
        // WebKit sometimes crashes the page instead of throwing when a Worker
        // runs out of stack, so a crash here means the same thing.
        page.on('crash', () => testInfo.annotations.push({ type: 'crash', description: 'stack' }))
        await page.goto('/tests/browser/fixtures/engine-stack.html')
        const root = '/stack'
        const collection = 'nests'
        const deepest = nested(MAX_DEPTH, 'deep')
        const other = nested(MAX_DEPTH, 'other')
        const batches = [
            [{ op: 'createCollection', collection }],
            [{ op: 'putData', collection, data: deepest }],
            [{ op: 'getDoc', collection, id: '<result:1>' }],
            [{ op: 'getLatest', collection, id: '<result:1>' }],
            [{ op: 'findDocs', collection, query: {} }],
            [{ op: 'findDocs', collection, query: { $ops: [{ kind: { $eq: 'deep' } }] } }],
            [{ op: 'patchDoc', collection, id: '<result:1>', newDoc: { child: other.child } }],
            [{ op: 'batchPutData', collection, batch: [deepest, other] }],
            [{ op: 'patchDocs', collection, update: { $set: { touched: true } } }],
            [{ op: 'executeSQL', sql: `SELECT * FROM ${collection}` }],
            [
                {
                    op: 'executeSQL',
                    sql: `UPDATE ${collection} SET kind = 'sql' WHERE kind = 'deep'`
                }
            ],
            [{ op: 'delDoc', collection, id: '<result:1>' }],
            [{ op: 'findDeletedDocs', collection, query: {} }],
            [{ op: 'restoreDoc', collection, id: '<result:1>' }],
            [{ op: 'getDoc', collection, id: '<result:1>' }],
            [{ op: 'putData', collection, data: nested(MAX_DEPTH + 1, 'too-deep') }]
        ].map((batch) =>
            batch.map((request) => `${JSON.stringify({ root, ...request })}\n`).join('')
        )

        // Text both ways: the page's main thread has the capped stack too.
        const { answers, error } = await page.evaluate(
            (batches) => globalThis.run(batches),
            batches
        )

        expect(error, `the engine stopped after ${answers.length} batches`).toBeUndefined()
        const frames = answers.map((ndjson) => JSON.parse(ndjson.split('\n')[0]))
        const failed = frames.slice(0, -1).findIndex((frame) => !frame.ok)
        expect(
            failed,
            failed === -1 ? '' : `${frames[failed].op}: ${JSON.stringify(frames[failed].error)}`
        ).toBe(-1)
        // One level more is refused by the format, so the rest really was the deepest.
        expect(frames.at(-1).error?.code).toBe('EFORMAT_DEPTH')
        const id = frames[1].result
        expect(frames[2].result[id]).toEqual(deepest)
        expect(frames[14].result[id]).toEqual({ kind: 'sql', child: other.child, touched: true })
    } finally {
        await browser.close()
    }
})
