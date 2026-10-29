# ADR 0017: Schemas in the database, validated at the commit

- Status: **Accepted** (Samuel, 2026-10-29)
- Date: **2026-10-29**
- Owners: **FYLO maintainers**
- Builds on: ADR 0013 (record format and commit protocol), ADR 0015
  (append-only segments), ADR 0016 (single-file `.fxdb` databases), ADR 0008
  (the portable WebAssembly engine) and ADR 0014 (the hot set).
- Amends: the "Schema Versioning" section of `README.md`, the schema row of
  `docs/releases/artifact-matrix.md`, and `docs/cohesion/schema-rules.md`.
- Plan and options considered: "Schema validation and migration rework"
  (https://claude.ai/artifact/NPvZYPGVYSca65GEYP9vhQ).

## Context

FYLO validates documents with CHEX, a JSON schema whose every leaf is a regex.
CHEX now runs in-process (`crates/fylo-chex`), and a collection can keep a
history of schema versions with a JavaScript upgrader between each pair,
executed by the embedded Boa runtime. Reading the code on `develop` on
2026-10-29 turned up six problems.

1. **Some writes skip validation.** `putData`, `batchPutData` and
   `importBulkData` reach `WriteEngine::encode`, which validates in strict
   mode. `patchDoc`, `patchDocs` and SQL `INSERT` and `UPDATE` call the storage
   writer directly (`fylo-machine/src/lib.rs`, `patch_document` and
   `execute_sql`) and never see the schema. A test against `f660b3e` in strict
   mode confirmed it: `putData` refused an invalid document, while `patchDoc`,
   `patchDocs`, SQL `INSERT` and SQL `UPDATE` all stored invalid values and
   fields the schema does not have. Only `putData` stamps `_v`; a SQL `INSERT`
   stores none. **The same paths skip field encryption.** With
   `$encrypted: ["ssn"]` and keys configured, `patchDoc`, SQL `UPDATE` and SQL
   `INSERT` wrote `ssn` to disk in plaintext, after which `getDoc` and every
   `findDocs` over the collection failed with `EENGINE_ENCRYPTION`.
   `WriteEngine::patch_document` validates and encrypts correctly, but the
   machine never calls it.
2. **Reads never migrate.** The README says reads materialize old documents
   to the head shape and that a document without `_v` is upgraded on read. The
   Rust engine's read path (`ReadOnlyEngine::decode_document`) does neither.
   Only the explicit `schemaMaterialize` operation runs the upgrader chain, so
   an application sees whatever version each document was written at.
3. **The schema lives outside the database.** Schemas are files under
   `fylo.schema`, beside the root rather than in it. Copying a root, packing a
   `.fxdb` file or opening one in Explorer leaves the schema behind, a schema
   edited on disk applies to the next write with no record of the change, and
   nothing ties a schema change to the documents written under it.
4. **The browser cannot validate.** Neither WebAssembly build links CHEX,
   because the `regex` crate's Unicode tables add about 390 KB to a module
   whose budget is 1,792 KiB raw and 640 KiB gzipped. Explorer will edit `.fxdb`
   files in the browser (ADR 0016 section 10), so every browser edit would skip
   validation. Explorer's folder view runs the JavaScript engine, which has no
   validator at all.
5. **Validation is slow.** Every validated write re-reads the manifest and the
   schema file and recompiles each leaf's regex, once per property per
   document. With a 10-field schema, validation alone took 0.29 ms per
   document in a release build and added roughly 0.3 to 0.8 ms to a put that
   otherwise took about 1.2 ms. That is a large share of a write now that the
   write-ahead log has brought a commit down to milliseconds. Each one-shot
   invocation with encryption keys also derives them again (PBKDF2, 100,000
   iterations, about 28 ms in release).
6. **Upgraders cannot run everywhere.** An upgrader is arbitrary JavaScript.
   Boa is native-only, a fresh runtime is built for every step of every
   document, and a `.fxdb` file that carried JavaScript into Explorer would run
   code from a file the user merely opened.

The same test found three more defects:

7. **Strict mode and `$encrypted` cannot be combined.** CHEX treats the
   `$encrypted` key as an ordinary required property, so with both on every
   `putData` fails with "Property '$encrypted' cannot be null or undefined".
   Encryption reads `$encrypted` from the same head schema file. No test covers
   the combination; every encryption test runs with strict mode off.
8. **Encrypted strings change type.** A string field is encrypted as its raw
   text and decrypted by parsing that text as JSON, so `"999"` comes back as
   the number `999`, and `"true"` and `"null"` would too
   (`encryption.rs`, `stringify_stored_value` and `decode_stored_value`).
9. **Arrays of objects pass the schema and fail the store.** CHEX validates a
   schema like `{"items": [{"sku": "^[a-z]+$"}]}`, but the document format
   refuses every such document with `EARRAYOFOBJECTS`. The README describes a
   different error, on read, that no code produces.

There is also a semantic limit worth stating plainly, because it decides what
"validated" means. CHEX tests the `String()` form of a scalar, so the number
`30` and the string `"30"` both pass `^[0-9]+$`. It distinguishes arrays and
objects from scalars, rejects unknown properties and missing required ones,
and validates nested objects, arrays and key-pattern records. It does not tell
scalar types apart. This ADR keeps CHEX's semantics unchanged and leaves typed
leaves to a separate ADR (see "Not decided here").

## Decision

A database owns its schemas. Each schema version is committed into the
database through the collection's normal commit path, every mutation is
checked at one point inside the storage writer, the same compiled validator
runs natively and in the browser, and migration is declarative so that every
build can read old documents.

### 1. One enforcement point

Validation moves from `WriteEngine::encode` into the storage writer's record
builder, the step every mutation already passes through (ADR 0013 section 4,
step 2: "checks the identifier, access and schema rules"). A `DocumentPolicy`
for the collection runs on each post-image, in this order:

1. migrate the pre-image to the head version, if it is older (section 5);
2. apply the mutation (put, shallow patch, SQL `SET`);
3. validate the post-image against the head version;
4. stamp `_v` with the head label;
5. encrypt `$encrypted` fields.

`putData`, `batchPutData`, `importBulkData`, `patchDoc`, `patchDocs`, SQL
`INSERT` and `UPDATE` all reach it, because none of
them can build a record any other way. `restoreDoc`, `restoreCommit`,
`checkout` and `merge` reinstate stored bodies as they were, with their
original `_v`, and are not revalidated: they bring back history, and the read
path migrates them like any other old document.

A patch is validated as the whole document it produces, not as the fragment
sent.

### 2. Validation modes

Each collection's catalog descriptor gains a `validation` field, with
MongoDB's split between what is checked and what happens on failure:

| Mode     | Checks                     | On failure                                            |
| -------- | -------------------------- | ----------------------------------------------------- |
| `off`    | nothing                    | n/a                                                   |
| `warn`   | every write                | the write commits; the violation is counted and logged |
| `strict` | every write                | the write fails with `ESCHEMA` and nothing is written |

- A new `.fxdb` database, and any collection created with a schema, defaults
  to `strict`.
- An existing directory root keeps today's behavior: `fylo.strict: true`
  means `strict`, otherwise `off`. `fylo schema mode <collection> <mode>`
  changes it.
- `warn` exists for adopting a schema over data that does not yet match it.
  `fylo schema check <collection>` lists the documents that fail.

### 3. The schema registry is part of the database

A collection's **schema registry** is its ordered list of schema versions.
Version records are append-only and never edited; a rollback is a new
version.

Each version holds:

| Field        | Meaning                                                                 |
| ------------ | ----------------------------------------------------------------------- |
| `ordinal`    | 1, 2, 3, … in commit order                                              |
| `label`      | the `_v` value, e.g. `v2`; unique within the collection                 |
| `chex`       | the CHEX schema, canonical JSON, including `$encrypted`                 |
| `sha256`     | of `chex`                                                               |
| `compiled`   | the compiled validator (section 4); derived, rebuildable from `chex`    |
| `migration`  | the steps from the previous version (section 5); absent for ordinal 1   |
| `committed`  | commit time and actor                                                   |

**Where it is stored.**

- **`.fxdb` files and segment collections**: each version is a record of a new
  kind, `5` schema, with manifest `fylo.schema-version.v1`, appended through
  the collection's commit path (ADR 0013 section 4, ADR 0015 section 6). A
  schema change is therefore atomic, ordered against the documents around it,
  and replayed by recovery. For a fast open, a checkpoint also writes a
  derived **schemas snapshot**: extent kind `7` in `.fxdb`, named by a third
  snapshot reference in the checkpoint slot, holding the registry, the
  compiled validators and the per-version document counts (section 6). The
  head is the newest schema record at or before the checkpoint LSN, plus
  replay. A missing or mismatched snapshot is rebuilt from the records.
- **Directory roots on the file layout**: the registry is
  `<root>/.fylo-schemas/<collection>/<ordinal>.json`, one immutable file per
  version, and the head ordinal is in the catalog descriptor. Adding a version
  writes its file, then changes the descriptor through the transaction
  journal, as `reshard` does.

**Fencing.** A collection with an in-database registry sets
`"schemas": "registry"` in its catalog descriptor. Today's descriptor parser
ignores unknown fields, so that alone would not stop an older binary.
Correction (2026-10-29): the collection's generation state is also written as
a new `fylo.collection-generation` format version, which older binaries
already refuse with `ENATIVE_CORRUPT_METADATA` (the second fence ADR 0015
uses), so an older binary refuses the collection instead of writing
unvalidated documents or stopping its log scan at an unknown record kind. A `.fxdb` file sets read-only-compatible feature bit
`READ_ONLY_COMPAT_SCHEMAS` (bit 0), so a build that predates this ADR opens it
read-only.

**`fylo.schema` becomes the authoring folder.** The per-collection folder
layout (`manifest.json`, `history/`, `upgraders/`) stays the way schemas are
written and kept in source control. The database no longer reads it on every
write:

- `fylo schema push <collection> [--schema-dir DIR]` compares the folder with
  the registry and commits each version the database does not have yet. It
  refuses a version whose label exists with a different `sha256`, because
  committed versions are immutable.
- `fylo schema pull <collection> --out DIR` writes the registry back out as
  the folder layout.
- `fylo.schema.autoPush: true` pushes at open, which keeps today's "edit the
  file, the next write uses it" workflow for development. It is off by
  default.
- The first time a binary with this ADR opens a root whose `fylo.schema`
  folder has versions the root does not, it pushes them once and records that
  it did, so upgrading does not silently drop validation.

### 4. One compiled validator, native and browser

CHEX source is compiled once per version, when it is pushed, into a
`CompiledSchema`: the schema tree with each leaf's pattern compiled, the
nullable and record markers resolved, and the `$encrypted` paths listed. The
engine keeps compiled schemas in memory keyed by `(collection, ordinal)`, and
the hot set (ADR 0014) keeps them in each collection's resident header. Error
names and messages stay byte-for-byte those of `fylo-chex`.

Two rules are fixed at compile time:

- **`$`-prefixed top-level keys are directives**, not properties. `$encrypted`
  is removed from the tree that validates documents and kept as the version's
  encryption list. Any other `$` key is refused at push, which reserves the
  prefix for later directives.
- **A schema cannot promise what the store refuses.** An array whose item
  schema is an object is refused at push with `EARRAYOFOBJECTS`, the code the
  document format already uses, instead of loading and then failing on every
  write.

**Each leaf compiles to a DFA.** At push, the native build compiles every
leaf pattern with `regex-automata` into a sparse, little-endian DFA, and the
version record stores the serialized DFAs as its `compiled` field. Every
build then validates by searching those DFAs: the WebAssembly builds link only
`regex-automata`'s `dfa-search` feature, which deserializes and runs a DFA but
has no parser or compiler. Because the browser runs the exact automaton the
native build produced, the two cannot disagree on a match. Natively the
engine recompiles the DFAs from `chex` at open and caches them rather than
trusting the stored bytes; the browser checks the stored DFAs with
`regex-automata`'s validating `from_bytes`, which never trusts its input for
memory safety.

Measured on `develop` (`f660b3e`, 2026-10-29) with the release build of
`fylo-browser.wasm` that `scripts/build-browser.mjs` produces:

| Browser engine variant                         |      Raw |    Gzip | Change, gzip | Fits budget |
| ---------------------------------------------- | -------: | ------: | -----------: | ----------- |
| `develop`, no validator                        | 1,610,766 | 535,338 |            — | yes         |
| CHEX with today's `regex` features             | 1,978,775 | 674,550 |    +139,212 | no          |
| CHEX with `regex`, no Unicode tables           | 1,909,632 | 639,655 |    +104,317 | no (raw)    |
| CHEX ported to `regex-lite`                    | 1,686,384 | 564,142 |     +28,804 | yes         |
| `regex-automata` DFA search only (this ADR)    | 1,634,891 | 544,275 |      +8,937 | yes         |

Most of `regex`'s cost is the engine, not the Unicode tables (about 69 KB of
the 368 KB). `regex-lite` fits, but its `\w`, `\d`, `\s`, `\b` and `(?i)` are
ASCII-only, so `^\w+$` rejects `héllo` in the browser and accepts it natively.
The DFA runtime costs a third as much and gives identical answers.

