# ADR 0025: Browser `.vdbx` encryption with a host key

- Status: **Accepted** (built 2026-11-11)
- Date: **2026-11-11**
- Owners: **FYLO maintainers**
- Request: Mesail's F4 (CADUCEUS), part 2
- Amends: ADR 0021 decision P5-1 and Q2 (an exception, the user's OK on
  2026-11-11), ADR 0016 section 10
- Related: ADR 0016 section 9 (shared-key encryption)

## Context

Mesail keeps a copy of each user's recent mail and chat on the device, in a
WebView or a browser tab. That copy must be encrypted at rest, and the page
cannot always turn on cross-origin isolation (COOP and COEP): WebView shells
and pages that embed third-party content often cannot.

What FYLO had:

- Native `.vdbx` shared-key encryption (ADR 0016 section 9): a file key is
  derived with HKDF-SHA256 from a secret and a 32-byte salt in the
  superblock; every record, the directory, dictionaries and snapshots are
  sealed with AES-256-GCM. This code is already inside `fylo-browser.wasm`;
  only a way to give it a key was missing.
- In the browser, the only encrypted file was a protected one opened with a
  `webcrypto:` identity (ADR 0021 P5-5). Its records are signed and its
  wraps opened one by one through a crypto bridge Worker, with
  `Atomics.wait` on a `SharedArrayBuffer`, which needs cross-origin
  isolation.
- The folder engine (`fylo-web.mjs`) refuses encryption options with
  `EBROWSER_UNSUPPORTED`.
- The browser created no `.vdbx` at all (P5-1): a new database is protected,
  and the browser does not write policies.

## Decision

### 1. The page gives a key; WebCrypto runs once, before the engine

`SingleFileDatabase.open(source, { encryption: { key }, create })` takes one
of two keys:

- **The secret as bytes** (`Uint8Array`, `ArrayBuffer`, a typed-array view,
  or a string taken as its UTF-8 bytes), at least 32 bytes. It is the same
  secret native `fylo.encryption.key` names, so a file moves between native
  and the browser with no conversion. The engine derives the file key from
  it, as native does.
- **A non-extractable HKDF `CryptoKey`** with the `deriveBits` usage,
  imported from that same secret. Before the engine starts, the worker
  reads the file's salt from the superblock (or draws a new one for a file
  it will create) and calls `deriveBits` twice with native's labels
  (`fylo.vdbx.file-key.v1`, 256 bits; `fylo.vdbx.key-check.v1`, 128 bits).
  This is RFC 5869 with one output block, which is exactly what native
  computes. Only the 32-byte file key, the 16-byte check and the salt reach
  the engine; the secret stays in WebCrypto.

The engine never waits on WebCrypto: every seal and open after that is the
Rust AES-GCM already in the module, called synchronously. So no
`SharedArrayBuffer`, no `Atomics.wait` and no cross-origin isolation are
needed.

**Refused, with `EBROWSER_UNSUPPORTED`:**

- an AES-GCM (or any non-HKDF) `CryptoKey`: using it would mean an
  asynchronous WebCrypto call for every record, which needs the isolated
  bridge;
- `encryption` together with `identity`: a protected file has its own keys;
- `create: true` without a key, or for a source that is not an OPFS working
  copy by name.

**Refused with `EKEY`:** a key shorter than 32 bytes or of another type, and
a wrong key (the key-check value differs, as natively). A derived key whose
salt is not the file's is a wrong key.

### 2. The key never travels as text

The worker hands the key to the engine through a separate export,
`fylo_set_database_key`, not the JSON of `fylo_configure`: one flags byte
(1 = derived, 2 = create), then the secret, or salt, file key and check.
The engine copies it into a `DatabaseKey` (zeroizing), and overwrites the
input buffer with zeros before it returns. The worker wipes the bytes it
built and its own copy of a byte key. A string key cannot be wiped in
JavaScript, so the docs recommend bytes or a `CryptoKey`. Error text names
the rule ("at least 32 bytes"), never the key or its length.

### 3. Creating an encrypted, unprotected database (exception to P5-1)

With `create: true` and a key, a missing OPFS working copy is created as an
**encrypted, unprotected** `.vdbx`: the shared-key format of ADR 0016
section 9, with no identities, no policy and no signed records.

- This is an exception to ADR 0021's "protected by default" (Q2, P5-1),
  agreed by the user on 2026-11-11 for Mesail's device store. ADR 0021
  section 5 records it.
- The browser never creates a plain database. Without a key, nothing is
  created, as before.
- The worker opens the empty working copy as "absent", so the engine's
  usual path for a missing `.vdbx` runs: its Wasm build now creates a
  database when, and only when, a creation key is set
  (`ApplicationConfig::creation_key`). It writes the superblock with the
  salt the key carries (a fresh one for a secret; the worker's for a
  derived key).

### 4. Threat model

What is protected:

- **The file at rest.** OPFS bytes on the device's disk, in profile
  backups and sync, and from another OS user who can read the profile: only
  sizes, counts and the format version are readable; collection names and
  documents are sealed (ADR 0016 section 9). This holds only if the key is
  not stored next to the file. A key in `localStorage`, IndexedDB as raw
  bytes, or the same disk unprotected, gives nothing.
- **Tampering.** AES-GCM's tag and associated data (collection, LSN, entry)
  refuse a changed or moved frame, as natively.
- **The secret itself, with an HKDF `CryptoKey`.** Script cannot export a
  non-extractable key, so a later script injection cannot carry the secret
  away for use elsewhere.

What is not protected:

- **Script running in the origin** (cross-site scripting, a compromised
  dependency, an extension with access to the page). It can use the open
  database, and it can call `deriveBits` with a `CryptoKey` it can reach to
  get a file key. Non-extractable stops export, not use.
- **Memory of the running Worker.** The file key sits in the module's
  memory while the database is open, as protected files' collection keys
  already do. Without cross-origin isolation, the page can share a process
  with other sites; reading that memory through a side channel (Spectre
  class) then relies on the browser's site isolation, which desktop
  Chromium and Firefox turn on by default. A page that can be isolated
  should still be.
- **Rollback.** A whole older copy of the file, sealed with the same key,
  is not detected (as natively).
- **A low-entropy secret.** The key-check value lets someone with the file
  test guesses offline. The 32-byte minimum applies to byte keys; a
  `CryptoKey`'s secret cannot be checked from here, so it is the page's job.
- **Access control.** A file created this way has no identities: anyone
  with the key reads and writes all of it.

### 5. Size

The parser and export add a few hundred bytes to `fylo-browser.wasm`; the
cipher, HKDF and creation code were already linked or are shared with the
plain create path. The size is checked by
`scripts/verify-browser-wasm-budgets.mjs`.

## Consequences

- Mesail's device store can be encrypted in any current browser and WebView
  with OPFS, with no isolation headers.
- One new module export, `fylo_set_database_key` (module ABI stays 2: the
  export is optional for hosts that do not encrypt).
- Files made in the browser this way open natively with the same secret,
  and the reverse.

## Verification

`tests/browser/e2e/engine-vdbx.spec.mjs`, on the non-isolated origin, in
Chromium and Firefox:

- a native encrypted database opens with the secret as bytes, as a string
  and as an HKDF `CryptoKey`; the browser's write is read back natively;
- a wrong key (bytes and `CryptoKey`) fails with `EKEY`; a short key with
  `EKEY`; an AES `CryptoKey` and a keyless `create` with
  `EBROWSER_UNSUPPORTED`;
- a database created in the browser (bytes and `CryptoKey`) holds no
  document text and opens natively with the secret, reporting
  `encrypted: true`.
