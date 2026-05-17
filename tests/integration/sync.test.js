import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo, { FyloSyncError } from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const roots = []

async function createRoot(prefix) {
    const root = await createTestRoot(prefix)
    roots.push(root)
    return root
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('sync hooks', () => {
    test('await-sync emits write, patch, and delete events with filesystem paths', async () => {
        const root = await createRoot('fylo-sync-await-')
        const calls = []
        const fylo = new Fylo({
            root,
            sync: {
                onWrite: async (event) => {
                    calls.push({ hook: 'write', ...event })
                },
                onDelete: async (event) => {
                    calls.push({ hook: 'delete', ...event })
                }
            }
        })

        const collection = 'sync-posts'
        await fylo.createCollection(collection)

        const id = await fylo.putData(collection, { title: 'Hello sync' })
        const nextId = await fylo.patchDoc(collection, {
            [id]: { title: 'Hello sync 2' }
        })
        await fylo.delDoc(collection, nextId)

        expect(calls).toEqual([
            {
                hook: 'write',
                operation: 'put',
                collection,
                docId: id,
                path: path.join(
                    root,
                    '.collections',
                    collection,
                    'docs',
                    id.slice(0, 2),
                    `${id}.json`
                ),
                data: { title: 'Hello sync' }
            },
            {
                hook: 'delete',
                operation: 'patch',
                collection,
                docId: id,
                path: path.join(
                    root,
                    '.collections',
                    collection,
                    'docs',
                    id.slice(0, 2),
                    `${id}.json`
                )
            },
            {
                hook: 'write',
                operation: 'patch',
                collection,
                docId: nextId,
                previousDocId: id,
                path: path.join(
                    root,
                    '.collections',
                    collection,
                    'docs',
                    nextId.slice(0, 2),
                    `${nextId}.json`
                ),
                data: { title: 'Hello sync 2' }
            },
            {
                hook: 'delete',
                operation: 'delete',
                collection,
                docId: nextId,
                path: path.join(
                    root,
                    '.collections',
                    collection,
                    'docs',
                    nextId.slice(0, 2),
                    `${nextId}.json`
                )
            }
        ])
    })

    test('worm sync hooks expose lineage/head metadata for append-only patches and tombstones', async () => {
        const root = await createRoot('fylo-sync-worm-')
        const calls = []
        const fylo = new Fylo({
            root,
            worm: {
                mode: 'append-only',
                deletePolicy: 'tombstone'
            },
            sync: {
                onWrite: async (event) => {
                    calls.push({ hook: 'write', ...event })
                },
                onDelete: async (event) => {
                    calls.push({ hook: 'delete', ...event })
                }
            }
        })

        const collection = 'worm-sync-posts'
        await fylo.createCollection(collection)

        const id = await fylo.putData(collection, { title: 'Hello worm sync' })
        const nextId = await fylo.patchDoc(collection, {
            [id]: { title: 'Hello worm sync 2' }
        })
        await fylo.delDoc(collection, nextId)

        expect(calls).toEqual([
            {
                hook: 'write',
                operation: 'put',
                collection,
                docId: id,
                path: path.join(
                    root,
                    '.collections',
                    collection,
                    'docs',
                    id.slice(0, 2),
                    `${id}.json`
                ),
                data: { title: 'Hello worm sync' },
                worm: {
                    lineageId: id,
                    headOperation: 'create',
                    headDocId: id,
                    headPath: path.join(root, '.collections', collection, 'heads', `${id}.json`)
                }
            },
            {
                hook: 'write',
                operation: 'patch',
                collection,
                docId: nextId,
                previousDocId: id,
                path: path.join(
                    root,
                    '.collections',
                    collection,
                    'docs',
                    nextId.slice(0, 2),
                    `${nextId}.json`
                ),
                data: { title: 'Hello worm sync 2' },
                worm: {
                    lineageId: id,
                    headOperation: 'advance',
                    headDocId: nextId,
                    headPath: path.join(root, '.collections', collection, 'heads', `${id}.json`)
                }
            },
            {
                hook: 'delete',
                operation: 'delete',
                collection,
                docId: nextId,
                path: path.join(root, '.collections', collection, 'heads', `${id}.json`),
                worm: {
                    lineageId: id,
                    headOperation: 'delete',
                    headDocId: nextId,
                    headPath: path.join(root, '.collections', collection, 'heads', `${id}.json`),
                    deleteMode: 'tombstone',
                    versionPath: path.join(
                        root,
                        '.collections',
                        collection,
                        'docs',
                        nextId.slice(0, 2),
                        `${nextId}.json`
                    )
                }
            }
        ])
    })

    test('fire-and-forget does not block the local write', async () => {
        const root = await createRoot('fylo-sync-fire-')
        let releaseHook
        let writeStarted = false
        const started = Promise.withResolvers()
        const hookBlocker = new Promise((resolve) => {
            releaseHook = resolve
        })

        const fylo = new Fylo({
            root,
            syncMode: 'fire-and-forget',
            sync: {
                onWrite: async () => {
                    writeStarted = true
                    started.resolve()
                    await hookBlocker
                }
            }
        })

        await fylo.createCollection('fire-posts')

        const putPromise = fylo.putData('fire-posts', { title: 'Fast local write' })
        await started.promise

        const state = await Promise.race([
            putPromise.then(() => 'resolved'),
            Bun.sleep(25).then(() => 'pending')
        ])

        expect(writeStarted).toBe(true)
        expect(state).toBe('resolved')

        releaseHook()
        await putPromise
    })

    test('await-sync surfaces sync failures as FyloSyncError after the local write', async () => {
        const root = await createRoot('fylo-sync-error-')
        let failedDocId
        const fylo = new Fylo({
            root,
            sync: {
                onWrite: async (event) => {
                    failedDocId = event.docId
                    throw new Error('remote unavailable')
                }
            }
        })

        await fylo.createCollection('error-posts')

        await expect(
            fylo.putData('error-posts', { title: 'Still written locally' })
        ).rejects.toBeInstanceOf(FyloSyncError)

        expect(failedDocId).toBeDefined()
        const stored = await fylo.getDoc('error-posts', failedDocId).once()
        expect(stored[failedDocId]).toEqual({ title: 'Still written locally' })
    })
})
