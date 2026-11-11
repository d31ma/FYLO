// src/browser/host-vfs.mjs
var MODULE_ABI_VERSION = 2;
var KIND_MISSING = 0;
var KIND_FILE = 1;
var KIND_DIRECTORY = 2;
var OPEN_READ = 1 << 0;
var OPEN_WRITE = 1 << 1;
var OPEN_CREATE = 1 << 2;
var OPEN_EXCLUSIVE = 1 << 3;
var OPEN_TRUNCATE = 1 << 4;
var OPEN_APPEND = 1 << 5;
var ENOENT = -2;
var EIO = -5;
var EBADF = -9;
var EACCES = -13;
var EEXIST = -17;
var EROFS = -30;
var ESTALE = -116;
var ENOSYS = -38;
function createHostImports({ memory, backend, identity, onError }) {
  const view = () => new Uint8Array(memory().buffer);
  const data = () => new DataView(memory().buffer);
  const decoder = new TextDecoder;
  const readPath = (pointer, length) => decoder.decode(view().subarray(pointer, pointer + length));
  const guard = (action) => (...args) => {
    try {
      return action(...args);
    } catch (error) {
      onError?.(error instanceof Error ? error.message : String(error));
      return errno(error);
    }
  };
  return {
    fylo_host: {
      open: guard((pathPointer, pathLength, flags, handlePointer) => {
        const handle = backend.open(readPath(pathPointer, pathLength), flags);
        data().setBigUint64(handlePointer, BigInt(handle), true);
        return 0;
      }),
      close: guard((handle) => {
        backend.close(Number(handle));
        return 0;
      }),
      read_at: guard((handle, offset, buffer, length, readPointer) => {
        const read = backend.readAt(Number(handle), Number(offset), view().subarray(buffer, buffer + length));
        data().setUint32(readPointer, read, true);
        return 0;
      }),
      write_at: guard((handle, offset, buffer, length, writtenPointer) => {
        const written = backend.writeAt(Number(handle), Number(offset), view().subarray(buffer, buffer + length));
        data().setUint32(writtenPointer, written, true);
        return 0;
      }),
      truncate: guard((handle, length) => {
        backend.truncate(Number(handle), Number(length));
        return 0;
      }),
      flush: guard((handle) => {
        backend.flush(Number(handle));
        return 0;
      }),
      stat: guard((pathPointer, pathLength, out) => {
        const entry = backend.stat(readPath(pathPointer, pathLength));
        const fields = data();
        fields.setUint32(out, entry.kind, true);
        fields.setBigUint64(out + 8, BigInt(entry.len ?? 0), true);
        fields.setBigUint64(out + 16, BigInt(entry.modifiedMs ?? 0), true);
        return 0;
      }),
      mkdir: guard((pathPointer, pathLength, recursive) => {
        backend.mkdir(readPath(pathPointer, pathLength), recursive !== 0);
        return 0;
      }),
      unlink: guard((pathPointer, pathLength) => {
        backend.unlink(readPath(pathPointer, pathLength));
        return 0;
      }),
      rmdir: guard((pathPointer, pathLength, recursive) => {
        backend.rmdir(readPath(pathPointer, pathLength), recursive !== 0);
        return 0;
      }),
      rename: guard((fromPointer, fromLength, toPointer, toLength) => {
        backend.rename(readPath(fromPointer, fromLength), readPath(toPointer, toLength));
        return 0;
      }),
      read_attrs: guard((pathPointer, pathLength, buffer, capacity, neededPointer) => {
        const manifest = backend.readAttrs(readPath(pathPointer, pathLength));
        data().setUint32(neededPointer, manifest.length, true);
        if (manifest.length <= capacity)
          view().set(manifest, buffer);
        return 0;
      }),
      write_attrs: guard((pathPointer, pathLength, manifest, length) => {
        backend.writeAttrs(readPath(pathPointer, pathLength), view().subarray(manifest, manifest + length));
        return 0;
      }),
      log: (pointer, length) => {
        backend.log?.(readPath(pointer, length));
      },
      now_unix_ms: guard((out) => {
        data().setBigUint64(out, BigInt(backend.nowUnixMs()), true);
        return 0;
      }),
      random: guard((buffer, length) => {
        backend.random(view().subarray(buffer, buffer + length));
        return 0;
      }),
      identity_public: guard((out) => {
        if (!identity)
          return ENOSYS;
        view().set(identity.publicKey(), out);
        return 0;
      }),
      identity_sign: guard((message, length, out) => {
        if (!identity)
          return ENOSYS;
        view().set(identity.sign(view().slice(message, message + length)), out);
        return 0;
      }),
      identity_verify: guard((publicKey, message, length, signature) => {
        if (!identity)
          return ENOSYS;
        const valid = identity.verify(view().slice(publicKey, publicKey + 65), view().slice(message, message + length), view().slice(signature, signature + 64));
        return valid ? 0 : 1;
      }),
      identity_unwrap: guard((info, infoLength, wrapped, wrappedLength, out) => {
        if (!identity)
          return ENOSYS;
        const key = identity.unwrap(view().slice(info, info + infoLength), view().slice(wrapped, wrapped + wrappedLength));
        view().set(key, out);
        return 0;
      }),
      identity_wrap: guard((publicKey, info, infoLength, secret, secretLength, out, capacity, needed) => {
        if (!identity)
          return ENOSYS;
        const wrapped = identity.wrap(view().slice(publicKey, publicKey + 65), view().slice(info, info + infoLength), view().slice(secret, secret + secretLength));
        data().setUint32(needed, wrapped.length, true);
        if (wrapped.length <= capacity)
          view().set(wrapped, out);
        return 0;
      }),
      read_dir: guard((pathPointer, pathLength, buffer, capacity, neededPointer) => {
        const names = backend.readDir(readPath(pathPointer, pathLength));
        const encoded = encodeNames(names);
        data().setUint32(neededPointer, encoded.length, true);
        if (encoded.length <= capacity)
          view().set(encoded, buffer);
        return 0;
      })
    }
  };
}
function encodeNames(names) {
  const encoder = new TextEncoder;
  const parts = names.map((name) => encoder.encode(`${name}\x00`));
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const encoded = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    encoded.set(part, offset);
    offset += part.length;
  }
  return encoded;
}
function errno(error) {
  switch (error?.code) {
    case "ENOENT":
    case "NotFoundError":
      return ENOENT;
    case "EEXIST":
      return EEXIST;
    case "EBADF":
      return EBADF;
    case "EACCES":
      return EACCES;
    case "EROFS":
      return EROFS;
    case "ESTALE":
      return ESTALE;
    default:
      return EIO;
  }
}
function exec(instance, ndjson, trace = () => {}) {
  const { fylo_alloc, fylo_free, fylo_exec, fylo_abi_version, memory } = instance.exports;
  if (fylo_abi_version() !== MODULE_ABI_VERSION) {
    throw new Error(`unsupported FYLO module ABI: ${fylo_abi_version()}`);
  }
  trace("abi-ok");
  const request = new TextEncoder().encode(ndjson);
  const requestPointer = fylo_alloc(request.length);
  trace("allocated");
  new Uint8Array(memory.buffer).set(request, requestPointer);
  trace("request-written");
  const packed = fylo_exec(requestPointer, request.length);
  trace("exec-returned");
  fylo_free(requestPointer, request.length);
  if (packed === 0n)
    return "";
  const pointer = Number(packed >> 32n);
  const length = Number(packed & 0xffffffffn);
  const response = new Uint8Array(memory.buffer).slice(pointer, pointer + length);
  fylo_free(pointer, length);
  return new TextDecoder().decode(response);
}
function configure(instance, settings) {
  const { fylo_configure, fylo_abi_version } = instance.exports;
  if (fylo_abi_version() !== MODULE_ABI_VERSION) {
    throw new Error(`unsupported FYLO module ABI: ${fylo_abi_version()}`);
  }
  const request = new TextEncoder().encode(JSON.stringify(settings));
  sendSetting(instance, fylo_configure, request, "EIDENTITY");
}
function setDatabaseKey(instance, material) {
  try {
    sendSetting(instance, instance.exports.fylo_set_database_key, material, "EKEY");
  } finally {
    material.fill(0);
  }
}
function sendSetting(instance, setting, bytes, code) {
  const { fylo_alloc, fylo_free, memory } = instance.exports;
  const requestPointer = fylo_alloc(bytes.length);
  new Uint8Array(memory.buffer).set(bytes, requestPointer);
  const packed = setting(requestPointer, bytes.length);
  fylo_free(requestPointer, bytes.length);
  if (packed === 0n)
    return;
  const pointer = Number(packed >> 32n);
  const length = Number(packed & 0xffffffffn);
  const reason = new TextDecoder().decode(new Uint8Array(memory.buffer).slice(pointer, pointer + length));
  fylo_free(pointer, length);
  throw Object.assign(new Error(reason), { code });
}

