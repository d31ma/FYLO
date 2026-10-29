# ADR 0018: JSON-only schema migrations

- Status: **Accepted** (Samuel, 2026-10-29)
- Date: **2026-10-29**
- Owners: **FYLO maintainers**
- Amends: ADR 0017 section 5 (migrations), and the parts of sections 6 and 7
  that exist only because of JavaScript steps.

## Context

ADR 0017 made migrations a list of declarative JSON steps that every build can
run, and kept JavaScript upgraders as a native-only escape hatch: a `js` step
runs an upgrader module in the embedded Boa runtime. That escape hatch is the
only reason ADR 0017 needs:

- a JavaScript runtime in the native binary;
- "eager-only" versions, which builds without Boa cannot read in the old shape;
- the `ESCHEMASTALE` error for those reads;
- a rule that the WebAssembly builds never run code carried by a database
  file.

Samuel asked (2026-10-29) whether migrations can be JSON only.

## Decision

A migration is JSON and nothing else. No FYLO build executes code from a
schema folder or a database.

### 1. No JavaScript steps

- The `js` step is removed from ADR 0017's step table.
- `fylo schema push` refuses a version whose migration names a `.js`
  upgrader, or whose schema folder has an `upgraders/` file for that step,
  with `ESCHEMA` and a message that names the file and points to this ADR.
- `schemaMaterialize` runs the version's JSON steps instead of an upgrader
  chain. A collection that still has `upgraders/*.js` files and no JSON steps
  fails `schemaMaterialize` with the same message, so an old upgrader is
  never silently skipped.
- `boa_engine` is removed from `fylo-engine`, with the module loader and the
  upgrader source normalizer.

### 2. String steps

So the common transformations stay expressible, the step table gains:

| Step                                          | Effect                                                                 |
| --------------------------------------------- | ---------------------------------------------------------------------- |
| `{"lower": "x"}`, `{"upper": "x"}`            | change a string's case (Unicode full case mapping)                     |
| `{"trim": "x"}`                               | remove leading and trailing whitespace                                 |
| `{"replace": "x", "find": "a", "with": "b"}`  | replace every occurrence of literal text                               |
| `{"template": "x", "value": "{a} {b/c}"}`     | build a string from other fields, by slash-separated path              |
| `{"slugify": "x", "from": "title"}`           | lower-case, ASCII letters and digits joined by `-`, `untitled` if empty |

`replace` matches literal text, not a pattern, so a migration needs no regex
compiler in the browser. `slugify` is the README's slug upgrader:
`String(doc.title ?? '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'untitled'`,
reproduced byte for byte.

A step whose source field is missing is a no-op, as in ADR 0017. A step whose
source is not the type it needs (for example `lower` on a number) fails the
migration, and `fylo schema push` reports it in its dry run.

### 3. Anything else is an application update

A change the step table cannot express is made by the application, as data:

1. push a version whose schema accepts both the old and the new shape;
2. rewrite the documents with `patchDocs` or SQL `UPDATE`, which validate
   against that version since phase 1 of ADR 0017;
3. push a version that accepts only the new shape, with no steps.

`fylo schema push` refuses step 3 while any live document still fails the new
version, which is the same dry run ADR 0017 already requires.

### 4. What ADR 0017 no longer needs

- Every version is readable lazily by every build, so "eager-only" versions,
  `ESCHEMASTALE`, and Boa context reuse are dropped.
- Explorer never has to disable writes for a collection waiting on a native
  migration.
- `fylo schema migrate`, idle-time migration in an embedded process that
  holds the exclusive lease, and migration in `fylo pack` stay, as ways to
  converge, not as requirements to read.

## Consequences

Benefits:

- every migration runs in every build, including the browser and mobile;
- nothing in FYLO executes code from a file it opens;
- the native binary loses the JavaScript runtime (size measured when the
  change lands);
- migrations are data, so `fylo schema doctor` can check them without running
  them.

Costs:

- existing `upgraders/*.js` files must be rewritten as JSON steps; the
  repository's own fixtures and the README example are converted in the same
  change;
- some transformations become a two-version application update instead of
  one upgrader.

## Alternatives considered

- **Keep JavaScript as a native-only escape hatch** (ADR 0017 as accepted).
  Keeps every upgrader working, at the cost of a runtime, eager-only versions
  and a class of documents the browser cannot read.
- **A small expression language** (JSONata, JSON Logic, or our own). More
  expressive than the step table, but it is a language to specify, version
  and keep identical across builds. The step table can grow one step at a
  time when a real migration needs it.
- **Regex `replace`.** Needs a regex compiler in every build that migrates,
  which ADR 0017 keeps out of the browser.

## Verification

- Each step, natively and in WebAssembly, on the same corpus, with
  byte-identical results.
- `slugify` against the README upgrader's output on a corpus of titles,
  including empty, punctuation-only and non-ASCII input.
- `fylo schema push` refuses a `.js` upgrader; `schemaMaterialize` refuses a
  collection that has one and no JSON steps.
- The native binary builds and passes its suite with no `boa_engine` in
  `cargo tree`.
