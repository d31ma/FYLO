#!/usr/bin/env bun

import { createHash } from 'node:crypto'

const FILES = ['fylo.js', 'fylo-web.mjs', 'shared.js', 'dedicated.js', 'fylo-index.wasm']

async function verifyAssetSet(root, label, fetcher) {
    const checksumResponse = await fetcher(new URL('SHA256SUMS', root), { cache: 'no-store' })
    if (!checksumResponse.ok) {
        throw new Error(`${label} SHA256SUMS returned HTTP ${checksumResponse.status}`)
    }
    const expected = new Map()
    for (const line of (await checksumResponse.text()).trim().split('\n')) {
        const match = line.match(
            /^([a-f0-9]{64})  (fylo\.js|fylo-web\.mjs|shared\.js|dedicated\.js|fylo-index\.wasm)$/
        )
        if (!match) throw new Error(`Invalid ${label} SHA256SUMS entry: ${line}`)
        expected.set(match[2], match[1])
    }
    const assets = new Map()
    for (const file of FILES) {
        if (!expected.has(file)) throw new Error(`${label} SHA256SUMS is missing ${file}`)
        const response = await fetcher(new URL(file, root), { cache: 'no-store' })
        if (!response.ok) throw new Error(`${label} ${file} returned HTTP ${response.status}`)
        const bytes = Buffer.from(await response.arrayBuffer())
        const actual = createHash('sha256').update(bytes).digest('hex')
        if (actual !== expected.get(file)) throw new Error(`${label} ${file} checksum mismatch`)
        assets.set(file, bytes)
    }
    return assets
}

export async function verifyPagesRelease(baseUrl, version, fetcher = fetch) {
    if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`Invalid release version: ${version}`)
    const base = baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`
    const immutableRoot = new URL(`version/${version}/`, base)
    const latestRoot = new URL('version/latest/', base)
    const immutable = await verifyAssetSet(immutableRoot, `immutable ${version}`, fetcher)
    const latest = await verifyAssetSet(latestRoot, 'latest', fetcher)
    for (const file of FILES) {
        if (!immutable.get(file).equals(latest.get(file))) {
            throw new Error(`latest ${file} differs from immutable ${version}`)
        }
    }
    return { version, files: FILES }
}

export async function verifyPagesReleaseWithRetry(baseUrl, version, attempts = 6) {
    let failure
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
        try {
            return await verifyPagesRelease(baseUrl, version)
        } catch (error) {
            failure = error
            if (attempt < attempts) await Bun.sleep(10_000)
        }
    }
    throw failure
}

async function main() {
    const [version, baseUrl = 'https://d31ma.github.io/FYLO/'] = process.argv.slice(2)
    if (!version) throw new Error('Usage: bun scripts/pages-smoke.mjs <version> [pages-base-url]')
    console.log(JSON.stringify(await verifyPagesReleaseWithRetry(baseUrl, version)))
}

if (import.meta.main)
    main().catch((error) => {
        console.error(error.message)
        process.exitCode = 1
    })