// src/browser/vdbx-key.mjs
class VdbxKey {
  static FLAG_DERIVED = 1;
  static FLAG_CREATE = 2;
  static MINIMUM_SECRET_BYTES = 32;
  static SALT_BYTES = 32;
  static FILE_KEY_BITS = 256;
  static CHECK_BITS = 128;
  static SUPERBLOCK_BYTES = 4096;
  static SALT_OFFSET = 80;
  static INCOMPAT_OFFSET = 20;
  static INCOMPAT_ENCRYPTED = 1;
  static MAGIC = [137, 86, 68, 66, 88, 13, 10, 26];
  static FILE_KEY_INFO = "fylo.vdbx.file-key.v1";
  static CHECK_INFO = "fylo.vdbx.key-check.v1";
  static UNSUPPORTED = "EBROWSER_UNSUPPORTED";
  static WRONG_KEY = "EKEY";
  static async material(encryption, backend, create) {
    const key = VdbxKey.#keyOf(encryption);
    if (VdbxKey.#isCryptoKey(key))
      return await VdbxKey.#derived(key, backend, create);
    return VdbxKey.#secret(key, create);
  }
  static #keyOf(encryption) {
    const record = encryption;
    if (record?.key === undefined || record.key === null) {
      throw VdbxKey.#error(VdbxKey.WRONG_KEY, "encryption.key is missing");
    }
    return record.key;
  }
  static #isCryptoKey(key) {
    return typeof CryptoKey === "function" && key instanceof CryptoKey;
  }
  static #secret(key, create) {
    const secret = VdbxKey.#bytesOf(key);
    try {
      if (secret.length < VdbxKey.MINIMUM_SECRET_BYTES) {
        throw VdbxKey.#error(VdbxKey.WRONG_KEY, `the encryption key must be at least ${VdbxKey.MINIMUM_SECRET_BYTES} bytes`);
      }
      return VdbxKey.#packed(VdbxKey.#flags(false, create), [secret]);
    } finally {
      secret.fill(0);
    }
  }
  static #bytesOf(key) {
    if (typeof key === "string")
      return new TextEncoder().encode(key);
    if (key instanceof ArrayBuffer)
      return new Uint8Array(key.slice(0));
    if (ArrayBuffer.isView(key)) {
      return new Uint8Array(key.buffer.slice(key.byteOffset, key.byteOffset + key.byteLength));
    }
    throw VdbxKey.#error(VdbxKey.WRONG_KEY, "encryption.key must be bytes, a string or an HKDF CryptoKey");
  }
  static async#derived(key, backend, create) {
    VdbxKey.#requireHkdf(key);
    const salt = VdbxKey.#salt(backend, create);
    if (!salt)
      return null;
    const fileKey = await VdbxKey.#derive(key, salt, VdbxKey.FILE_KEY_INFO, VdbxKey.FILE_KEY_BITS);
    const check = await VdbxKey.#derive(key, salt, VdbxKey.CHECK_INFO, VdbxKey.CHECK_BITS);
    try {
      return VdbxKey.#packed(VdbxKey.#flags(true, create), [salt, fileKey, check]);
    } finally {
      fileKey.fill(0);
    }
  }
  static #requireHkdf(key) {
    if (key.algorithm?.name === "HKDF" && key.usages.includes("deriveBits"))
      return;
    throw VdbxKey.#error(VdbxKey.UNSUPPORTED, `the browser engine takes an HKDF CryptoKey with the deriveBits usage, not ${key.algorithm?.name}: ` + "using an AES key for every record needs a cross-origin isolated page");
  }
  static #salt(backend, create) {
    const size = backend.stat(backend.path).len ?? 0;
    if (size === 0)
      return VdbxKey.#newSalt(create);
    const superblock = VdbxKey.#superblock(backend, size);
    if (!VdbxKey.#encrypted(superblock))
      return null;
    return superblock.slice(VdbxKey.SALT_OFFSET, VdbxKey.SALT_OFFSET + VdbxKey.SALT_BYTES);
  }
  static #newSalt(create) {
    if (!create)
      throw VdbxKey.#error(VdbxKey.WRONG_KEY, "the database file is empty");
    return crypto.getRandomValues(new Uint8Array(VdbxKey.SALT_BYTES));
  }
  static #superblock(backend, size) {
    const first = VdbxKey.#read(backend, 0, size);
    if (VdbxKey.#hasMagic(first))
      return first;
    return VdbxKey.#read(backend, VdbxKey.SUPERBLOCK_BYTES, size);
  }
  static #read(backend, offset, size) {
    const bytes = new Uint8Array(Math.max(0, Math.min(VdbxKey.SUPERBLOCK_BYTES, size - offset)));
    const handle = backend.open(backend.path, 0);
    try {
      backend.readAt(handle, offset, bytes);
    } finally {
      backend.close(handle);
    }
    return bytes;
  }
  static #hasMagic(bytes) {
    return VdbxKey.MAGIC.every((value, index) => bytes[index] === value);
  }
  static #encrypted(superblock) {
    if (superblock.length < VdbxKey.SALT_OFFSET + VdbxKey.SALT_BYTES)
      return false;
    const view = new DataView(superblock.buffer, superblock.byteOffset, superblock.byteLength);
    return (view.getUint32(VdbxKey.INCOMPAT_OFFSET, true) & VdbxKey.INCOMPAT_ENCRYPTED) !== 0;
  }
  static async#derive(key, salt, info, bits) {
    const algorithm = {
      name: "HKDF",
      hash: "SHA-256",
      salt,
      info: new TextEncoder().encode(info)
    };
    return new Uint8Array(await crypto.subtle.deriveBits(algorithm, key, bits));
  }
  static #flags(derived, create) {
    const derivedFlag = derived ? VdbxKey.FLAG_DERIVED : 0;
    return derivedFlag | (create ? VdbxKey.FLAG_CREATE : 0);
  }
  static #packed(flags, parts) {
    const length = parts.reduce((total, part) => total + part.length, 1);
    const packed = new Uint8Array(length);
    packed[0] = flags;
    let offset = 1;
    for (const part of parts) {
      packed.set(part, offset);
      offset += part.length;
    }
    return packed;
  }
  static #error(code, message) {
    return Object.assign(new Error(message), { code });
  }
}

