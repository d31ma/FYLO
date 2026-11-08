import { expect, test } from 'bun:test'
import { BrowserMetadataStore } from '../../src/browser/core/browser-metadata-store.js'
import { MetadataRecord } from '../../src/browser/core/metadata-record.js'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'

import { MetadataSamples } from './helpers/metadata-samples.js'

test.each([[null], [[]], [1], ['text'], [undefined]])(
    'metadata validation rejects non-record input %p',
    (invalid) => {
        expect(() => MetadataRecord.validate(invalid)).toThrow('Metadata must be a plain object')
    }
)

test('metadata validation retains exact encoded size and record boundaries', () => {
    expect(() =>
        MetadataRecord.validate({
            a: MetadataSamples.valueCharacter.repeat(
                MetadataSamples.valueKilobytes * MetadataSamples.bytesPerKilobyte -
                    MetadataSamples.quoteBytes
            )
        })
    ).not.toThrow()
    expect(() =>
        MetadataRecord.validate({
            a: MetadataSamples.valueCharacter.repeat(
                MetadataSamples.valueKilobytes * MetadataSamples.bytesPerKilobyte - 1
            )
        })
    ).toThrow('at most 60 KiB')
    expect(() => MetadataRecord.validate({ a: undefined })).toThrow('JSON-serializable')
    expect(() => MetadataRecord.validate({ '-name': null })).toThrow(MetadataSamples.nameError)
    expect(() => MetadataRecord.validate({ a: null, b: false, c: 0 })).not.toThrow()
    expect(() =>
        MetadataRecord.validate({ [MetadataSamples.nameCharacter.repeat(64)]: true })
    ).not.toThrow()
    expect(() =>
        MetadataRecord.validate({ [MetadataSamples.nameCharacter.repeat(65)]: true })
    ).toThrow(MetadataSamples.nameError)
})

test('metadata store preserves absence, deletion, safe records and monotonic time', async () => {
    const filesystem = createMemoryFilesystem()
    const store = new BrowserMetadataStore(filesystem, (collection) => `/${collection}`)
    const id = MetadataSamples.id
    const missing = await store.read(MetadataSamples.collection, id)
    expect(missing.updatedAt).toBe(0)
    expect(Object.getPrototypeOf(missing.values)).toBeNull()
    const future = Date.now() + 60_000
    await store.write(
        MetadataSamples.collection,
        id,
        { keep: 1, remove: MetadataSamples.quoteBytes },
        future
    )
    const updated = await store.mutate(MetadataSamples.collection, id, {
        remove: null,
        nested: JSON.parse('{"__proto__":{"polluted":true}}')
    })
    expect(updated.updatedAt).toBe(future + 1)
    expect(updated.values.keep).toBe(1)
    expect(Object.hasOwn(updated.values, 'remove')).toBe(false)
    expect(Object.getPrototypeOf(updated.values)).toBeNull()
    expect(Object.getPrototypeOf(updated.values.nested)).toBeNull()
    expect({}.polluted).toBeUndefined()
    expect((await store.read(MetadataSamples.collection, id)).values).toEqual(updated.values)
    expect(() => store.path(MetadataSamples.collection, '../escape')).toThrow('Invalid document ID')
})

test.each(['null', '{}', '{"values":[],"updatedAt":"wrong"}'])(
    'metadata reading applies sidecar defaults for %p',
    async (content) => {
        const filesystem = createMemoryFilesystem()
        const store = new BrowserMetadataStore(filesystem, (collection) => `/${collection}`)
        const id = MetadataSamples.id
        await store.write(MetadataSamples.collection, id, {}, 1)
        await filesystem.writeText(store.path(MetadataSamples.collection, id), content)
        const read = await store.read(MetadataSamples.collection, id)
        expect(read.updatedAt).toBe(0)
        expect(Object.keys(read.values)).toHaveLength(0)
    }
)

test('metadata reading keeps null values and malformed JSON failures', async () => {
    const filesystem = createMemoryFilesystem()
    const store = new BrowserMetadataStore(filesystem, (collection) => `/${collection}`)
    const id = MetadataSamples.id
    await store.write(MetadataSamples.collection, id, {}, 1)
    await filesystem.writeText(store.path(MetadataSamples.collection, id), '{"values":null}')
    expect((await store.read(MetadataSamples.collection, id)).values).toBeNull()
    await filesystem.writeText(store.path(MetadataSamples.collection, id), '{broken')
    await expect(store.read(MetadataSamples.collection, id)).rejects.toThrow()
})