Serialized DFA sizes, full Unicode: `^[0-9]+$` 778 bytes, `^.+$` 991,
`^[a-z0-9-]+$` 802, an e-mail pattern 2,347, `^\w+$` 11,665, an ISO
timestamp written with `\d` 12,757 (1,099 with `[0-9]`). A schema of a few
dozen leaves stores tens of kilobytes per version.

**Limits of the DFA form.** A Unicode word boundary (`\b` without `(?-u)`)
cannot be compiled to a DFA. A version that uses one is marked
`portable: false`: native builds validate it with `regex` as today, and the
WebAssembly builds refuse writes to that collection (below). `fylo schema
push` warns, and for a `.fxdb` database refuses such a version unless given
`--native-only`. Today's CHEX already rejects `\p{…}` classes, so nothing else
changes. The browser cannot compile a new pattern, so schemas are authored and
pushed natively; Explorer shows them but does not edit them.

Native and browser builds must agree on every document. The CHEX golden
corpus (`crates/fylo-chex/tests/golden.rs`) and a new differential corpus run
against both builds in CI, and any disagreement fails the build.

**Fail closed.** A WebAssembly build that cannot validate a version (for
example, a future pattern feature it lacks) refuses writes to that collection
with `ESCHEMAUNAVAILABLE`. It never writes an unvalidated document into a
collection whose mode is `strict`.

