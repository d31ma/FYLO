# Access control by application identity ("security groups" for FYLO)

- Status: **Design study, for the user's decision.** Nothing is implemented.
- Date: 2026-11-07
- Base: `origin/develop` at `f27df4c`
- Request (the user's words): "lets replace the current access mechanism
  (POSIX) with a form of how security groups are done in AWS whereby everyone
  is blocked by default (no write/reads), but we need a static identifer for
  applications/processes that we can use to say this entity is allowed to make
  reads/write to the database".
- Standing rules this design keeps: no server or listener of any kind,
  synchronous writes, documents as the source of truth with rebuildable
  zero-payload indexes, filesystem-first, Turso's SDKs as the language clients
  through `fylo link`.

## 1. Summary

**Recommendation:** make a protected database an encrypted `.vdbx` file
whose keys are released only to listed application identities. Each identity
is a key pair. Its public-key fingerprint is the static identifier (for example
`fid1:p256:7QK...`), the way `sg-0abc...` names a security group. Its private
key lives in the operating system's secret store, bound to the application's
code signature where the OS supports that.

A signed policy inside the database lists which identity may `read`, `write`
or `admin` which collection. Everything else is denied.

Reads are enforced by encryption: a process that is not granted a
collection's key gets ciphertext. Writes are enforced by signatures: every
commit is signed by a writer identity, and readers reject a commit that no
granted writer signed.

The rules are not a check inside the library that a process could skip.
Each rule is backed by a key the process either holds or does not hold.

**Why not a policy check alone:** FYLO runs inside the application's process,
or as a CLI, with no server in between. Any check written in FYLO's code can be
bypassed by a process that reads the database files directly or links its own
copy of the library. AWS security groups work because an enforcement point
outside the instance sees every packet. FYLO has only two enforcement points
outside the calling process: the operating system (file permissions, secret
stores, mandatory access control) and cryptography. The design uses both.

**What it changes:** POSTIX (per-record `uid`/`gid`/`mode`, `chown`/`chmod`,
`.as()`, the machine protocol's per-request `access`, `--uid`/`--groups`) is
removed. Its replacement works per application and per collection, not per end
user and per record. Per-end-user rules move into the application, which is
where Yon already authenticates its users.

## 2. What FYLO does today

### 2.1 Where "POSIX" (POSTIX) lives

| Layer | What it is | Code |
| --- | --- | --- |
| Record descriptor | `AccessDescriptor { version, uid, gid, mode }`, stored in the `user.fylo.access` xattr (file layout) or inline in the record (segments and `.vdbx`, ADR 0015 section 10) | `crates/fylo-storage-native/src/lib.rs` (`AccessDescriptor`, `ACCESS_XATTR`) |
| OS projection | On a file-layout record, `apply_access` writes the xattr, then `chown` and `chmod`. Unix only; Windows has no projection | `crates/fylo-storage-native/src/write.rs` (`apply_access`) |
| Caller identity | `AccessContext { uid, groups }` (engine) and `WriteActor { uid, groups }` (storage). The doc comments call it a "trusted actor identity supplied by a client or application boundary" | `crates/fylo-engine/src/lib.rs`, `crates/fylo-storage-native/src/write.rs` |
| Evaluation | Owner bits for the owner UID, otherwise group bits for a member, otherwise other bits. A record with **no descriptor is open to everyone**. Queries and SQL drop unreadable rows; direct operations fail with `EACCES` | `read_access_conditions` in `crates/fylo-engine/src/lib.rs` |
| Root files | The root's profile manifest must be `0600` and owned by the root owner, or the root refuses to open | `crates/fylo-storage-native/src/profile_manifest.rs` |
| Docs | README "POSTIX access control", `clients/README.md` "POSIX access context", ADR 0015 section 10, ADR 0016 "Costs" | |

### 2.2 What is actually enforced

- **By the OS:** only on a file-layout collection on macOS or Linux, where each
  record file gets its own owner and mode. A process of another OS user cannot
  open a record its mode denies. The root's own files are `0600`.
- **By the engine only:** everything else. On segment collections (ADR 0015)
  and in `.vdbx` files (ADR 0016), records share files, so the descriptor is
  enforced only by FYLO's code. ADR 0015 says so: "stores the descriptor and is
  enforced by FYLO, but no `chown` or `chmod` happens". Any process that can
  read the segment or `.vdbx` file reads every record.
- **The identity is a claim, not a proof.** The UID comes from the caller: a
  `--uid` flag, or the `access` object of a machine frame. README: "The UID is
  still an authorization claim supplied by your application. Fylo does not
  authenticate it."
- **Default allow.** A record written without `.as()` has no descriptor and is
  readable and writable by any caller.
- **Not on Windows, not in the browser.** README says POSTIX "is not a Windows
  authorization boundary", and the browser and WebView clients have no `.as()`.

### 2.3 Encryption today

| Feature | Key | Cipher | Scope |
| --- | --- | --- | --- |
| `$encrypted` fields (schema) | PBKDF2-HMAC-SHA256, 100,000 rounds, from `fylo.encryption.key` and `fylo.cipher.salt`; also an HMAC key for blind indexes (`idx1.` tokens, equality only) | AES-256-GCM, random 96-bit nonce, `v2.` prefix | Named fields of a collection |
| Encrypted `.vdbx` (ADR 0016 section 9) | HKDF-SHA256 from `fylo.encryption.key` and a 32-byte salt in the superblock; key-check value gives `EKEY` | AES-256-GCM per frame, random nonce, associated data binds collection id, LSN and entry index | Records, directory, dictionaries, snapshot and index extents. Off by default |
| `fylo rekey` | New secret named by reference | Rewrites the file (a `pack`) | Whole database |

Code: `crates/fylo-engine/src/encryption.rs`,
`crates/fylo-storage-native/src/database_key.rs`,
`crates/fylo-storage-native/src/write/commit_log/database_rekey.rs`.

**One shared secret opens everything.** Anyone who has
`FYLO_ENCRYPTION_KEY` can read and write every collection. Encryption today
protects data at rest from people who do not have the key. It cannot tell one
application from another.

### 2.4 How each entry point names its caller today

| Entry point | Caller identity today |
| --- | --- |
| `fylo` crate (ADR 0019 layer 1, used by Yon) | **None.** `Collection::put` passes `WriteAccess::default()`; `patch` and `delete` pass `None` as the actor. A record with a descriptor reads as "other". |
| `fylo link` in Turso's SDKs (JS, Go, Python, Java, .NET) | **None.** The open hook opens the root with the crate. |
| `fylo exec --loop` (NDJSON v1) | Optional per-request `access: { uid, groups, gid, mode }`, a trusted claim from the parent process (`actor()` in `crates/fylo-machine/src/lib.rs`) |
| CLI | `--uid` and `--groups` flags (`access_context` in `crates/fylo-cli/src/main.rs`) |
| Browser, Wasm, OPFS, Explorer, WebView mobile | **None.** The browser's origin is the only boundary: OPFS is private to the origin. |
| TACHYON Yon | Uses the crate in-process (TACHYON ADR 0032), so none |

### 2.5 Leaks around the records

- **Directory roots leak through file names and the index.** Collection names
  are folder names. Prefix-index keys are stored in plain text:
  `name/f/alice/4UUB32VGUDW`, `age/n/c03e.../...`. "Zero-payload" means the
  index does not copy the document, but each key does contain the indexed field's value. Any
  process that reads `keys.snapshot` reads every indexed value, and the
  trigram keys spell out most of the indexed text.
- **Blind indexes leak equality.** `$encrypted` fields indexed through HMAC show
  which documents share a value, and so how often each value occurs.
- **TTIDs leak creation times,** and file sizes and counts leak volume.
- **An encrypted `.vdbx` leaks none of the above** except sizes, counts and the
  format version, because the directory and index extents are sealed. The only
  protection model that hides data is therefore an encrypted `.vdbx`.

## 3. The hard constraint

The library runs as the caller. So for every rule the question is which party
outside the caller enforces it.

| Mechanism | Who enforces it | Per application? | Holds against a same-user process that skips FYLO? |
| --- | --- | --- | --- |
| A check in FYLO's code | The caller itself | Yes, but as a claim | **No.** It reads the files directly |
| File owner and mode (today) | The kernel | No, per OS user | Yes against other users; **no** against the same user |
| Encryption with keys the caller does not have | Mathematics | Yes | **Yes for reads,** as long as the key is out of its reach |
| Signatures on writes | Mathematics, checked by every reader | Yes | Forged writes are **detected and refused**; deletion and rollback of the file are **not** prevented |
| OS secret store bound to code identity | `securityd`, Keystore, the package system | Yes, where the OS offers it | Yes on macOS (signed apps), iOS, Android and packaged Windows apps; **no** on Linux desktops or unpackaged Windows apps |
| Mandatory access control (AppArmor, SELinux, macOS sandbox, Windows AppContainer ACLs) | The kernel | Yes | Yes, but an administrator installs it per host |
| A broker under its own OS user | The kernel plus the broker | Yes | Yes, but see section 4.4 |

The recommended design therefore rests on cryptography. The OS's job is to
keep each identity's private key away from other applications, which is the
part that varies by platform. Section 6 states plainly what each platform does
and does not prevent.

## 4. Options considered

### 4.1 A. Keep OS users and groups (POSTIX, today)

Strong where it applies, but it works per OS user, not per application. It
needs `chown` privilege, applies to file-layout collections only, does not
exist on Windows or in the browser, and covers no segment or `.vdbx`
collection. It does not meet the request.

### 4.2 B. A policy inside the library, with a claimed identity

An allow-list keyed by an identity string the caller supplies. This is
today's virtual UID with a new name. Any process can claim any identity or
read the files directly. It is useful only as a guard against mistakes, and
it is rejected as a security boundary.

### 4.3 C. Identities as keys, policy backed by encryption and signatures (recommended)

Detailed in section 5. Reads hold against any process that does not have the
key. Writes from an identity without a write grant are detected and refused.
It works the same on every platform and in the browser, and needs no listener.
How strong it is depends on where each platform keeps the private key.

### 4.4 D. A privileged helper on anonymous pipes

The files belong to a dedicated OS user (`fylo`) with mode `0600`. An
application spawns a setuid or setgid `fylo-guard` binary on anonymous pipes,
as `fylo exec --loop` works today. Anonymous pipes are allowed by ADR 0019's
rule 1, so this needs no listener. The helper checks its parent's code
identity (macOS: the parent's code signature through its audit token; Linux:
`/proc/<ppid>/exe` and its hash) and then applies the policy.

This is the only design where the kernel stops a same-user process from
reading the bytes. But:

- it brings back the subprocess that ADR 0019 just replaced with the
  in-process crate;
- a setuid binary is a large attack surface, and installing it needs root;
- it has no Windows equivalent without a service, which ADR 0012's withdrawal
  ruled out;
- it cannot exist in the browser;
- on Linux, checking the parent races with process ID reuse and does not stop
  `ptrace` of the legitimate parent.

It is noted here as a possible server-hardening tier, not for version 1.

### 4.5 E. Mandatory access control profiles

An AppArmor or SELinux profile, or an AppContainer ACL, can let only one
executable open the database file, and the kernel enforces it. It is the
right complement to C on a server, but each host's administrator must
install it, and it differs on every OS. FYLO would document it and ship
sample profiles; it is not part of the engine. Phase 8 added them:
[`docs/hardening/`](hardening/README.md) has an AppArmor profile, an
SELinux module and an AppContainer manifest, as samples.

## 5. Recommended design

### 5.1 Concepts

| AWS | FYLO |
| --- | --- |
| Security group | The database's **policy** |
| Source security group or principal ARN | An **identity**: a P-256 key pair; its identifier is `fid1:p256:<base32 of SHA-256 of the public key>` |
| Inbound rule | An **allow rule**: identity, actions, resources, optional conditions |
| Implicit deny | Nothing is readable or writable without a rule that grants it, and the key that comes with the grant |
| Enforcement in the hypervisor | Enforcement by encryption (reads) and signatures (writes) |

**Why P-256, not Ed25519 or X25519:** the hardware keystores (Secure Enclave,
TPM 2.0, Windows CNG with the Platform Crypto Provider, Android StrongBox)
support P-256 ECDH and ECDSA, and WebCrypto supports non-extractable P-256
keys in every browser. One curve covers software and hardware keys
everywhere.

### 5.2 The policy

The policy is stored as a sealed extent of the `.vdbx` file, with a plain-text
header that names its version and hash. Every change is a new version, signed
by an identity that holds `admin`:

```json
{
  "format": "fylo.policy/1",
  "database": "vdbx:01JD...",
  "version": 7,
  "previous": "sha256:...",
  "identities": {
    "yon": {
      "keys": ["fid1:p256:7QKX..."],
      "label": "TACHYON Yon",
      "expect": { "macosSigning": "JAU9V4WH99.com.delma.yon" }
    },
    "reporting": { "keys": ["fid1:p256:M2PA..."] },
    "ops-admin": { "keys": ["fid1:p256:Z8QE...", "fid1:p256:RECOVERY..."] }
  },
  "rules": [
    { "id": "yon-orders", "identity": "yon", "actions": ["write"], "resources": ["collection:orders", "collection:users"] },
    { "id": "reports", "identity": "reporting", "actions": ["read"], "resources": ["collection:*"], "conditions": { "notAfter": "2027-06-30T00:00:00Z" } },
    { "id": "root-admin", "identity": "ops-admin", "actions": ["admin"], "resources": ["database"] }
  ],
  "signatures": [{ "identity": "ops-admin", "key": "fid1:p256:Z8QE...", "signature": "..." }]
}
```

- **Allow rules only**, as in a security group: rules add up, and nothing is
  allowed without one.
- **Actions:**
  - `read` reads documents and metadata, and runs queries and SQL `SELECT`;
  - `write` covers put, patch, delete and SQL mutations. **Write includes
    read**: a writer must read the collection's index and directory to append
    correctly, so it needs the collection key. A write-only "drop box" is
    possible later with public-key sealing, but it could not maintain
    indexes;
  - `admin` changes the policy, creates and drops collections, rotates keys
    and recovers. It includes read and write, because granting `read` to a
    new identity means wrapping a key the admin must hold.
- **Resources:** `database` (every collection, including future ones),
  `collection:<name>`, and `collection:*`.
- **Conditions:** `notAfter` and `notBefore` are checked against the clock of
  whoever opens the database. That is advisory, because clocks can be changed.
  `expect` names the code-signing identity the identity's key is meant to live
  in. FYLO checks it in-process and logs a mismatch, but the real check is
  the OS secret store refusing other applications the key (section 6).
  Conditions that only code can check are marked advisory in the
  documentation and in `fylo access list`.

### 5.3 Keys

```
identity private key (in the OS secret store, never in the database)
   └─ unwraps (HPKE, RFC 9180: DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM)
        ├─ catalog key       wrapped for every granted identity: collection names, the policy, the audit log
        └─ collection key    one per collection and epoch, wrapped for each identity with read, write or admin on it
              ├─ seals the collection's record frames, dictionaries, location and index extents (AES-256-GCM, as ADR 0016 section 9)
              └─ derives the blind-index key and the `$encrypted` field key (HKDF with distinct labels)
```

- The wraps live in a **keyring** extent: one HPKE-sealed collection key per
  (collection, epoch, identity key). With 10 identities and 50 collections that
  is a few hundred wraps of about 100 bytes each.
- At open, the process unwraps only the keys its identity was granted, once.
  Every later frame uses the cached AES key, exactly as an encrypted `.vdbx`
  does today.
- `fylo.encryption.key` (one shared secret) is retired for protected
  databases. A plain `.vdbx` and a plain directory root still open without
  keys (see question Q2).

### 5.4 Writes are signed

A collection key alone cannot stop a reader from writing. Anyone who can
decrypt can also encrypt, and AES-GCM proves only that the writer knew the
key. Writes therefore carry signatures:

1. **Session certificate.** At open, a writer creates a fresh in-memory P-256
   session key and signs a short certificate with its identity key. The
   certificate names the session key, the identity, the policy version and an
   expiry. This costs one identity-key operation per open, which matters when
   the identity key is in a Secure Enclave or TPM: those take milliseconds
   per operation.
2. **Signed commits.** Every commit record (ADR 0013, ADR 0016) carries the
   session certificate's hash and an ECDSA signature by the session key. The
   signature covers the commit's frame digests, the LSN range, the previous
   commit's digest and the policy version.
3. **Signed checkpoints.** A checkpoint (superblock) is signed the same way
   and covers the extent map's digest. So a reader that opens the database
   verifies the latest checkpoint and the commits after it, not the whole
   history.
4. **Readers check** that each commit's signer held `write` on that
   collection under the policy version the commit names. If not, the reader
   ignores the commit and every commit after it, opens the database read-only
   at the last valid commit, and reports `ESIGNATURE` and `fylo verify`
   names the offending range.
5. **Compaction and pack re-sign.** The compactor is a writer (or an admin)
   and signs the frames it rewrites. A record's original signer is kept in
   the record's attributes, for audit.

A process without a write grant can still change bytes in the file if the OS
lets it. The change is detected and refused, and the database falls back to
its last valid commit. It cannot be prevented, only detected (section 6).

### 5.5 The trust anchor, the first admin, and changes

- **Creation:** `fylo create app.vdbx --admin keychain:com.delma.fylo-admin`.
  This writes policy version 1 with that identity as `admin` on `database`,
  wraps the catalog key for it, and stores the admin's identifier in the
  signed superblock as the **trust anchor**.
- **Pinning:** an application names the expected anchor in its configuration
  (`fylo.trustAnchor: "fid1:p256:Z8QE..."`). Without pinning, a process that
  can write the file could replace the whole database with one it made
  itself, with its own anchor. A pinned anchor makes the application refuse
  it with `EPOLICY`. An unpinned first open trusts on first use and warns,
  which is question Q7.
- **Granting:** `fylo access grant --identity yon --key fid1:p256:7QKX...
  --actions write --collection orders`, run with an admin identity. It writes
  a new policy version and a wrap of the collection key for the new key.
- **Revoking:** `fylo access revoke --rule reports`. It writes a new policy
  version and **starts a new key epoch** for every collection the rule
  covered: new frames use the new key, and the revoked identity gets no wrap
  for it. Data written before the revocation stays readable to anyone who
  kept the old key. This is forward-only, as with AWS KMS key rotation.
  `fylo rekey --collection orders` rewrites the old frames under the new
  epoch to finish the revocation (a per-collection form of today's
  `fylo rekey` pack).
- **Rotating an identity's key:** add the new key to the identity, which
  wraps the keys for it, then remove the old key, which revokes it.
- **Recovery:** create a second `admin` key that is kept offline (a key file
  on removable media, or a hardware key) as break-glass. Losing every admin
  key freezes the policy, but granted identities keep their access. Losing
  every key loses the data, as a lost `fylo.encryption.key` does today.

### 5.6 Audit log

A sealed, append-only audit extent under the catalog key, signed like
commits. It records:

- policy versions: who signed them and what changed;
- every open: identity, process path, OS user, code-signing identity where
  known, and the actions granted;
- operations that FYLO's own code refused, such as a write attempt without a
  grant;
- `ESIGNATURE` findings.

`fylo access audit app.vdbx` prints it. **It records only processes that run
FYLO's code honestly.** A process that reads the file without FYLO leaves no
trace in it: it gets ciphertext, and it cannot write an entry. An
application's own audit of its end users stays in the application.

### 5.7 How each entry point presents its identity

One identity per open database, per process. The identity is named by a
**key reference**, never by the key itself:

| Reference | Where the private key lives | Platforms |
| --- | --- | --- |
| `keychain:<label>` | macOS data-protection keychain item in the app's access group, or a Secure Enclave key (`se:` prefix) | macOS, iOS |
| `cng:<name>` | Windows CNG key, Platform Crypto Provider (TPM) when present, else software KSP; DPAPI-protected | Windows |
| `creds:<name>` | `systemd-creds` credential (TPM2 or host key), handed by systemd to one named unit | Linux services |
| `keystore:<alias>` | Android Keystore, which keeps keys per app | Android WebView host |
| `file:<path>` | PKCS#8 file, which must be `0400` or `0600` and owned by the process's user | Everywhere; development and CI |
| `webcrypto:<name>` | Non-extractable `CryptoKey` in the origin's IndexedDB | Browser |

| Entry point | How it names the identity |
| --- | --- |
| `fylo` crate (Yon) | `OpenOptions::identity(Identity::from_reference("keychain:com.delma.yon")?)` and `OpenOptions::trust_anchor("fid1:...")` |
| `fylo link` (Turso SDKs) | Turso's `Database::open` takes only a path, so the hook reads `fylo.identity` and `fylo.trustAnchor` from the `application.json` beside the database, or `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR` from the environment. A URI parameter (`?fylo_identity=`) is possible only if the pinned `turso_core` parses URIs on that path; to be checked |
| `fylo exec --loop` | `--identity <ref>` at spawn. The handshake reports the identity and its grants. The per-request `access` field is removed (machine protocol v2) |
| CLI | `--identity <ref>`, `FYLO_IDENTITY`, or `fylo.identity` in `application.json` |
| Browser and Wasm | `webcrypto:<name>`. In the browser the origin is the real boundary: OPFS is already private to the origin, and the identity matters when a `.vdbx` file moves between origins or apps |
| Explorer | Opens a protected database with an identity the user imports or picks; otherwise shows it as locked (ADR 0016's locked mode) |

Opening a protected database without an identity, or with one that has no
grant, opens it **locked** (ADR 0016 section 9 already defines this). It lists
no collections, every read fails with `EACCES`, and every write fails with
`EACCES`. A new error, `ENOIDENTITY`, says that no identity was given.

### 5.8 Rust and CLI contracts (sketch, not final)

```rust
pub struct Identity { /* a handle to a private key in a key store */ }
impl Identity {
    pub fn from_reference(reference: &str) -> Result<Identity, Error>;
    pub fn id(&self) -> IdentityId; // fid1:p256:...
}

impl OpenOptions {
    pub fn identity(self, identity: Identity) -> OpenOptions;
    pub fn trust_anchor(self, anchor: IdentityId) -> OpenOptions;
}

impl Root {
    pub fn access(&self) -> AccessAdmin; // grant, revoke, list, rotate, audit (admin only)
    pub fn grants(&self) -> Grants;      // what this process may do
}
```

```bash
fylo identity create --store keychain:com.delma.yon
fylo identity show keychain:com.delma.yon
fylo create app.vdbx --admin keychain:com.delma.fylo-admin
fylo access grant app.vdbx --identity yon --key fid1:p256:7QKX... --actions write --collection orders
fylo access revoke app.vdbx --rule reports
fylo access list app.vdbx
fylo access audit app.vdbx
fylo rekey app.vdbx --collection orders
```

Error codes added to `api/errors/v1.json`: `ENOIDENTITY`, `EPOLICY`
(policy missing, unsigned, or not signed by the anchor) and `ESIGNATURE`
(a commit or checkpoint without a valid granted signer). `EACCES` keeps its
meaning, now for an action without a grant. `EKEY` keeps its meaning.

## 6. Threat model

| Attacker | Reads | Writes | Notes |
| --- | --- | --- | --- |
| A process of another OS user | Stopped (OS file mode, then encryption) | Stopped (OS) | As today, and stronger |
| A stolen disk, backup or snapshot, or a copied `.vdbx` | Stopped | Not applicable | Unless a granted identity's key is stolen too |
| An allowed application acting beyond its grant (a `read` app trying to write) | Not applicable | Detected and refused (`ESIGNATURE`) | Its raw byte writes can corrupt the file; readers fall back to the last valid commit |
| An allowed application reading another collection | Stopped | Stopped | It has no key for that collection |
| **Same-user malware, macOS**, key in a keychain access group or Secure Enclave, app signed with hardened runtime | **Stopped** | Detected | `securityd` releases the key only to the signed app, and hardened runtime blocks `task_for_pid` memory reads. Keys in a `file:` reference give no such protection |
| **Same-user malware, Linux desktop** | **Not stopped** | Not stopped | It can read a `file:` key, or `ptrace` the app when `kernel.yama.ptrace_scope` is 0. A Linux service with `creds:` under its own OS user is protected, with a dedicated user and an AppArmor or SELinux profile (option E) |
| **Same-user malware, Windows unpackaged app** | **Not stopped** | Not stopped | DPAPI and CNG keys are per user, and a same-user process can open the app's memory. A packaged (MSIX) app with an AppContainer ACL is protected |
| Root, Administrator, a kernel exploit | **Not stopped** | Not stopped | Root can read process memory and debug any key store's client |
| A stolen identity key | Stopped after revocation, for **new** data | Stopped after revocation | Old data stays readable to the thief until `fylo rekey --collection`. Hardware keys (Secure Enclave, TPM) cannot be copied, only used while on the host |
| Deleting the file, rolling it back to an older signed copy, filling the disk | **Not stopped** | **Not stopped** | Rollback is undetectable without state outside the file. The application can pin the last LSN it saw, as an option. OS permissions and backups remain the defence |
| Traffic analysis | Partly | Not applicable | Sizes, counts and timing remain visible |

In plain words: this design makes data unreadable and unforgeable for every
process that does not hold a granted key. Against same-user malware it is
only as strong as the place the key is kept: strong on macOS, iOS, Android
and packaged Windows apps, and weak on Linux desktops and unpackaged Windows
apps unless the application runs under its own OS user. It does nothing
against root.

## 7. Existing data, recovery and indexes

- **POSTIX descriptors do not map** onto per-application rules. The new
  `fylo access report <root>` lists the collections whose records carry
  descriptors, with each UID, GID and mode it found. Packing into a protected
  `.vdbx` drops the descriptors and needs `--drop-record-access`, as
  `--allow-engine-only-access` works today. `--keep-record-access-as-metadata`
  instead copies each descriptor into `user.fylo.meta.legacyAccess`, for an
  application that wants to keep enforcing it itself.
- **Migration path:** `fylo pack <root> --out app.vdbx --protect --admin <ref>
  --policy policy.json`, then `fylo verify`. ADR 0019's next release already
  breaks compatibility (its rule 3), so the old fields, `.as()`, `--uid`,
  `--groups` and the machine `access` field go in the same release.
- **Prerequisites in ADR 0016 version 1:** buckets (raw files), the queue
  (ADR 0009) and document version control are not in `.vdbx` version 1, and
  the `fylo` crate and `fylo link` do not open `.vdbx` yet (ADR 0019). A
  protected database therefore covers document collections only, until
  those land. The crate opening `.vdbx` is phase 2's first step.
- **Recovery:** replaying the commit tail, `fylo salvage` and index rebuilds
  need the collection key, so only an identity with `write` or `admin` can
  run them. A reader that finds a database needing recovery opens it
  read-only at the last signed checkpoint. `fylo verify` without keys still
  checks CRCs, as today. With an admin key it also checks every signature.
- **Rebuildable indexes stay rebuildable.** The index and location extents
  are sealed under the collection key and rebuilt from the records by any
  writer. Blind-index tokens are keyed per collection epoch, so a key
  rotation rebuilds them. This fits "documents are truth, indexes are
  accelerators" exactly.
- **Can indexes leak?** Inside a protected `.vdbx`, no: they are sealed. In a
  directory root, yes, and badly (section 2.5). This is the main reason
  protection is offered only for `.vdbx`. A directory root stays an
  unprotected, OS-permission-only layout for development and tooling.
- **The hot set** (ADR 0014) holds plaintext in the process's memory. That is
  inside the trust boundary, because the process holds the key anyway.
- **`$encrypted` fields** keep working, with their key derived from the
  collection key instead of `fylo.encryption.key`. They become optional
  defence in depth, and a later version could grant fields to a narrower set
  of identities.

## 8. Performance

These are estimates from published figures for the same algorithms, not
measurements. Phase 2 measures them on the latency benchmark.

| Cost | When | Estimate |
| --- | --- | --- |
| AES-256-GCM on each frame | Every read and write | Already paid by an encrypted `.vdbx` today. With ARMv8 or AES-NI instructions, GB/s per core: about a microsecond for a 1 KB document, against milliseconds for an `fsync` |
| HPKE unwrap of the catalog and collection keys | Once per open, per granted collection | About 0.1–0.3 ms each in software; 5–50 ms each with a Secure Enclave or TPM key. Collection keys can be unwrapped lazily on first use |
| Session certificate | Once per writer open | One identity-key signature (software below 0.1 ms; hardware 5–100 ms) |
| ECDSA P-256 signature on each commit | Every commit | About 30–60 µs in software, small next to the commit's `fsync` |
| Signature checks for readers | Once per new commit seen, plus the latest checkpoint at open | About 80–150 µs each in software. A reader caches "verified up to LSN", so steady-state reads pay nothing per record |
| Keyring and policy size | At open | A few kilobytes |

The hot path for a point read adds nothing beyond today's encrypted `.vdbx`.
Writes add one software signature per commit. A bulk import should batch many
records per commit, as it does already.

## 9. Phased plan

1. **Decisions and ADR.** The user answers section 10, then a new ADR
   ("ADR 0021: Application identities and signed policies") is drafted.
2. **Identities and key stores.** The `Identity` type, key references `file:`,
   `keychain:`, `cng:`, `creds:` and `webcrypto:`, `fylo identity create` and
   `show`. Blackbox tests for each store, and for refusing a key file
   readable by others.
3. **Protected `.vdbx` reads.** The crate opens `.vdbx` (the ADR 0019
   follow-up). Then the policy, keyring and catalog extents, HPKE key
   wrapping, the locked open, and the `read`, `write` and `admin` grants
   enforced by the keys. `fylo create --admin`, `fylo access grant`, `list`
   and `revoke`, with key epochs.
4. **Signed writes.** Session certificates, signed commits and checkpoints,
   reader verification with an LSN cache, `ESIGNATURE` and read-only
   fallback, compaction re-signing, `fylo verify` with signatures.
5. **Entry points.** `OpenOptions::identity` (Yon), `fylo link`'s
   configuration path, machine protocol v2, CLI flags, the browser's
   WebCrypto identity and Explorer's locked view.
6. **Migration and removal.** `fylo access report`, `fylo pack --protect`,
   the removal of POSTIX (descriptors, `apply_access`, `.as()`, `--uid`,
   `--groups`, the machine `access` field), and documentation.
7. **Revocation and audit.** `fylo rekey --collection`, the audit extent and
   `fylo access audit`.
8. **Hardening (optional).** Secure Enclave and TPM keys, sample AppArmor,
   SELinux and AppContainer profiles, and an optional LSN pin against
   rollback.

## 10. Questions for the user

- **Q1. Per application only?** This design answers "which application may
  read and write" and drops per-record, per-end-user POSTIX. Yon and other
  applications keep end-user rules themselves. Is that what you want, or must
  FYLO keep a per-record rule as well?
- **Q2. Protected by default?** Should a new `.vdbx` be protected (encrypted,
  with a policy) unless created with `--unprotected`? That reverses ADR 0016's
  "encryption is opt-in and off by default". Should directory roots stay
  available as an unprotected layout, or be limited to development?
- **Q3. Key storage per platform.** On Linux desktops and unpackaged Windows
  apps, same-user malware can take the key (section 6). Is that acceptable
  for version 1? Or must servers run each application under its own OS user
  (with `creds:` on Linux), or ship the option D helper later?
- **Q4. Write implies read.** Is that acceptable? A write-only identity cannot
  maintain indexes.
- **Q5. Admin implies read and write.** Or do you want separation of duties,
  where an admin signs a grant and the next writer to open the database
  completes it by wrapping the key?
- **Q6. Revocation is forward-only** until `fylo rekey --collection` rewrites
  old data. Should revocation always rewrite at once, slowly on large
  collections, or stay forward-only by default?
- **Q7. Trust anchor.** Must every application pin the database's admin
  anchor in its configuration, or is trust on first use with a warning
  acceptable?
- **Q8. Hardware keys** (Secure Enclave, TPM) in version 1, or later?
- **Q9. `fylo link`.** Is naming the identity through `application.json` or an
  environment variable acceptable for the Turso SDKs, since their open call
  carries only a path?
- **Q10. The browser.** Is "the origin is the boundary, plus an optional
  WebCrypto identity for files moved between origins" enough?
- **Q11. Scope before buckets and the queue.** Protection covers document
  collections until `.vdbx` holds buckets and the queue. Should those
  features wait, or ship unprotected in the meantime?

## 11. Contracts and records that change

- **Superseded:** ADR 0015 section 10 (POSTIX on segments) and the POSTIX
  parts of ADR 0016's costs.
- **Amended:** ADR 0016 section 9 (encryption: per-identity key wrapping,
  policy and keyring extents, signed commits and checkpoints, possibly on by
  default); ADR 0019 (`OpenOptions::identity`, and `fylo link`'s
  configuration); ADR 0013 (signed commit records).
- **New:** ADR 0021, the identities and policy; `api/access/v1`
  (`policy.schema.json`, identity and key-reference formats).
- **Changed:** `api/machine` v2 (no per-request `access`; `--identity` and the
  handshake grants); `api/errors/v1.json` (`ENOIDENTITY`, `EPOLICY`,
  `ESIGNATURE`); `api/storage/v1` (the `.vdbx` extents);
  `docs/configuration.md` (`fylo.identity`, `fylo.trustAnchor`, and
  `fylo.encryption.key` retired for protected databases); README "POSTIX
  access control" and `clients/README.md` "POSIX access context", both
  replaced; `CONTEXT.md` (identity, policy, grant, trust anchor, key epoch).
- **Code removed:** `AccessDescriptor`, `ACCESS_XATTR`, `apply_access`,
  `AccessContext`, `WriteActor`, `WriteAccess`, the CLI's `access_context`
  and the machine protocol's `actor()` and `access()`.
- **Downstream:** TACHYON ADR 0032 (Yon opens its datasource with an identity
  and pins the anchor).
