import { RedisClient } from 'bun'

/**
 * @typedef {import('../types/vendor.js').TTID} TTID
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 */

/**
 * @typedef {object} FyloMemoryCacheOptions
 * @property {'memory'=} backend
 * @property {'cache-aside' | 'read-through' | 'write-through' | 'write-around'=} method
 * @property {number=} ttl
 * @property {boolean=} stampedeProtection
 */

/**
 * @typedef {object} FyloRedisCacheOptions
 * @property {'redis'} backend
 * @property {'cache-aside' | 'read-through' | 'write-through' | 'write-around'=} method
 * @property {number=} ttl
 * @property {boolean=} required
 * @property {boolean=} stampedeProtection
 * @property {{ url?: string, keyPrefix?: string }=} redis
 */

/**
 * @typedef {boolean | FyloMemoryCacheOptions | FyloRedisCacheOptions} FyloCacheOptions
 */

/**
 * @typedef {object} NormalizedCacheOptions
 * @property {'memory' | 'redis'} backend
 * @property {'cache-aside' | 'read-through' | 'write-through' | 'write-around'} method
 * @property {number} ttl
 * @property {boolean} required
 * @property {boolean} stampedeProtection
 * @property {string} keyPrefix
 * @property {string | undefined} redisUrl
 */

/**
 * @typedef {object} QueryCache
 * @property {boolean} required
 * @property {'cache-aside' | 'read-through' | 'write-through' | 'write-around'} method
 * @property {boolean} stampedeProtection
 * @property {(collection: string) => Promise<number>} version
 * @property {(kind: 'active' | 'deleted', collection: string, version: number, query: StoreQuery | undefined) => string} key
 * @property {(kind: 'active' | 'deleted', collection: string, version: number, query: StoreQuery | undefined) => Promise<TTID[] | null>} getIds
 * @property {(kind: 'active' | 'deleted', collection: string, version: number, query: StoreQuery | undefined, ids: TTID[]) => Promise<void>} setIds
 * @property {(collection: string) => Promise<void>} bumpCollection
 * @property {() => void | Promise<void>=} close
 */

const DEFAULT_CACHE_TTL_SECONDS = 30
const DEFAULT_CACHE_METHOD = 'cache-aside'

/**
 * @param {string} value
 * @returns {string}
 */
function stableHash(value) {
    let hash = 2166136261
    for (let index = 0; index < value.length; index++) {
        hash ^= value.charCodeAt(index)
        hash = Math.imul(hash, 16777619)
    }
    return (hash >>> 0).toString(36)
}

/**
 * @param {unknown} value
 * @returns {unknown}
 */
function sortForStableJson(value) {
    if (Array.isArray(value)) return value.map(sortForStableJson)
    if (!value || typeof value !== 'object') return value
    /** @type {Record<string, unknown>} */
    const sorted = {}
    for (const key of Object.keys(value).sort()) {
        sorted[key] = sortForStableJson(/** @type {Record<string, unknown>} */ (value)[key])
    }
    return sorted
}

/**
 * @param {StoreQuery | undefined} query
 * @returns {string}
 */
function stableQueryKey(query) {
    return stableHash(JSON.stringify(sortForStableJson(query ?? {})))
}

/**
 * @param {FyloCacheOptions | undefined} options
 * @returns {NormalizedCacheOptions | undefined}
 */
export function normalizeCacheOptions(options) {
    if (!options) return undefined
    const config = options === true ? {} : options
    const backend = config.backend ?? 'memory'
    if (backend !== 'memory' && backend !== 'redis') {
        throw new Error(`Unsupported FYLO cache backend: ${backend}`)
    }
    const method = config.method ?? DEFAULT_CACHE_METHOD
    if (
        method !== 'cache-aside' &&
        method !== 'read-through' &&
        method !== 'write-through' &&
        method !== 'write-around'
    ) {
        throw new Error(`Unsupported FYLO cache method: ${method}`)
    }
    const ttl = config.ttl ?? DEFAULT_CACHE_TTL_SECONDS
    if (!Number.isFinite(ttl) || ttl < 0) throw new Error('FYLO cache ttl must be >= 0')
    const redisOptions = 'redis' in config ? config.redis : undefined
    return {
        backend,
        method,
        ttl,
        required: 'required' in config ? config.required === true : false,
        stampedeProtection:
            'stampedeProtection' in config ? config.stampedeProtection !== false : true,
        keyPrefix: redisOptions?.keyPrefix || 'fylo',
        redisUrl: resolveRedisUrl(redisOptions?.url)
    }
}

/**
 * @param {string | undefined} explicitUrl
 * @returns {string | undefined}
 */
export function resolveRedisUrl(explicitUrl) {
    return explicitUrl || process.env.FYLO_REDIS_URL || undefined
}

/**
 * @param {string} keyPrefix
 * @param {string} root
 * @returns {string}
 */
function cacheNamespace(keyPrefix, root) {
    return `${keyPrefix}:${stableHash(root)}`
}