### 5. Migrations

A version's `migration` is a list of **declarative steps**, which the Rust
engine runs in every build, including WebAssembly, with no script runtime.
Paths are slash-separated, as in `$encrypted`.

| Step                                   | Effect                                                      |
| -------------------------------------- | ----------------------------------------------------------- |
| `{"rename": "a/b", "to": "a/c"}`       | move a field; missing source is a no-op                     |
| `{"default": "x", "value": …}`         | set a field only where it is missing or null                |
| `{"set": "x", "value": …}`             | set a field unconditionally                                 |
| `{"remove": "x"}`                      | delete a field                                              |
| `{"copy": "x", "to": "y"}`             | copy a field                                                |
| `{"convert": "x", "to": "number"}`     | convert between string, number, boolean; fails on bad input |
| `{"map": "x", "values": {…}, "else": …}` | replace a value through a lookup table                    |
| `{"wrap": "x"}`                        | turn a scalar into a one-element array                      |
| `{"js": "upgraders/v1-to-v2.js"}`      | run a JavaScript upgrader, native only (below)              |

Steps run in order and must leave a document that the target version
accepts. `fylo schema push` dry-runs a new migration over a sample of the
collection (all of it below 10,000 documents) and refuses the push if any
migrated document fails validation, reporting the first failures.

