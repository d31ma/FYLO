import { afterEach, describe, expect, test } from 'bun:test'
import { normalizeCacheOptions, resolveRedisUrl } from '../../src/cache/query.js'

const originalFyloRedisUrl = process.env.FYLO_REDIS_URL

afterEach(() => {
    if (originalFyloRedisUrl === undefined) delete process.env.FYLO_REDIS_URL
    else process.env.FYLO_REDIS_URL = originalFyloRedisUrl
})

describe('query cache configuration', () => {
    test('cache: true enables memory id caching with safe defaults', () => {
        expect(normalizeCacheOptions(true)).toMatchObject({
            backend: 'memory',
            method: 'cache-aside',
            ttl: 30,
            required: false,
            stampedeProtection: true
        })
    })

    test('supported cache methods describe the cache architecture', () => {
        for (const method of ['cache-aside', 'read-through', 'write-through', 'write-around']) {
            expect(
                normalizeCacheOptions({
                    backend: 'memory',
                    method
                })?.method
            ).toBe(method)
        }
    })

    test('FYLO_REDIS_URL is used only when cache.redis.url is omitted', () => {
        process.env.FYLO_REDIS_URL = 'redis://fylo.example:6379'

        expect(resolveRedisUrl('redis://explicit.example:6379')).toBe(
            'redis://explicit.example:6379'
        )
        expect(resolveRedisUrl(undefined)).toBe('redis://fylo.example:6379')
    })

    test('omitted Redis URL leaves Bun RedisClient to resolve REDIS_URL or VALKEY_URL', () => {
        delete process.env.FYLO_REDIS_URL

        expect(
            normalizeCacheOptions({
                backend: 'redis'
            })?.redisUrl
        ).toBeUndefined()
    })

    test('unsupported cache methods fail closed', () => {
        expect(() =>
            normalizeCacheOptions({
                backend: 'memory',
                // @ts-expect-error exercising runtime validation
                method: 'result'
            })
        ).toThrow('Unsupported FYLO cache method')
    })
})
