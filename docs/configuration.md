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
needed. The resolved key must contain at least 32 UTF-8 bytes and the salt must
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

| Old environment setting | New JSON location | JSON type |
| --- | --- | --- |
| `FYLO_ROOT` | `fylo.root` | string path |
| `FYLO_SCHEMA` | `fylo.schema` | string path |
| `FYLO_STRICT` | `fylo.strict` | boolean |
| `FYLO_SHARD_WIDTH` | `fylo.shard.width` | integer, 1-4 |
| `FYLO_CHEX_BINARY` | removed; CHEX is built in | — |
| `FYLO_ENCRYPTION_KEY` | `fylo.encryption.key` | string secret reference |
| `FYLO_CIPHER_SALT` | `fylo.cipher.salt` | string secret reference |

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