**JavaScript steps** stay for transformations the table cannot express, such
as the README's slug example. They run only natively, in one reused Boa
context per migration batch rather than a fresh one per document, with the
existing loop limit and module root. A version whose migration contains a
`js` step is **eager-only**: it is not readable in the old shape by a build
without Boa until the collection has been migrated (below). The WebAssembly
builds never execute JavaScript from a database.

**When documents move to the new shape.** Both, by design:

- **Lazy, on read.** Every read returns the head shape. A document whose `_v`
  is older is migrated in memory by the declarative steps; the stored bytes do
  not change. A document with no `_v` is at ordinal 1, as the README already
  promises. If a step needed by the read is `js` and the build has no Boa, the
  read fails with `ESCHEMASTALE` naming the collection and version.
- **On write.** Any write to an old document stores it at the head (section 1,
  step 1), so a collection that is written converges on its own.
- **Eager, in the background.** `fylo schema migrate <collection>` (and, in
  an embedded process that holds the root's exclusive lease, an optional
  idle-time task) rewrites old documents in batches of 256 as
  ordinary commits with new LSNs, under the collection lock for one batch at a
  time. Compaction (ADR 0015 section 7) then drops the old entries. ADR 0015
  compaction keeps an entry's LSN and bytes, so migration is never folded
  into compaction itself.

