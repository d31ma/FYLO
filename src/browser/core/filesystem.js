/**
 * Host-agnostic filesystem contract for FYLO's browser-targeted runtime.
 *
 * Every browser-core module that touches storage (documents, prefix index, event
 * journal) composes over a `FyloFilesystem`. The same code runs against an
 * in-memory implementation in tests and Node, against OPFS in the browser, and
 * (later) against any other persistence the browser core might be wired into.
 *
 * The contract is intentionally POSIX-flavoured and minimal. It deliberately
 * does NOT promise:
 *   - directory `fsync` (no host can offer it cross-platform)
 *   - mtime writes (OPFS cannot set `lastModified`; deletedAt lives in the
 *     tombstone body instead)
 *   - cross-process atomicity (browser tabs share OPFS without locks; the browser
 *     core uses in-process write lanes and a SharedWorker for serialisation)
 *
 * Paths are POSIX strings rooted at the host's namespace. Hosts MUST treat
 * paths case-sensitively, even on platforms that don't natively (the browser core
 * controls all path generation and matches FYLO's existing layout).
 */

/**
 * @typedef {object} FyloFilesystem
 * @property {(path: string) => Promise<boolean>} exists
 * @property {(path: string) => Promise<boolean>} isDirectory
 * @property {(path: string) => Promise<number>} mtimeMs
 * @property {(path: string, options?: { recursive?: boolean }) => Promise<void>} mkdir
 * @property {(path: string) => Promise<string[]>} list
 * @property {(path: string, options?: { recursive?: boolean }) => Promise<void>} rmdir
 * @property {(path: string) => Promise<string>} readText
 * @property {(path: string) => Promise<Uint8Array>} readBytes
 * @property {(path: string, data: string) => Promise<void>} writeText
 * @property {(path: string, data: Uint8Array) => Promise<void>} writeBytes
 * @property {(path: string, data: string) => Promise<void>} appendText
 * @property {(path: string) => Promise<void>} remove
 * @property {(source: string, target: string) => Promise<void>} move
 * @property {<T>(path: string, body: () => Promise<T>) => Promise<T>=} withSession
 */

/**
 * Wraps a function `body` so it runs once with mutual exclusion against
 * concurrent invocations keyed by the same lane.
 *
 * @template T
 * @param {Map<string, Promise<unknown>>} lanes
 * @param {string} key
 * @param {() => Promise<T>} body
 * @returns {Promise<T>}
 */
export function runInLane(lanes, key, body) {
    const previous = lanes.get(key) ?? Promise.resolve()
    const next = previous.then(
        () => body(),
        () => body()
    )
    lanes.set(
        key,
        next.then(
            () => undefined,
            () => undefined
        )
    )
    return next
}

export {}
