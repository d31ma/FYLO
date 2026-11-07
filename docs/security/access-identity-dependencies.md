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

Not yet added: `hpke` (RFC 9180 key wrapping) waits for phase 3, so it is
reviewed when protected databases need it. `cng:` (Windows) is defined but
not implemented: the CNG API is `unsafe` FFI, so it needs either a reviewed
safe wrapper crate or an ADR 0004 exception, which is a decision for that
phase.

Advisories: none known for these versions at the time of review.
`cargo-deny` licenses, bans and sources still apply in CI.
