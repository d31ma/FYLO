import { describe, expect, test } from 'bun:test'
import { createBrowserFylo } from '../../src/browser/index.js'
import { WordText } from '../../src/browser/core/word-text.js'
import fixture from '../fixtures/word-normalization-v1.json' with { type: 'json' }

const BODIES = [
    'Meeting with Tomás on Monday',
    'Lunch menu: café crème and croissants',
    'The quarterly MEETING notes are attached',
    'Re: Re: meet-up at 2026-11-11?',
    'Straße closed; use the north road',
    'Tom says hello',
    '',
    'Résumé attached for the opening'
]

async function messages() {
    const fylo = createBrowserFylo({ worker: false })
    await fylo.messages.create()
    /** @type {string[]} */
    const ids = []
    for (const [number, body] of BODIES.entries()) {
        ids.push(
            await fylo.messages.put({
                conversation: number % 2 === 0 ? 'c-1' : 'c-2',
                body,
                sentAt: 1_700_000_000_000 + ((number * 7) % 5) * 1000 + number,
                meta: { subject: body }
            })
        )
    }
    ids.push(await fylo.messages.put({ conversation: 'c-2', body: ['agenda', 'meeting room'] }))
    ids.push(await fylo.messages.put({ conversation: 'c-1', body: 42, sentAt: 'soon' }))
    return { fylo, ids, core: /** @type {any} */ (fylo).core }
}

/** @param {any} fylo @param {Record<string, any>} query */
async function found(fylo, query) {
    /** @type {string[]} */
    const ids = []
    for await (const doc of fylo.findDocs('messages', query).collect()) {
        ids.push(...Object.keys(doc))
    }
    return ids
}

/** @param {any} core @param {Record<string, any>} query */
async function scanned(core, query) {
    /** @type {string[]} */
    const ids = []
    for (const id of await core.documents.listDocIds('messages')) {
        const stored = await core.documents.readStoredDoc('messages', id)
        if (stored && core.queryEngine.matchesQuery(id, stored.data, query, stored)) ids.push(id)
    }
    return ids.sort()
}

const MATCH_QUERIES = [
    { $ops: [{ body: { $match: 'meet' } }] },
    { $ops: [{ body: { $match: 'MEETING notes' } }] },
    { $ops: [{ body: { $match: 'meet tom' } }] },
    { $ops: [{ body: { $match: 'café creme' } }] },
    { $ops: [{ body: { $match: 'strasse' } }] },
    { $ops: [{ body: { $match: 'agenda room' } }] },
    { $ops: [{ body: { $match: '42' } }] },
    { $ops: [{ body: { $match: 'meet' }, conversation: { $eq: 'c-1' } }] },
    { $ops: [{ 'meta.subject': { $match: 'lunch' } }] }
]

describe('word index (ADR 0023)', () => {
    test('words match the shared normalization fixture', () => {
        expect(WordText.MAX_WORD_BYTES).toBe(fixture.maxWordBytes)
        for (const entry of fixture.cases) {
            expect(WordText.words(entry.text)).toEqual(entry.words)
        }
    })

    test('$match equals a full scan before and after word fields are set', async () => {
        const { fylo, core } = await messages()
        for (const query of MATCH_QUERIES) {
            expect((await found(fylo, query)).sort()).toEqual(await scanned(core, query))
        }
        expect(
            await core.queryEngine.candidateDocIdsForQuery('messages', MATCH_QUERIES[0])
        ).toBeNull()
        const result = await fylo.setWordFields('messages', ['body', 'meta.subject'])
        expect(result.fields).toEqual(['body', 'meta/subject'])
        expect((await fylo.inspectCollection('messages')).wordFields).toEqual([
            'body',
            'meta/subject'
        ])
        expect(
            await core.queryEngine.candidateDocIdsForQuery('messages', MATCH_QUERIES[0])
        ).toBeInstanceOf(Set)
        for (const query of MATCH_QUERIES) {
            expect((await found(fylo, query)).sort()).toEqual(await scanned(core, query))
        }
    })

    test('word keys follow puts, patches and deletes', async () => {
        const { fylo, ids, core } = await messages()
        await fylo.messages.words(['body'])
        await fylo.patchDoc('messages', { [ids[1]]: { body: 'meeting moved' } })
        await fylo.delDoc('messages', ids[0])
        await fylo.messages.put({ conversation: 'c-3', body: 'Meetings galore' })
        expect(await core.index.completeWordField('messages', 'body')).toBe(true)
        for (const query of MATCH_QUERIES) {
            expect((await found(fylo, query)).sort()).toEqual(await scanned(core, query))
        }
    })

    test('a write without word keys is detected and rebuilt before $match', async () => {
        const { fylo, core } = await messages()
        await fylo.messages.words(['body'])
        core.index.wordFieldsCache.set('messages', [])
        const late = await fylo.messages.put({ conversation: 'c-9', body: 'meeting, late' })
        core.index.wordFieldsCache.delete('messages')
        expect(await core.index.completeWordField('messages', 'body')).toBe(false)
        const query = { $ops: [{ body: { $match: 'meeting' } }] }
        expect(await found(fylo, query)).toContain(late)
        expect(await core.index.completeWordField('messages', 'body')).toBe(true)
        expect((await found(fylo, query)).sort()).toEqual(await scanned(core, query))
    })

    test('malformed $match operands and word field lists are refused', async () => {
        const { fylo } = await messages()
        for (const operand of ['', ' -- ', 42, 'a b c d e f g h i j k l m n o p q']) {
            await expect(found(fylo, { $ops: [{ body: { $match: operand } }] })).rejects.toThrow(
                '$match'
            )
        }
        await expect(fylo.setWordFields('messages', ['body', 'body'])).rejects.toThrow()
        await expect(
            fylo.setWordFields(
                'messages',
                Array.from({ length: 17 }, (_, number) => `field${number}`)
            )
        ).rejects.toThrow()
    })
})

