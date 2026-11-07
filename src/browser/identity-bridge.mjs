// The browser's application identity (ADR 0021, decisions P5-3 and P5-5).
//
// A `webcrypto:<name>` identity is one P-256 key pair kept in the origin's
// IndexedDB as two non-extractable CryptoKeys: an ECDSA key that signs
// records and policies, and an ECDH key that opens the database keys wrapped
// for it (RFC 9180 HPKE). WebCrypto answers only asynchronously, but the
// engine signs and checks records synchronously inside its frame codec, so
// the engine's Worker calls a crypto bridge Worker through a
// `SharedArrayBuffer` and blocks in `Atomics.wait` until it answers: the
// pattern of `opfs-bridge.mjs`, and like it this needs a cross-origin
// isolated page.
//
// Layout: a 4-slot Int32 control block, then the payload area.
//   [0] state   [1] request bytes   [2] status   [3] reply bytes

const STATE = 0
const REQUEST_LEN = 1
const STATUS = 2
const REPLY_LEN = 3
const CONTROL_SLOTS = 4
const STATE_IDLE = 0
const STATE_REQUEST = 1
const STATE_RESPONSE = 2
const BRIDGE_BYTES = 1024 * 1024

const OP_PUBLIC = 1
const OP_SIGN = 2
const OP_VERIFY = 3
const OP_UNWRAP = 4
const OP_WRAP = 5

const DATABASE = 'fylo-identities'
const STORE = 'keys'
const ECDSA = { name: 'ECDSA', namedCurve: 'P-256' }
const ECDH = { name: 'ECDH', namedCurve: 'P-256' }
const SIGNING = { name: 'ECDSA', hash: 'SHA-256' }

/** @typedef {Uint8Array<ArrayBuffer>} Bytes */

export function createIdentityBridgeBuffer() {
    return new SharedArrayBuffer(CONTROL_SLOTS * 4 + BRIDGE_BYTES)
}

// --- RFC 9180 base mode: DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM ---

const encoder = new TextEncoder()
const KEM_SUITE = concat(encoder.encode('KEM'), u16(0x0010))
const HPKE_SUITE = concat(encoder.encode('HPKE'), u16(0x0010), u16(0x0001), u16(0x0002))
const VERSION_LABEL = encoder.encode('HPKE-v1')

/** @param {number} value @returns {Bytes} */
function u16(value) {
    return new Uint8Array([(value >> 8) & 0xff, value & 0xff])
}

/** @param {...Uint8Array} parts @returns {Bytes} */
function concat(...parts) {
    const out = new Uint8Array(parts.reduce((total, part) => total + part.length, 0))
    let at = 0
    for (const part of parts) {
        out.set(part, at)
        at += part.length
    }
    return out
}

/** @param {Bytes} key @param {Bytes} message @returns {Promise<Bytes>} */
async function hmac(key, message) {
    const hmacKey = await crypto.subtle.importKey(
        'raw',
        key.length ? key : new Uint8Array(32),
        { name: 'HMAC', hash: 'SHA-256' },
        false,
        ['sign']
    )
    return new Uint8Array(await crypto.subtle.sign('HMAC', hmacKey, message))
}

/** @param {Bytes} suite @param {Bytes} salt @param {string} label @param {Bytes} ikm */
function labeledExtract(suite, salt, label, ikm) {
    return hmac(salt, concat(VERSION_LABEL, suite, encoder.encode(label), ikm))
}

/** @param {Bytes} suite @param {Bytes} prk @param {string} label @param {Bytes} info @param {number} length */
async function labeledExpand(suite, prk, label, info, length) {
    const labeled = concat(u16(length), VERSION_LABEL, suite, encoder.encode(label), info)
    const out = new Uint8Array(length)
    let previous = new Uint8Array(0)
    for (let block = 1, at = 0; at < length; block += 1) {
        previous = await hmac(prk, concat(previous, labeled, new Uint8Array([block])))
        out.set(previous.subarray(0, Math.min(previous.length, length - at)), at)
        at += previous.length
    }
    return out
}

/** @param {Bytes} dh @param {Bytes} kemContext */
async function sharedSecret(dh, kemContext) {
    const prk = await labeledExtract(KEM_SUITE, new Uint8Array(0), 'eae_prk', dh)
    return labeledExpand(KEM_SUITE, prk, 'shared_secret', kemContext, 32)
}

