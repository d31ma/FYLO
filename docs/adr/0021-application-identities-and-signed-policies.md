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

The user asked to build the study's recommendations, so each answer is the
decision for this work. Q2, Q3, Q7 and Q11, where the study gave no clear
recommendation, were proposed as the most conservative option consistent with
deny by default and **decided** by the user on 2026-11-07 exactly as
proposed. The ADR itself stays Proposed until phase 3 starts.

| | Question | Proposed decision | Reason |
| --- | --- | --- | --- |
| Q1 | Per application only? | **Yes.** POSTIX (per-record `uid`/`gid`/`mode`, `.as()`, `--uid`, `--groups`, the machine `access` field) is removed in the breaking release; end-user rules stay in the application | A per-record rule is only a claim FYLO's own code checks, which the caller can skip |
| Q2 | Protected by default? | **Yes.** A new `.vdbx` is protected unless created with `--unprotected`; directory roots stay as an unprotected layout for development and tooling. **Decided** (the user, 2026-11-07) | Deny by default; a directory root leaks indexed values through file names (study 2.5) and cannot be protected |
| Q3 | Key storage per platform | **Accepted for version 1, documented:** same-user malware on Linux desktops and unpackaged Windows apps can take a software key. Servers are told to run each application under its own OS user (`creds:` on Linux). The option D helper is not built. **Decided** (the user, 2026-11-07) | The study's threat model (section 6) shows no in-process design does better there; option D brings back a privileged subprocess |
| Q4 | Write implies read? | **Yes** | A writer must read the collection's directory and index to append and maintain indexes |
| Q5 | Admin implies read and write? | **Yes** | Granting `read` to a new identity means wrapping a key the admin must hold |
| Q6 | Revocation | **Forward-only by default** (a new key epoch); `fylo rekey --collection` rewrites old frames on request | Rewriting at once would make a revoke as slow as a full pack on a large collection |
| Q7 | Trust anchor | **Pinning required.** An open of a protected database without a pinned anchor fails with `EPOLICY`, unless the caller opts in to trust on first use, which warns. **Decided** (the user, 2026-11-07) | Without a pin, whoever can write the file can replace the database with one carrying their own anchor |
| Q8 | Hardware keys | **Later** (phase 8); version 1 uses software keys in the OS secret stores | Hardware keys take milliseconds per operation and differ per platform |
| Q9 | `fylo link` | **Yes:** `fylo.identity` and `fylo.trustAnchor` in `application.json`, or `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR` | Turso's `Database::open` carries only a path |
| Q10 | The browser | **Yes:** the origin is the boundary, plus an optional `webcrypto:` identity for files moved between origins | OPFS is already private to the origin |
| Q11 | Buckets and the queue | **They wait in protected databases:** a protected `.vdbx` refuses them until `.vdbx` holds them; they stay available in unprotected roots. **Decided** (the user, 2026-11-07) | Shipping them unprotected beside protected collections would be an allow by default |

### 4. Protected files (phase 3)

What a protected `.vdbx` file holds, in addition to ADR 0016:

- **Superblock.** Incompatible feature bit 4, `INCOMPAT_PROTECTED`, always
  with `INCOMPAT_ENCRYPTED` and never with `INCOMPAT_SEALED`; a build that
  predates it refuses the file. The 32-byte key-salt field holds a random
  **database id** instead of a salt, and the key-check field holds the
  catalog key's check value, so a catalog key from another file is refused.
- **Access slots.** The last 512 KiB of the directory extent's payload hold
  two copies, A and B, of the **access document**: `VDXACCS\0`, a
  generation, a length, a CRC-32C, then the document. A change writes the
  copy that does not hold the newest valid generation and flushes it, so a
  stop leaves the old document or the new one whole (the superblock rule).
  The directory therefore holds 383 collections at 2 MiB extents and 127 at
  1 MiB. A document is at most 256 KiB less 24 bytes; a change that would
  pass that fails with `EPOLICY`.
- **Access document** (`fylo.access/1`, JSON): every policy version, oldest
  first, and the keyring.
    - Each policy version (`fylo.policy/1`) is sealed with the catalog key
      (AES-256-GCM, a new seal context bound to its version number) and
      signed with ECDSA P-256 over `fylo.policy/1\0`, the database id and its
      bytes.
    - Version 1 is signed by an identity it makes admin of the database; that
      signer is the **trust anchor**. Each later version names the one before
      by SHA-256 and is signed by an admin of the one before. A chain that
      breaks anywhere is refused with `EPOLICY`.
    - A policy names identities by public key (`fpk1:p256:` and the
      base64url SEC1 point), since a wrap and a signature check need the
      key, not only its fingerprint. `fylo identity show --json` prints it.
    - The keyring holds HPKE wraps (section 2) of the catalog key, for every
      identity a rule names, and of each collection key, per collection and
      epoch, for every identity a rule covering that collection names. The
      HPKE `info` binds a wrap to the database id, the key's scope, the
      collection and the epoch.