describe('sorted queries (ADR 0023)', () => {
    /** @param {any} core @param {boolean} descending @param {(doc: any) => boolean} [keep] */
    async function expected(core, descending, keep = () => true) {
        /** @type {Array<[number, string]>} */
        const keyed = []
        for (const id of await core.documents.listDocIds('messages')) {
            const stored = await core.documents.readStoredDoc('messages', id)
            if (!stored || !keep(stored.data)) continue
            const value = Number(stored.data.sentAt)
            if (stored.data.sentAt === undefined || !Number.isFinite(value)) continue
            keyed.push([value, id])
        }
        keyed.sort((left, right) => {
            const order = descending ? right[0] - left[0] : left[0] - right[0]
            return order || (left[1] < right[1] ? -1 : 1)
        })
        return keyed.map(([, id]) => id)
    }

    /** @param {any} fylo @param {Record<string, any>} query @param {number} limit */
    async function allPages(fylo, query, limit) {
        /** @type {string[]} */
        const ids = []
        /** @type {Record<string, any>} */
        let next = { ...query, $limit: limit }
        for (;;) {
            const page = await fylo.findPage('messages', next)
            expect(page.items.length).toBeLessThanOrEqual(limit)
            ids.push(...page.items.map((/** @type {any} */ item) => item.id))
            if (!page.nextCursor) return ids
            next = { ...next, $after: page.nextCursor }
        }
    }

    test('pages equal a sorted scan in both directions', async () => {
        const { fylo, core } = await messages()
        for (const descending of [false, true]) {
            const query = { $sort: { sentAt: descending ? 'desc' : 'asc' } }
            const wanted = await expected(core, descending)
            expect(await found(fylo, query)).toEqual(wanted)
            for (const limit of [1, 3, 100]) {
                expect(await allPages(fylo, query, limit)).toEqual(wanted)
            }
        }
    })

    test('a filter and a sort combine, and $onlyIds returns ids', async () => {
        const { fylo, core } = await messages()
        const query = { $ops: [{ conversation: { $eq: 'c-2' } }], $sort: { sentAt: 'desc' } }
        const wanted = await expected(core, true, (doc) => doc.conversation === 'c-2')
        expect(await allPages(fylo, query, 2)).toEqual(wanted)
        const ids = await fylo.findPage('messages', { ...query, $onlyIds: true })
        expect(ids.items).toEqual(wanted)
    })

    test('a cursor from another query or garbage is refused', async () => {
        const { fylo } = await messages()
        const first = await fylo.findPage('messages', { $sort: { sentAt: 'desc' }, $limit: 2 })
        expect(first.nextCursor).toStartWith('fys1.d.')
        for (const query of [
            { $sort: { sentAt: 'asc' }, $after: first.nextCursor },
            { $sort: { body: 'desc' }, $after: first.nextCursor },
            { $after: first.nextCursor },
            { $sort: { sentAt: 'desc' }, $after: 'garbage' }
        ]) {
            await expect(fylo.findPage('messages', query)).rejects.toThrow()
        }
        await expect(fylo.findPage('messages', { $sort: { sentAt: 'up' } })).rejects.toThrow()
    })
})