// src/browser/identity-bridge.mjs
var STATE = 0;
var REQUEST_LEN = 1;
var STATUS = 2;
var REPLY_LEN = 3;
var CONTROL_SLOTS = 4;
var STATE_IDLE = 0;
var STATE_REQUEST = 1;
var STATE_RESPONSE = 2;
var BRIDGE_BYTES = 1024 * 1024;
var OP_PUBLIC = 1;
var OP_SIGN = 2;
var OP_VERIFY = 3;
var OP_UNWRAP = 4;
var OP_WRAP = 5;
function createIdentityBridgeBuffer() {
  return new SharedArrayBuffer(CONTROL_SLOTS * 4 + BRIDGE_BYTES);
}
var encoder = new TextEncoder;
var KEM_SUITE = concat(encoder.encode("KEM"), u16(16));
var HPKE_SUITE = concat(encoder.encode("HPKE"), u16(16), u16(1), u16(2));
var VERSION_LABEL = encoder.encode("HPKE-v1");
function u16(value) {
  return new Uint8Array([value >> 8 & 255, value & 255]);
}
function concat(...parts) {
  const out = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}
var BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
function base32(bytes) {
  let text = "";
  let buffer = 0;
  let bits = 0;
  for (const byte of bytes) {
    buffer = (buffer << 8 | byte) & 65535;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      text += BASE32[buffer >> bits & 31];
    }
  }
  if (bits > 0)
    text += BASE32[buffer << 5 - bits & 31];
  return text;
}
function base64url(bytes) {
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
async function describeIdentity(publicKey) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", publicKey));
  return { publicKey: `fpk1:p256:${base64url(publicKey)}`, id: `fid1:p256:${base32(digest)}` };
}
function encodeFields(fields) {
  const header = new Uint32Array([fields.length, ...fields.map((field) => field.length)]);
  return concat(new Uint8Array(header.buffer), ...fields);
}
function createIdentityBridgeBackend(buffer) {
  const control = new Int32Array(buffer, 0, CONTROL_SLOTS);
  const data = new Uint8Array(buffer, CONTROL_SLOTS * 4);
  function call(op, fields) {
    const request = encodeFields([new Uint8Array([op]), ...fields]);
    data.set(request);
    Atomics.store(control, REQUEST_LEN, request.length);
    Atomics.store(control, STATE, STATE_REQUEST);
    Atomics.notify(control, STATE);
    while (Atomics.load(control, STATE) !== STATE_RESPONSE) {
      Atomics.wait(control, STATE, STATE_REQUEST);
    }
    const status = Atomics.load(control, STATUS);
    const reply = data.slice(0, Atomics.load(control, REPLY_LEN));
    Atomics.store(control, STATE, STATE_IDLE);
    if (status !== 0)
      throw new Error(new TextDecoder().decode(reply));
    return reply;
  }
  return {
    publicKey: () => call(OP_PUBLIC, []),
    sign: (message) => call(OP_SIGN, [message]),
    verify: (publicPoint, message, signature) => call(OP_VERIFY, [publicPoint, message, signature])[0] === 1,
    unwrap: (info, wrapped) => call(OP_UNWRAP, [info, wrapped]),
    wrap: (publicPoint, info, secret) => call(OP_WRAP, [publicPoint, info, secret])
  };
}

// src/browser/indexed-db-store.mjs
class IndexedDbStore {
  static DATABASE_NAME = "fylo-vdbx";
  static DATABASE_VERSION = 1;
  static FILES = "files";
  static PAGES = "pages";
  static OWNERS = "owners";
  static LOCK_PREFIX = "fylo-vdbx-idb:";
  static READY = "ready";
  static LAST_PAGE = Number.MAX_SAFE_INTEGER;
  static async remove(name, options = {}) {
    const database = await IndexedDbStore.openDatabase(options.indexedDB);
    const lock = await IndexedDbStore.takeWebLock(name, options.locks);
    try {
      await IndexedDbStore.run(database, "readwrite", (stores, fail) => {
        stores.owners.get(name).onsuccess = (event) => {
          const holder = IndexedDbStore.resultOf(event);
          if (IndexedDbStore.heldByOther(holder, "", lock.held)) {
            return fail(IndexedDbStore.busy(name));
          }
          IndexedDbStore.deleteAll(stores, name);
        };
      });
    } finally {
      lock.release();
      database.close();
    }
  }
  static async list(options = {}) {
    const database = await IndexedDbStore.openDatabase(options.indexedDB);
    try {
      let records = [];
      await IndexedDbStore.run(database, "readonly", (stores) => {
        stores.files.getAll().onsuccess = (event) => {
          records = IndexedDbStore.resultOf(event);
        };
      });
      return IndexedDbStore.readyCopies(records);
    } finally {
      database.close();
    }
  }
  static openDatabase(given) {
    const factory = given ?? globalThis.indexedDB;
    if (!factory) {
      return Promise.reject(IndexedDbStore.coded(new Error("IndexedDB is unavailable here"), "ENOSYS"));
    }
    return new Promise((resolve, reject) => {
      const request = factory.open(IndexedDbStore.DATABASE_NAME, IndexedDbStore.DATABASE_VERSION);
      request.onupgradeneeded = () => IndexedDbStore.createStores(request.result);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(IndexedDbStore.coded(new Error(`IndexedDB refused to open: ${request.error?.name}`), "EIO"));
      request.onblocked = () => reject(IndexedDbStore.coded(new Error("IndexedDB is blocked by an older version"), "EBUSY"));
    });
  }
  static createStores(database) {
    database.createObjectStore(IndexedDbStore.FILES, { keyPath: "name" });
    database.createObjectStore(IndexedDbStore.PAGES);
    database.createObjectStore(IndexedDbStore.OWNERS, { keyPath: "name" });
  }
  static async takeWebLock(name, given) {
    const locks = given === undefined ? globalThis.navigator?.locks : given;
    if (!locks)
      return { held: false, release: () => {} };
    return new Promise((resolve, reject) => {
      locks.request(IndexedDbStore.LOCK_PREFIX + name, { mode: "exclusive", ifAvailable: true }, (lock) => IndexedDbStore.holdLock(name, lock, resolve, reject)).catch(reject);
    });
  }
  static holdLock(name, lock, resolve, reject) {
    if (!lock) {
      reject(IndexedDbStore.busy(name));
      return;
    }
    return new Promise((release) => resolve({ held: true, release: () => release(undefined) }));
  }
  static heldByOther(holder, owner, lockHeld) {
    if (!holder || holder.owner === owner || lockHeld)
      return false;
    return holder.expiresAt > Date.now();
  }
  static pagesOf(name) {
    return IDBKeyRange.bound([name, 0], [name, IndexedDbStore.LAST_PAGE]);
  }
  static deleteAll(stores, name) {
    stores.pages.delete(IndexedDbStore.pagesOf(name));
    stores.files.delete(name);
    stores.owners.delete(name);
  }
  static readyCopies(records) {
    return records.filter((record) => record.state === IndexedDbStore.READY).map((record) => ({
      name: record.name,
      size: record.size,
      modifiedMs: record.modifiedMs
    })).sort((a, b) => b.modifiedMs - a.modifiedMs);
  }
  static run(database, mode, body) {
    return new Promise((resolve, reject) => {
      const names = [IndexedDbStore.FILES, IndexedDbStore.PAGES, IndexedDbStore.OWNERS];
      const transaction = database.transaction(names, mode, { durability: "strict" });
      let reason = null;
      const fail = (error) => {
        reason = error;
        transaction.abort();
      };
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(reason ?? transaction.error ?? new Error("IndexedDB aborted the transaction"));
      body(IndexedDbStore.storesOf(transaction, names), fail);
    });
  }
  static storesOf(transaction, names) {
    return Object.fromEntries(names.map((name) => [name, transaction.objectStore(name)]));
  }
  static resultOf(event) {
    return event.target.result;
  }
  static busy(name) {
    return IndexedDbStore.coded(new Error(`${name} is open in another tab or window`), "EBUSY");
  }
  static coded(error, code) {
    return Object.assign(error, { code });
  }
}

