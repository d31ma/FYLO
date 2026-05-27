/**
 * @typedef {import('./vendor.js').TTID} TTID
 * @typedef {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} StoreJoin
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 */

/**
 * @template T
 * @typedef {AsyncIterable<TTID | Record<TTID, T>> & {
 *   once: <U = T>() => Promise<Record<TTID, U>>,
 *   onDelete: () => AsyncGenerator<TTID, void, unknown>
 * }} GetDocResult
 */

/**
 * @template T
 * @typedef {AsyncIterable<TTID | Record<TTID, T> | Record<string, TTID[]> | Record<TTID, Partial<T>> | undefined> & {
 *   collect: <U = T>() => AsyncGenerator<TTID | Record<TTID, U> | Record<string, TTID[]> | Record<TTID, Partial<U>> | undefined, void, unknown>,
 *   onDelete: () => AsyncGenerator<TTID, void, unknown>
 * }} FindDocsResult
 */

/**
 * @template T
 * @typedef {AsyncIterable<TTID | Record<TTID, T> | undefined> & {
 *   collect: <U = T>() => AsyncGenerator<TTID | Record<TTID, U> | undefined, void, unknown>
 * }} DeletedDocsResult
 */

/**
 * @template {Record<string, any>} T
 * @template {Record<string, any>} U
 * @typedef {TTID[] | Record<string, TTID[]> | Record<string, Record<TTID, Partial<T | U>>> | Record<string, T | U | (T & U) | (Partial<T> & Partial<U>)>} JoinDocsResult
 */

export {}