- **Keys.** The catalog key seals the directory and the policy. A collection
  key seals that collection's records, dictionaries and snapshots; a unit is
  opened with the newest epoch's key first, then older ones. A collection
  created by an admin gets a fresh key, wrapped and flushed to the access
  slots before its directory block names it.
- **Opening.** An open with an identity unwraps the catalog key, checks it,
  checks the policy chain and the anchor (pinned, trusted on first use, or
  refused when neither is given), and unwraps its collection keys. Without an
  identity the file is locked (`ENOIDENTITY`). An identity with no wrap gets
  `EACCES`. One process opens a database as one identity; a second is
  refused.
- **Grants in phase 3.** An admin may add and drop collections; changing a
  collection's descriptor needs `write` on it; sealing any unit of a
  collection needs `write` on it. Phase 4 backs the write grant with a
  signature on every record.
- **Grants and revocation.** `fylo access grant` and `fylo access revoke`,
  run as an admin, write the next policy version, signed by that admin, and
  rebuild the keyring from it: the catalog key for every identity a rule
  names, and every epoch of each live collection's key for every identity a
  rule covering that collection names. So a grant wraps the keys the new
  rule covers, and a revocation drops the revoked identity's wraps.
    - A revocation starts a new key epoch for every collection the removed
      rule covered; new units seal with it. Units sealed before stay under
      the old epoch, readable by an identity that kept the old key, until a
      rekey rewrites them (Q6). The catalog key is not rotated: an identity
      that kept it can still read collection names and the policy until a
      rekey (phase 7).
    - A policy keeps at least one admin; a rule id is used once; resources
      are `database`, `collection:*` and `collection:<name>`.
    - The change is written under the allocation lock and checkpointed. A
      writer in another process takes it in at its next refresh; until then
      it may seal units with the epoch it holds.
    - A frame of a collection the identity has no key for stops a scan as
      unreadable, never as a torn tail, and reads fail with `EACCES`.
- **Signed records (phase 4).** Every record frame of a protected file
  (commits, aborts, checkpoints, compactions and schema versions) is signed
  by the identity that wrote it.
    - The stored body's header byte gets flag `0x40`, and a 97-byte trailer
      follows the stored bytes: a signer kind (1, an identity key), the
      signer's fingerprint (the 32 bytes its `fid1:` identifier encodes) and
      an ECDSA P-256 signature over `fylo.frame/1\0`, the database id, the
      collection id, the record's LSN and the SHA-256 of the stored bytes
      before the trailer. The frame's length and CRC cover the trailer.
    - A checkpoint is signed as the `Checkpoint` record it appends, whose
      manifest names the snapshots' SHA-256; the checkpoint slot itself has
      no room for a signature.
    - A reader accepts a frame when its signer is a key some policy version
      names, the signature verifies, and that key could write the
      collection at that LSN: under the newest version whose revocation
      boundary for the collection is at or before the LSN, or a later one.
      A revocation records, in the version it writes, each covered
      collection's next LSN as that boundary (`boundaries`), so a revoked
      writer's earlier records stay valid and its later ones do not.
    - A frame that fails is `ESIGNATURE`. It is never cut as a torn tail. A
      reader reads the collection as it stood before the first such record;
      a writer refuses to write past it, since readers would stop before
      what it added. `fylo verify <db.vdbx>` run with an identity checks
      every frame's signature (`badSignature`); without one it checks only
      the structures, as before.
    - Compaction and checkpoints sign what they write, so a compactor needs
      `write` on the collection.
    - A process remembers the frames it verified (and signed), so a record
      is checked once per process. Measured on this Mac (Apple M4, release,
      software keys): ECDSA P-256 sign 93 µs and verify 94 µs at p50; a
      durable put 202 µs protected against 106 µs plain and 107 µs
      encrypted with a shared key; an open and list of a 400-document
      collection 542 µs protected against 58 µs plain, the difference being
      the one-time key unwraps and policy-chain check.
    - **Session certificates wait for hardware keys (phase 8).** With
      software keys, which version 1 uses (Q8), a session key costs the same
      per signature as the identity key, and a certificate needs storage the
      access slots would fill. The signer-kind byte leaves room for them; a
      build that predates a new kind refuses its frames.
    - **Not covered:** rollback (a whole older copy of the file), and a
      location or index snapshot rewritten by a process that holds the
      collection key while the checkpoint record names the old digests:
      writers' recovery compares them, readers trust the slot.
