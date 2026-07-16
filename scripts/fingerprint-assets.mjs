/**
 * Cache-bust the static shared assets. Tachyon already content-hashes its own
 * JS chunks, but the vendored/static files under `shared/assets/` (explorer.css,
 * site.css, duvay, highlight, …) keep stable names, so browsers serve stale
 * copies after a deploy. This appends `?v=<contenthash>` to every
 * `/shared/assets/...` reference in the built HTML — a content change flips the
 * hash and forces a fresh fetch. Run from either app directory after bundling.
 */

import { copyFile, readdir, readFile, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import path from 'node:path'

const output = path.join(globalThis.process.cwd(), 'dist', 'web')
const ASSET_REF = /(?:href|src)="(\/shared\/assets\/[^"?#]+)"/g
const ASSET_VERSION_PLACEHOLDER = '__FYLO_ASSET_VERSION__'

/** @type {Map<string, string>} asset url -> short content hash ('' = missing) */
const hashes = new Map()

/** @param {string} url @returns {Promise<string>} */
async function hashOf(url) {
    if (hashes.has(url)) return /** @type {string} */ (hashes.get(url))
    let hash = ''
    try {
        const bytes = await readFile(path.join(output, url.replace(/^\//, '')))
        hash = createHash('sha256').update(bytes).digest('hex').slice(0, 8)
    } catch {
        // Referenced asset isn't in the build (e.g. removed by the gate); skip it.
    }
    hashes.set(url, hash)
    return hash
}

/** @param {string} dir @returns {AsyncGenerator<string>} */
async function* htmlFiles(dir) {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
        const target = path.join(dir, entry.name)
        if (entry.isDirectory()) yield* htmlFiles(target)
        else if (entry.name.endsWith('.html')) yield target
    }
}

let stamped = 0
for await (const file of htmlFiles(output)) {
    const html = await readFile(file, 'utf8')
    const urls = new Set([...html.matchAll(ASSET_REF)].map((match) => match[1]))
    let next = html
    for (const url of urls) {
        const hash = await hashOf(url)
        if (!hash) continue
        next = next.replaceAll(`"${url}"`, `"${url}?v=${hash}"`)
        stamped += 1
    }
    if (next !== html) await writeFile(file, next)
}

// Explorer loads static assets from its runtime imports entry rather than HTML.
// Replace its stable source placeholder with one deterministic fingerprint of
// every emitted shared asset, making clean builds byte-reproducible while still
// invalidating browser caches whenever any static dependency changes.
const sharedAssets = path.join(output, 'shared', 'assets')
const assetHash = createHash('sha256')
async function hashTree(directory) {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) =>
        a.name.localeCompare(b.name)
    )) {
        const target = path.join(directory, entry.name)
        if (entry.isDirectory()) await hashTree(target)
        else {
            assetHash.update(path.relative(sharedAssets, target))
            assetHash.update(await readFile(target))
        }
    }
}

try {
    await hashTree(sharedAssets)
    const importsPath = path.join(output, 'imports.js')
    const imports = await readFile(importsPath, 'utf8')
    if (imports.includes(ASSET_VERSION_PLACEHOLDER)) {
        await writeFile(
            importsPath,
            imports.replaceAll(ASSET_VERSION_PLACEHOLDER, assetHash.digest('hex').slice(0, 12))
        )
    }
} catch (error) {
    if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
}

// The marketing site advertises these stable root URLs. Keep promotion in the
// app build itself so every host (Pages, Amplify, or a local artifact upload)
// receives the same complete bundle.
for (const installer of ['install.sh', 'install.ps1']) {
    try {
        await copyFile(
            path.join(globalThis.process.cwd(), 'client', 'shared', 'assets', installer),
            path.join(output, installer)
        )
    } catch (error) {
        if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
    }
}

console.log(`Fingerprinted ${stamped} shared-asset references`)
