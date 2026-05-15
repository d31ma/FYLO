import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test'

/** @type {typeof import('../../src/security/cipher.js').Cipher} */
let Cipher

describe('Cipher.configure FYLO_CIPHER_SALT requirement', () => {
    /** @type {string | undefined} */
    let previousSalt
    beforeEach(async () => {
        previousSalt = process.env.FYLO_CIPHER_SALT
        mock.restore()
        ;({ Cipher } = await import(`../../src/security/cipher.js?cipher-salt=${Date.now()}`))
        Cipher.reset()
    })
    afterEach(() => {
        if (previousSalt === undefined) delete process.env.FYLO_CIPHER_SALT
        else process.env.FYLO_CIPHER_SALT = previousSalt
        Cipher.reset()
    })

    test('throws when FYLO_CIPHER_SALT is absent', async () => {
        delete process.env.FYLO_CIPHER_SALT
        await expect(Cipher.configure('any-secret')).rejects.toThrow('FYLO_CIPHER_SALT')
    })

    test('configures successfully when FYLO_CIPHER_SALT is set', async () => {
        process.env.FYLO_CIPHER_SALT = 'deadbeef'.repeat(8)
        await Cipher.configure('any-secret')
        expect(Cipher.isConfigured()).toBe(true)
    })

    test('different FYLO_CIPHER_SALT values produce different blind indexes', async () => {
        process.env.FYLO_CIPHER_SALT = 'aaaaaaaa'.repeat(8)
        await Cipher.configure('same-secret')
        const idxA = await Cipher.blindIndex('user@example.com')
        Cipher.reset()
        process.env.FYLO_CIPHER_SALT = 'bbbbbbbb'.repeat(8)
        await Cipher.configure('same-secret')
        const idxB = await Cipher.blindIndex('user@example.com')
        expect(idxA).not.toBe(idxB)
    })
})