class MemoryQueryCache {
    /** @type {boolean} */
    required = false
    /** @type {'cache-aside' | 'read-through' | 'write-through' | 'write-around'} */
    method
    /** @type {boolean} */
    stampedeProtection
    /** @type {string} */
    namespace
    /** @type {number} */
    ttl
    /** @type {Map<string, number>} */
    versions = new Map()
    /** @type {Map<string, { expiresAt: number, ids: TTID[] }>} */
    entries = new Map()

    /**
     * @param {string} root
     * @param {NormalizedCacheOptions} options
     */
    constructor(root, options) {
        this.namespace = cacheNamespace(options.keyPrefix, root)
        this.ttl = options.ttl
        this.required = options.required
        this.method = options.method
        this.stampedeProtection = options.stampedeProtection
    }

    /** @param {string} collection @returns {string} */
    versionKey(collection) {
        return `${this.namespace}:${collection}:version`
    }

    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {number} version
     * @param {StoreQuery | undefined} query
     * @returns {string}
     */
    key(kind, collection, version, query) {
        return `${this.namespace}:query:${kind}:${collection}:${version}:${stableQueryKey(query)}`
    }

    /** @param {string} collection @returns {Promise<number>} */
    async version(collection) {
        return this.versions.get(this.versionKey(collection)) ?? 0
    }

    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {number} version
     * @param {StoreQuery | undefined} query
     * @returns {Promise<TTID[] | null>}
     */
    async getIds(kind, collection, version, query) {
        const key = this.key(kind, collection, version, query)
        const entry = this.entries.get(key)
        if (!entry) return null
        if (entry.expiresAt <= Date.now()) {
            this.entries.delete(key)
            return null
        }
        return [...entry.ids]
    }

    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {number} version
     * @param {StoreQuery | undefined} query
     * @param {TTID[]} ids
     */
    async setIds(kind, collection, version, query, ids) {
        if (this.ttl === 0) return
        this.entries.set(this.key(kind, collection, version, query), {
            expiresAt: Date.now() + this.ttl * 1000,
            ids: [...ids]
        })
    }

    /** @param {string} collection */
    async bumpCollection(collection) {
        const key = this.versionKey(collection)
        this.versions.set(key, (this.versions.get(key) ?? 0) + 1)
    }
}

class RedisQueryCache {
    /** @type {boolean} */
    required
    /** @type {'cache-aside' | 'read-through' | 'write-through' | 'write-around'} */
    method
    /** @type {boolean} */
    stampedeProtection
    /** @type {string} */
    namespace
    /** @type {number} */
    ttl
    /** @type {RedisClient} */
    client

    /**
     * @param {string} root
     * @param {NormalizedCacheOptions} options
     */
    constructor(root, options) {
        this.namespace = cacheNamespace(options.keyPrefix, root)
        this.ttl = options.ttl
        this.required = options.required
        this.method = options.method
        this.stampedeProtection = options.stampedeProtection
        this.client = options.redisUrl ? new RedisClient(options.redisUrl) : new RedisClient()
    }

    /** @param {string} collection @returns {string} */
    versionKey(collection) {
        return `${this.namespace}:${collection}:version`
    }

    /** @param {string} collection @returns {Promise<number>} */
    async version(collection) {
        const value = await this.client.get(this.versionKey(collection))
        return value ? Number(value) || 0 : 0
    }

    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {number} version
     * @param {StoreQuery | undefined} query
     * @returns {string}
     */
    key(kind, collection, version, query) {
        return `${this.namespace}:query:${kind}:${collection}:${version}:${stableQueryKey(query)}`
    }

    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {number} version
     * @param {StoreQuery | undefined} query
     * @returns {Promise<TTID[] | null>}
     */
    async getIds(kind, collection, version, query) {
        const value = await this.client.get(this.key(kind, collection, version, query))
        if (!value) return null
        const parsed = JSON.parse(value)
        return Array.isArray(parsed) && parsed.every((id) => typeof id === 'string') ? parsed : null
    }

    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {number} version
     * @param {StoreQuery | undefined} query
     * @param {TTID[]} ids
     */
    async setIds(kind, collection, version, query, ids) {
        if (this.ttl === 0) return
        const key = this.key(kind, collection, version, query)
        await this.client.set(key, JSON.stringify(ids))
        await this.client.expire(key, this.ttl)
    }

    /** @param {string} collection */
    async bumpCollection(collection) {
        await this.client.incr(this.versionKey(collection))
    }

    close() {
        this.client.close()
    }
}

/**
 * @param {string} root
 * @param {FyloCacheOptions | undefined} options
 * @returns {QueryCache | undefined}
 */
export function createQueryCache(root, options) {
    const normalized = normalizeCacheOptions(options)
    if (!normalized) return undefined
    return normalized.backend === 'redis'
        ? new RedisQueryCache(root, normalized)
        : new MemoryQueryCache(root, normalized)
}
