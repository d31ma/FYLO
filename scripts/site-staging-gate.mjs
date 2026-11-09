#!/usr/bin/env bun

const STAGING_CONTEXT = 'site-staging/floci'
const SHA256_PREFIX_LENGTH = 12
const ATTEMPTS = 40
const WAIT_SECONDS = 30
const MISSING = { context: STAGING_CONTEXT, state: 'missing', description: '' }
const FAILED_STATES = ['failure', 'error']

export function stagingDescription(version, sha256) {
    return `${version} ${sha256.slice(0, SHA256_PREFIX_LENGTH)}`
}

export async function waitForStagingStatus(
    gate,
    {
        fetcher = fetch,
        sleep = (seconds) => Bun.sleep(seconds * 1000),
        log = (line) => console.log(line)
    } = {}
) {
    const expected = stagingDescription(gate.version, gate.sha256)
    for (let attempt = 0; attempt < ATTEMPTS; attempt += 1) {
        const status = newestStagingStatus(await readStatuses(gate, fetcher))
        log(`${STAGING_CONTEXT}: ${status.state} ${status.description}`)
        if (stagingPassed(status, expected)) return expected
        await sleep(WAIT_SECONDS)
    }
    throw new Error(`No ${STAGING_CONTEXT} success "${expected}" on ${gate.commit} in time`)
}

async function readStatuses(gate, fetcher) {
    const response = await fetcher(
        `https://api.github.com/repos/${gate.repository}/commits/${gate.commit}/statuses?per_page=100`,
        {
            headers: {
                accept: 'application/vnd.github+json',
                authorization: `Bearer ${gate.token}`,
                'x-github-api-version': '2022-11-28'
            }
        }
    )
    if (!response.ok) {
        throw new Error(`Reading the commit statuses failed with HTTP ${response.status}`)
    }
    return response.json()
}

function newestStagingStatus(statuses) {
    return statuses.find((status) => status.context === STAGING_CONTEXT) ?? MISSING
}

function stagingPassed(status, expected) {
    if (FAILED_STATES.includes(status.state)) {
        throw new Error(`Staging on Floci reported ${status.state}: ${status.description}`)
    }
    return status.state === 'success' && status.description === expected
}

async function main() {
    const [repository, commit, version, sha256] = process.argv.slice(2)
    if (!repository || !commit || !version || !/^[a-f0-9]{64}$/.test(sha256 ?? '')) {
        throw new Error(
            'Usage: bun scripts/site-staging-gate.mjs <owner/repo> <commit> <version> <sha256>'
        )
    }
    const token = process.env.GITHUB_TOKEN ?? ''
    const expected = await waitForStagingStatus({ repository, commit, version, sha256, token })
    console.log(`${STAGING_CONTEXT} passed on ${commit}: ${expected}`)
}

if (import.meta.main)
    main().catch((error) => {
        console.error(error.message)
        process.exitCode = 1
    })
