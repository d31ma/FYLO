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

| Reference          | Private key                                                                                                               | Platforms                      |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------- | ------------------------------ |
| `file:<path>`      | PKCS#8 PEM file, mode `0400` or `0600`, owned by the process's user. FYLO refuses a file any other user can read or write | Everywhere; development and CI |
| `keychain:<label>` | A macOS keychain item; `FYLO_KEYCHAIN` names a keychain file to use instead of the default keychain                       | macOS                          |
| `enclave:<label>`  | A Secure Enclave key that never leaves the enclave; its token blob is a keychain item (phase 8, section 8)                | macOS with a Secure Enclave    |
| `cng:<name>`       | A Windows CNG key, DPAPI-protected                                                                                        | Windows                        |
| `creds:<name>`     | A `systemd-creds` credential handed to one unit                                                                           | Linux services                 |
| `webcrypto:<name>` | A non-extractable `CryptoKey` in the origin's IndexedDB                                                                   | Browser                        |

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

|     | Question                      | Proposed decision                                                                                                                                                                                                                                                                         | Reason                                                                                                                          |
| --- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | Per application only?         | **Yes.** POSTIX (per-record `uid`/`gid`/`mode`, `.as()`, `--uid`, `--groups`, the machine `access` field) is removed in the breaking release; end-user rules stay in the application                                                                                                      | A per-record rule is only a claim FYLO's own code checks, which the caller can skip                                             |
| Q2  | Protected by default?         | **Yes.** A new `.vdbx` is protected unless created with `--unprotected`; directory roots stay as an unprotected layout for development and tooling. **Decided** (the user, 2026-11-07)                                                                                                    | Deny by default; a directory root leaks indexed values through file names (study 2.5) and cannot be protected                   |
| Q3  | Key storage per platform      | **Accepted for version 1, documented:** same-user malware on Linux desktops and unpackaged Windows apps can take a software key. Servers are told to run each application under its own OS user (`creds:` on Linux). The option D helper is not built. **Decided** (the user, 2026-11-07) | The study's threat model (section 6) shows no in-process design does better there; option D brings back a privileged subprocess |
| Q4  | Write implies read?           | **Yes**                                                                                                                                                                                                                                                                                   | A writer must read the collection's directory and index to append and maintain indexes                                          |
| Q5  | Admin implies read and write? | **Yes**                                                                                                                                                                                                                                                                                   | Granting `read` to a new identity means wrapping a key the admin must hold                                                      |
| Q6  | Revocation                    | **Forward-only by default** (a new key epoch); `fylo rekey --collection` rewrites old frames on request                                                                                                                                                                                   | Rewriting at once would make a revoke as slow as a full pack on a large collection                                              |
| Q7  | Trust anchor                  | **Pinning required.** An open of a protected database without a pinned anchor fails with `EPOLICY`, unless the caller opts in to trust on first use, which warns. **Decided** (the user, 2026-11-07)                                                                                      | Without a pin, whoever can write the file can replace the database with one carrying their own anchor                           |
| Q8  | Hardware keys                 | **Later** (phase 8); version 1 uses software keys in the OS secret stores                                                                                                                                                                                                                 | Hardware keys take milliseconds per operation and differ per platform                                                           |
| Q9  | `fylo link`                   | **Yes:** `fylo.identity` and `fylo.trustAnchor` in `application.json`, or `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR`                                                                                                                                                                         | Turso's `Database::open` carries only a path                                                                                    |
| Q10 | The browser                   | **Yes:** the origin is the boundary, plus an optional `webcrypto:` identity for files moved between origins                                                                                                                                                                               | OPFS is already private to the origin                                                                                           |
| Q11 | Buckets and the queue         | **They wait in protected databases:** a protected `.vdbx` refuses them until `.vdbx` holds them; they stay available in unprotected roots. **Decided** (the user, 2026-11-07)                                                                                                             | Shipping them unprotected beside protected collections would be an allow by default                                             |

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
      it may seal units with the epoch it holds. Since phase 7 a
      revocation also rotates the catalog key (section 7).
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
- **The catalog key was not rotated on revocation** in phases 3 to 6;
  phase 7 rotates it (section 7).
