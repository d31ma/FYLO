// The Node half of the engine harness: a `fylo` fixture that drives
// `fylo-browser.wasm` in the page, and the native binary that every browser
// answer is held against.
//
// Parity is the point. Each scenario runs once in the browser over OPFS and
// once through `fylo-rust exec --loop` over a temporary directory, with the
// same caller-chosen identifiers, and the answers must match frame for frame
// once the fields that legitimately differ (timings, the build) are removed.

import { expect, test as browserTest } from './browser-test.mjs'
import { execFileSync, spawnSync } from 'node:child_process'
import {
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    rmSync,
    statSync
} from 'node:fs'
import { writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export { expect }

const REPOSITORY = fileURLToPath(new URL('../../../', import.meta.url))
const TARGET = process.env.CARGO_TARGET_DIR
    ? resolve(process.env.CARGO_TARGET_DIR)
    : join(REPOSITORY, 'target')
const NATIVE = join(TARGET, 'release', process.platform === 'win32' ? 'fylo-rust.exe' : 'fylo-rust')

let nativeBuilt = false

/** The release `fylo-rust`, built once per test process (a no-op when current). */
export function nativeBinary() {
    if (!nativeBuilt && process.env.FYLO_E2E_SKIP_NATIVE_BUILD !== '1') {
        execFileSync(
            'cargo',
            ['build', '--quiet', '--release', '--locked', '-p', 'fylo-cli', '--bin', 'fylo-rust'],
            { cwd: REPOSITORY, stdio: ['ignore', 'ignore', 'inherit'] }
        )
    }
    nativeBuilt = true
    if (!existsSync(NATIVE)) throw new Error(`native binary missing: ${NATIVE}`)
    return NATIVE
}

/** A fresh temporary directory for a native root. */
export function nativeRoot(label) {
    return mkdtempSync(join(tmpdir(), `fylo-e2e-${label}-`))
}

/**
 * Run `requests` through the native engine against `root` in one process.
 *
 * Each request gets `root` unless it names one, matching what the harness
 * page does for the browser.
 */
export function nativeExec(root, requests, { env } = {}) {
    const outcome = spawnSync(nativeBinary(), ['exec', '--loop', '--root', root], {
        input: `${requests.map((request) => JSON.stringify({ root, ...request })).join('\n')}\n`,
        encoding: 'utf8',
        maxBuffer: 512 * 1024 * 1024,
        env: { ...process.env, ...env }
    })
    if (outcome.error) throw outcome.error
    const frames = outcome.stdout
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
    if (frames.length !== requests.length) {
        throw new Error(
            `native answered ${frames.length} of ${requests.length} frames (exit ${outcome.status}): ${outcome.stderr}`
        )
    }
    return frames
}

/** A unique OPFS root path for one test. */
export function browserRoot(label) {
    return `/e2e-${label}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
}

/**
 * `count` document identifiers the engine accepts, minted by the native
 * engine so they are real, ordered TTIDs. Parity needs both engines to store
 * the same keys, so scenarios pass these as `id` instead of letting each
 * engine allocate its own.
 */
export function mintIds(count) {
    const root = nativeRoot('ids')
    try {
        const frames = nativeExec(root, [
            { op: 'createCollection', collection: 'mint' },
            ...Array.from({ length: count }, () => ({
                op: 'putData',
                collection: 'mint',
                data: {}
            }))
        ])
        return frames.slice(1).map((frame) => {
            if (!frame.ok) throw new Error(`could not mint an identifier: ${frame.error?.message}`)
            return frame.result
        })
    } finally {
        rmSync(root, { recursive: true, force: true })
    }
}

/** Strip what may differ between two correct answers to the same frame. */
export function comparable(frame) {
    const { durationMs, requestId, protocolVersion, ...rest } = frame
    return scrub(rest)
}

const VOLATILE = new Set([
    'durationMs',
    'elapsedMs',
    'buildTarget',
    'binaryVersion',
    'engineVersion',
    'pid',
    'root',
    'path',
    'createdAt',
    'updatedAt',
    'modifiedMs',
    'mtimeMs',
    'timestamp',
    'committedAt'
])

function scrub(value) {
    if (Array.isArray(value)) return value.map(scrub)
    if (value && typeof value === 'object') {
        const out = {}
        for (const [key, inner] of Object.entries(value)) {
            if (VOLATILE.has(key)) continue
            out[key] = scrub(inner)
        }
        return out
    }
    return value
}

/** Every file under `directory`, base64 encoded, in the harness page's `dump` shape. */
export function readTree(directory) {
    const files = {}
    const walk = (path) => {
        for (const name of readdirSync(path)) {
            const full = join(path, name)
            const key = relative(directory, full).split('\\').join('/')
            if (statSync(full).isDirectory()) {
                files[`${key}/`] = ''
                walk(full)
            } else {
                files[key] = readFileSync(full).toString('base64')
            }
        }
    }
    walk(directory)
    return files
}

/** Write a `dump` from the page into `directory`. */
export function writeTree(directory, files) {
    rmSync(directory, { recursive: true, force: true })
    mkdirSync(directory, { recursive: true })
    for (const [path, content] of Object.entries(files)) {
        const full = join(directory, path)
        if (path.endsWith('/')) {
            mkdirSync(full, { recursive: true })
            continue
        }
        mkdirSync(dirname(full), { recursive: true })
        writeFileSync(full, Buffer.from(content, 'base64'))
    }
}

/** Assert every frame succeeded, naming the first that did not. */
export function expectOk(frames, label = 'batch') {
    const failed = frames.findIndex((frame) => !frame.ok)
    expect(
        failed,
        failed === -1
            ? ''
            : `${label}: frame ${failed} (${frames[failed].op}) failed: ${JSON.stringify(frames[failed].error)}`
    ).toBe(-1)
    return frames
}

/**
 * The harness page, opened and checked. A browser that cannot run the engine
 * over OPFS skips with its reason; everything else must work.
 */
export const test = browserTest.extend({
    fylo: async ({ page }, use, testInfo) => {
        await page.goto('/tests/browser/fixtures/engine-harness.html', {
            waitUntil: 'domcontentloaded'
        })
        await expect(page.locator('[data-status="ready"]')).toBeVisible()
        const unsupported = await page.evaluate(() => globalThis.fylo.supported())
        testInfo.skip(unsupported !== '', unsupported)
        await use(new BrowserEngine(page))
    }
})

/** One page's handle on the harness; `page` may be any page on the harness. */
export class BrowserEngine {
    constructor(page) {
        this.page = page
    }

    /** Open `root` in a new session (bridge + engine Worker). */
    open(root) {
        return this.page.evaluate((root) => globalThis.fylo.open(root), root)
    }

    /** Run a batch; throws on a Worker-level failure, returns the frames. */
    async exec(session, requests) {
        const answer = await this.page.evaluate(
            ([session, requests]) => globalThis.fylo.exec(session, requests),
            [session, requests]
        )
        if (answer.error) {
            throw new Error(`engine worker: ${answer.error} ${answer.bridgeError ?? ''}`)
        }
        if (answer.frames.length !== requests.length) {
            throw new Error(
                `browser answered ${answer.frames.length} of ${requests.length} frames: ${answer.hostError ?? ''}`
            )
        }
        return answer.frames
    }

    /** Open a root, run one batch, and end the session. */
    async once(root, requests) {
        const session = await this.open(root)
        try {
            return await this.exec(session, requests)
        } finally {
            await this.close(session)
        }
    }

    start(session, requests) {
        return this.page.evaluate(
            ([session, requests]) => globalThis.fylo.start(session, requests),
            [session, requests]
        )
    }

    settled(ticket) {
        return this.page.evaluate((ticket) => globalThis.fylo.settled(ticket), ticket)
    }

    wait(ticket, ms) {
        return this.page.evaluate(([ticket, ms]) => globalThis.fylo.wait(ticket, ms), [ticket, ms])
    }

    trace(session) {
        return this.page.evaluate((session) => globalThis.fylo.trace(session), session)
    }

    kill(session) {
        return this.page.evaluate((session) => globalThis.fylo.kill(session), session)
    }

    close(session) {
        return this.page.evaluate((session) => globalThis.fylo.close(session), session)
    }

    dump(root) {
        return this.page.evaluate((root) => globalThis.fylo.dump(root), root)
    }

    seed(root, files) {
        return this.page.evaluate(
            ([root, files]) => globalThis.fylo.seed(root, files),
            [root, files]
        )
    }

    remove(root) {
        return this.page.evaluate((root) => globalThis.fylo.remove(root), root)
    }

    moduleCost() {
        return this.page.evaluate(() => globalThis.fylo.moduleCost())
    }

    estimate() {
        return this.page.evaluate(() => globalThis.fylo.estimate())
    }
}

/** Open the harness in another tab of the same browser context. */
export async function openTab(context) {
    const page = await context.newPage()
    await page.goto('/tests/browser/fixtures/engine-harness.html', {
        waitUntil: 'domcontentloaded'
    })
    await expect(page.locator('[data-status="ready"]')).toBeVisible()
    return new BrowserEngine(page)
}
