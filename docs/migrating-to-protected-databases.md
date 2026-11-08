# Migrating to protected databases

This release removes POSTIX, FYLO's per-record access control (a `uid`,
`gid` and `mode` on each record, checked against a `uid` and `groups` the
caller claimed). Access is now granted per **application identity** on a
**protected `.vdbx` database**, and FYLO denies by default
([ADR 0021](adr/0021-application-identities-and-signed-policies.md)).

This guide moves an existing directory root, or a plain or shared-key
`.vdbx` database, into a protected database.

## What changed

| Before (POSTIX)                                       | Now (application identities)                                                    |
| ----------------------------------------------------- | ------------------------------------------------------------------------------- |
| A caller claimed a `uid` and `groups` on each request | A process opens the database as one identity, a P-256 key it holds              |
| `.as({ uid, gid, mode })` in clients and shims        | Removed; configure `fylo.identity` (or `FYLO_IDENTITY`) once                    |
| `--uid` and `--groups` on the CLI                     | Removed; use `--identity <key reference>` and `--trust-anchor <fid1:...>`       |
| The machine protocol's per-request `access` object    | Removed; a request that carries it fails with `EBADREQUEST`                     |
| `capabilities.machineAccess` in the handshake         | Removed; the handshake reports `identity`, with the database's grants           |
| A record with no descriptor was open to every caller  | Nothing opens a protected database without an identity (`ENOIDENTITY`)          |
| Rules checked by FYLO's code, on macOS and Linux only | Rules backed by encryption and signatures, on every platform and in the browser |
| One shared `fylo.encryption.key` opened everything    | Each collection has its own key, wrapped only for the identities granted it     |
| `--allow-engine-only-access` for `layout` and `pack`  | Removed; nothing is enforced per record any more                                |

Rules about end users (which person may read which record) stay in your
application. FYLO answers which application may read and write which
collection.

## Old data keeps opening

A root or database whose records carry POSTIX descriptors still opens and
reads in full: the descriptors are ignored. The first read of such a record
in a process prints one warning to stderr that points here. Nothing is
lost: the descriptor stays on the record, unenforced, until you migrate.

## Step 1: see what you have

```bash
fylo access report ./data
```

The report reads every record and writes nothing. For each collection it
lists the layout, live and deleted documents, the descriptors its records
carry (grouped by `uid`, `gid` and `mode`) and its `$encrypted` fields. For
the root it lists its encryption (`none`, `shared-key` or `protected`),
what `fylo pack` cannot carry yet (`blockers`), what the move changes, and
the command to run. A shared-key database is reported locked unless you
pass its key through `fylo.encryption.key`.

Resolve every blocker first. A `.vdbx` database holds document collections
only, so a root that uses buckets, the embedded queue or document version
control cannot be packed yet (ADR 0016 section 1).

## Step 2: make the identities

Each application gets its own key. Keep keys out of the database's folder.

```bash
fylo identity create --store file:/etc/myapp/admin.pem
fylo identity create --store keychain:myapp-web
fylo identity show keychain:myapp-web --json
```

`show --json` prints the identity's `id` (`fid1:...`), which other
applications pin as the trust anchor, and its `publicKey` (`fpk1:...`),
which an admin grants. Key stores: `file:` (a key file only its owner may
read), `keychain:` (macOS), `cng:` (Windows), `creds:` (Linux, under
systemd) and `webcrypto:` (the browser). On Linux desktops and unpackaged
Windows apps another process of the same OS user can read a software key:
on a server, run each application under its own OS user.

## Step 3: list the grants

Write the grants the new database starts with to a file:

```json
{
    "grants": [
        {
            "grantee": "web",
            "key": "fpk1:p256:...",
            "actions": ["read"],
            "collection": "users"
        },
        {
            "grantee": "worker",
            "key": "fpk1:p256:...",
            "actions": ["write"],
            "allCollections": true
        }
    ]
}
```

Each grant names exactly one of `"collection": "<name>"`,
`"allCollections": true` and `"database": true`. `actions` are `read`,
`write` (which includes read) and `admin` (which includes both). `key` is
one key or a list; `rule` names the rule (default `<grantee>-<resource>`)
and `label` adds a note. You can also grant later with `fylo access grant`.

## Step 4: pack

```bash
fylo pack ./data --out app.vdbx --protect --admin file:/etc/myapp/admin.pem \
    --grants grants.json --drop-record-access
```

- `--admin` names the database's admin and trust anchor.
- If any record carries a POSTIX descriptor, the pack refuses with
  `EPOLICY` until you choose:
    - `--drop-record-access` drops the descriptors;
    - `--keep-record-access-as-metadata` keeps each one, unchanged and
      unenforced, as the metadata field `legacyAccess`
      (`{"version":1,"uid":...,"gid":...,"mode":...}`), for an application
      that enforces it itself. It refuses a record whose metadata already
      has a `legacyAccess` field.
- A shared-key `.vdbx` source is packed the same way, opened with its key
  (`fylo.encryption.key` in the configuration).
- A protected source is refused; rewriting one (`fylo rekey`) comes later
  (ADR 0021, phase 7).

What the pack carries over: every collection with its documents, deleted
documents, schema versions, validation mode and `$encrypted` field list.
Each collection's indexes are folded again from its records inside the new
file, sealed under the collection's key. Record file modes are not carried
over. `$encrypted` fields stay sealed with `fylo.encryption.key`, which
their readers still need.

The pack is crash-safe. It builds a staging file beside `--out`, checks it
against the source record by record (same documents, bytes, attributes and
modification times, after the descriptor change you chose) and as
`fylo verify` checks it with the admin's identity (structures and every
record's signature), flushes it, and only then gives it its name. The
source is never written, and every collection stays locked from the first
read to the rename. A pack that stops leaves no `--out`; the next pack to
the same name removes the staging file.

## Step 5: check it

```bash
fylo verify app.vdbx --identity file:/etc/myapp/admin.pem --trust-anchor fid1:...
fylo access report app.vdbx --identity file:/etc/myapp/admin.pem --trust-anchor fid1:...
fylo access list app.vdbx --identity keychain:myapp-web --trust-anchor fid1:...
```

`verify` with an identity checks every signature. The report shows the
same document counts per collection as the source, `encryption:
"protected"` and no records with access. `access list` run as each
application shows its grants.

## Step 6: point the applications at it

Each application opens the database as its own identity and pins the
admin's `fid1:` identifier:

```json
{
    "fylo": {
        "root": "./app.vdbx",
        "identity": "keychain:myapp-web",
        "trustAnchor": "fid1:..."
    }
}
```

or `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR` in the environment. The same
settings serve the CLI (`--identity`, `--trust-anchor`), the machine
protocol (`fylo exec --loop`, whose handshake reports the identity and its
grants), the `fylo` crate (`OpenOptions::with_identity`) and `fylo link`
(the `application.json` beside the database). In the browser, open the
database with `identity: { name, workerUrl, trustAnchor }`.

Remove `.as(...)` calls, `--uid` and `--groups`, and the machine
`access` object from your code: they now fail rather than being ignored.
Keep the old root until you have checked the new database; then delete it
yourself.
