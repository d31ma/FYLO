import { SiteArtifacts } from './site-artifacts.mjs'
import path from 'node:path'
import { createHash } from 'node:crypto'
import { readFile, realpath, stat } from 'node:fs/promises'

const project = process.argv[2] ? path.resolve(process.argv[2]) : process.cwd()
const web = SiteArtifacts.web(project)
const legacyRuntime = path.join(web, '.tachyon', 'islands.js')
const clientRuntime = path.join(web, '.tachyon', 'tac-client.js')

const loop = `  for (const marker of root.querySelectorAll('tachyon-expr[data-tachyon-expression]')) {
`
const scopedLoop = `${loop}    if (marker.closest('tachyon-island') !== root) continue
`

if (await Bun.file(legacyRuntime).exists()) {
    const source = await Bun.file(legacyRuntime).text()
    if (source.includes(scopedLoop)) {
        console.log('Tachyon nested-island expression ownership is already patched')
    } else if (!source.includes(loop)) {
        throw new Error(
            'Tachyon islands runtime changed; refusing to apply the supported nested-island patch'
        )
    } else {
        await Bun.write(legacyRuntime, source.replace(loop, scopedLoop))
        console.log('Patched legacy Tachyon nested-island expression ownership')
    }
} else if (await Bun.file(clientRuntime).exists()) {
    const source = await Bun.file(clientRuntime).text()
    const compiledViewRuntime = [
        "document.getElementById('tachyon-view')",
        'const componentRecords = new Map()',
        'const ownedEnvironment ='
    ]
    if (!compiledViewRuntime.every((marker) => source.includes(marker))) {
        throw new Error(
            'Tachyon client runtime changed; refusing to accept an unrecognized component-ownership model'
        )
    }
    if (source.includes('tachyon-expr[data-tachyon-expression]')) {
        throw new Error(
            'Tachyon compiled-view runtime unexpectedly contains legacy expression markers'
        )
    }
    console.log('Tachyon compiled-view runtime owns component expressions without the legacy patch')
} else {
    throw new Error('Tachyon runtime is missing from the generated website')
}

const browserEntry = '<script type="module" src="/shared/scripts/imports.js"></script>'
let documents = 0
for await (const relative of new Bun.Glob('**/*.html').scan({ cwd: web })) {
    const file = path.join(web, relative)
    const html = await Bun.file(file).text()
    if (html.includes(browserEntry)) continue
    if (!html.includes('</body>')) throw new Error(`Generated HTML has no </body>: ${relative}`)
    await Bun.write(file, html.replace('</body>', `${browserEntry}</body>`))
    documents++
}
console.log(`Added the shared browser entry to ${documents} generated routes`)

const serviceWorkerFile = path.join(web, 'tachyon-sw.js')
const serviceWorker = await Bun.file(serviceWorkerFile).text()
const staticAssetPattern =
    'const STATIC = /\\.(?:avif|css|gif|ico|jpe?g|js|json|mjs|mp3|mp4|ogg|otf|png|svg|ttf|wasm|webm|webp|woff2?)$/i\n\n'
const cacheFirstFetch = `  // A navigation stays network-first so a deployment is picked up at once,
  // with the cache as the offline fallback. A versioned static asset cannot
  // change under its own URL, so it is served cache-first.
  const cacheFirst = STATIC.test(url.pathname) && request.mode !== 'navigate'
  event.respondWith(cacheFirst ? fromCache(request) : fromNetwork(request))
})

async function fromCache(request) {
  const cache = await caches.open(CACHE)
  return (await cache.match(request)) || fromNetwork(request)
}
`
const networkFirstFetch = `  // Generated asset paths are stable across releases. Always check the network
  // first so a controlling worker cannot combine new HTML with stale runtime
  // JavaScript. The versioned cache remains an offline-only fallback.
  event.respondWith(fromNetwork(request))
})
`
const liveReloadGuards = [
    "if (url.pathname.startsWith('/.tachyon/live-reload')) return",
    "if (url.pathname.startsWith('/.tachyon/live-reload') || url.pathname === '/.tachyon/hot') return"
]
const verifiedCacheFirstFetch = `  const cacheFirst = rule ? rule.policy === 'cache-first' : request.mode !== 'navigate'
  event.respondWith(cacheFirst ? fromCache(request, asset) : fromNetwork(request, asset))
`
const verifiedNetworkFirstFetch = `  // FYLO keeps stable asset URLs network-first; preserve Tachyon's verified offline fallback.
  event.respondWith(fromNetwork(request, asset))
`
const verifiedWorkerGuards = [
    'const ASSETS = {',
    'if (url.origin !== self.location.origin) return',
    "if (request.method !== 'GET' && request.method !== 'HEAD')",
    'event.respondWith(writeThrough(request, url))',
    "if (request.method === 'HEAD') return",
    'const asset = !url.search && Object.hasOwn(ASSETS, url.pathname) ? ASSETS[url.pathname] : null',
    "if (request.cache === 'no-store' || request.headers.has('authorization')",
    "|| request.headers.has('range') || rule?.policy === 'no-store'",
    "|| (!asset && (!rule || request.credentials !== 'omit')))",
    'const publicResponse = (response) => response && response.ok && !response.redirected',
    "&& response.type !== 'opaque' && response.type !== 'opaqueredirect'",
    "&& !response.headers.has('vary')",
    'if (asset && !await matchesAsset(await bodyBytes(response.clone(), 4 * 1024 * 1024), asset))',
    'if (asset && !await matchesAsset(bytes, asset)) { await evict(request); return }',
    'if (request.signal.aborted) throw error',
    'const cached = await cacheRead(request, asset)',
    "...(asset ? { credentials: 'omit' } : {})",
    'await bounded(cacheWrite(request, response, asset)).catch(() => {})',
    'if (name.startsWith(PREFIX) && name !== CACHE) await caches.delete(name)'
]

