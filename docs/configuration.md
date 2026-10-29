# Native configuration

FYLO reads native runtime settings from `application.json`, under the `fylo`
key. Environment variables no longer configure the engine implicitly. Keep
non-secret settings in JSON and secrets in a secret manager or a private `.env`
file beside that JSON file.

```json
{
    "fylo": {
        "root": "./data",
        "schema": "./schemas",
        "strict": true,
        "shard": { "width": 1 },
        "encryption": { "key": "${FYLO_ENCRYPTION_KEY}" },
        "cipher": { "salt": "${FYLO_CIPHER_SALT}" }
    }
}
```

The encryption key and cipher salt must be configured together, and must be
secret references, not literal credentials. Omit both when encryption is not
needed. An encrypted `.vdbx` database needs only the key (see
[encrypted databases](#encrypted-databases)). The resolved key must contain at least 32 UTF-8 bytes and the salt must
be nonempty. Start with [application.example.json](../application.example.json)
for a configuration that does not require any secrets or schemas.

## Durability

Every write is in its collection's write-ahead log before it returns
([ADR 0013](adr/0013-collection-write-ahead-log.md)). `fylo.durability` chooses
how hard that log is flushed:

- `"process"` (the default) flushes with `fsync`. Every acknowledged write
  survives process death on every platform, and power loss on Linux and
  Windows. On macOS, power loss can lose writes made after the last
  checkpoint; everything up to the checkpoint survives.
- `"power-loss"` also flushes the drive's cache (`F_FULLFSYNC` on macOS) on
  every group commit, so every acknowledged write survives power loss there
  too. Each commit then costs a full drive flush, several milliseconds on a
  Mac.

```json
{ "fylo": { "durability": "power-loss" } }
```

Before downgrading FYLO, stop every writer or run `fylo recover` on each
collection: a release older than the write-ahead log refuses a collection
whose log has not been checkpointed, rather than ignore its writes.

## Collection layout

`fylo.layout` chooses how document collections created from now on store
their documents ([ADR 0015](adr/0015-append-only-document-segments.md)):

- `"files"` (the default) keeps one JSON file per document under `docs/`,
  with its metadata in extended attributes.
- `"segments"` keeps documents in a few append-only segment files under
  `segments/`. A read is one positioned read and a broad query a few large
  reads, which matters most when the root sits on a network share. A write
  appends to a file that already exists.

```json
{ "fylo": { "layout": "segments" } }
```

The setting never changes an existing collection. `createCollection` also
takes `"layout"` for one collection, and `fylo layout <collection> --to
segments|files` moves an existing one, after checking the copy against every
document. Raw-file collections, the EFS profile and roots with version control
keep the file layout. A segment collection enforces `.as()` access in FYLO
but gives records no operating-system owner of their own, so a collection
whose records name other owners needs `--allow-engine-only-access` to move.

`fylo compact <collection>` reclaims the space old versions take,
`fylo export <collection> --out <directory>` writes the one-file-per-document
tree without changing the collection, and `fylo verify-segments` and
`fylo salvage` check and rescue a damaged one. Before downgrading to a release
older than segments, move each segment collection back with
`fylo layout <collection> --to files`; older releases refuse it.

`fylo pack <root> --out app.vdbx` copies a whole directory root, in either
layout, into a new single-file `.vdbx` database, after checking the copy
against every document; `fylo unpack app.vdbx --out <directory>` writes a
database back out as a directory root in the file layout, each collection
as `fylo export` writes it. Pack refuses a root with buckets, version
control or the embedded queue, which a database does not hold yet, and
never replaces an existing file (ADR 0016).

## Encrypted databases

A `.vdbx` database is plain unless you ask for encryption: it is opt-in per
database and off by default. `fylo.encryption.file` asks for it:

```json
{
    "fylo": {
        "root": "./app.vdbx",
        "encryption": { "key": "${FYLO_ENCRYPTION_KEY}", "file": true }
    }
}
```

With `file` set, a database this configuration creates (by a session that
finds none at `fylo.root`, by `fylo create`, or by `fylo pack`) seals its
records, dictionaries, schema snapshots and directory with a key derived
from `fylo.encryption.key` (ADR 0016 section 9). `fylo create app.vdbx
--encrypt` and `fylo pack <root> --out app.vdbx --encrypt` ask for it for
one file and also need the key. `file` needs the key, but the key alone with
`file` set needs no cipher salt: the salt pairs with the key only for
`$encrypted` fields. Without `file`, the key and salt configure field
encryption as above, and the key also opens an encrypted database.

An encrypted database opened without its key is locked: it lists no
collections, and a read of a collection and every write fail with `EKEY`.
A different key fails with `EKEY` too. `fylo verify app.vdbx` checks the
file without the key, and `fylo inspect app.vdbx` reports `encrypted` and
`locked`. Losing the key loses the documents: there is no recovery.

`fylo rekey app.vdbx --new-key '${FYLO_NEW_ENCRYPTION_KEY}'` rewrites an
encrypted database under another key. `--new-key` names the new secret the
way `fylo.encryption.key` names the current one, from the environment or a
`.env` file beside the configuration, and a literal key is refused; the
database opens with `fylo.encryption.key` first, so a wrong one changes
nothing. The new file replaces the old in one rename, so a stop leaves one
or the other whole. Then set `fylo.encryption.key` to the new secret.
`fylo rekey app.vdbx --new-key '${NAME}' --encrypt` encrypts a plain
database and `fylo rekey app.vdbx --decrypt` writes an encrypted one plain.

## Discovery and precedence

The CLI looks for `application.json` in its current working directory. Use
`fylo --config /srv/app/application.json ...` to select a different file. Native
SDKs expose the same selection through an optional `config` path; they do not
load JSON or secret files themselves.

1. Explicit constructor options passed as CLI flags, and explicit CLI flags.
2. Settings in the selected `application.json`.
3. Engine defaults.

A missing automatically discovered file uses defaults. An explicitly selected
missing file, malformed JSON, invalid setting, or unresolved secret reference
fails initialization. Paths inside the configuration, including `root` and
`schema`, resolve relative to the configuration directory.
An explicit relative `--config` path resolves from the process working directory.
An explicit constructor root or `--root` remains an override, not a path inside
the JSON file.

## Secret references

```dotenv
# .env beside application.json; real values must not be committed.
FYLO_ENCRYPTION_KEY=replace-with-a-secret-of-at-least-32-characters
FYLO_CIPHER_SALT=replace-with-a-secret-salt
```

Use an exact, whole-value `${NAME}` string to reference a secret. FYLO resolves
only explicitly referenced names, first from the process's inherited environment,
then from the `.env` beside the selected configuration. The variable name does
not need a `FYLO_` prefix. Unreferenced variables never become engine settings.
For example, setting `FYLO_ROOT` in a shell has no effect on `fylo.root`.

References are not shell expressions: no commands, recursive expansion, or
embedded interpolation is performed. A missing reference fails with a sanitized
error; secret values are not included in diagnostics. Resolution does not assign
values to the host or child process environment. In particular, `.env` cannot
replace `PATH` or inject dynamic-loader variables into the child process.

Use `.env` only for secrets, not root paths, booleans, schema settings, or shard
width. Keep it out of source control and restrict access (for example,
`chmod 600 .env`). Prefer injected secrets from your deployment's secret manager
in production. Configuration paths and executable paths are trusted bootstrap
input: never accept them from requests, uploads, or tenant-controlled data.

## Migration from environment configuration

Remove the `FYLO_` prefix, lowercase each word, and turn each underscore into a
nested key:

| Old environment setting | New JSON location         | JSON type               |
| ----------------------- | ------------------------- | ----------------------- |
| `FYLO_ROOT`             | `fylo.root`               | string path             |
| `FYLO_SCHEMA`           | `fylo.schema`             | string path             |
| `FYLO_STRICT`           | `fylo.strict`             | boolean                 |
| `FYLO_SHARD_WIDTH`      | `fylo.shard.width`        | integer, 1-4            |
| `FYLO_CHEX_BINARY`      | removed; CHEX is built in | —                       |
| `FYLO_ENCRYPTION_KEY`   | `fylo.encryption.key`     | string secret reference |
| `FYLO_CIPHER_SALT`      | `fylo.cipher.salt`        | string secret reference |

`fylo.schema` may also be an object, `{ "path": "./schemas", "autoPush": true }`.
`path` is the folder, and `autoPush` (default `false`) pushes the folder's new
versions into the root's schema registry whenever a write opens the root, as
`fylo schema push` would (ADR 0017 section 3).

CHEX schema validation now runs inside FYLO, so `fylo.chex.binary` no longer
exists. A configuration that still sets `fylo.chex` fails with a message asking
you to remove it.

Replace native SDK `env` options (file paths or environment maps) with `config`
paths. Separate components such as an application and its embedded dependency
can select separate JSON files and adjacent secret files without changing global
environment state. See [native SDK examples](../clients/README.md).

Installer, build, and release-tool variables such as `FYLO_VERIFY_PROVENANCE`
are not engine configuration and are unchanged by this migration.

## Browser and mobile clients

Browser and mobile clients do not spawn the native CLI or implicitly read local
`application.json` or `.env` files. Continue configuring their storage adapters
through explicit host options. Never put secrets in a publicly served JSON file
or bundle a native secret file into Explorer or your website.
