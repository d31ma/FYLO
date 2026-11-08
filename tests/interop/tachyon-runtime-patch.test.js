import { afterAll, expect, test } from 'bun:test'
import { createHash, webcrypto } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { runInNewContext } from 'node:vm'

const script = path.resolve('scripts/patch-tachyon-runtime.mjs')
const template = await readFile('tests/fixtures/tachyon-verified-service-worker.txt', 'utf8')
const roots = []
const originalDispatch =
    "  const cacheFirst = rule ? rule.policy === 'cache-first' : request.mode !== 'navigate'\n  event.respondWith(cacheFirst ? fromCache(request, asset) : fromNetwork(request, asset))\n"
const patchedDispatch =
    "  // FYLO keeps stable asset URLs network-first; preserve Tachyon's verified offline fallback.\n  event.respondWith(fromNetwork(request, asset))\n"
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex')

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

async function fixture() {
    const root = await mkdtemp(path.join(tmpdir(), 'fylo-tachyon-patch-'))
    roots.push(root)
    const web = path.join(root, 'dist', 'web')
    await mkdir(path.join(web, '.tachyon'), { recursive: true })
    await writeFile(
        path.join(web, '.tachyon', 'tac-client.js'),
        [
            "document.getElementById('tachyon-view')",
            'const componentRecords = new Map()',
            'const ownedEnvironment = {}'
        ].join('\n')
    )
    await writeFile(path.join(web, 'index.html'), '<html><body>Home</body></html>')
    await writeFile(path.join(web, 'runtime.js'), 'current-runtime')
    const assets = { '/': {}, '/index.html': {}, '/runtime.js': {} }
    const source = template
        .replace('__VERSION__', 'original')
        .replace('__CACHE_RULES__', '[]')
        .replace('__PUBLIC_ASSETS__', JSON.stringify(assets))
    const worker = path.join(web, 'tachyon-sw.js')
    await writeFile(worker, source)
    return { root, web, worker, source }
}

async function patch(root) {
    const child = Bun.spawn([process.execPath, '--no-env-file', script, root], {
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const timeout = setTimeout(() => child.kill(), 10_000)
    try {
        const [exitCode, stdout, stderr] = await Promise.all([
            child.exited,
            new Response(child.stdout).text(),
            new Response(child.stderr).text()
        ])
        return { exitCode, stdout, stderr }
    } finally {
        clearTimeout(timeout)
    }
}

test('patches the verified worker without changing its security guards and refreshes final-byte hashes', async () => {
    const built = await fixture()
    const result = await patch(built.root)
    expect(result.exitCode, result.stderr).toBe(0)
    const source = await readFile(built.worker, 'utf8')
    const html = await readFile(path.join(built.web, 'index.html'))
    expect(html.toString()).toContain(
        '<script type="module" src="/shared/scripts/imports.js"></script>'
    )
    const manifest = JSON.parse(source.match(/^const ASSETS = (.*)$/m)[1])
    expect(manifest['/']).toEqual({ bytes: html.byteLength, sha256: digest(html) })
    expect(manifest['/index.html']).toEqual(manifest['/'])
    expect(manifest['/runtime.js']).toEqual({ bytes: 15, sha256: digest('current-runtime') })
    const normalize = (value) =>
        value.replace(/^const VERSION = .*$/m, 'VERSION').replace(/^const ASSETS = .*$/m, 'ASSETS')
    expect(normalize(source)).toBe(
        normalize(built.source.replace(originalDispatch, patchedDispatch))
    )
    expect((await patch(built.root)).exitCode).toBe(0)
    expect(await readFile(built.worker, 'utf8')).toBe(source)
})

test('rejects unknown worker guards and unsafe manifest paths', async () => {
    for (const mutate of [
        (source) => source.replace("request.headers.has('authorization')", 'false'),
        (source) => source.replace("&& !response.headers.has('vary')", ''),
        (source) => source.replace('"/runtime.js":{}', '"/../outside.js":{}')
    ]) {
        const built = await fixture()
        const source = mutate(built.source)
        await writeFile(built.worker, source)
        const result = await patch(built.root)
        expect(result.exitCode).not.toBe(0)
        expect(await readFile(built.worker, 'utf8')).toBe(source)
    }
})

test('verified assets stay network-first with checked offline fallback and private requests bypass cache', async () => {
    const built = await fixture()
    expect((await patch(built.root)).exitCode).toBe(0)
    const source = await readFile(built.worker, 'utf8')
    const events = new Map()
    const stored = new Map()
    const key = (request) => (typeof request === 'string' ? request : request.url)
    const cache = {
        async match(request) {
            return stored.get(key(request))?.clone()
        },
        async put(request, response) {
            stored.set(key(request), response.clone())
        },
        async delete(request) {
            return stored.delete(key(request))
        },
        async keys() {
            return [...stored.keys()].map((url) => new Request(url))
        }
    }
    let offline = false
    const requests = []
    // Bun 1.3 reports credentials="include" even for an explicit "omit".
    // Model that browser-only Request field at this service-worker boundary.
    class BrowserRequest extends Request {
        constructor(input, options = {}) {
            super(input, options)
            this.requestCredentials = options.credentials ?? input.credentials ?? 'same-origin'
        }
        get credentials() {
            return this.requestCredentials
        }
    }
    runInNewContext(source, {
        self: {
            location: { origin: 'https://example.test' },
            addEventListener: (name, handler) => events.set(name, handler)
        },
        URL,
        Request: BrowserRequest,
        Response,
        Headers,
        crypto: webcrypto,
        setTimeout,
        clearTimeout,
        caches: {
            async open() {
                return cache
            }
        },
        async fetch(request) {
            requests.push(request)
            if (offline) throw new Error('offline')
            return new Response('current-runtime')
        }
    })
    const dispatch = async (request) => {
        let response
        events.get('fetch')({
            request,
            respondWith(value) {
                response = value
            }
        })
        return await response
    }
    const url = 'https://example.test/runtime.js'
    stored.set(url, new Response('stale-runtime'))
    expect(await (await dispatch(new Request(url))).text()).toBe('current-runtime')
    expect(requests).toHaveLength(1)
    expect(requests[0].credentials).toBe('omit')
    expect(requests[0].cache).toBe('no-store')
    offline = true
    expect(await (await dispatch(new Request(url))).text()).toBe('current-runtime')
    expect(requests).toHaveLength(2)
    stored.set(url, new Response('tampered-runtime'))
    await expect(dispatch(new Request(url))).rejects.toThrow('offline')
    expect(stored.has(url)).toBe(false)
    offline = false
    stored.set(url, new Response('private-stale-runtime'))
    const authorized = new Request(url, { headers: { authorization: 'Bearer test' } })
    expect(await (await dispatch(authorized)).text()).toBe('current-runtime')
    expect(stored.has(url)).toBe(false)
    expect(requests.at(-1)).toBe(authorized)
    const count = requests.length
    expect(await dispatch(new Request('https://other.test/runtime.js'))).toBeUndefined()
    expect(await dispatch(new Request('https://example.test/.tachyon/hot'))).toBeUndefined()
    expect(requests).toHaveLength(count)
})