async function refreshAssetManifest(source) {
    const declaration = source.match(/^const ASSETS = (\{.*\})$/m)
    if (!declaration || !/^const VERSION = '[^']+'$/m.test(source)) {
        throw new Error('Tachyon verified asset manifest changed; refusing to rewrite it')
    }
    const assets = JSON.parse(declaration[1])
    const root = await realpath(web)
    const refreshed = Object.create(null)
    for (const url of Object.keys(assets)) {
        const pathname = decodeURIComponent(url)
        if (
            !pathname.startsWith('/') ||
            /[\\?#\0]/.test(pathname) ||
            pathname.split('/').some((part) => part === '.' || part === '..')
        ) {
            throw new Error('Tachyon verified asset manifest contains an unsafe path')
        }
        let file = path.join(root, pathname)
        if ((await stat(file)).isDirectory()) file = path.join(file, 'index.html')
        const resolved = await realpath(file)
        if (!resolved.startsWith(root + path.sep)) {
            throw new Error('Tachyon verified asset resolves outside the generated website')
        }
        const bytes = await readFile(resolved)
        refreshed[url] = {
            bytes: bytes.byteLength,
            sha256: createHash('sha256').update(bytes).digest('hex')
        }
    }
    const manifest = JSON.stringify(refreshed)
    const version = createHash('sha256').update(manifest).digest('hex').slice(0, 16)
    return source
        .replace(declaration[0], `const ASSETS = ${manifest}`)
        .replace(/^const VERSION = '[^']+'$/m, `const VERSION = '${version}'`)
}

if (
    verifiedWorkerGuards.every((guard) => serviceWorker.includes(guard)) &&
    liveReloadGuards.some((guard) => serviceWorker.includes(guard)) &&
    (serviceWorker.includes(verifiedCacheFirstFetch) ||
        serviceWorker.includes(verifiedNetworkFirstFetch))
) {
    // Hash the final generated bytes after imports injection and fingerprinting;
    // otherwise the upstream digest checks correctly refuse our modified HTML.
    const patched = serviceWorker.replace(verifiedCacheFirstFetch, verifiedNetworkFirstFetch)
    await Bun.write(serviceWorkerFile, await refreshAssetManifest(patched))
    console.log(
        'Patched verified Tachyon service worker for network-first assets and refreshed final-byte hashes'
    )
} else if (
    serviceWorker.includes(networkFirstFetch) &&
    !serviceWorker.includes(staticAssetPattern) &&
    !serviceWorker.includes('async function fromCache(request)')
) {
    console.log('Tachyon service worker is already patched for network-first assets')
} else if (
    !serviceWorker.includes(staticAssetPattern) ||
    !serviceWorker.includes(cacheFirstFetch) ||
    !liveReloadGuards.some((guard) => serviceWorker.includes(guard)) ||
    !serviceWorker.includes(
        'if (name.startsWith(PREFIX) && name !== CACHE) await caches.delete(name)'
    )
) {
    throw new Error(
        'Tachyon service worker changed; refusing to apply the supported network-first patch'
    )
} else {
    await Bun.write(
        serviceWorkerFile,
        serviceWorker.replace(staticAssetPattern, '').replace(cacheFirstFetch, networkFirstFetch)
    )
    console.log('Patched Tachyon service worker for network-first assets')
}
