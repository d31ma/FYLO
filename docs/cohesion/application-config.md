# Application configuration cohesion

## Contract and plan

Preserve `ApplicationConfig::load`, configuration JSON shape, nested-key
validation, exact errors and error precedence, lazy adjacent `.env` reads,
environment precedence, one-MiB reads, and credential redaction. The existing
application source is pre-existing untracked work and is being retained through
a behavior-preserving split, not discarded.

Separate application assembly, bounded configuration-file reading, settings
validation, reference syntax, secret lookup, declaration parsing, and quoted
secret text. Each production file contains one type and associated methods.
Tests remain exempt from the source-shape rules.

## Progress

1. Read workspace/project instructions and existing Graphify context. Reviewed
   the complete loader and CLI configuration regression suite. The parent
   coordinates final Graphify refresh and serialized heavy verification.
2. Split the loader into seven one-type modules and an import-only facade.
   Retained the original two parser tests and added four tests covering error
   precedence, lazy secret-file reads, relative paths, BOM/UTF-8/file limits,
   and quoted-secret escape/comment behavior. No machine-protocol or crate-root
   edits were needed.
3. Scoped formatting and `git diff --check` passed. Manual source-cohesion and
   behavior review completed: public methods coordinate, private helpers hold
   at most one decision or loop, and branch helpers do not directly call other
   branch helpers. Parent source review agreed that behavior, error precedence,
   and security checks are preserved.
4. Parent ran focused verification: all six application unit tests and all eight
   CLI application-configuration tests passed. Combined workspace verification
   passed all 192 native tests. Strict Clippy first stopped in the separate cache
   slice; proactively changed the quote predicate to receive `char` by value to
   avoid the analogous trivial-copy-by-reference lint. Final strict workspace
   Clippy passed. After the predicate fixes, all 26 machine tests and 78 native
   storage tests passed again.
5. Prepared a delegated Graphify semantic fragment at
   `graphify-out/cohesion-semantic.json`, using the shared cohesion plan and four
   slice notes. It contains 19 stable boundary/invariant concepts and explicit
   documented relationships, without test-count or repository-wide compliance
   claims. This worker did not change the current graph, report, or caches. The
   parent subsequently merged the fragment and preserved all 8,148 unrelated
   graph nodes with before-image/audit evidence. Token cost is unavailable: both
   zero token counters are placeholders, not measured usage.
6. Slice complete. Parent final gates also passed 75 browser tests, TypeScript
   typechecking and formatting. No release or external mutation was performed.

## Verification

The parent reports baseline full-workspace tests and strict Clippy passed before
this wave. Post-refactor focused functional tests passed (six machine unit tests,
eight CLI integration tests), followed by all 192 native workspace tests and
strict workspace Clippy. The final predicate adjustment was reverified by all
26 machine tests. Formatting and whitespace checks passed.

Focused commands recommended (through the pinned runner and ANNEX launcher):

- `cargo test -p fylo-machine application::tests`
- `cargo test -p fylo-cli --test application_config`

## Review

Application assembly keeps the original order: top-level keys, root, schema,
strict, shard, encryption reference, salt reference, credential pairing, Chex.
Credential values remain excluded from Debug/Serialize. The file reader retains
both pre-open and post-open regular-file checks, existing symlink behavior,
optional-not-found semantics and a bounded one-MiB-plus-one read. Neither process
environment nor configuration files are mutated by loading.
