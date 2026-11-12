# ADR 0026: Mobile builds and a C ABI

- Status: **Accepted** (the user, 2026-11-11)
- Date: **2026-11-11**
- Owners: **FYLO maintainers**
- Accepts: section 1 of ADR 0019 (the `fylo` crate is FYLO's native,
  in-process API) and its section 5 (no listener). The rest of ADR 0019 stays
  Proposed.
- Amends: ADR 0004 (one more `unsafe` boundary, and the `jni` dependency),
  ADR 0021 (two key references, and a key given as a value).
- Request: Mesail's F2 (an offline-first client on iOS and Android).

## Context

Phones cannot start a process, so `fylo exec --loop` does not exist there.
Until now a phone app could only run FYLO's browser engine inside a WebView.
ADR 0019 says mobile apps get FYLO by linking the `fylo` crate, but the crate
had no C ABI, was not built for iOS or Android, and no identity store existed
for either. The device copy of an app's data must be encrypted with a key the
operating system protects.

## Decision

### 1. Two ways to run FYLO on a phone (D1)

- The **native engine** (this ADR): the `fylo` crate linked into the app
  through a C ABI, shipped as an `.xcframework` and an Android `.aar`.
- The **WebView engine** (`fylo-browser.wasm`, ADR 0008 and its WebKit and
  IndexedDB work). It stays.
- The app picks one. They share the `.vdbx` format.

### 2. Publishing (D2)

- ADR 0019 section 1 is accepted: the `fylo` crate is FYLO's native API.
- Phone binaries ship from GitHub Releases, and the `.aar` also from GitHub
  Packages (Maven), FYLO's registry of record. Rust apps depend on a git tag
  of `d31ma/Fylo`.
- crates.io comes later, as its own decision. Two things block it today: about
  ten crates would have to be published together, and the `sql` feature
  depends on the patched, vendored `turso_core`, which crates.io cannot take.

### 3. The C ABI (D3)

- A new crate, `crates/fylo-ffi`, built as a `staticlib` for iOS and a
  `cdylib` for Android. Its Rust library name is `fylo_ffi`, because it
  wraps the crate named `fylo`; the iOS and Android packages name the
  binary they ship. It depends on the `fylo` crate, not on `fylo-machine`,
  so phones carry no machine protocol and no TLS client.
- Small typed functions only, no generic request function:
  `fylo_abi_version`, `fylo_open`, `fylo_close`, `fylo_create_collection`,
  `fylo_put`, `fylo_get`, `fylo_delete`, `fylo_find`, `fylo_find_page`,
  `fylo_identity_create`, `fylo_identity_show` and `fylo_bytes_free`.
- Every input is a pointer and a length (UTF-8 JSON or text, no NUL needed),
  borrowed for the call only. Every output is a `FyloBytes` buffer owned by
  FYLO and returned to `fylo_bytes_free` exactly once.
- Results use the JSON shape of the matching machine-protocol `result`
  (`putData`, `getDoc`, `findDocs`, `delDoc`, `createCollection`). Errors are
  `{"code", "message"}` with the codes of `api/errors/v1.json`.
- Statuses: `FYLO_OK`, `FYLO_NOT_FOUND`, `FYLO_ERROR`,
  `FYLO_INVALID_ARGUMENT`, `FYLO_CLOSED`, `FYLO_PANIC`.
- A root is a handle **number** in a table inside FYLO, never a pointer. A
  closed or unknown number returns `FYLO_CLOSED`. Closing marks the handle
  closed at once; calls already running finish first.
- Calls block and may flush to disk: apps never call FYLO on the UI thread.
  Calls may come from any thread at the same time.
- `fylo_find_page` uses the same query, sort and cursor contract as
  `findDocs` (ADR 0023): `{items: [{id, doc}], nextCursor}`.
- A missing database is created only when the open options name
  `create.mode` (`protected` with an `admin`, `encrypted` with a key, or
  `plain`); otherwise the open fails.
- The header `include/fylo.h` is committed, and a check fails when it and the
  code differ.

### 4. `unsafe` (amends ADR 0004)

- `fylo-ffi` sets `unsafe_code = "deny"`. Only its boundary module, which
  reads the caller's pointers and hands out and frees output buffers, contains
  `unsafe`, each block with a `SAFETY:` comment. It is a new row of
  `docs/security/unsafe-inventory.md`.
- On Android the JNI glue uses the `jni` crate. Its own `unsafe` is upstream,
  reviewed like any dependency (ADR 0004's checklist,
  `docs/security/mobile-dependencies.md`). FYLO's glue, `jni_calls.rs`, calls
  only its safe API; its exported names are its only `unsafe` items.

### 5. Panics (D6)