/** @param {Bytes} secret @param {Bytes} info */
async function keySchedule(secret, info) {
    const empty = new Uint8Array(0)
    const pskIdHash = await labeledExtract(HPKE_SUITE, empty, 'psk_id_hash', empty)
    const infoHash = await labeledExtract(HPKE_SUITE, empty, 'info_hash', info)
    const context = concat(new Uint8Array([0]), pskIdHash, infoHash)
    const extracted = await labeledExtract(HPKE_SUITE, secret, 'secret', empty)
    const key = await labeledExpand(HPKE_SUITE, extracted, 'key', context, 32)
    const nonce = await labeledExpand(HPKE_SUITE, extracted, 'base_nonce', context, 12)
    const aesKey = await crypto.subtle.importKey('raw', key, 'AES-GCM', false, [
        'encrypt',
        'decrypt'
    ])
    return { aesKey, nonce }
}

/** @param {CryptoKey} privateKey @param {Bytes} publicPoint @returns {Promise<Bytes>} */
async function ecdh(privateKey, publicPoint) {
    const publicKey = await crypto.subtle.importKey('raw', publicPoint, ECDH, false, [])
    return new Uint8Array(
        await crypto.subtle.deriveBits({ name: 'ECDH', public: publicKey }, privateKey, 256)
    )
}

/**
 * Wrap `secret` for the 65-byte SEC1 point `recipient`: encapsulated key, then ciphertext.
 *
 * @param {Bytes} recipient @param {Bytes} info @param {Bytes} secret @returns {Promise<Bytes>}
 */
export async function hpkeWrap(recipient, info, secret) {
    const ephemeral = await crypto.subtle.generateKey(ECDH, true, ['deriveBits'])
    const enc = new Uint8Array(await crypto.subtle.exportKey('raw', ephemeral.publicKey))
    const dh = await ecdh(ephemeral.privateKey, recipient)
    const secretKey = await sharedSecret(dh, concat(enc, recipient))
    const { aesKey, nonce } = await keySchedule(secretKey, info)
    const sealed = await crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce }, aesKey, secret)
    return concat(enc, new Uint8Array(sealed))
}

/**
 * Open a wrap made for `privateKey` (ECDH, P-256) whose public point is `own`.
 *
 * @param {CryptoKey} privateKey @param {Bytes} own @param {Bytes} info @param {Bytes} wrapped
 * @returns {Promise<Bytes>}
 */
export async function hpkeUnwrap(privateKey, own, info, wrapped) {
    const enc = wrapped.subarray(0, 65)
    const dh = await ecdh(privateKey, enc)
    const secretKey = await sharedSecret(dh, concat(enc, own))
    const { aesKey, nonce } = await keySchedule(secretKey, info)
    const opened = await crypto.subtle.decrypt(
        { name: 'AES-GCM', iv: nonce },
        aesKey,
        wrapped.subarray(65)
    )
    return new Uint8Array(opened)
}

// --- The origin's identities in IndexedDB ---

/** @returns {Promise<IDBDatabase>} */
function openStore(indexedDB = globalThis.indexedDB) {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(DATABASE, 1)
        request.onupgradeneeded = () => request.result.createObjectStore(STORE)
        request.onsuccess = () => resolve(request.result)
        request.onerror = () => reject(request.error)
    })
}

/**
 * @param {IDBDatabase} database @param {IDBTransactionMode} mode
 * @param {(store: IDBObjectStore) => IDBRequest} act @returns {Promise<any>}
 */
function storeRequest(database, mode, act) {
    return new Promise((resolve, reject) => {
        const request = act(database.transaction(STORE, mode).objectStore(STORE))
        request.onsuccess = () => resolve(request.result)
        request.onerror = () => reject(request.error)
    })
}

/**
 * Load the identity `name`, or undefined.
 *
 * @param {string} name @returns {Promise<StoredIdentity | undefined>}
 */
export async function loadWebCryptoIdentity(name) {
    const database = await openStore()
    return storeRequest(database, 'readonly', (store) => store.get(name))
}