- **Not yet** (as of phase 3; phase 7, section 7, added both).
  `fylo pack --sealed` and `fylo rekey` refused a protected database. The browser build has no identities (phase 5)
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
  SQL over a `.vdbx` database, protected or not (decision P5-6): `Root::sql`
  registers the database's live document collections as tables and reads
  and writes them through the same root, so a protected one runs as the
  identity it was opened with; opened without one it is refused with
  `ENOIDENTITY` rather than shown with no tables. Each statement's rows
  commit as one segment plan (all or none), the `.vdbx` form of the files
  layout's batch log record. The hook opens any path
  named `*.vdbx` (and directory roots, as before), and refuses a missing
  one (`ENOENT`) instead of creating a database or a SQLite file.
- **`cng:` (phase 5, decision P5-2).** A key persisted under its name in the
  Microsoft Software Key Storage Provider (DPAPI-protected, per user),
  imported from PKCS#8 with an export policy so FYLO signs in software;
  `crates/fylo-identity/src/cng_key_store.rs` is that crate's one `unsafe`
  module (`docs/security/unsafe-inventory.md`). Checked for the Windows
  target; it runs only on the `windows-vm` lane.
- **Browser and Explorer (phase 5, decisions P5-3 and P5-5).** The
  browser build keeps P-256, ECDSA and HPKE out of `fylo-browser.wasm`. A
  `webcrypto:<name>` identity is one P-256 key pair kept in the origin's
  IndexedDB as two non-extractable `CryptoKey`s (ECDSA to sign, ECDH to
  open wraps), made by `createWebCryptoIdentity`; `describeIdentity` gives
  its `fpk1:` public key, which an admin grants, and its `fid1:`
  identifier. WebCrypto is asynchronous, while the engine signs and checks
  each record synchronously inside its frame codec, so the host table
  carries five identity calls (public key, sign, verify, unwrap, wrap; host
  ABI 2) and the single-file Worker answers them through a crypto bridge
  Worker (`dist-web/identity-worker.js`): the engine blocks on
  `Atomics.wait` over a `SharedArrayBuffer` while the bridge runs WebCrypto,
  the pattern of the OPFS bridge. HPKE (RFC 9180 base mode, DHKEM P-256,
  HKDF-SHA256, AES-256-GCM) is written in JavaScript over WebCrypto and
  opens native wraps, as native opens its wraps. The page passes
  `identity: { name, workerUrl, trustAnchor }` when it opens a database,
  and the module takes it through `fylo_configure` (module ABI 2). The
  browser gets full read and write, and an admin identity can create
  collections; opening as an identity needs a cross-origin isolated page
  (`ENOISOLATION` otherwise). Without an identity a protected file opens
  locked (`ENOIDENTITY`), and the browser still creates no database through
  the machine protocol. Explorer shows such a file as protected and
  locked, with a note that its collections and documents stay sealed.
- **The browser engine's size (phase 5).** The access code in
  `fylo-browser.wasm` (policy, grants, signed records, key epochs, and the
  identity paths `fylo_configure` makes reachable) measured about 125 KB.
  Half the overshoot first blamed on `Session::dispatch_once` was a
  measuring error: mangled names carry per-build crate hashes, and with
  them stripped that function did not grow. The growth was the access code
  itself plus about 30 KB of sort and `BTreeMap` copies per element type,
  which building maps by insertion, `sort_unstable` and one
  `&dyn Fn` filter removed. What remains is the identity wiring, so the raw
  budget in `scripts/verify-browser-wasm-budgets.mjs` rose from 2,432 KiB
  to 2,448 KiB (2,498,895 bytes at the change); the gzip budget (800 KiB)
  did not change, and the module is 804,949 bytes gzipped. The user had
  agreed to about 20 KB if the module was still over after one more cut.

### 5. Decisions for phase 5 (the user, 2026-11-07)

Each was decided as recommended. Phase 5 starts when the user says so.