- Phone builds use a `mobile` profile with `panic = "unwind"`. Every C entry
  catches a panic, returns `FYLO_PANIC`, and marks its handle dead. Desktop
  release builds keep `panic = "abort"`.

### 6. Targets

| Platform | Rust targets | Package |
| --- | --- | --- |
| iOS | `aarch64-apple-ios`, `aarch64-apple-ios-sim` | `Fylo.xcframework` (static) and the Swift package `FyloKit` |
| Android | `aarch64-linux-android`, `x86_64-linux-android` | `fylo-<version>.aar` with a Kotlin wrapper |

- `x86_64-apple-ios` is not built: Xcode's Macs are Apple silicon.
- Android libraries are linked with 16 KB page alignment.
- The minimum OS versions follow Mesail's shells (D8). The
  `android-keystore:` store needs Android 12 (API 31) whatever the `.aar`'s
  minimum is.
- `.vdbx` single-file databases are the supported layout on phones.

### 7. Tests

- The Rust tests of the engine crates run on the iOS simulator
  (`xcrun simctl spawn`) and the Android emulator (`adb`), through Cargo
  target runners in `scripts/mobile/`. Tests that start other programs are
  left out by a filterset that the scripts name.
- The Swift and Kotlin wrappers and both key stores are tested inside an app:
  XCTest on the simulator, instrumented tests on the emulator.
- `x86_64` Android is built on the Mac and tested in the release job (a Linux
  runner with KVM).
- The simulator's files are on the Mac's disk, and the emulator's Keystore is
  software-backed: these runs prove the code paths, not the phone's file
  protection or hardware.

### 8. Key stores (amends ADR 0021, D4)

| Reference | Private key | Platforms |
| --- | --- | --- |
| `ios-keychain:<label>` | A P-256 key in PKCS#8 form, a generic-password item in the iOS Data Protection keychain: readable after the first unlock, this device only, never synchronized to iCloud | iOS |
| `android-keystore:<alias>` | A P-256 key made inside Android Keystore (StrongBox when present, else the TEE) that signs and agrees (ECDH) and never leaves it; records are signed with session keys (ADR 0021 section 8) | Android 12+ |

- `ios-keychain:` is a software store (`KeyStore`), like `keychain:`.
  `android-keystore:` is a hardware store (`HardwareKeyStore`), like
  `enclave:` and `tpm:`. Keystore's API is Java only, so FYLO calls a small
  Kotlin object through JNI. User authentication is off: FYLO signs in the
  background.
- The trust model of ADR 0021 does not change.

### 9. A key given as a value (amends ADR 0021, D5)

- `fylo::OpenOptions::with_database_key` and the `key` argument of
  `fylo_open` take an encryption key as bytes, at least 32 of them. The rules
  and error codes are the browser engine's (ADR 0024).
- The key is never part of JSON, a log or an error message. FYLO wipes its
  copy. The wrappers take bytes, never strings, and make 32 random bytes by
  default.
- The CLI and `application.json` still refuse a literal key.
- Risks stated plainly: the key is in the app's memory while the database is
  open; an app that stores it badly (preferences, logs, backups) loses the
  protection; whoever holds the key and a copy of the file reads it; the
  derivation is HKDF, so a password must never be used as the key.

### 10. Size

- Turso's `sql` feature is not built for phones.
- A gate measures the stripped library linked into a test app per
  architecture and fails above its budget (set from the first measurement).

## Consequences

- Phone apps get the whole native engine: schemas, identities, field
  encryption and the OS key stores, with real files.
- FYLO gains a C ABI to keep stable (`fylo_abi_version`), two packaging
  pipelines, and simulator and emulator runs in its gates.
- One more `unsafe` boundary and the `jni` dependency.

## Decisions (the user, 2026-11-11)

| | Question | Decision |
| --- | --- | --- |
| D1 | Phone paths | Both the native engine and the WebView engine; the app picks |
| D2 | Publishing | Accept ADR 0019 section 1 only; binaries from GitHub Releases and GitHub Packages Maven; Rust apps use a git tag; crates.io later |
| D3 | C ABI width | Small typed functions only |
| D4 | `android-keystore:` | A hardware Keystore key that signs and does ECDH (API 31), with session keys |
| D5 | Key as a value | At least 32 bytes; 32 random bytes by default in the wrappers; an encrypted database is created only when the app sets `create.mode`; the browser engine's rules and codes |
| D6 | Panics | Catch, return `FYLO_PANIC`, mark the handle dead |
| D7 | Mac setup | Download the iOS simulator runtime when the simulator work starts, with enough disk; `x86_64` Android tested in the release job |
| D8 | Minimum OS versions | Mesail's shells' versions |
