# ADR 0021: Application identities and signed policies

- Status: **Proposed** (draft for the user, 2026-11-07)
- Date: **2026-11-07**
- Owners: **FYLO maintainers**
- Supersedes, once accepted: ADR 0015 section 10 (POSTIX on segments) and the
  POSTIX parts of ADR 0016's costs.
- Amends, once accepted: ADR 0016 section 9 (encryption), ADR 0013 (commit
  records), ADR 0019 (`OpenOptions`, `fylo link`'s configuration).
- Background: the design study
  ["Access control by application identity"](../access-control-design.md)
  (section numbers below refer to it).

## Context

FYLO's access control today is POSTIX: a per-record `uid`, `gid` and `mode`
that the caller claims and FYLO's own code checks (study section 2). A record
without a descriptor is open to everyone, and segment and `.vdbx` collections
are enforced only by the library a process could skip.

The user asked for access control in the shape of AWS security groups:
everything is denied by default, and an application or process named by a
static identifier is allowed to read or write. FYLO runs inside the caller's
process and never opens a listener, so a rule must be backed by something the
caller cannot skip: the operating system's secret store and cryptography
(study section 3).

## Decision

### 1. An identity is a P-256 key pair

- The static identifier is the key's fingerprint:
  `fid1:p256:<base32 of SHA-256 of the public key>`.
    - The public key is hashed in its SEC1 uncompressed form (65 bytes), the
      form WebCrypto's `raw` export, the macOS keychain and CNG all produce.
    - Base32 is RFC 4648, upper case, without padding: 52 characters.
- P-256 because every hardware key store FYLO targets (Secure Enclave,
  TPM 2.0, Android StrongBox) and WebCrypto's non-extractable keys support it.
- The private key never enters a database. A process names it by a **key
  reference**:

| Reference | Private key | Platforms |
| --- | --- | --- |
| `file:<path>` | PKCS#8 PEM file, mode `0400` or `0600`, owned by the process's user. FYLO refuses a file any other user can read or write | Everywhere; development and CI |
| `keychain:<label>` | A macOS keychain item; `FYLO_KEYCHAIN` names a keychain file to use instead of the default keychain | macOS |
| `cng:<name>` | A Windows CNG key, DPAPI-protected | Windows |
| `creds:<name>` | A `systemd-creds` credential handed to one unit | Linux services |
| `webcrypto:<name>` | A non-extractable `CryptoKey` in the origin's IndexedDB | Browser |

A reference to a store the platform does not have fails with
`EIDENTITY_UNSUPPORTED`. `fylo identity create --store <reference>` makes a
key and prints its identifier; `fylo identity show <reference>` prints the
identifier of an existing key.

### 2. Protected databases are encrypted `.vdbx` files with a signed policy

As in study section 5: an allow-only policy, signed by an `admin` identity,
grants `read`, `write` or `admin` on `database`, `collection:<name>` or
`collection:*`. Reads are enforced by per-collection keys wrapped with HPKE
(RFC 9180, DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM) for each
granted identity; writes by ECDSA-signed commits and checkpoints. An open
without an identity, or without a grant, is locked.

### 3. Answers to the study's questions

Each answer is the proposed decision. Answers marked **needs the user's
confirmation** are ones where the study gave no clear recommendation, so the
most conservative option consistent with deny by default was chosen.

| | Question | Proposed decision | Reason |
| --- | --- | --- | --- |
| Q1 | Per application only? | **Yes.** POSTIX (per-record `uid`/`gid`/`mode`, `.as()`, `--uid`, `--groups`, the machine `access` field) is removed in the breaking release; end-user rules stay in the application | A per-record rule is only a claim FYLO's own code checks, which the caller can skip |
| Q2 | Protected by default? | **Yes.** A new `.vdbx` is protected unless created with `--unprotected`; directory roots stay as an unprotected layout for development and tooling. **Needs the user's confirmation** | Deny by default; a directory root leaks indexed values through file names (study 2.5) and cannot be protected |
| Q3 | Key storage per platform | **Accepted for version 1, documented:** same-user malware on Linux desktops and unpackaged Windows apps can take a software key. Servers are told to run each application under its own OS user (`creds:` on Linux). The option D helper is not built. **Needs the user's confirmation** | The study's threat model (section 6) shows no in-process design does better there; option D brings back a privileged subprocess |
| Q4 | Write implies read? | **Yes** | A writer must read the collection's directory and index to append and maintain indexes |
| Q5 | Admin implies read and write? | **Yes** | Granting `read` to a new identity means wrapping a key the admin must hold |
| Q6 | Revocation | **Forward-only by default** (a new key epoch); `fylo rekey --collection` rewrites old frames on request | Rewriting at once would make a revoke as slow as a full pack on a large collection |
| Q7 | Trust anchor | **Pinning required.** An open of a protected database without a pinned anchor fails with `EPOLICY`, unless the caller opts in to trust on first use, which warns. **Needs the user's confirmation** | Without a pin, whoever can write the file can replace the database with one carrying their own anchor |
| Q8 | Hardware keys | **Later** (phase 8); version 1 uses software keys in the OS secret stores | Hardware keys take milliseconds per operation and differ per platform |
| Q9 | `fylo link` | **Yes:** `fylo.identity` and `fylo.trustAnchor` in `application.json`, or `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR` | Turso's `Database::open` carries only a path |
| Q10 | The browser | **Yes:** the origin is the boundary, plus an optional `webcrypto:` identity for files moved between origins | OPFS is already private to the origin |
| Q11 | Buckets and the queue | **They wait in protected databases:** a protected `.vdbx` refuses them until `.vdbx` holds them; they stay available in unprotected roots. **Needs the user's confirmation** | Shipping them unprotected beside protected collections would be an allow by default |

### 4. Phases

The study's section 9 is the plan. Phase 2 (identities and key stores) needs
none of the answers above and lands first. Phase 3 (protected reads) and later
phases start only after this ADR is accepted.

## Consequences

- **Breaking:** POSTIX and the machine protocol's per-request `access` field
  go in the same release as ADR 0019 (its rule 3).
- **New contracts:** `api/access/v1` (policy schema, identity and key-reference
  formats); error codes `ENOIDENTITY`, `EPOLICY`, `ESIGNATURE` and the
  `EIDENTITY_*` codes of `fylo identity`.
- **Dependencies (ADR 0004):** RustCrypto's `p256` for keys and signatures,
  `hpke` for key wrapping (phase 3), and `security-framework` for the macOS
  keychain. Each is reviewed in `docs/security/access-identity-dependencies.md`
  when it is added.
- **Limits stated plainly:** nothing here stops root, deletion of the file, or
  rollback to an older signed copy (study section 6).