/**
 * Make the identity `name` and keep it in this origin's IndexedDB. Returns its
 * public key (65-byte SEC1 point). An existing identity is never replaced.
 *
 * @param {string} name @returns {Promise<Bytes>}
 */
export async function createWebCryptoIdentity(name) {
    const database = await openStore()
    if (await storeRequest(database, 'readonly', (store) => store.getKey(name))) {
        throw Object.assign(new Error(`webcrypto:${name} already has a key`), {
            code: 'EIDENTITY_EXISTS'
        })
    }
    const pair = await crypto.subtle.generateKey(ECDSA, true, ['sign', 'verify'])
    const pkcs8 = await crypto.subtle.exportKey('pkcs8', pair.privateKey)
    const publicKey = new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey))
    const sign = await crypto.subtle.importKey('pkcs8', pkcs8, ECDSA, false, ['sign'])
    const derive = await crypto.subtle.importKey('pkcs8', pkcs8, ECDH, false, ['deriveBits'])
    await storeRequest(database, 'readwrite', (store) =>
        store.add({ sign, derive, publicKey }, name)
    )
    return publicKey
}

const BASE32 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'

/** @param {Uint8Array} bytes */
function base32(bytes) {
    let text = ''
    let buffer = 0
    let bits = 0
    for (const byte of bytes) {
        buffer = ((buffer << 8) | byte) & 0xffff
        bits += 8
        while (bits >= 5) {
            bits -= 5
            text += BASE32[(buffer >> bits) & 31]
        }
    }
    if (bits > 0) text += BASE32[(buffer << (5 - bits)) & 31]
    return text
}

/** @param {Uint8Array} bytes */
function base64url(bytes) {
    return btoa(String.fromCharCode(...bytes))
        .replaceAll('+', '-')
        .replaceAll('/', '_')
        .replace(/=+$/, '')
}

/**
 * The text forms FYLO names an identity by: its public key
 * (`fpk1:p256:...`, what an admin grants) and its identifier
 * (`fid1:p256:...`, what a trust anchor pins).
 *
 * @param {Bytes} publicKey the 65-byte SEC1 point
 * @returns {Promise<{ publicKey: string, id: string }>}
 */
export async function describeIdentity(publicKey) {
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', publicKey))
    return { publicKey: `fpk1:p256:${base64url(publicKey)}`, id: `fid1:p256:${base32(digest)}` }
}

/**
 * @typedef {{ sign: CryptoKey, derive: CryptoKey, publicKey: Bytes }} StoredIdentity
 * @typedef {{
 *   publicKey: () => Promise<Bytes>,
 *   sign: (message: Bytes) => Promise<Bytes>,
 *   verify: (publicPoint: Bytes, message: Bytes, signature: Bytes) => Promise<boolean>,
 *   unwrap: (info: Bytes, wrapped: Bytes) => Promise<Bytes>,
 *   wrap: (publicPoint: Bytes, info: Bytes, secret: Bytes) => Promise<Bytes>
 * }} IdentityOperations
 */

/**
 * The answers the engine's identity calls need, asynchronously.
 *
 * @param {StoredIdentity} identity @returns {IdentityOperations}
 */
export function identityOperations(identity) {
    return {
        publicKey: async () => identity.publicKey,
        sign: async (message) =>
            new Uint8Array(await crypto.subtle.sign(SIGNING, identity.sign, message)),
        verify: async (publicPoint, message, signature) => {
            const key = await crypto.subtle.importKey('raw', publicPoint, ECDSA, false, ['verify'])
            return crypto.subtle.verify(SIGNING, key, signature, message)
        },
        unwrap: (info, wrapped) => hpkeUnwrap(identity.derive, identity.publicKey, info, wrapped),
        wrap: (publicPoint, info, secret) => hpkeWrap(publicPoint, info, secret)
    }
}

// --- The bridge: engine side (synchronous) and Worker side (asynchronous) ---

/** @param {Uint8Array[]} fields */
function encodeFields(fields) {
    const header = new Uint32Array([fields.length, ...fields.map((field) => field.length)])
    return concat(new Uint8Array(header.buffer), ...fields)
}

/** @param {Bytes} bytes @returns {Bytes[]} */
function decodeFields(bytes) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    const count = view.getUint32(0, true)
    let at = 4 + count * 4
    const fields = []
    for (let index = 0; index < count; index += 1) {
        const length = view.getUint32(4 + index * 4, true)
        fields.push(bytes.slice(at, at + length))
        at += length
    }
    return fields
}