// src/browser/indexed-db-file.mjs
class IndexedDbFile {
  static PAGE_SIZE = 16 * 1024;
  static LEASE_MS = 6000;
  static RENEW_MS = 2000;
  static COPY_COMMIT_BYTES = 4 * 1024 * 1024;
  static COPYING = "copying";
  constructor({ database, name, owner, record, pages, lease, releaseLock }) {
    this.database = database;
    this.name = name;
    this.owner = owner;
    this.pageSize = record.pageSize;
    this.length = record.size;
    this.shrunkTo = Infinity;
    this.modified = record.modifiedMs;
    this.state = record.state;
    this.pages = pages;
    this.dirtyPages = new Set;
    this.recordDirty = false;
    this.failure = null;
    this.lease = lease;
    this.releaseLock = releaseLock;
    this.renewTimer = undefined;
  }
  static async open(name, options = {}) {
    const lease = IndexedDbFile.leaseOf(options);
    const database = await IndexedDbStore.openDatabase(options.indexedDB);
    const lock = await IndexedDbStore.takeWebLock(name, options.locks);
    const owner = IndexedDbFile.newOwnerId();
    try {
      const record = await IndexedDbFile.claim(database, name, owner, lock.held, options, lease);
      const pages = await IndexedDbFile.loadPages(database, name, Boolean(options.copyFrom));
      const file = new IndexedDbFile({
        database,
        name,
        owner,
        record,
        pages,
        lease,
        releaseLock: lock.release
      });
      await file.copyIn(options);
      file.startRenewing();
      return file;
    } catch (error) {
      await IndexedDbFile.giveUp(database, name, owner, lock.release);
      throw error;
    }
  }
  size() {
    return this.length;
  }
  modifiedMs() {
    return this.modified;
  }
  readAt(offset, into) {
    const end = Math.min(offset + into.length, this.length);
    let position = offset;
    while (position < end) {
      position = this.readPagePart(position, end, into.subarray(position - offset));
    }
    return Math.max(0, end - offset);
  }
  writeAt(offset, from) {
    this.refuseWhenFailed();
    const end = offset + from.length;
    let position = offset;
    while (position < end) {
      position = this.writePagePart(position, end, from.subarray(position - offset));
    }
    this.length = Math.max(this.length, end);
    this.touch();
    return from.length;
  }
  truncate(length) {
    this.refuseWhenFailed();
    if (length < this.length)
      this.dropPagesFrom(length);
    this.shrunkTo = Math.min(this.shrunkTo, length);
    this.length = length;
    this.touch();
  }
  hasChanges() {
    return this.recordDirty;
  }
  async commit() {
    this.refuseWhenFailed();
    if (!this.recordDirty)
      return;
    try {
      await this.writeChanges();
    } catch (error) {
      this.failure = IndexedDbFile.commitFailure(error);
      throw this.failure;
    }
  }
  async close() {
    clearInterval(this.renewTimer);
    try {
      await this.releaseLease();
    } finally {
      this.releaseLock();
      this.database.close();
    }
  }
  refuseWhenFailed() {
    if (this.failure)
      throw this.failure;
  }
  touch() {
    this.modified = Date.now();
    this.recordDirty = true;
  }
  readPagePart(position, end, into) {
    const index = Math.floor(position / this.pageSize);
    const start = position - index * this.pageSize;
    const count = Math.min(end - position, this.pageSize - start);
    const page = this.pages.get(index);
    if (page)
      into.set(page.subarray(start, start + count));
    else
      into.fill(0, 0, count);
    return position + count;
  }
  writePagePart(position, end, from) {
    const index = Math.floor(position / this.pageSize);
    const start = position - index * this.pageSize;
    const count = Math.min(end - position, this.pageSize - start);
    this.pageForWrite(index).set(from.subarray(0, count), start);
    this.dirtyPages.add(index);
    return position + count;
  }
  pageForWrite(index) {
    const found = this.pages.get(index);
    if (found)
      return found;
    const page = new Uint8Array(this.pageSize);
    this.pages.set(index, page);
    return page;
  }
  dropPagesFrom(length) {
    const keep = Math.ceil(length / this.pageSize);
    for (const index of [...this.pages.keys()]) {
      if (index >= keep)
        this.pages.delete(index);
    }
    this.zeroTailOf(length);
  }
  zeroTailOf(length) {
    const index = Math.floor(length / this.pageSize);
    const page = this.pages.get(index);
    if (!page)
      return;
    page.fill(0, length - index * this.pageSize);
    this.dirtyPages.add(index);
  }
  async writeChanges() {
    const dirty = [...this.dirtyPages];
    const record = this.fileRecord();
    await IndexedDbStore.run(this.database, "readwrite", (stores, fail) => {
      stores.owners.get(this.name).onsuccess = (event) => {
        const holder = IndexedDbStore.resultOf(event);
        if (holder?.owner !== this.owner)
          return fail(IndexedDbFile.lockLost(this.name));
        stores.owners.put(this.ownerRecord());
        this.deleteDroppedPages(stores.pages);
        this.putPages(stores.pages, dirty);
        stores.files.put(record);
      };
    });
    this.forgetCommitted(dirty);
  }
  putPages(pages, dirty) {
    for (const index of dirty)
      this.putPage(pages, index);
  }
  putPage(pages, index) {
    const page = this.pages.get(index);
    if (!page || IndexedDbFile.isZero(page)) {
      pages.delete([this.name, index]);
      return;
    }
    pages.put(page.buffer, [this.name, index]);
  }
  deleteDroppedPages(pages) {
    if (this.shrunkTo === Infinity)
      return;
    const first = Math.ceil(this.shrunkTo / this.pageSize);
    pages.delete(IDBKeyRange.bound([this.name, first], [this.name, IndexedDbStore.LAST_PAGE]));
  }
  forgetCommitted(committed) {
    for (const index of committed)
      this.dirtyPages.delete(index);
    this.shrunkTo = Infinity;
    this.recordDirty = this.dirtyPages.size > 0;
  }
  fileRecord() {
    return {
      name: this.name,
      size: this.length,
      pageSize: this.pageSize,
      state: this.state,
      modifiedMs: this.modified
    };
  }
  ownerRecord() {
    return { name: this.name, owner: this.owner, expiresAt: Date.now() + this.lease.leaseMs };
  }
  async copyIn(options) {
    if (!options.copyFrom)
      return;
    const reader = options.copyFrom.stream().getReader();
    let copied = 0;
    let uncommitted = 0;
    for (let chunk = await reader.read();!chunk.done; chunk = await reader.read()) {
      this.writeAt(copied, chunk.value);
      copied += chunk.value.length;
      uncommitted = await this.commitCopyChunk(uncommitted + chunk.value.length);
      options.onProgress?.({ copied, total: options.copyFrom.size });
    }
    await this.finishCopy(copied, options.copyFrom.size);
  }
  async commitCopyChunk(uncommitted) {
    if (uncommitted < IndexedDbFile.COPY_COMMIT_BYTES)
      return uncommitted;
    await this.commit();
    return 0;
  }
  async finishCopy(copied, total) {
    if (copied !== total)
      throw new Error(`copied ${copied} of ${total} bytes`);
    this.state = IndexedDbStore.READY;
    this.touch();
    await this.commit();
  }
  startRenewing() {
    this.renewTimer = setInterval(() => this.renew(), this.lease.renewMs);
  }
  async renew() {
    try {
      await this.writeLease();
    } catch (error) {
      clearInterval(this.renewTimer);
      this.failure = this.failure ?? IndexedDbFile.commitFailure(error);
    }
  }
  writeLease() {
    return IndexedDbStore.run(this.database, "readwrite", (stores, fail) => {
      stores.owners.get(this.name).onsuccess = (event) => {
        const holder = IndexedDbStore.resultOf(event);
        if (holder?.owner !== this.owner)
          return fail(IndexedDbFile.lockLost(this.name));
        stores.owners.put(this.ownerRecord());
      };
    });
  }
  releaseLease() {
    return IndexedDbFile.dropLease(this.database, this.name, this.owner);
  }
  static leaseOf(options) {
    return {
      leaseMs: options.leaseMs ?? IndexedDbFile.LEASE_MS,
      renewMs: options.renewMs ?? IndexedDbFile.RENEW_MS
    };
  }
  static async claim(database, name, owner, lockHeld, options, lease) {
    let record;
    await IndexedDbStore.run(database, "readwrite", (stores, fail) => {
      stores.owners.get(name).onsuccess = (event) => {
        const holder = IndexedDbStore.resultOf(event);
        if (IndexedDbStore.heldByOther(holder, owner, lockHeld))
          return fail(IndexedDbStore.busy(name));
        stores.owners.put({ name, owner, expiresAt: Date.now() + lease.leaseMs });
        record = IndexedDbFile.startRecord(stores, name, options, fail);
      };
    });
    return await IndexedDbFile.existingRecord(database, name, record);
  }
  static startRecord(stores, name, options, fail) {
    if (!options.copyFrom)
      return;
    const record = {
      name,
      size: 0,
      pageSize: IndexedDbFile.PAGE_SIZE,
      state: IndexedDbFile.COPYING,
      modifiedMs: Date.now()
    };
    stores.pages.delete(IndexedDbStore.pagesOf(name));
    stores.files.put(record);
    return record;
  }
  static async existingRecord(database, name, started) {
    if (started)
      return started;
    let record;
    await IndexedDbStore.run(database, "readonly", (stores) => {
      stores.files.get(name).onsuccess = (event) => {
        record = IndexedDbStore.resultOf(event);
      };
    });
    return IndexedDbFile.openableRecord(name, record);
  }
  static openableRecord(name, record) {
    if (!record)
      throw IndexedDbStore.coded(new Error(`${name} does not exist`), "ENOENT");
    if (record.state !== IndexedDbStore.READY) {
      throw IndexedDbStore.coded(new Error(`${name} is an unfinished copy; copy it again`), "EIO");
    }
    return record;
  }
  static async loadPages(database, name, fresh) {
    const pages = new Map;
    if (fresh)
      return pages;
    const range = IndexedDbStore.pagesOf(name);
    let keys = [];
    let values = [];
    await IndexedDbStore.run(database, "readonly", (stores) => {
      stores.pages.getAllKeys(range).onsuccess = (event) => {
        keys = IndexedDbStore.resultOf(event);
      };
      stores.pages.getAll(range).onsuccess = (event) => {
        values = IndexedDbStore.resultOf(event);
      };
    });
    keys.forEach((key, position) => pages.set(key[1], new Uint8Array(values[position])));
    return pages;
  }
  static async giveUp(database, name, owner, releaseLock) {
    try {
      await IndexedDbFile.dropLease(database, name, owner);
    } finally {
      releaseLock();
      database.close();
    }
  }
  static dropLease(database, name, owner) {
    return IndexedDbStore.run(database, "readwrite", (stores) => {
      stores.owners.get(name).onsuccess = (event) => {
        const holder = IndexedDbStore.resultOf(event);
        if (holder?.owner === owner)
          stores.owners.delete(name);
      };
    });
  }
  static isZero(page) {
    const words = new Uint32Array(page.buffer, page.byteOffset, page.byteLength / 4);
    return words.every((word) => word === 0);
  }
  static commitFailure(error) {
    const failure = error instanceof Error ? error : new Error(String(error));
    if (failure.code)
      return failure;
    const code = failure.name === "QuotaExceededError" ? "ENOSPC" : "EIO";
    return IndexedDbStore.coded(new Error(`storing the working copy failed: ${failure.name}: ${failure.message}`), code);
  }
  static lockLost(name) {
    return IndexedDbStore.coded(new Error(`another tab took over ${name}; open it again`), "ELOCKLOST");
  }
  static newOwnerId() {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  }
}

