import { describe, expect, test } from 'bun:test'
import { stagingDescription, waitForStagingStatus } from '../../scripts/site-staging-gate.mjs'

const SHA256 = '99682b0a0ca3dc5ef7f397d8af10427180f209fb48a9ef4016ce3ed509150fdc'
const GATE = {
    repository: 'd31ma/FYLO',
    commit: 'abc123',
    version: '26.46.01',
    sha256: SHA256,
    token: 'github-token'
}
const STATUSES_URL = 'https://api.github.com/repos/d31ma/FYLO/commits/abc123/statuses?per_page=100'

function status(state, description, context = 'site-staging/floci') {
    return { context, state, description }
}

function statusSource(rounds, httpStatus = 200) {
    const requests = []
    const sleeps = []
    const remaining = [...rounds]
    return {
        requests,
        sleeps,
        tools: {
            fetcher: async (url, options) => {
                requests.push([url, options.headers.authorization])
                return Response.json(remaining.shift() ?? [], { status: httpStatus })
            },
            sleep: async (seconds) => {
                sleeps.push(seconds)
            },
            log: () => {}
        }
    }
}

describe('site staging gate', () => {
    test('describes the zip as the version and the first 12 hex of its SHA-256', () => {
        expect(stagingDescription('26.46.01', SHA256)).toBe('26.46.01 99682b0a0ca3')
    })

    test('passes on the newest site-staging/floci success for this zip', async () => {
        const source = statusSource([
            [status('success', '26.46.01 99682b0a0ca3'), status('pending', '26.46.01 99682b0a0ca3')]
        ])
        await expect(waitForStagingStatus(GATE, source.tools)).resolves.toBe(
            '26.46.01 99682b0a0ca3'
        )
        expect(source.sleeps).toEqual([])
    })

    test('reads the statuses of the release commit with the workflow token', async () => {
        const source = statusSource([[status('success', '26.46.01 99682b0a0ca3')]])
        await waitForStagingStatus(GATE, source.tools)
        expect(source.requests).toEqual([[STATUSES_URL, 'Bearer github-token']])
    })

    test('keeps waiting while staging is pending, missing or on another zip', async () => {
        const source = statusSource([
            [],
            [status('success', 'other check', 'ci/other')],
            [status('pending', '26.46.01 99682b0a0ca3')],
            [status('success', '26.46.01 000000000000')],
            [status('success', '26.46.01 99682b0a0ca3')]
        ])
        await waitForStagingStatus(GATE, source.tools)
        expect(source.sleeps).toEqual([30, 30, 30, 30])
    })

    test('stops at once when staging reports a failure', async () => {
        const source = statusSource([[status('failure', '26.46.01 99682b0a0ca3')]])
        await expect(waitForStagingStatus(GATE, source.tools)).rejects.toThrow(
            'Staging on Floci reported failure'
        )
        expect(source.sleeps).toEqual([])
    })

    test('stops at once when staging reports an error', async () => {
        const source = statusSource([[status('error', 'probe timed out')]])
        await expect(waitForStagingStatus(GATE, source.tools)).rejects.toThrow(
            'reported error: probe timed out'
        )
    })

    test('fails closed after 20 minutes without a matching success', async () => {
        const source = statusSource(Array(40).fill([status('pending', '26.46.01 99682b0a0ca3')]))
        await expect(waitForStagingStatus(GATE, source.tools)).rejects.toThrow(
            'No site-staging/floci success "26.46.01 99682b0a0ca3" on abc123 in time'
        )
        expect(source.sleeps.reduce((total, seconds) => total + seconds, 0)).toBe(1200)
    })

    test('fails when the statuses cannot be read', async () => {
        const source = statusSource([], 403)
        await expect(waitForStagingStatus(GATE, source.tools)).rejects.toThrow(
            'Reading the commit statuses failed with HTTP 403'
        )
    })
})
