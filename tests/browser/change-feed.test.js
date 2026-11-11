import { describe, expect, test } from 'bun:test'
import { BrowserCore } from '../../src/browser/core/engine.js'
import { BrowserChangeFeed } from '../../src/browser/core/browser-change-feed.js'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'

class ChangeFeedSamples {
    static collection = 'notes'

    static async core() {
        const fylo = new BrowserCore({ fs: createMemoryFilesystem(), root: '/' })
        await fylo.ready()
        await fylo.createCollection(ChangeFeedSamples.collection)
        return fylo
    }

    /** @param {BrowserCore} fylo @param {number} count */
    static async put(fylo, count) {
        const ids = []
        for (let number = 0; number < count; number += 1) {
            ids.push(await fylo.putData(ChangeFeedSamples.collection, { number }))
        }
        return ids
    }

    /** @param {string} id */
    static putChange(id) {
        return { id, kind: 'put' }
    }

    /** @param {BrowserCore} fylo @param {string} id */
    static async patch(fylo, id) {
        await fylo.patchDoc(ChangeFeedSamples.collection, { [id]: { number: -1 } })
    }

    /** @param {unknown} action */
    static async errorCode(action) {
        try {
            await action()
        } catch (error) {
            return /** @type {{ code?: string }} */ (error).code
        }
        return 'no error'
    }
}

describe('browser change feed', () => {
    test('pages follow the cursor and say whether more follow', async () => {
        const fylo = await ChangeFeedSamples.core()
        const ids = await ChangeFeedSamples.put(fylo, 3)
        const first = await fylo.changes(ChangeFeedSamples.collection, null, 2)
        expect(first.changes).toEqual([ids[0], ids[1]].map(ChangeFeedSamples.putChange))
        expect(first.more).toBe(true)
        const second = await fylo.changes(ChangeFeedSamples.collection, first.cursor, 2)
        expect(second.changes).toEqual([ChangeFeedSamples.putChange(ids[2])])
        expect(second.more).toBe(false)
        const empty = await fylo.changes(ChangeFeedSamples.collection, second.cursor, 2)
        expect(empty.changes).toEqual([])
        expect(empty.cursor).toBe(second.cursor)
    })

    test('a document appears once, at its newest change, with its newest kind', async () => {
        const fylo = await ChangeFeedSamples.core()
        const ids = await ChangeFeedSamples.put(fylo, 3)
        const start = await fylo.changes(ChangeFeedSamples.collection)
        await ChangeFeedSamples.patch(fylo, ids[0])
        await fylo.delDoc(ChangeFeedSamples.collection, ids[1])
        const later = await fylo.changes(ChangeFeedSamples.collection, start.cursor)
        expect(later.changes.map((change) => change.kind)).toEqual(['put', 'delete'])
        expect(later.changes[1].id).toBe(ids[1])
        const everything = await fylo.changes(ChangeFeedSamples.collection)
        expect(everything.changes.map((change) => change.id).sort()).toEqual(
            [...new Set(everything.changes.map((change) => change.id))].sort()
        )
    })

    test('a cursor from an empty collection stays valid after the first write', async () => {
        const fylo = await ChangeFeedSamples.core()
        const empty = await fylo.changes(ChangeFeedSamples.collection)
        const ids = await ChangeFeedSamples.put(fylo, 1)
        const later = await fylo.changes(ChangeFeedSamples.collection, empty.cursor)
        expect(later.changes).toEqual([ChangeFeedSamples.putChange(ids[0])])
    })

    test('a collection created again expires old cursors', async () => {
        const fylo = await ChangeFeedSamples.core()
        await ChangeFeedSamples.put(fylo, 2)
        const start = await fylo.changes(ChangeFeedSamples.collection)
        await fylo.dropCollection(ChangeFeedSamples.collection)
        await fylo.createCollection(ChangeFeedSamples.collection)
        await ChangeFeedSamples.put(fylo, 3)
        const code = await ChangeFeedSamples.errorCode(() =>
            fylo.changes(ChangeFeedSamples.collection, start.cursor)
        )
        expect(code).toBe('ECURSOR_EXPIRED')
    })

    test('bad cursors and limits are refused with stable codes', async () => {
        expect(await ChangeFeedSamples.errorCode(() => BrowserChangeFeed.page('', 'fqc1.1', 1))).toBe(
            'EINVALIDCURSOR'
        )
        expect(await ChangeFeedSamples.errorCode(() => BrowserChangeFeed.page('', 5, 1))).toBe(
            'EBADREQUEST'
        )
        expect(await ChangeFeedSamples.errorCode(() => BrowserChangeFeed.page('', null, 0))).toBe(
            'EBADREQUEST'
        )
        expect(
            await ChangeFeedSamples.errorCode(() =>
                BrowserChangeFeed.page('', null, BrowserChangeFeed.MAX_LIMIT + 1)
            )
        ).toBe('EBADREQUEST')
    })

    test('a cursor past the end of the journal is expired', async () => {
        const journal = `${JSON.stringify({ ts: 7, action: 'insert', id: 'A' })}\n`
        const code = await ChangeFeedSamples.errorCode(() =>
            BrowserChangeFeed.page(journal, 'fch1.7-A.9.A', 1)
        )
        expect(code).toBe('ECURSOR_EXPIRED')
    })
})
