// The key a page gives the single-file Worker for an encrypted `.vdbx` file
// (ADR 0025), turned into the bytes `fylo_set_database_key` takes: one flags
// byte, then the secret, or the salt, file key and key-check value derived
// from an HKDF `CryptoKey`. WebCrypto runs once, here, before the engine
// starts, so the engine never waits on it and the page needs no cross-origin
// isolation.

/**
 * @typedef {{ key: CryptoKey | ArrayBuffer | ArrayBufferView | string }} DatabaseEncryption
 * @typedef {{ open(path: string, flags: number): number, close(handle: number): void,
 *   readAt(handle: number, offset: number, into: Uint8Array): number,
 *   stat(path: string): { kind: number, len?: number }, path: string }} KeyBackend
 */

export class VdbxKey {
    static FLAG_DERIVED = 1
    static FLAG_CREATE = 2
    static MINIMUM_SECRET_BYTES = 32
    static SALT_BYTES = 32
    static FILE_KEY_BITS = 256
    static CHECK_BITS = 128
    static SUPERBLOCK_BYTES = 4096
    static SALT_OFFSET = 80
    static INCOMPAT_OFFSET = 20
    static INCOMPAT_ENCRYPTED = 1
    static MAGIC = [0x89, 0x56, 0x44, 0x42, 0x58, 0x0d, 0x0a, 0x1a]
    static FILE_KEY_INFO = 'fylo.vdbx.file-key.v1'
    static CHECK_INFO = 'fylo.vdbx.key-check.v1'
    static UNSUPPORTED = 'EBROWSER_UNSUPPORTED'
    static WRONG_KEY = 'EKEY'

    /**
     * The bytes for the engine, or `null` when the file is not encrypted and
     * needs no key.
     *
     * @param {DatabaseEncryption} encryption
     * @param {KeyBackend} backend
     * @param {boolean} create
     * @returns {Promise<Uint8Array | null>}
     */
    static async material(encryption, backend, create) {
        const key = VdbxKey.#keyOf(encryption)
        if (VdbxKey.#isCryptoKey(key)) return await VdbxKey.#derived(key, backend, create)
        return VdbxKey.#secret(key, create)
    }

    /** @param {unknown} encryption */
    static #keyOf(encryption) {
        const record = /** @type {{ key?: unknown } | null} */ (encryption)
        if (record?.key === undefined || record.key === null) {
            throw VdbxKey.#error(VdbxKey.WRONG_KEY, 'encryption.key is missing')
        }
        return record.key
    }

