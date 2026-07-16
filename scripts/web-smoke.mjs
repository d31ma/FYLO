#!/usr/bin/env bun

import { readFile } from 'node:fs/promises'

const REQUEST_TIMEOUT_MS = 15_000

export async function smokeSite(site, fetcher = fetch) {
    const results = []
    for (const probe of site.probes) {
        const url = new URL(probe.path, site.origin).href
        const response = await fetcher(url, {
            redirect: 'follow',
            signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
            headers: { 'cache-control': 'no-cache', 'user-agent': 'fylo-release-smoke/1' }
        })
        const body = await response.text()
        if (!response.ok) throw new Error(`${url} returned HTTP ${response.status}`)
        if (probe.contains && !body.includes(probe.contains)) {
            throw new Error(
                `${url} did not contain expected marker ${JSON.stringify(probe.contains)}`
            )
        }
        results.push({ url, status: response.status })
    }
    return results
}

async function main() {
    const [siteName, configPath = 'ops/web-release.json'] = process.argv.slice(2)
    const config = JSON.parse(await readFile(configPath, 'utf8'))
    const site = config.sites?.[siteName]
    if (!site) throw new Error(`Unknown site ${JSON.stringify(siteName)}`)
    console.log(JSON.stringify(await smokeSite(site)))
}

if (import.meta.main)
    main().catch((error) => {
        console.error(error.message)
        process.exitCode = 1
    })
