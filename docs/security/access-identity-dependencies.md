# Dependency review: application identities

ADR 0004 review for the direct dependencies of `crates/fylo-identity`, the
application identities of ADR 0021 (proposed), phase 2. `fylo-cli` uses the
crate for `fylo identity create` and `fylo identity show`.

| | `p256` 0.14.0 | `security-framework` 3.7.0 |
| --- | --- | --- |
| Purpose | P-256 key generation, the public key behind the `fid1:p256:` fingerprint, and PKCS#8 PEM and DER for stored keys. Later phases use its ECDSA (signed commits) and ECDH (HPKE key wrapping) | The `keychain:<label>` key store: generic-password items in a macOS keychain |
| Owning crate | `fylo-identity` | `fylo-identity`, macOS only (`cfg(target_os = "macos")`) |
| Targets | Native; pure Rust, so the browser build can use it in phase 5 | macOS only |
| Features | `arithmetic`, `pem`, `std`; defaults off, so no precomputed tables and no `ecdh` yet. `pem` brings `ecdsa` and `pkcs8` along; `std` brings `getrandom` for key generation | Defaults off: no TLS (`alpn`, `session-tickets`) and no macOS version gates beyond the crate's baseline |
| License | Apache-2.0 OR MIT | MIT OR Apache-2.0 |
| New transitive crates | `base16ct`, `base64ct`, `crypto-bigint`, `der`, `ecdsa`, `elliptic-curve`, `ff`, `group`, `pem-rfc7468`, `pkcs8`, `primefield`, `primeorder`, `sec1`, `serdect`, `signature`, `spki`, `wnaf`, all RustCrypto. They share `sha2` 0.11, `digest` 0.11, `rand_core` 0.10 and `getrandom` 0.4 with FYLO's existing `aes-gcm`, `hmac` and `sha2`, so no second version of those | None: it and `security-framework-sys` and `core-foundation` were already in the graph through `rustls-platform-verifier` (`reqwest`, `fylo-machine`) |
| Unsafe and native code | Pure Rust. Scalar and field arithmetic is constant-time by design (`subtle`, `crypto-bigint`) | FFI to Apple's Security.framework, behind safe wrappers; FYLO's code calls only the safe API, so `unsafe_code = "forbid"` stands |
| Build script | None | None (links the system framework) |
| Secrets | The private key, held in `SecretKey`, which zeroizes on drop; PEM and DER encodings are `Zeroizing` | The PKCS#8 DER key passes through the item's data; FYLO copies it into a `Zeroizing` buffer |
| Untrusted input | Key files and keychain items: decoded with `pkcs8`'s DER parser, refused with `EIDENTITY_KEY` if not a P-256 PKCS#8 key | Keychain labels are checked by `KeyReference` (letters, digits, `.`, `_`, `-`) before they reach the keychain |
| Maintenance | RustCrypto `elliptic-curves`, active; the de facto pure-Rust P-256 | kornelski, active; used by `native-tls` and `rustls-platform-verifier` |
| Removal path | `ring` or `aws-lc-rs` offer P-256 too, but need a C or assembly build and no PKCS#8 PEM writer for EC keys; `p256` is the RustCrypto line FYLO already uses for AES-GCM and HMAC | Shell out to `/usr/bin/security`, which would put the key on a command line; or drop `keychain:` for `file:` |

## Phase 3: `hpke` 0.15.1, and `p256`'s `ecdsa` feature

| | `hpke` 0.15.1 |
| --- | --- |
| Purpose | Wrapping a protected database's catalog and collection keys for each identity: RFC 9180 base mode, DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM, single shot |
| Owning crate | `fylo-identity` (`KeyWrap`), used by `fylo-storage-native` natively |
| Targets | Native only: `fylo-storage-native` and `fylo-machine` take `fylo-identity` outside WebAssembly, so `fylo-browser.wasm` does not grow (measured: +190,624 bytes with it, +1,002 without) |
| Features | `aes`, `alloc`, `getrandom`, `nistp`; defaults off, so no X25519, ChaCha20-Poly1305 or ML-KEM. `nistp` also builds `p384` and `p521`, which FYLO never calls; the linker drops them |
| License | MIT OR Apache-2.0 |
| New transitive crates | `hkdf` 0.13, `p384`, `p521`, `fiat-crypto` (for `p521`), `rfc6979` (deterministic ECDSA nonces, from `p256`'s `ecdsa`), `zeroize_derive`. Same `p256` 0.14, `aes-gcm` 0.11, `sha2` 0.11, `rand_core` 0.10 and `getrandom` 0.4 as the rest of the graph |
| Unsafe and native code | Pure Rust |
| Secrets | Key bytes to wrap and the recipient's private scalar, copied into `Zeroizing` buffers |
| Untrusted input | Wraps from the access document: the 65-byte encapsulated key is parsed as a P-256 point, and the AEAD refuses anything else |
| Maintenance | rozbb/rust-hpke, active; the reference Rust HPKE, with RFC 9180 known-answer tests |
| Removal path | RFC 9180 base mode on `p256`'s ECDH, `hkdf` and `aes-gcm` directly, about 150 lines plus the RFC's test vectors |

`p256`'s `ecdsa` feature signs and verifies policy versions (ECDSA P-256,
SHA-256, RFC 6979 nonces).

`cng:` (Windows, phase 5) uses the workspace's `windows-sys` 0.61.2 with
`Win32_Foundation`, `Win32_Security` and `Win32_Security_Cryptography`, in
`fylo-identity` only and only on Windows; it adds no crate to the graph.
Before phase 5, `cng:` was defined but
not implemented: the CNG API is `unsafe` FFI, so it needs either a reviewed
safe wrapper crate or an ADR 0004 exception, which is a decision for that
phase.

Advisories: none known for these versions at the time of review.
`cargo-deny` licenses, bans and sources still apply in CI.
