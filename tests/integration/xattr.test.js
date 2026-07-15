import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { writeFileSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import {
    getXattr,
    listXattr,
    removeXattr,
    setXattr,
    WindowsAdsManifestStore
} from '../../src/storage/xattr.js'

// Linux requires the user. namespace; macOS accepts it too.
const NAME = 'user.fylo.test'
const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-xattr-'))
const target = path.join(root, 'doc.json')
await Bun.write(target, '{"hello":"world"}')

describe('xattr', () => {
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })
    test('set and get round-trips a string value', () => {
        setXattr(target, NAME, 'metadata-value')
        expect(new TextDecoder().decode(getXattr(target, NAME) ?? new Uint8Array())).toBe(
            'metadata-value'
        )
    })
    test('set and get round-trips binary bytes', () => {
        const bytes = new Uint8Array([0, 1, 2, 255, 0, 42])
        setXattr(target, NAME, bytes)
        expect(getXattr(target, NAME)).toEqual(bytes)
    })
    test('overwrites an existing attribute', () => {
        setXattr(target, NAME, 'first')
        setXattr(target, NAME, 'second')
        expect(new TextDecoder().decode(getXattr(target, NAME) ?? new Uint8Array())).toBe('second')
    })
    test('get returns null for an absent attribute', () => {
        expect(getXattr(target, 'user.fylo.missing')).toBeNull()
    })
    test('list includes the attribute name', () => {
        setXattr(target, NAME, 'listed')
        expect(listXattr(target)).toContain(NAME)
    })
    test('remove deletes the attribute and tolerates absence', () => {
        setXattr(target, NAME, 'doomed')
        removeXattr(target, NAME)
        expect(getXattr(target, NAME)).toBeNull()
        removeXattr(target, NAME)
    })
    test('empty value round-trips as zero bytes', () => {
        setXattr(target, NAME, '')
        expect(getXattr(target, NAME)).toEqual(new Uint8Array())
    })
    test('get throws for a missing file', () => {
        expect(() => getXattr(path.join(root, 'nope.json'), NAME)).toThrow(/getxattr/)
    })
    test('xattr operations reject symbolic links', async () => {
        const link = path.join(root, 'doc-link.json')
        await symlink(target, link)
        expect(() => getXattr(link, NAME)).toThrow(/regular, non-link file/)
        expect(() => setXattr(link, NAME, 'blocked')).toThrow(/regular, non-link file/)
    })
    test('Windows ADS adapter preserves updates and recovers an interrupted manifest copy', async () => {
        const adapterTarget = path.join(root, 'ads-adapter.bin')
        await writeFile(adapterTarget, 'bytes')
        const store = new WindowsAdsManifestStore()
        store.update(adapterTarget, 'setxattr', (attributes) => {
            attributes.alpha = Buffer.from('a').toString('base64')
        })
        store.update(adapterTarget, 'setxattr', (attributes) => {
            attributes.beta = Buffer.from('b').toString('base64')
        })
        expect(store.read(adapterTarget, 'listxattr')).toEqual({
            alpha: Buffer.from('a').toString('base64'),
            beta: Buffer.from('b').toString('base64')
        })

        await writeFile(`${adapterTarget}:fylo.xattrs`, '{interrupted')
        await writeFile(
            `${adapterTarget}:fylo.xattrs.next`,
            JSON.stringify({ recovered: Buffer.from('ok').toString('base64') })
        )
        expect(store.read(adapterTarget, 'getxattr')).toEqual({
            recovered: Buffer.from('ok').toString('base64')
        })
    })
    test('Windows ADS recovery promotes a newer valid next copy over an older valid primary', async () => {
        const adapterTarget = path.join(root, 'ads-valid-primary-crash-window.bin')
        await writeFile(adapterTarget, 'bytes')
        const store = new WindowsAdsManifestStore()
        const oldPrimary = { generation: Buffer.from('old').toString('base64') }
        const committedNext = { generation: Buffer.from('new').toString('base64') }
        await writeFile(`${adapterTarget}:fylo.xattrs`, JSON.stringify(oldPrimary))
        await writeFile(`${adapterTarget}:fylo.xattrs.next`, JSON.stringify(committedNext))

        expect(store.read(adapterTarget, 'getxattr')).toEqual(committedNext)
        expect(JSON.parse(await readFile(`${adapterTarget}:fylo.xattrs`, 'utf8'))).toEqual(
            committedNext
        )
        expect(await Bun.file(`${adapterTarget}:fylo.xattrs.next`).exists()).toBe(false)
    })
    test('Windows ADS lock release never removes a successor owner sentinel', async () => {
        const adapterTarget = path.join(root, 'ads-lock-owner.bin')
        await writeFile(adapterTarget, 'bytes')
        const store = new WindowsAdsManifestStore()
        const successor = { owner: 'successor', ts: Date.now() }
        store.withLock(adapterTarget, 'test', () => {
            writeFileSync(store.lockPath(adapterTarget), JSON.stringify(successor))
        })
        expect(JSON.parse(await readFile(store.lockPath(adapterTarget), 'utf8'))).toEqual(successor)
        await rm(store.lockPath(adapterTarget), { force: true })
    })
    test('Windows ADS lock acquisition reclaims an abandoned owner generation', async () => {
        const adapterTarget = path.join(root, 'ads-stale-lock.bin')
        await writeFile(adapterTarget, 'bytes')
        const store = new WindowsAdsManifestStore({ staleLockMs: 1 })
        await writeFile(
            store.lockPath(adapterTarget),
            JSON.stringify({ owner: 'dead', ts: Date.now() - 60_000 })
        )
        expect(store.read(adapterTarget, 'getxattr')).toEqual({})
        expect(await Bun.file(store.lockPath(adapterTarget)).exists()).toBe(false)
    })
    test('Windows ADS updates serialize across processes without losing attributes', async () => {
        if (process.platform !== 'win32') return
        const concurrentTarget = path.join(root, 'ads-concurrent.bin')
        await writeFile(concurrentTarget, 'bytes')
        const moduleUrl = pathToFileURL(path.resolve('src/storage/xattr.js')).href
        const children = Array.from({ length: 12 }, (_, index) =>
            Bun.spawn(
                [
                    'bun',
                    '-e',
                    `import { setXattr } from ${JSON.stringify(moduleUrl)}; setXattr(process.env.FYLO_XATTR_TARGET, process.env.FYLO_XATTR_NAME, process.env.FYLO_XATTR_NAME)`
                ],
                {
                    cwd: process.cwd(),
                    env: {
                        ...process.env,
                        FYLO_XATTR_TARGET: concurrentTarget,
                        FYLO_XATTR_NAME: `user.fylo.concurrent-${index}`
                    },
                    stdout: 'ignore',
                    stderr: 'pipe'
                }
            )
        )
        for (const child of children) {
            const [exitCode, stderr] = await Promise.all([
                child.exited,
                new Response(child.stderr).text()
            ])
            expect(stderr).toBe('')
            expect(exitCode).toBe(0)
        }
        expect(
            listXattr(concurrentTarget).filter((name) => name.includes('concurrent-'))
        ).toHaveLength(12)
    })
})