// src/browser/vdbx-names.mjs
var WORKING_COPY_DIRECTORY = "fylo-vdbx";
function isWorkingCopyName(name) {
  return typeof name === "string" && name.length <= 200 && /^[A-Za-z0-9][A-Za-z0-9 ._()-]*\.vdbx$/.test(name) && !name.includes("..");
}

// src/browser/single-file-vfs.mjs
var DEFAULT_DATABASE_PATH = "/database.vdbx";
function createFileBackend(file, { path = DEFAULT_DATABASE_PATH, reader } = {}) {
  const sync = reader ?? createReaderSync();
  const modifiedMs = file.lastModified ?? 0;
  return createSingleFileBackend({
    path,
    writable: false,
    size: () => file.size,
    modifiedMs: () => modifiedMs,
    readAt(offset, into) {
      const end = Math.min(offset + into.length, file.size);
      if (end <= offset)
        return 0;
      let bytes;
      try {
        bytes = sync.readAsArrayBuffer(file.slice(offset, end));
      } catch (error) {
        throw stale(error);
      }
      into.set(new Uint8Array(bytes));
      return bytes.byteLength;
    }
  });
}
function createAccessHandleBackend(handle, { path = DEFAULT_DATABASE_PATH, writable = true, absent = false } = {}) {
  let modifiedMs = Date.now();
  return createSingleFileBackend({
    path,
    writable,
    absent,
    size: () => handle.getSize(),
    modifiedMs: () => modifiedMs,
    readAt: (offset, into) => handle.read(into, { at: offset }),
    writeAt(offset, from) {
      const written = handle.write(from, { at: offset });
      modifiedMs = Date.now();
      return written;
    },
    truncate(length) {
      handle.truncate(length);
      modifiedMs = Date.now();
    },
    flush: () => handle.flush()
  });
}
function createRangeBackend(url, { path = DEFAULT_DATABASE_PATH, ...options } = {}) {
  const Request = options.XMLHttpRequest ?? globalThis.XMLHttpRequest;
  let validator = "";
  let modifiedMs = 0;
  function fetchRange(first, last) {
    const request = new Request;
    request.open("GET", url, false);
    request.responseType = "arraybuffer";
    request.setRequestHeader("Range", `bytes=${first}-${last}`);
    if (validator)
      request.setRequestHeader("If-Range", validator);
    request.send();
    if (request.status === 416)
      return { total: null, bytes: new Uint8Array };
    if (request.status === 200) {
      throw validator ? withCode(new Error(`${url} changed on the server since it was opened`), "ESTALE") : withCode(new Error(`${url} does not support byte-range requests`), "EIO");
    }
    if (request.status !== 206) {
      throw withCode(new Error(`range request for ${url} failed with HTTP ${request.status}`), request.status === 404 ? "ENOENT" : "EIO");
    }
    const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(request.getResponseHeader("Content-Range") ?? "");
    if (!range || Number(range[1]) !== first) {
      throw withCode(new Error(`${url} answered a range it was not asked for`), "EIO");
    }
    return {
      total: Number(range[3]),
      bytes: new Uint8Array(request.response),
      validator: strongEtag(request.getResponseHeader("ETag")) || request.getResponseHeader("Last-Modified") || "",
      modified: Date.parse(request.getResponseHeader("Last-Modified") ?? "")
    };
  }
  const probe = fetchRange(0, 0);
  if (probe.total === null)
    throw withCode(new Error(`${url} is empty`), "EIO");
  const size = probe.total;
  validator = probe.validator ?? "";
  modifiedMs = Number.isFinite(probe.modified) ? probe.modified : 0;
  return createSingleFileBackend({
    path,
    writable: false,
    size: () => size,
    modifiedMs: () => modifiedMs,
    readAt(offset, into) {
      const end = Math.min(offset + into.length, size);
      if (end <= offset)
        return 0;
      const { bytes } = fetchRange(offset, end - 1);
      if (bytes.length !== end - offset) {
        throw withCode(new Error(`${url} returned ${bytes.length} of ${end - offset} bytes`), "EIO");
      }
      into.set(bytes);
      return bytes.length;
    }
  });
}
function createSingleFileBackend(file) {
  const path = normalize(file.path);
  const parent = path.slice(0, path.lastIndexOf("/")) || "/";
  const name = path.slice(path.lastIndexOf("/") + 1);
  const handles = new Map;
  let nextHandle = 1;
  let absent = file.absent === true;
  const handleOf = (handle) => {
    const open = handles.get(handle);
    if (!open)
      throw withCode(new Error(`handle ${handle} is not open`), "EBADF");
    return open;
  };
  const refuse = (what) => {
    throw withCode(new Error(`${what} is refused: this host holds the single file ${path}`), file.writable ? "EACCES" : "EROFS");
  };
  const openHandle = (write) => {
    const handle = nextHandle++;
    handles.set(handle, { write });
    return handle;
  };
  const createOnOpen = (flags) => {
    if ((flags & OPEN_CREATE) === 0) {
      throw withCode(new Error(`${path} does not exist`), "ENOENT");
    }
    absent = false;
  };
  return {
    path,
    writable: file.writable,
    size: file.size,
    open(requested, flags) {
      const at = normalize(requested);
      if (at === parent && (flags & (OPEN_WRITE | OPEN_CREATE)) === 0) {
        return openHandle(false);
      }
      if (at !== path) {
        if (flags & OPEN_CREATE)
          refuse(`creating ${requested}`);
        throw withCode(new Error(`${requested} does not exist`), "ENOENT");
      }
      const write = (flags & OPEN_WRITE) !== 0;
      if (write && !file.writable)
        refuse(`opening ${path} for writing`);
      if (absent)
        createOnOpen(flags);
      return openHandle(write);
    },
    close(handle) {
      handleOf(handle);
      handles.delete(handle);
    },
    readAt(handle, offset, into) {
      handleOf(handle);
      return file.readAt(offset, into);
    },
    writeAt(handle, offset, from) {
      const { writeAt } = file;
      if (!handleOf(handle).write || !writeAt)
        return refuse(`writing ${path}`);
      return writeAt(offset, from);
    },
    truncate(handle, length) {
      const { truncate } = file;
      if (!handleOf(handle).write || !truncate)
        return refuse(`truncating ${path}`);
      truncate(length);
    },
    flush(handle) {
      handleOf(handle);
      file.flush?.();
    },
    stat(requested) {
      const at = normalize(requested);
      if (at === path && !absent) {
        return { kind: KIND_FILE, len: file.size(), modifiedMs: file.modifiedMs() };
      }
      if (at === parent)
        return { kind: KIND_DIRECTORY, len: 0, modifiedMs: 0 };
      return { kind: KIND_MISSING };
    },
    readDir(requested) {
      if (normalize(requested) === parent)
        return absent ? [] : [name];
      throw withCode(new Error(`${requested} is not a directory`), "ENOENT");
    },
    mkdir: (requested) => refuse(`creating directory ${requested}`),
    unlink: (requested) => refuse(`removing ${requested}`),
    rmdir: (requested) => refuse(`removing directory ${requested}`),
    rename: (from) => refuse(`renaming ${from}`),
    readAttrs(requested) {
      if (normalize(requested) !== path) {
        throw withCode(new Error(`${requested} does not exist`), "ENOENT");
      }
      return new Uint8Array;
    },
    writeAttrs: (requested) => refuse(`writing attributes of ${requested}`),
    random: (into) => crypto.getRandomValues(into),
    nowUnixMs: () => Date.now(),
    log: (message) => console.error(message)
  };
}
function createReaderSync() {
  if (typeof FileReaderSync !== "function") {
    throw new Error("FileReaderSync is unavailable; open the database inside a Worker");
  }
  return new FileReaderSync;
}
function normalize(path) {
  if (typeof path !== "string" || !path.startsWith("/")) {
    throw withCode(new Error("single-file host paths must be absolute"), "EACCES");
  }
  const parts = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".")
      continue;
    if (part === "..") {
      throw withCode(new Error(`${path} leaves the database directory`), "EACCES");
    }
    parts.push(part);
  }
  return `/${parts.join("/")}`;
}
function strongEtag(etag) {
  return etag && !etag.startsWith("W/") ? etag : "";
}
function stale(error) {
  if (error?.name === "NotReadableError") {
    return withCode(new Error("the file changed on disk since it was opened; open it again"), "ESTALE");
  }
  return error;
}
function withCode(error, code) {
  return Object.assign(error, { code });
}