    /** @param {unknown} key @returns {key is CryptoKey} */
    static #isCryptoKey(key) {
        return typeof CryptoKey === 'function' && key instanceof CryptoKey
    }

    /** @param {unknown} key @param {boolean} create */
    static #secret(key, create) {
        const secret = VdbxKey.#bytesOf(key)
        try {
            if (secret.length < VdbxKey.MINIMUM_SECRET_BYTES) {
                throw VdbxKey.#error(
                    VdbxKey.WRONG_KEY,
                    `the encryption key must be at least ${VdbxKey.MINIMUM_SECRET_BYTES} bytes`
                )
            }
            return VdbxKey.#packed(VdbxKey.#flags(false, create), [secret])
        } finally {
            secret.fill(0)
        }
    }

    /** @param {unknown} key @returns {Uint8Array} */
    static #bytesOf(key) {
        if (typeof key === 'string') return new TextEncoder().encode(key)
        if (key instanceof ArrayBuffer) return new Uint8Array(key.slice(0))
        if (ArrayBuffer.isView(key)) {
            return new Uint8Array(key.buffer.slice(key.byteOffset, key.byteOffset + key.byteLength))
        }
        throw VdbxKey.#error(
            VdbxKey.WRONG_KEY,
            'encryption.key must be bytes, a string or an HKDF CryptoKey'
        )
    }

    /**
     * @param {CryptoKey} key
     * @param {KeyBackend} backend
     * @param {boolean} create
     */
    static async #derived(key, backend, create) {
        VdbxKey.#requireHkdf(key)
        const salt = VdbxKey.#salt(backend, create)
        if (!salt) return null
        const fileKey = await VdbxKey.#derive(
            key,
            salt,
            VdbxKey.FILE_KEY_INFO,
            VdbxKey.FILE_KEY_BITS
        )
        const check = await VdbxKey.#derive(key, salt, VdbxKey.CHECK_INFO, VdbxKey.CHECK_BITS)
        try {
            return VdbxKey.#packed(VdbxKey.#flags(true, create), [salt, fileKey, check])
        } finally {
            fileKey.fill(0)
        }
    }

    /** @param {CryptoKey} key */
    static #requireHkdf(key) {
        if (key.algorithm?.name === 'HKDF' && key.usages.includes('deriveBits')) return
        throw VdbxKey.#error(
            VdbxKey.UNSUPPORTED,
            `the browser engine takes an HKDF CryptoKey with the deriveBits usage, not ${key.algorithm?.name}: ` +
                'using an AES key for every record needs a cross-origin isolated page'
        )
    }

    /**
     * The salt of the file the backend holds, a new one for a file about to
     * be created, or `null` for a file that is not encrypted.
     *
     * @param {KeyBackend} backend
     * @param {boolean} create
     * @returns {Uint8Array | null}
     */
    static #salt(backend, create) {
        const size = backend.stat(backend.path).len ?? 0
        if (size === 0) return VdbxKey.#newSalt(create)
        const superblock = VdbxKey.#superblock(backend, size)
        if (!VdbxKey.#encrypted(superblock)) return null
        return superblock.slice(VdbxKey.SALT_OFFSET, VdbxKey.SALT_OFFSET + VdbxKey.SALT_BYTES)
    }

    /** @param {boolean} create */
    static #newSalt(create) {
        if (!create) throw VdbxKey.#error(VdbxKey.WRONG_KEY, 'the database file is empty')
        return crypto.getRandomValues(new Uint8Array(VdbxKey.SALT_BYTES))
    }

    /** @param {KeyBackend} backend @param {number} size */
    static #superblock(backend, size) {
        const first = VdbxKey.#read(backend, 0, size)
        if (VdbxKey.#hasMagic(first)) return first
        return VdbxKey.#read(backend, VdbxKey.SUPERBLOCK_BYTES, size)
    }

    /** @param {KeyBackend} backend @param {number} offset @param {number} size */
    static #read(backend, offset, size) {
        const bytes = new Uint8Array(Math.max(0, Math.min(VdbxKey.SUPERBLOCK_BYTES, size - offset)))
        const handle = backend.open(backend.path, 0)
        try {
            backend.readAt(handle, offset, bytes)
        } finally {
            backend.close(handle)
        }
        return bytes
    }

    /** @param {Uint8Array} bytes */
    static #hasMagic(bytes) {
        return VdbxKey.MAGIC.every((value, index) => bytes[index] === value)
    }

    /** @param {Uint8Array} superblock */
    static #encrypted(superblock) {
        if (superblock.length < VdbxKey.SALT_OFFSET + VdbxKey.SALT_BYTES) return false
        const view = new DataView(superblock.buffer, superblock.byteOffset, superblock.byteLength)
        return (view.getUint32(VdbxKey.INCOMPAT_OFFSET, true) & VdbxKey.INCOMPAT_ENCRYPTED) !== 0
    }

    /**
     * @param {CryptoKey} key
     * @param {Uint8Array} salt
     * @param {string} info
     * @param {number} bits
     */
    static async #derive(key, salt, info, bits) {
        const algorithm = {
            name: 'HKDF',
            hash: 'SHA-256',
            salt,
            info: new TextEncoder().encode(info)
        }
        return new Uint8Array(await crypto.subtle.deriveBits(algorithm, key, bits))
    }

    /** @param {boolean} derived @param {boolean} create */
    static #flags(derived, create) {
        const derivedFlag = derived ? VdbxKey.FLAG_DERIVED : 0
        return derivedFlag | (create ? VdbxKey.FLAG_CREATE : 0)
    }

    /** @param {number} flags @param {Uint8Array[]} parts */
    static #packed(flags, parts) {
        const length = parts.reduce((total, part) => total + part.length, 1)
        const packed = new Uint8Array(length)
        packed[0] = flags
        let offset = 1
        for (const part of parts) {
            packed.set(part, offset)
            offset += part.length
        }
        return packed
    }

    /** @param {string} code @param {string} message */
    static #error(code, message) {
        return Object.assign(new Error(message), { code })
    }
}
