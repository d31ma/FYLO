import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import {
    normalizeImportOptions,
    assertImportUrlAllowed,
    redactImportUrl
} from '../../src/security/import-guard.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-import-sec-')
const fylo = new Fylo(root)
const COLL = 'import-sec-posts'

describe('importBulkData SSRF/rebinding hardening', () => {
    beforeAll(async () => {
        await fylo[COLL].create()
    })

    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })

    test('literal IPv4 loopback is rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://127.0.0.1/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('literal IPv4 unspecified (0.0.0.0) is rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://0.0.0.0/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('literal IPv4 private RFC1918 ranges are rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://10.0.0.1/x.json'))).rejects.toThrow(
            'private address'
        )
        await expect(fylo[COLL].import(new URL('http://192.168.1.1/x.json'))).rejects.toThrow(
            'private address'
        )
        await expect(fylo[COLL].import(new URL('http://172.16.0.1/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('link-local 169.254.0.0/16 (cloud metadata) is rejected', async () => {
        await expect(
            fylo[COLL].import(new URL('http://169.254.169.254/latest/meta-data/'))
        ).rejects.toThrow('private address')
    })

    test('literal IPv6 loopback ([::1]) is rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://[::1]/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('literal IPv6 unspecified ([::]) is rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://[::]/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('literal IPv6 ULA (fc00::/7) is rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://[fc00::1]/x.json'))).rejects.toThrow(
            'private address'
        )
        await expect(fylo[COLL].import(new URL('http://[fd12:3456::1]/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('literal IPv6 link-local (fe80::/10) is rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://[fe80::1]/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('IPv4-mapped IPv6 private addresses are rejected', async () => {
        await expect(fylo[COLL].import(new URL('http://[::ffff:10.0.0.1]/x.json'))).rejects.toThrow(
            'private address'
        )
        await expect(
            fylo[COLL].import(new URL('http://[::ffff:127.0.0.1]/x.json'))
        ).rejects.toThrow('private address')
    })

    test('localhost hostname is rejected without DNS lookup', async () => {
        await expect(fylo[COLL].import(new URL('http://localhost/x.json'))).rejects.toThrow(
            'private address'
        )
        await expect(fylo[COLL].import(new URL('http://foo.localhost/x.json'))).rejects.toThrow(
            'private address'
        )
    })

    test('unlisted protocol is rejected', async () => {
        await expect(fylo[COLL].import(new URL('file:///etc/passwd'))).rejects.toThrow(
            'protocol is not allowed'
        )
    })

    test('allowedHosts pin blocks other hosts', async () => {
        await expect(
            fylo[COLL].import(new URL('http://example.com/x.json'), {
                allowedHosts: ['trusted.example.com']
            })
        ).rejects.toThrow('host is not allowed')
    })

    test('allowPrivateNetwork: true skips private-IP guard (opt-in)', async () => {
        // Should not throw on the private-address guard — will fail later on fetch refused.
        await expect(
            fylo[COLL].import(new URL('http://127.0.0.1:1/x.json'), {
                allowPrivateNetwork: true
            })
        ).rejects.not.toThrow('private address')
    })

    test('assertImportUrlAllowed returns pinned URL with IP host for http/https', async () => {
        const options = normalizeImportOptions(undefined)
        const pin = await assertImportUrlAllowed(new URL('https://1.1.1.1/'), options)
        expect(pin).not.toBeNull()
        expect(pin?.serverName).toBe('1.1.1.1')
        expect(pin?.pinnedUrls).toHaveLength(1)
        expect(pin?.pinnedUrls[0].hostname).toBe('1.1.1.1')
    })

    test('assertImportUrlAllowed returns null for data: URLs (no pinning needed)', async () => {
        const options = normalizeImportOptions(undefined)
        const pin = await assertImportUrlAllowed(new URL('data:application/json,%5B%5D'), options)
        expect(pin).toBeNull()
    })

    test('redactImportUrl strips userinfo, query, and fragment', () => {
        const u = new URL('https://user:secret@example.com/path?token=abc&x=1#frag')
        expect(redactImportUrl(u)).toBe('https://example.com/path')
    })

    test('redactImportUrl returns sentinel for unparseable input', () => {
        expect(redactImportUrl('not a url')).toBe('[unparseable-url]')
    })
})