- **The catalog key is not rotated on revocation** (phase 3's gap, kept).
  Rotating it means re-sealing every directory block and policy version and
  changing the superblock's key-check value, three writes a crash can
  separate; doing it crash-safe needs either a second catalog key readers
  try during the change or the whole-file rewrite of `fylo rekey`. What it
  protects is collection names and the policy text, from an identity that
  kept the old key; the records stay under collection keys whose epochs do
  rotate. It waits for the protected `fylo rekey` (phase 7), which rewrites
  the file under fresh keys anyway.
- **Not yet.** `fylo pack --sealed` and `fylo rekey` refuse a protected
  database (phases 6 and 7). The browser build has no identities (phase 5)
  and opens a protected file locked; P-256, ECDSA and HPKE stay out of
  `fylo-browser.wasm`, which they would grow by about 190 KB. Policy
  conditions (`notBefore`, `notAfter`) are not in `fylo.policy/1` yet.
- **Creation.** `fylo create <db.vdbx> --admin <reference>`;
  `fylo create` without `--admin` needs `--unprotected` (Q2). The machine
  protocol creates a missing database protected, with the configured
  identity as its admin, and refuses with `ENOIDENTITY` when none is
  configured (decision P5-1, phase 5).
- **Entry points (phase 5).** The `fylo` crate opens `.vdbx` databases
  (no directory lease; the file coordinates its writers) and takes
  `OpenOptions::with_identity(identity, trust)`; a database holds no queue
  (`ELAYOUTUNSUPPORTED`). `fylo::LinkIdentity` reads `FYLO_IDENTITY` and
  `FYLO_TRUST_ANCHOR`, or `fylo.identity` and `fylo.trustAnchor` from the
  `application.json` beside the database, for `fylo link`'s open hook (Q9).
  The hook still opens directory roots only: SQL over a `.vdbx` database,
  protected or not, is not built yet, and `Root::sql` refuses one with
  `ELAYOUTUNSUPPORTED`.
- **`cng:` (phase 5, decision P5-2).** A key persisted under its name in the
  Microsoft Software Key Storage Provider (DPAPI-protected, per user),
  imported from PKCS#8 with an export policy so FYLO signs in software;
  `crates/fylo-identity/src/cng_key_store.rs` is that crate's one `unsafe`
  module (`docs/security/unsafe-inventory.md`). Checked for the Windows
  target; it runs only on the `windows-vm` lane.
- **Browser and Explorer (phase 5, decision P5-3).** The browser build
  keeps P-256, ECDSA and HPKE out of `fylo-browser.wasm` and opens a
  protected file locked; it creates no database through the machine
  protocol. Explorer shows such a file as protected and locked, with a note
  that its collections and documents stay sealed. A `webcrypto:` identity
  is not built yet: WebCrypto is asynchronous, while the engine signs and
  checks each record synchronously inside a frame codec. The proposed way
  (a decision for the user) is a crypto bridge Worker over a
  `SharedArrayBuffer`, the pattern the OPFS bridge already uses, so the
  engine blocks on `Atomics.wait` while the bridge calls WebCrypto with the
  origin's non-extractable key. The user chose that way (P5-5).

### 5. Decisions for phase 5 (the user, 2026-11-07)

Each was decided as recommended. Phase 5 starts when the user says so.

| | Question | Decision | Reason |
| --- | --- | --- | --- |
| P5-1 | What does the machine protocol do with a missing `.vdbx`? | Create it **protected**, with the configured identity as its admin and trust anchor; with no identity configured, refuse with `ENOIDENTITY` | Deny by default (Q2): a database no one is named for is not created open |
| P5-2 | How does `cng:` reach Windows' key APIs? | An **exception to ADR 0004's no-`unsafe` rule** for one small, reviewed FYLO module over Microsoft's `windows-sys` bindings, recorded in `docs/security/unsafe-inventory.md`; the rest of FYLO stays `unsafe`-free | CNG is a C API with no safe binding of its own; one owned, reviewed module is easier to audit than a third-party wrapper |
| P5-3 | How does the browser hold a `webcrypto:` identity? | The **browser's own WebCrypto**, called from FYLO's JavaScript adapter (non-extractable P-256 keys in the origin's IndexedDB); the Wasm engine keeps no P-256, ECDSA or HPKE code | Putting the Rust crypto in `fylo-browser.wasm` adds about 190 KB and breaks its size budget (section 4) |
| P5-4 | Session certificates | **Wait for phase 8** (hardware keys) | With software keys they cost the same per signature and need storage (section 4, "Signed records") |
| P5-5 | How does the browser engine sign and check records with a `webcrypto:` identity? | A **crypto bridge Worker** calls WebCrypto with the origin's non-extractable key, and the engine waits on a `SharedArrayBuffer` with `Atomics.wait`, as the OPFS bridge does; the browser gets full read and write, and the engine stays within its size budget | WebCrypto is asynchronous, and the engine signs and checks each record synchronously |
| P5-6 | SQL over `.vdbx` | **Built before phase 6**, protected and unprotected, so Turso-SDK apps reached through `fylo link` open protected databases | `fylo link` is how most applications reach FYLO (ADR 0019) |
| P5-7 | Run `cng:` on Windows | **Yes**, on the `windows-vm` lane under the shared VM lock, then stop the VMs | It is the only way to run the one `unsafe` module |
| P5-8 | Phase 6: `fylo pack --protect` and per-record access descriptors | **Refuse** unless `--drop-record-access` or `--keep-record-access-as-metadata` is given | A silent drop would lose access rules the old root enforced |

Also settled: POSTIX (`.as()`, `--uid`, `--groups`, the per-request
`access` field) goes in the same breaking release (Q1); `fylo rekey`,
`fylo pack --sealed` and catalog-key rotation for protected databases stay
in phase 7.

### 6. Phases

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
