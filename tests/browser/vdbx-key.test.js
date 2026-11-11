import { describe, expect, test } from 'bun:test'
import { hkdfSync } from 'node:crypto'
import { VdbxKey } from '../../src/browser/vdbx-key.mjs'
import { KIND_FILE, KIND_MISSING } from '../../src/browser/host-vfs.mjs'

class KeySamples {
    static secret = 'a-test-secret-that-is-long-enough-for-fylo-0001'
    static path = '/database.vdbx'

    /** An encrypted superblock: magic, the encrypted flag and a salt. */
    static superblock(salt) {
        const bytes = new Uint8Array(VdbxKey.SUPERBLOCK_BYTES * 2)
        bytes.set(VdbxKey.MAGIC, 0)
        new DataView(bytes.buffer).setUint32(
            VdbxKey.INCOMPAT_OFFSET,
            VdbxKey.INCOMPAT_ENCRYPTED,
            true
        )
        bytes.set(salt, VdbxKey.SALT_OFFSET)
        return bytes
    }

    /** A backend over `bytes`, or over no file at all. */
    static backend(bytes) {
        return {
            path: KeySamples.path,
            stat: () => (bytes ? { kind: KIND_FILE, len: bytes.length } : { kind: KIND_MISSING }),
            open: () => 1,
            close: () => {},
            readAt(handle, offset, into) {
                into.set(bytes.subarray(offset, offset + into.length))
                return into.length
            }
        }
    }

    static async hkdfKey(secret = KeySamples.secret) {
        return await crypto.subtle.importKey(
            'raw',
            new TextEncoder().encode(secret),
            'HKDF',
            false,
            ['deriveBits']
        )
    }

    static async code(action) {
        try {
            await action()
        } catch (error) {
            return error.code
        }
        return 'no error'
    }
}

describe('VdbxKey', () => {
    test('a byte key is passed whole, after its flags', async () => {
        const secret = new TextEncoder().encode(KeySamples.secret)
        const material = await VdbxKey.material({ key: secret }, KeySamples.backend(null), true)
        expect(material[0]).toBe(VdbxKey.FLAG_CREATE)
        expect(new TextDecoder().decode(material.subarray(1))).toBe(KeySamples.secret)
        expect(new TextDecoder().decode(secret)).toBe(KeySamples.secret)
    })

    test('an HKDF CryptoKey gives the file key and check native derives for the salt', async () => {
        const salt = crypto.getRandomValues(new Uint8Array(VdbxKey.SALT_BYTES))
        const backend = KeySamples.backend(KeySamples.superblock(salt))
        const material = await VdbxKey.material({ key: await KeySamples.hkdfKey() }, backend, false)
        const fileKey = new Uint8Array(
            hkdfSync('sha256', KeySamples.secret, salt, VdbxKey.FILE_KEY_INFO, 32)
        )
        const check = new Uint8Array(
            hkdfSync('sha256', KeySamples.secret, salt, VdbxKey.CHECK_INFO, 16)
        )
        expect(material[0]).toBe(VdbxKey.FLAG_DERIVED)
        expect(material.subarray(1, 33)).toEqual(salt)
        expect(material.subarray(33, 65)).toEqual(fileKey)
        expect(material.subarray(65, 81)).toEqual(check)
        expect(material.length).toBe(81)
    })

    test('a new file gets a fresh salt, and a plain file needs no key', async () => {
        const created = await VdbxKey.material(
            { key: await KeySamples.hkdfKey() },
            KeySamples.backend(null),
            true
        )
        expect(created[0]).toBe(VdbxKey.FLAG_DERIVED | VdbxKey.FLAG_CREATE)
        const plain = new Uint8Array(VdbxKey.SUPERBLOCK_BYTES * 2)
        plain.set(VdbxKey.MAGIC, 0)
        const none = await VdbxKey.material(
            { key: await KeySamples.hkdfKey() },
            KeySamples.backend(plain),
            false
        )
        expect(none).toBeNull()
    })

    test('short, missing and wrongly typed keys fail with EKEY', async () => {
        const backend = KeySamples.backend(null)
        expect(await KeySamples.code(() => VdbxKey.material({ key: 'short' }, backend, true))).toBe(
            'EKEY'
        )
        expect(await KeySamples.code(() => VdbxKey.material({}, backend, true))).toBe('EKEY')
        expect(await KeySamples.code(() => VdbxKey.material({ key: 42 }, backend, true))).toBe(
            'EKEY'
        )
    })

    test('an AES CryptoKey is refused as unsupported without isolation', async () => {
        const aes = await crypto.subtle.importKey('raw', new Uint8Array(32), 'AES-GCM', false, [
            'encrypt',
            'decrypt'
        ])
        const code = await KeySamples.code(() =>
            VdbxKey.material({ key: aes }, KeySamples.backend(null), true)
        )
        expect(code).toBe('EBROWSER_UNSUPPORTED')
    })
})