// src/browser/fylo-single-file-worker.mjs
var COPY_CHUNK_BYTES = 4 * 1024 * 1024;
var HANDLE_WAIT_MS = 5000;
var HANDLE_RETRY_MS = 100;
var CREATE_PROBE_COLLECTION = "fylo-create";
var backend;
var accessHandle;
var indexedFile;
var storage;
var releaseLock;
var instance;
var probe;
var lastHostError;
var identityWorker;
var identityId = null;
var queue = Promise.resolve();
self.onmessage = (event) => {
  const message = event.data ?? {};
  queue = queue.then(() => handle(message));
};
async function handle(message) {
  const { id } = message;
  try {
    const result = await dispatch(message);
    const transfer = result?.bytes instanceof Uint8Array ? [result.bytes.buffer] : [];
    self.postMessage({ id, result, hostError: lastHostError }, transfer);
  } catch (error) {
    self.postMessage({
      id,
      error: error instanceof Error ? error.message : String(error),
      code: error?.code ?? error?.name,
      hostError: lastHostError
    });
  }
}
async function dispatch(message) {
  switch (message.op) {
    case "open":
      return await open(message);
    case "exec":
      if (!instance)
        throw new Error("the engine was not loaded; pass moduleUrl to open");
      return await stored(() => ({
        ndjson: exec(instance, message.ndjson)
      }));
    case "host":
      return await stored(() => opened().call(message));
    case "snapshot":
      indexedFile?.refuseWhenFailed();
      return snapshot();
    case "close":
      await close();
      return { closed: true };
    default:
      throw new Error(`unknown op: ${message.op}`);
  }
}
async function open({
  id,
  source,
  path = DEFAULT_DATABASE_PATH,
  moduleUrl,
  identity,
  encryption,
  create = false
}) {
  if (backend)
    throw new Error("this Worker already has a database open");
  lastHostError = undefined;
  try {
    requireSupportedOpen(source, { moduleUrl, identity, encryption, create });
    if (source?.kind === "file") {
      backend = createFileBackend(source.file, { path });
    } else if (source?.kind === "url") {
      backend = createRangeBackend(new URL(source.url, self.location.href).href, { path });
    } else if (source?.kind === "opfs") {
      await openWorkingCopy(source, path, (progress) => self.postMessage({ id, progress }), create);
    } else if (source?.kind === "indexeddb") {
      await openIndexedCopy(source, path, (progress) => self.postMessage({ id, progress }));
    } else if (source?.kind === "local") {
      const onProgress = (progress) => self.postMessage({ id, progress });
      if (await hasOpfs())
        await openWorkingCopy(source, path, onProgress, false);
      else
        await openIndexedCopy(source, path, onProgress);
    } else {
      throw new Error(`unknown database source: ${source?.kind}`);
    }
    const key = encryption ? await VdbxKey.material(encryption, backend, create) : null;
    if (moduleUrl)
      instance = await instantiate(moduleUrl, backend, identity, key);
    if (create)
      createIfMissing(path);
    probe = createProbe(backend);
    storage = storage ?? source.kind;
    return describe();
  } catch (error) {
    await close();
    throw error;
  }
}
async function stored(action) {
  indexedFile?.refuseWhenFailed();
  try {
    return action();
  } finally {
    await indexedFile?.commit();
  }
}
async function hasOpfs() {
  if (typeof navigator.storage?.getDirectory !== "function")
    return false;
  if (typeof FileSystemFileHandle?.prototype?.createSyncAccessHandle !== "function")
    return false;
  try {
    await navigator.storage.getDirectory();
    return true;
  } catch {
    return false;
  }
}
async function openIndexedCopy({ name, copyFrom }, path, onProgress) {
  if (!isWorkingCopyName(name))
    throw new Error(`invalid working copy name: ${name}`);
  indexedFile = await IndexedDbFile.open(name, { copyFrom, onProgress });
  const file = indexedFile;
  storage = "indexeddb";
  backend = createSingleFileBackend({
    path,
    writable: true,
    size: () => file.size(),
    modifiedMs: () => file.modifiedMs(),
    readAt: (offset, into) => file.readAt(offset, into),
    writeAt: (offset, from) => file.writeAt(offset, from),
    truncate: (length) => file.truncate(length)
  });
}
function requireSupportedOpen(source, { moduleUrl, identity, encryption, create }) {
  if (encryption && identity) {
    throw unsupported("a protected database opens with an identity, not with encryption.key");
  }
  if (!create)
    return;
  if (!encryption) {
    throw unsupported("the browser creates only encrypted databases: pass encryption.key");
  }
  if (source?.kind !== "opfs" || source.copyFrom || !moduleUrl) {
    throw unsupported("create needs an OPFS working copy by name, with no copyFrom, and moduleUrl");
  }
}
function unsupported(message) {
  return Object.assign(new Error(message), { code: "EBROWSER_UNSUPPORTED" });
}
function createIfMissing(path) {
  if (backend.stat(path).kind === KIND_FILE)
    return;
  const request = { op: "findDocs", root: path, collection: CREATE_PROBE_COLLECTION, query: {} };
  const [frame] = exec(instance, `${JSON.stringify(request)}
`).split(`
`).filter(Boolean).map((line) => JSON.parse(line));
  if (backend.stat(path).kind === KIND_FILE)
    return;
  throw Object.assign(new Error(frame?.error?.message ?? "the database was not created"), {
    code: frame?.error?.code ?? "EUNKNOWN"
  });
}
async function instantiate(moduleUrl, target, identity, key) {
  let created;
  const imports = createHostImports({
    memory: () => created.exports.memory,
    backend: target,
    identity: identity ? await startIdentityBridge(identity) : undefined,
    onError: (message) => {
      lastHostError = message;
    }
  });
  created = (await WebAssembly.instantiateStreaming(fetch(moduleUrl), imports)).instance;
  if (identity) {
    configure(created, {
      identity: `webcrypto:${identity.name}`,
      trustAnchor: identity.trustAnchor,
      trustOnFirstUse: identity.trustOnFirstUse === true
    });
  }
  if (key)
    setDatabaseKey(created, key);
  return created;
}
async function startIdentityBridge({ name, workerUrl }) {
  if (typeof SharedArrayBuffer !== "function" || globalThis.crossOriginIsolated !== true) {
    throw Object.assign(new Error("opening a database as a webcrypto: identity needs a cross-origin isolated " + "page (COOP: same-origin, COEP: require-corp)"), { code: "ENOISOLATION" });
  }
  const buffer = createIdentityBridgeBuffer();
  identityWorker = new Worker(workerUrl, { type: "module" });
  const ready = await new Promise((resolve) => {
    identityWorker.onmessage = (event) => resolve(event.data ?? {});
    identityWorker.onerror = (event) => resolve({ error: event.message || "the crypto bridge Worker failed to start" });
    identityWorker.postMessage({ buffer, name });
  });
  if (!ready.ready) {
    throw Object.assign(new Error(ready.error), { code: ready.code ?? "EIDENTITY" });
  }
  identityId = (await describeIdentity(ready.publicKey)).id;
  return createIdentityBridgeBackend(buffer);
}
async function openWorkingCopy({ name, copyFrom }, path, progress, create) {
  if (!isWorkingCopyName(name))
    throw new Error(`invalid working copy name: ${name}`);
  storage = "opfs";
  await acquireLock(`fylo-vdbx:${name}`);
  const root = await navigator.storage.getDirectory();
  const directory = await root.getDirectoryHandle(WORKING_COPY_DIRECTORY, { create: true });
  const file = await directory.getFileHandle(name, { create: Boolean(copyFrom) || create });
  accessHandle = await createAccessHandle(file);
  if (copyFrom)
    await streamInto(accessHandle, copyFrom, progress);
  const absent = create && accessHandle.getSize() === 0;
  backend = createAccessHandleBackend(accessHandle, { path, absent });
}
async function createAccessHandle(file) {
  for (let waited = 0;; waited += HANDLE_RETRY_MS) {
    try {
      return await file.createSyncAccessHandle();
    } catch (error) {
      if (error?.name !== "NoModificationAllowedError")
        throw error;
      if (waited >= HANDLE_WAIT_MS) {
        throw Object.assign(new Error("this working copy is still held by a closing Worker"), {
          code: "EBUSY"
        });
      }
    }
    await new Promise((resolve) => setTimeout(resolve, HANDLE_RETRY_MS));
  }
}
async function streamInto(handle2, blob, progress) {
  handle2.truncate(0);
  const total = blob.size;
  let copied = 0;
  const reader = blob.stream().getReader();
  for (;; ) {
    const { done, value } = await reader.read();
    if (done)
      break;
    let written = 0;
    while (written < value.length) {
      written += handle2.write(value.subarray(written), { at: copied + written });
    }
    copied += value.length;
    progress({ copied, total });
  }
  if (copied !== total)
    throw new Error(`copied ${copied} of ${total} bytes`);
  handle2.flush();
}
function snapshot() {
  const file = opened();
  if (file.backend.writable)
    accessHandle?.flush();
  const size = file.backend.size();
  const parts = [];
  for (let offset = 0;offset < size; offset += COPY_CHUNK_BYTES) {
    const chunk = new Uint8Array(Math.min(COPY_CHUNK_BYTES, size - offset));
    let read = 0;
    while (read < chunk.length) {
      const count = file.backend.readAt(file.handle, offset + read, chunk.subarray(read));
      if (count === 0)
        throw new Error(`the file ended at ${offset + read} of ${size} bytes`);
      read += count;
    }
    parts.push(chunk);
  }
  return { blob: new Blob(parts, { type: "application/vnd.d31ma.vdbx" }), size };
}
function describe() {
  const { kind, len, modifiedMs } = opened().call({ call: "stat" });
  return {
    path: backend?.path,
    writable: backend?.writable ?? false,
    kind,
    size: len,
    modifiedMs,
    engine: Boolean(instance),
    storage,
    identity: identityId,
    crossOriginIsolated: globalThis.crossOriginIsolated === true
  };
}
function opened() {
  if (!probe)
    throw new Error("no database is open");
  return probe;
}
async function close() {
  probe?.close();
  probe = undefined;
  instance = undefined;
  backend = undefined;
  storage = undefined;
  const closing = indexedFile;
  indexedFile = undefined;
  identityWorker?.terminate();
  identityWorker = undefined;
  identityId = null;
  try {
    accessHandle?.close();
  } finally {
    accessHandle = undefined;
    releaseLock?.();
    releaseLock = undefined;
    await closing?.close();
  }
}
async function acquireLock(name) {
  if (!globalThis.navigator?.locks)
    return;
  await new Promise((granted, failed) => {
    navigator.locks.request(name, { mode: "exclusive", ifAvailable: true }, (lock) => {
      if (!lock) {
        failed(Object.assign(new Error("this working copy is open in another tab or window"), { code: "EBUSY" }));
        return;
      }
      granted(undefined);
      return new Promise((release) => {
        releaseLock = release;
      });
    }).catch(failed);
  });
}
function createProbe(target) {
  const memory = new WebAssembly.Memory({ initial: 1 });
  const table = createHostImports({
    memory: () => memory,
    backend: target,
    onError: (message) => {
      lastHostError = message;
    }
  }).fylo_host;
  const SCRATCH = 0;
  const PATH = 64;
  const DATA = 4096;
  const encoded = new TextEncoder().encode(target.path);
  const view = () => new DataView(memory.buffer);
  const bytes = () => new Uint8Array(memory.buffer);
  const room = (length) => {
    const needed = DATA + length - memory.buffer.byteLength;
    if (needed > 0)
      memory.grow(Math.ceil(needed / 65536));
  };
  const check = (code, what) => {
    if (code !== 0) {
      throw Object.assign(new Error(`${what} failed with errno ${-code}: ${lastHostError}`), {
        code: `E${-code}`
      });
    }
  };
  const openHandle = (flags) => {
    bytes().set(encoded, PATH);
    check(table.open(PATH, encoded.length, flags, SCRATCH), "open");
    return view().getBigUint64(SCRATCH, true);
  };
  const handle2 = openHandle(target.writable ? OPEN_READ | OPEN_WRITE : OPEN_READ);
  return {
    backend: target,
    handle: Number(handle2),
    call(request) {
      switch (request.call) {
        case "stat": {
          bytes().set(encoded, PATH);
          check(table.stat(PATH, encoded.length, SCRATCH), "stat");
          return {
            kind: view().getUint32(SCRATCH, true),
            len: Number(view().getBigUint64(SCRATCH + 8, true)),
            modifiedMs: Number(view().getBigUint64(SCRATCH + 16, true))
          };
        }
        case "read": {
          room(request.length);
          check(table.read_at(handle2, BigInt(request.offset), DATA, request.length, SCRATCH), "read_at");
          const read = view().getUint32(SCRATCH, true);
          return { read, bytes: bytes().slice(DATA, DATA + read) };
        }
        case "write": {
          const from = new Uint8Array(request.bytes);
          room(from.length);
          bytes().set(from, DATA);
          check(table.write_at(handle2, BigInt(request.offset), DATA, from.length, SCRATCH), "write_at");
          return { written: view().getUint32(SCRATCH, true) };
        }
        case "truncate":
          check(table.truncate(handle2, BigInt(request.length)), "truncate");
          return { length: request.length };
        case "flush":
          check(table.flush(handle2), "flush");
          return { flushed: true };
        case "openWrite":
          table.close(openHandle(OPEN_READ | OPEN_WRITE));
          return { opened: true };
        default:
          throw new Error(`unknown host call: ${request.call}`);
      }
    },
    close() {
      table.close(handle2);
    }
  };
}