/**
 * The engine side: a `HostIdentity` (see `host-vfs.mjs`) whose every call
 * blocks until the bridge Worker answers.
 *
 * @param {SharedArrayBuffer} buffer @returns {{
 *   publicKey: () => Bytes,
 *   sign: (message: Bytes) => Bytes,
 *   verify: (publicPoint: Bytes, message: Bytes, signature: Bytes) => boolean,
 *   unwrap: (info: Bytes, wrapped: Bytes) => Bytes,
 *   wrap: (publicPoint: Bytes, info: Bytes, secret: Bytes) => Bytes
 * }}
 */
export function createIdentityBridgeBackend(buffer) {
    const control = new Int32Array(buffer, 0, CONTROL_SLOTS)
    const data = new Uint8Array(buffer, CONTROL_SLOTS * 4)

    /** @param {number} op @param {Uint8Array[]} fields @returns {Bytes} */
    function call(op, fields) {
        const request = encodeFields([new Uint8Array([op]), ...fields])
        data.set(request)
        Atomics.store(control, REQUEST_LEN, request.length)
        Atomics.store(control, STATE, STATE_REQUEST)
        Atomics.notify(control, STATE)
        while (Atomics.load(control, STATE) !== STATE_RESPONSE) {
            Atomics.wait(control, STATE, STATE_REQUEST)
        }
        const status = Atomics.load(control, STATUS)
        const reply = data.slice(0, Atomics.load(control, REPLY_LEN))
        Atomics.store(control, STATE, STATE_IDLE)
        if (status !== 0) throw new Error(new TextDecoder().decode(reply))
        return reply
    }

    return {
        publicKey: () => call(OP_PUBLIC, []),
        sign: (message) => call(OP_SIGN, [message]),
        verify: (publicPoint, message, signature) =>
            call(OP_VERIFY, [publicPoint, message, signature])[0] === 1,
        unwrap: (info, wrapped) => call(OP_UNWRAP, [info, wrapped]),
        wrap: (publicPoint, info, secret) => call(OP_WRAP, [publicPoint, info, secret])
    }
}

/** @param {IdentityOperations} operations @param {Bytes} request @returns {Promise<Bytes>} */
async function answer(operations, request) {
    const [op, ...fields] = decodeFields(request)
    switch (op[0]) {
        case OP_PUBLIC:
            return operations.publicKey()
        case OP_SIGN:
            return operations.sign(fields[0])
        case OP_VERIFY:
            return new Uint8Array([
                (await operations.verify(fields[0], fields[1], fields[2])) ? 1 : 0
            ])
        case OP_UNWRAP:
            return operations.unwrap(fields[0], fields[1])
        case OP_WRAP:
            return operations.wrap(fields[0], fields[1], fields[2])
        default:
            throw new Error(`unknown identity bridge operation ${op[0]}`)
    }
}

/**
 * The Worker side: answer the engine's calls with `operations` (from
 * `identityOperations`) until `signal` aborts.
 *
 * @param {SharedArrayBuffer} buffer @param {IdentityOperations} operations @param {AbortSignal=} signal
 */
export async function serveIdentityBridge(buffer, operations, signal) {
    const control = new Int32Array(buffer, 0, CONTROL_SLOTS)
    const data = new Uint8Array(buffer, CONTROL_SLOTS * 4)
    while (!signal?.aborted) {
        if (Atomics.load(control, STATE) !== STATE_REQUEST) {
            await /** @type {any} */ (Atomics).waitAsync(
                control,
                STATE,
                Atomics.load(control, STATE),
                1000
            ).value
            continue
        }
        const request = data.slice(0, Atomics.load(control, REQUEST_LEN))
        let status = 0
        let reply
        try {
            reply = await answer(operations, request)
        } catch (error) {
            status = 1
            reply = encoder.encode(error instanceof Error ? error.message : String(error))
        }
        data.set(reply)
        Atomics.store(control, REPLY_LEN, reply.length)
        Atomics.store(control, STATUS, status)
        Atomics.store(control, STATE, STATE_RESPONSE)
        Atomics.notify(control, STATE)
    }
}