|      | Question                                                                         | Decision                                                                                                                                                                                                                                                        | Reason                                                                                                                   |
| ---- | -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| P5-1 | What does the machine protocol do with a missing `.vdbx`?                        | Create it **protected**, with the configured identity as its admin and trust anchor; with no identity configured, refuse with `ENOIDENTITY`                                                                                                                     | Deny by default (Q2): a database no one is named for is not created open                                                 |
| P5-2 | How does `cng:` reach Windows' key APIs?                                         | An **exception to ADR 0004's no-`unsafe` rule** for one small, reviewed FYLO module over Microsoft's `windows-sys` bindings, recorded in `docs/security/unsafe-inventory.md`; the rest of FYLO stays `unsafe`-free                                              | CNG is a C API with no safe binding of its own; one owned, reviewed module is easier to audit than a third-party wrapper |
| P5-3 | How does the browser hold a `webcrypto:` identity?                               | The **browser's own WebCrypto**, called from FYLO's JavaScript adapter (non-extractable P-256 keys in the origin's IndexedDB); the Wasm engine keeps no P-256, ECDSA or HPKE code                                                                               | Putting the Rust crypto in `fylo-browser.wasm` adds about 190 KB and breaks its size budget (section 4)                  |
| P5-4 | Session certificates                                                             | **Wait for phase 8** (hardware keys)                                                                                                                                                                                                                            | With software keys they cost the same per signature and need storage (section 4, "Signed records")                       |
| P5-5 | How does the browser engine sign and check records with a `webcrypto:` identity? | A **crypto bridge Worker** calls WebCrypto with the origin's non-extractable key, and the engine waits on a `SharedArrayBuffer` with `Atomics.wait`, as the OPFS bridge does; the browser gets full read and write, and the engine stays within its size budget | WebCrypto is asynchronous, and the engine signs and checks each record synchronously                                     |
| P5-6 | SQL over `.vdbx`                                                                 | **Built before phase 6**, protected and unprotected, so Turso-SDK apps reached through `fylo link` open protected databases                                                                                                                                     | `fylo link` is how most applications reach FYLO (ADR 0019)                                                               |
| P5-7 | Run `cng:` on Windows                                                            | **Yes**, on the `windows-vm` lane under the shared VM lock, then stop the VMs                                                                                                                                                                                   | It is the only way to run the one `unsafe` module                                                                        |
| P5-8 | Phase 6: `fylo pack --protect` and per-record access descriptors                 | **Refuse** unless `--drop-record-access` or `--keep-record-access-as-metadata` is given                                                                                                                                                                         | A silent drop would lose access rules the old root enforced                                                              |

Also settled: POSTIX (`.as()`, `--uid`, `--groups`, the per-request
`access` field) goes in the same breaking release (Q1); `fylo rekey`,
`fylo pack --sealed` and catalog-key rotation for protected databases stay
in phase 7.

### 6. Migration and the removal of POSTIX (phase 6)

- **`fylo access report <root|db.vdbx>`** reads every record and writes
  nothing. It lists, per collection, the POSTIX descriptors the records
  carry (grouped by `uid`, `gid` and `mode`), the live and deleted
  documents and the `$encrypted` fields; for the root, its encryption
  (`none`, `shared-key`, `protected`), what `fylo pack` refuses, what a
  move changes and the command to run.
- **`fylo pack --protect --admin <reference>`** copies a directory root, or
  a plain or shared-key database opened with its key, into a new protected
  database, through the staging file, the document oracle and the adopt
  step plain packs use, and checks the staging file as `fylo verify` does
  with the admin's identity (structures and signatures) before naming it.
  `--grants <file.json>` adds grants inside the same pack. Records with a
  descriptor are refused (`EPOLICY`) unless `--drop-record-access` or
  `--keep-record-access-as-metadata` is given (P5-8); a source with no
  descriptor needs neither, since nothing would be dropped. Kept
  descriptors become the metadata field `legacyAccess`, unchanged and
  unenforced. Record file modes, the descriptors' projection, are dropped.
  A protected source is refused (its rewrite is phase 7's `fylo rekey`).
- **POSTIX is removed** (Q1). Old descriptors stay on their records as
  opaque attributes: reads ignore them and print a one-time warning per
  process, and layout migrations and plain packs copy them, so no data is
  lost before a migration. A machine request that still carries `access`
  is refused with `EBADREQUEST`, and the removed CLI flags fail, rather
  than be ignored, so an application that relied on per-record rules
  finds out instead of losing them silently.
- **The handshake reports the identity** (`identity`: its `id`,
  `publicKey`, trust setting and, for a protected root, its grants or the
  code the database refused it with); it opens the root read-only and takes
  no write lease.

### 7. Revocation, rekey and audit (phase 7)

- **`$encrypted` fields use per-collection keys (decided by the user,
  2026-11-07).** In a protected database a collection's `$encrypted`
  field values are encrypted with a key derived from that collection's
  key, never with the shared `fylo.encryption.key`, which stays for
  unprotected roots only. Each collection key epoch derives one field key:
  64 bytes of HKDF-SHA256 with the epoch's collection key as input, the
  database id as salt and `fylo.field/1\0`, the collection id and the
  epoch as info; the first 32 bytes are the AES-256-GCM key of values
  (the same `v2.` envelope as before) and the last 32 the HMAC-SHA256 key
  of blind-index tokens. So only identities granted the collection read
  its fields, and a revocation's new epoch gives new values a new field
  key.
    - A value is written with the newest epoch's key and read with each
      epoch's key the identity holds, newest first, then with
      `fylo.encryption.key`'s when one is configured: AES-GCM refuses a
      value under any other key, so trying each is safe. That last step
      reads values written before this change, when a protected database
      still used the shared key; `fylo rekey` rewrites them under the
      collection's key, which is the migration path.
    - Indexes follow the key without a separate step. The prefix index
      of an `$encrypted` field is built from the stored values, which are
      ciphertext under a random nonce, so it holds nothing to rotate; a
      rewrite of the values rebuilds it. The blind-index tokens
      `fylo verify` expects are keyed with the newest epoch's field key.
    - `NativeRoot::field_keys` gives the engine a collection's field keys
      (`ENOIDENTITY` for a locked database, `EACCES` without a key of the
      collection), and `DatabaseAccess::field_keys` derives them.

- **`fylo rekey` of a protected database.** Run by an admin (its
  identity, `--identity`), `fylo rekey <db.vdbx>` gives the catalog key
  and every collection key new values, and `fylo rekey <db.vdbx>
  --collection <name>` only that collection's key, at the epoch after its
  newest, so a revoked identity that kept an old key reads none of the
  old data either. `--new-key`, `--encrypt` and `--decrypt` stay for
  shared-key databases.
    - **A new file, not a rewrite in place.** Rewriting the collection's
      units in place would free extents that still hold the old
      ciphertext, readable with the key the revoked identity kept, until
      something reuses them. So both forms write a staging file beside the
      database, as the shared-key rekey does, check it as `fylo verify`
      does with the admin's identity (structures and every signature),
      flush it, and rename it over the database: the rename is the commit
      point, a stop before it leaves the database as it was and a staging
      file the next pack or rekey removes, and running the rekey again does
      all of it again. The database is held exclusively throughout.
    - **What the new file holds.** The same database id, so the policy
      chain's signatures still verify; every policy version, sealed under
      the new catalog key (the signatures cover a version's bytes, not
      its seal, and carry over); then a version with the same rules,
      `change.kind` `rekey` or `rekeyCollection`, signed by the admin; and
      the catalog key wrapped for everyone the policy names. Each
      collection is copied with its documents, deleted documents and
      schema versions, every record signed again by the admin and sealed
      under the collection's newest epoch. A collection whose key is new
      has that one epoch; the others carry every epoch over.
    - **Signatures after a rekey.** Collection ids and LSNs change in the
      new file, so the boundaries of earlier versions no longer name the
      right records. A reader therefore checks a record's signer against
      the newest rekey version and the versions after it only, besides the
      boundary rule; every record the rekey wrote is the admin's.
    - **`$encrypted` values.** A collection whose key is new drops its old
      epochs, so the engine writes each `$encrypted` value of its records
      again under the new field key (`DocumentRekey`, which the CLI gives
      `rekey_protected_database`): read with the old field keys and, when
      configured, `fylo.encryption.key`, written with the new one, and
      checked to read back the same. That is also the migration of values
      a protected database wrote under the shared key: `fylo rekey
      --collection <name>` with `fylo.encryption.key` configured moves
      them to the collection's key.
    - A sealed protected database is never rekeyed: rekey its source and
      pack it again.

- **`fylo pack --sealed` of a protected database.** Run by an admin, it
  builds a copy as a rekey builds its new file, with every key, epoch and
  policy version as they are and no new version (each record signed again
  by the admin), then writes it as a sealed file whose blocks are not
  encrypted again: the superblock keeps `INCOMPAT_PROTECTED` and
  `INCOMPAT_ENCRYPTED` with `INCOMPAT_SEALED`, the database id and the
  catalog key's check value, and the sealed header's block encryption is
  off. Its units stay sealed under the catalog and collection keys and
  signed, so each identity reads only what it was granted, as in the
  source, and no shared key exists to leak. Renumbering the extents
  rewrites only the plain slots of the collection blocks; their sealed
  headers are opened with the catalog key, to see which are dropped, and
  copied as they are. The access slots are read through the image like
  any other byte. The copy is checked against the source with the oracle
  and as `fylo verify` checks it with the admin's identity, then flushed
  and named; a pack that stops leaves no output. `--encrypt` is refused
  with it. The section 4 rule "never with `INCOMPAT_SEALED`" no longer
  holds.

- **A revocation rotates the catalog key.** Until phase 7 an identity
  that was revoked and kept the catalog key could still read collection
  names and the policy. Rotating it touches three things a crash can
  separate (the policy versions in the access slots, every directory
  block, and the superblock's key-check value), so the rotation is laid
  out so that no state in between is ever wrong:
    - **The superblock is never rewritten.** Its check value keeps naming
      the original catalog key. The access document gains `original`, that
      key sealed under the current one (context `CatalogKey`, role 2), so
      an opener unwraps the current key, opens the original with it and
      checks that against the superblock: a key from another file still
      fails, and only a holder of the current key learns the original. The
      original key seals nothing once the rotation finishes.
    - **One write is the commit point.** The revocation's access-slot
      write (the A/B rule, so a stop leaves the old document or the new one
      whole) carries the new catalog key wrapped for every identity the
      new policy names, every policy version sealed under it (their
      signatures carry over), the new version, and `previous`, the old
      catalog key sealed under the new one (role 1). From then on the
      revoked identity has no wrap of the current key.
    - **Then the directory is resealed, and `previous` dropped.** Every
      directory block header is rewritten in place under the new key (as a
      descriptor change rewrites one), and a last slot write removes
      `previous`. Until then a reader opens each block with the current key
      and then the previous one; AES-GCM refuses the wrong one, so a mix of
      both is always readable. A stop after the commit point leaves the
      revocation in force and every block readable, and the next change of
      the access document or of the directory (a grant, a revocation, a new
      collection) finishes the reseal first. Until it does, the revoked
      identity can still open blocks that are under the old key, so the
      rotation runs to the end within the same `fylo access revoke`.
    - **A stopped rotation is settled at once, and reported (gap closed,
      the user's decision, 2026-11-07).** Phase 7 left a stopped rotation
      pending until the next access or directory change, with nothing to
      tell the admin. Now:
        - `fylo access settle <db.vdbx>`, as an admin, finishes it: under
          the allocation lock it takes in the newest access slot, reseals
          every directory block under the current key, and writes the
          access document without `previous` in one slot write, which also
          carries a `settle` policy version signed by the admin (the
          `change` kinds gain `settle`). Every step is the reseal or the
          slot write above, so a stop anywhere leaves the rotation pending
          and readable, and running it again finishes it; with nothing
          pending it says so and writes nothing. The name follows the
          one-verb `fylo access` commands (`grant`, `revoke`, `audit`),
          and "settled" is the state the reseal leaves, as section 7 calls
          it.
        - An admin's first write (taking a collection's writer lock) on a
          database it opened with a rotation pending settles it the same
          way first, before the write takes its lock. It adds no failure
          mode a write did not have: the reseal and slot write are writes
          to the same file under the allocation lock, which writes already
          take, and when the `settle` version would not fit in the 256 KiB
          access slot the rotation is finished without it, since dropping
          the old key matters more than its record. Another identity's
          write leaves the rotation for an admin: it may not sign a policy
          version. A new collection still finishes it first, signed when
          its creator is an admin.
        - `fylo access report` names the pending rotation
          (`catalogRotation`: `pendingSince`, the newest revocation's
          version, a message and the `fylo access settle` command), `fylo
          verify` warns `catalogRotationPending` (a warning: the file is
          sound), also without an identity since `previous` is visible in
          the access document, and the `exec --loop` handshake's
          `identity.database` carries `catalogRotationPendingSince`.
    - A process that had the file open takes the new key in at its next
      refresh of the access slots, as it takes in new collection keys.
    - A grant does not rotate the key; a rekey of the whole database
      writes a new file under a new one anyway.

- **Audit: the signed policy chain (option A, decided by the user,
  2026-11-07).** The record of grants, revocations, rekeys and policy
  changes this phase was asked for. The user chose it over option B, a
  separate append-only audit extent. Every such change already writes a policy version,
  signed by the admin who made it and chained to the one before, in the
  access slots, whose A/B write is crash-safe. Each version from phase 7
  on carries `change` (`grant`, `revoke`, `rekey`, `rekeyCollection`,
  `settle`;
  with the rule or collection, and `at`, milliseconds since the Unix epoch
  by the signer's clock), covered by the signature. `fylo access audit
  <db.vdbx>`, run as any identity the policy names, lists every version
  oldest first with the admin whose signature it carries, its change, and
  the rules it added and removed against the version before; version 1 is
  `create`, and a version written before changes were recorded is
  `unrecorded`. It writes nothing. A catalog-key rotation is part of its
  revocation and has no version of its own, unless it stopped and was
  settled later, which writes a `settle` version.
    - **Deferred by the user's decision: logging opens and refusals.**
      Design section 5.6 also lists every open, operations FYLO refused,
      and `ESIGNATURE` findings. Recording those would need a write on
      every open, also by identities that may only read, and a separate
      append-only extent (a new extent kind, sealed under the catalog key
      and resealed by its rotation). The user deferred it to a later
      phase; until then the audit records access changes only, and the
      access slots' size limit (256 KiB) bounds how many versions the
      chain holds. The `at` time is the
      signer's claim, not a trusted timestamp.

### 8. Hardware keys (phase 8)

- **`enclave:<label>`** (macOS). `fylo identity create --store
  enclave:<label>` makes a P-256 key inside the Secure Enclave
  (`SecKeyCreateRandomKey` with the Secure Enclave token). The key never
  leaves the enclave: FYLO asks it to sign (ECDSA, SHA-256) and to agree
  (ECDH), and never sees the private scalar.
    - The key is made **non-permanent**. A permanent key in the Data
      Protection keychain, and an access group, need the
      `keychain-access-groups` entitlement, which only a signed application
      with a provisioning profile carries; an unsigned or ad-hoc signed
      binary, such as `fylo` from `cargo build`, gets
      `errSecMissingEntitlement` (-34018). Measured on this Mac.
    - Instead FYLO keeps the enclave's **token blob** for the key, which
      only this Mac's Secure Enclave can use, as a generic-password item in
      the keychain: service `<label>`, account `fylo-enclave-identity`, in
      the default keychain or in `FYLO_KEYCHAIN`. The keychain's access
      list binds the item to the application that created it, as for
      `keychain:`, so it is the code-signature binding; another
      application needs the user's consent. A copied blob is useless on
      another Mac, and on this one it can only ask the enclave to use the
      key, never export it. `fylo identity show` restores the key from the
      blob (`SecKeyCreateWithData` with the blob as `kSecAttrTokenOID`).
    - An application that is signed with an access group may move to a
      permanent Data Protection keychain key later; that is a new store,
      not this one.
    - The blob and `SecKeyCreateWithData` have no safe binding, so
      `crates/fylo-identity/src/enclave_key.rs` is `fylo-identity`'s second
      `unsafe` module (`docs/security/unsafe-inventory.md`).
    - Measured (Apple M4 Mac mini, this Mac): create 10–12 ms, a signature
      4.5–4.7 ms, an ECDH agreement 4.5 ms.
- **Opening wraps with a hardware key.** The `hpke` crate takes only a
  private key, and the hardware gives out only the ECDH shared secret. So a
  hardware identity opens its wraps with RFC 9180 base mode written out
  (`hardware_key_wrap.rs`: DHKEM(P-256, HKDF-SHA256) decapsulation with the
  hardware's agreement, then the HKDF-SHA256 key schedule and
  AES-256-GCM). It opens exactly the wraps `hpke` seals; its tests open
  `hpke`'s wraps with a software stand-in that agrees as the hardware does.
- **Writes.** A hardware identity signs every record as a software one
  does until session certificates (below) take over; each signature then
  costs milliseconds.

### 9. Phases

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