`fylo pack` migrates eagerly by default, so a shipped `.fxdb` file has every
document at its head and needs no upgrader to read; `--keep-versions` skips
that.

### 6. Convergence tracking

Each collection keeps a count of live documents per schema ordinal. The
writer maintains it from the pre-image and post-image `_v` of each commit, a
checkpoint stores it in the schemas snapshot (or, for the file layout, beside
the registry), and a rebuild recounts it by scan. When every count below the
head is zero, the collection is **converged**: reads stop consulting
migrations, and `fylo schema inspect` reports it.

`fylo schema inspect` and `schemaInspect` report, per collection: the mode,
the head, every version with its document count, whether each migration is
declarative or eager-only, and whether the collection has converged.

### 7. Operations and error codes

New CLI subcommands: `fylo schema push`, `pull`, `mode`, `check`, `migrate`.
The existing `inspect`, `doctor`, `validate` and `materialize` keep their
names, and read from the registry.

New machine error codes, added to the protocol contract:

| Code                  | Meaning                                                              |
| --------------------- | -------------------------------------------------------------------- |
| `ESCHEMA`             | the document does not match the head version (already exists)       |
| `ESCHEMAUNAVAILABLE`  | this build cannot validate the collection's head version            |
| `ESCHEMASTALE`        | the document needs an eager-only migration this build cannot run    |
| `ESCHEMACONFLICT`     | a pushed label exists with different content                        |

The handshake's `schemaValidation` capability reports `true` for every build
that links the validator.

### 8. Rollout

Each step lands on `develop` on its own and is useful by itself.

1. **Close the gaps.** Route `patchDoc`, `patchDocs` and SQL mutations through
   the engine's validation and encryption, first, because today they write
   `$encrypted` fields in plaintext; make CHEX skip `$encrypted` so strict mode
   and encryption work together; keep an encrypted string a string; cache
   compiled schemas per `(collection, sha256)` and derived keys per process;
   correct the README. No format change except a tagged plaintext encoding
   for newly encrypted values, with existing `v2` values still read.
2. **One enforcement point.** Move `DocumentPolicy` into the storage writer's
   record builder, with the validation modes.
3. **Compiled validator in WebAssembly**, within the budget, with the
   differential corpus.
