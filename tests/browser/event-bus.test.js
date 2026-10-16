import { expect, test } from 'bun:test'
import { BrowserEventBus } from '../../src/browser/core/browser-event-bus.js'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'

import { EventSamples } from './helpers/event-samples.js'

test('event publication appends exact journal bytes before notifying subscribers', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    await filesystem.mkdir(EventSamples.directory, { recursive: true })
    const event = { ts: 1, action: EventSamples.insertAction, id: EventSamples.id }
    const received = []
    const journalReads = []
    const unsubscribe = events.subscribe(EventSamples.collection, (value) => {
        received.push(value)
        journalReads.push(filesystem.readText(events.journalPath(EventSamples.collection)))
    })
    await events.publish(EventSamples.collection, event)
    expect(await filesystem.readText(events.journalPath(EventSamples.collection))).toBe(
        `${JSON.stringify(event)}\n`
    )
    expect(received).toEqual([event])
    expect(received[0]).toBe(event)
    expect(await Promise.all(journalReads)).toEqual([`${JSON.stringify(event)}\n`])
    unsubscribe()
})

test('unsubscribed and different collection listeners do not receive publications', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    await filesystem.mkdir(EventSamples.directory, { recursive: true })
    const received = []
    const unsubscribe = events.subscribe(EventSamples.collection, (event) => received.push(event))
    const other = events.subscribe('files', (event) => received.push(event))
    unsubscribe()
    await events.publish(EventSamples.collection, {
        ts: 1,
        action: EventSamples.deleteAction,
        id: EventSamples.id
    })
    expect(received).toEqual([])
    other()
})

test('stream subscription is lazy and buffered events retain publication order', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    await filesystem.mkdir(EventSamples.directory, { recursive: true })
    const stream = events.listen(EventSamples.collection)
    await events.publish(EventSamples.collection, {
        ts: 1,
        action: EventSamples.insertAction,
        id: EventSamples.id
    })
    const pending = stream.next()
    const first = {
        ts: EventSamples.secondTimestamp,
        action: EventSamples.insertAction,
        id: EventSamples.id
    }
    const second = { ts: 3, action: 'meta', id: EventSamples.id }
    await events.publish(EventSamples.collection, first)
    await events.publish(EventSamples.collection, second)
    expect(await pending).toEqual({ done: false, value: first })
    expect(await stream.next()).toEqual({ done: false, value: second })
    expect(await stream.return()).toEqual({ done: true, value: undefined })
    await events.publish(EventSamples.collection, {
        ts: 4,
        action: EventSamples.deleteAction,
        id: EventSamples.id
    })
    expect(await stream.next()).toEqual({ done: true, value: undefined })
})

test('failed event serialization does not notify subscribers', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    await filesystem.mkdir(EventSamples.directory, { recursive: true })
    const received = []
    const unsubscribe = events.subscribe(EventSamples.collection, (event) => received.push(event))
    const event = {
        ts: 1,
        action: EventSamples.insertAction,
        id: EventSamples.id,
        doc: { value: 1n }
    }
    await expect(events.publish(EventSamples.collection, event)).rejects.toThrow()
    expect(received).toEqual([])
    expect(await filesystem.exists(events.journalPath(EventSamples.collection))).toBe(false)
    unsubscribe()
})

test('failed filesystem appends do not notify subscribers', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    await filesystem.mkdir(events.journalPath(EventSamples.collection), { recursive: true })
    const received = []
    const unsubscribe = events.subscribe(EventSamples.collection, (event) => received.push(event))
    await expect(
        events.publish(EventSamples.collection, {
            ts: 1,
            action: EventSamples.insertAction,
            id: EventSamples.id
        })
    ).rejects.toThrow()
    expect(received).toEqual([])
    unsubscribe()
})

test('an error sent into a yielded stream closes it without affecting another listener', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    const received = []
    const unsubscribe = events.subscribe(EventSamples.collection, (event) => received.push(event))
    const stream = events.listen(EventSamples.collection)
    const pending = stream.next()
    const event = { ts: 1, action: EventSamples.insertAction, id: EventSamples.id }
    await events.publish(EventSamples.collection, event)
    expect(await pending).toEqual({ done: false, value: event })
    const failure = new Error('consumer stopped')
    await expect(stream.throw(failure)).rejects.toBe(failure)
    expect(await stream.next()).toEqual({ done: true, value: undefined })
    await events.publish(EventSamples.collection, event)
    expect(received).toEqual([event, event])
    unsubscribe()
})

test('a stream resumes waiting after its buffered events are consumed', async () => {
    const filesystem = createMemoryFilesystem()
    const events = new BrowserEventBus(filesystem, (collection) => `/${collection}`)
    const stream = events.listen(EventSamples.collection)
    const firstPending = stream.next()
    const first = { ts: 1, action: EventSamples.insertAction, id: EventSamples.id }
    await events.publish(EventSamples.collection, first)
    expect(await firstPending).toEqual({ done: false, value: first })
    const secondPending = stream.next()
    const second = {
        ts: EventSamples.secondTimestamp,
        action: EventSamples.deleteAction,
        id: EventSamples.id
    }
    await events.publish(EventSamples.collection, second)
    expect(await secondPending).toEqual({ done: false, value: second })
    expect(await stream.return()).toEqual({ done: true, value: undefined })
})