4. **Registry in the database**: schema records, the schemas snapshot, the
   fences, `push` and `pull`, and the one-time import from `fylo.schema`.
   Needs ADR 0015 merged; `.fxdb` support follows ADR 0016's format core.
5. **Declarative migrations**, lazy reads, convergence counts, `migrate`, and
   Boa context reuse.
6. **Explorer**: validation errors shown on the field that failed, writes to a
   collection that needs an eager-only migration disabled with an explanation,
   and the folder view (JavaScript engine) made read-only for collections that
   have a schema until it is retired.

## Consequences

Benefits:

- every write path validates, and so does the browser;
- a database carries its schemas: a copied root, a `.fxdb` file in Explorer
  and a restored backup all validate the same way;
- a schema change is atomic with the data around it and recoverable;
- every build reads every document in the head shape;
- validation costs microseconds, not a file read and a regex compile.

Costs:

- schema changes need a `push`, not just a file edit, unless `autoPush` is on;
- three more record or snapshot types for recovery, verify and salvage to
  handle;
- JavaScript upgraders become a native-only, eager-only escape hatch;
- `_v` handling moves into the storage crate, which until now never parsed a
  schema.

## Alternatives considered

- **Keep schemas in `fylo.schema` only.** Simplest, but a `.fxdb` file opened
  anywhere else has no schema, and nothing records which schema a document was
  validated against.
- **A source-of-truth schema extent in `.fxdb`.** Needs its own atomic publish
  protocol beside the commit path (the format thread's objection). Committing
  versions as records reuses the one that exists.
- **Validate in the browser with the host's `RegExp`.** Free in code size, but
  JavaScript's regex dialect differs from Rust's (lookaround, backreferences,
  Unicode classes), so a document could pass in Explorer and fail natively,
  and JavaScript regexes backtrack, so a hostile pattern in a file could hang
  the tab. Rust's `regex` family guarantees linear time.
- **Defer browser validation to the next native open.** The user who made the
  bad edit is gone by then, and a `strict` collection would briefly hold
  documents it should have refused.
- **Eager-only migration on open, like Realm, SQLite's `user_version` or
  IndexedDB's `onupgradeneeded`.** Predictable, but opening a large database
  on network storage would block on rewriting it, and a read-only snapshot in
  the browser could not open at all.
- **Lazy-only migration, MongoDB's "schema versioning pattern".** Never blocks,
  but old shapes and the code that reads them live forever.
- **Run upgraders in a sandboxed browser Worker.** Possible later; it still
  means executing code carried by a file, so it is not part of this decision.

## Not decided here

- **Typed leaves in CHEX**, so `30` and `"30"` can be told apart, and numeric
  ranges. That changes the CHEX format, which is its own project, and deserves
  its own ADR.
- **Cross-collection rules** (foreign keys, uniqueness). FYLO has no
  multi-collection transactions yet (ADR 0016 section 1).

## Verification

- Every mutation operation, in strict mode, rejects an invalid post-image and
  writes nothing; in warn mode commits and counts it; in off mode commits.
- Encryption: a `patchDoc`, `patchDocs`, SQL `INSERT` or SQL `UPDATE` of an
  `$encrypted` field stores ciphertext; strict mode with `$encrypted`
  accepts a valid document; an encrypted `"999"` reads back as a string.
- A schema with an array of object items is refused at push.
- Native and WebAssembly agree on the CHEX golden corpus and the differential
  corpus.
- Crash matrix: failpoints `after-schema-record`, `after-schemas-snapshot`,
  `mid-migrate-batch`, and the file layout's `after-schema-file`, under
  `abort`, `enospc` and `edquot`. Recovery leaves the registry and the head
  consistent with the documents.
- A lazy read, a write and an eager migration of the same document give
  byte-identical results.
- Older-binary fences: a pre-ADR binary refuses a registry collection and
  opens a `.fxdb` with the new bit read-only.
- Performance: validation of the 5k fixture's documents adds under 20 µs per
  write at p99 natively, measured beside the write latency baselines.
