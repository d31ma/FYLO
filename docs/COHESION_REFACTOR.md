# Cohesion refactoring

## Contract

Apply the shared one-type-per-file and function-cohesion rules without changing
persisted bytes, error codes/messages, validation order, SDK behavior, or wire
protocols. Preserve unrelated work already present in the checkout. Tests and
generated/vendor sources are not refactoring targets.

The unpublished Rust crates may move free functions to associated methods, with
all repository callers updated together. Existing document and metadata types
remain re-exported from the crate root. No data migration is involved.

## Sequence and progress

1. Architecture inventory: complete. The format, query, engine, native storage,
   machine protocol, browser services, SDK shims, and tooling contain violations.
   Graphify identifies these same modules as dependency hubs. A single broad
   mechanical rewrite would make behavior changes difficult to isolate.
2. Portable format slice: complete. Split
   document parsing/validation, limits, canonical metadata, TTID timestamps, and
   errors into seven type files plus an import-only crate root. Moved existing
   tests out of production source and added two boundary contract tests before
   refactoring. Updated all repository callers to `TtidTimestamps::decode`.
3. Browser metadata slice: complete. Separated
   `MetadataRecord` validation from `BrowserMetadataStore`. Public methods only
   coordinate; private helpers separate validation, path selection, mutation,
   and sidecar decoding. Three new behavior tests passed before and after the
   refactor, alongside existing legacy-shard and prototype-safety tests.
4. Equality-candidate planning slice: complete.
   Separated the plan's union of operations from each operation's intersection
   of field candidates. Preserved encrypted-field fallback, conservative
   equality candidates, and direct insertion into the existing candidate set
   without introducing an intermediate set per prefix.
5. Validation and review: complete for these three slices. The full native
   workspace tests, strict workspace Clippy, TypeScript typecheck, formatting,
   and full browser suite passed. Focused AST checks passed for the refactored
   Rust and JavaScript types. Reviewed validation/error precedence, safe-record
   handling, legacy fallback, unchanged index candidate logic, and allocations.
6. Remaining codebase: pending. Remaining query planning/index construction, native storage
   and queues, engine/machine protocol, other browser services, SDK shims, and
   release/build tooling still require separately tested slices. This document
   does not claim repository-wide compliance.

## Execution constraints

- Work stays local: no commits, push, release, or CI dispatch.
- The pre-existing worktree is dirty and its changes must be preserved.
- No active build or soak was found before starting. No designated remote build
  host was found in the repository's instructions or operations documentation.
- Use the pinned Rust toolchain through `bun scripts/run-rust.mjs cargo ...`,
  wrapped in `/Users/iyor/.local/bin/annex-dev`.
- Only one heavy local verification job at a time.
- Plan usage was 40% with nearly six days remaining in the weekly window;
  no additional parallel agents are launched while usage is ahead of pace.
- The user authorized a coordinated swarm and explicitly approved an exception
  to the usage pacing rule. Usage was 42% at dispatch. Concurrency remains capped
  at three agents and heavy verification remains serialized.

## Coordinated wave

- Native index-cache worker completed cache input merging, retained entries and cache
  coordination. The 12 focused cache tests passed; root review found and removed
  a nested iterator-predicate pattern. Progress: `docs/cohesion/index-cache.md`.
  The same worker extracted the root lease's lock/owner/metadata types;
  progress: `docs/cohesion/root-lease.md`.
- Application-configuration worker completed machine-side configuration loading and
  secret-reference resolution. Progress: `docs/cohesion/application-config.md`.
- Root completed browser path utilities and their import/call-site migration,
  combined review, testing, and Graphify refresh. Worker tests were queued through
  root so heavy commands did not overlap.
  Browser path boundary tests and 7,070 differential checks passed; typecheck
  passed after updating an obsolete internal re-export.

### Combined wave evidence

- All 192 native workspace tests passed with all targets/features on this Mac.
- All 75 browser tests passed with 369 assertions.
- Application loader focused checks: six unit tests and eight CLI integration
  tests passed, including diagnostic precedence and secret-file boundaries.
- Cache focused checks: 12 tests passed; combined storage tests also cover the
  new root-lease clone/metadata/canonical-alias cases.
- Strict combined workspace Clippy, final Rust formatting, and TypeScript
  typecheck passed. Two trivial-copy-by-reference predicate arguments found by
  Clippy were corrected without lint suppressions. The final rerun passed all
  26 machine and 78 storage tests after those mechanical changes.
- Focused Rust structural checks passed for 23 production files / 182 methods.
  Manual reviews covered iterator predicates, lock/drop lifetimes, security
  boundaries, error precedence, and allocation behavior.
- No commit, push, CI dispatch, release, or new background soak was performed.

## Initial pass evidence

Baseline: 12 existing format tests passed; two added boundary tests also passed
against the original implementation. A direct Cargo invocation selected Rust 1.95.0 instead of the pinned
1.97.1; no code was changed in response. Verification uses the repository's
pinned-toolchain runner instead.

After refactoring:

- Full native workspace tests passed with all targets and features, including
  the additional stable-error-code test (15 format tests in total).
- Final focused engine/format tests passed after eliminating an unnecessary
  intermediate candidate set discovered in review.
- Full browser suite: 72 tests, 318 assertions, no failures.
- Strict workspace Clippy, TypeScript typecheck, Rust formatting, scoped
  Prettier, and `git diff --check` passed.
- Ad hoc AST checks passed for ten Rust files / sixty methods and the two
  browser metadata classes. These check type counts, branch-free public methods,
  one decision/loop per private helper, and direct branching-helper calls in
  Rust. They are focused checks, not a repository-wide compliance certification
  or a newly installed CI gate.

Graphify full incremental refresh was attempted. Its node-loss safeguard refused
to replace the existing 9,002-node graph with a smaller extraction. No forced
full overwrite was used. A scoped AST/semantic reconciliation now refreshes only
this refactor's files, preserves unrelated nodes, checks referential integrity
and concurrent changes, and retains a before-image and audit record in
`graphify-out/`. The scoped merge preserved all 8,148 unrelated nodes and added
19 semantic nodes. The final graph/report contains 9,397 nodes / 23,013 edges
across 394 communities. Existing legacy graph warnings remain; no unrelated
nodes were removed to silence them. Semantic token cost is unmeasured, not zero.

## Updated-rule wave (2026-09-27)

The user requested another coordinated pass after updating the shared rules.
Root plus two workers retained the prior pacing exception and three-agent cap.
This wave covers native cache/lease code, application configuration, document
format contracts, browser paths/metadata validation, and browser event streams.

The acceptance criteria remain behavior preservation: stable public APIs,
serialized keys and bytes, error precedence, secret-file boundaries, lock/cache
lifetimes, and event ordering. Private names and repeated literals are made
explicit; test cases contain assertions without branches or loops. Existing
public abbreviations and serde-required literals remain compatible.

Browser stream lifecycle moved into `BrowserEventStream`, leaving
`BrowserEventBus` responsible for journal publication and subscriptions.
Before the move, four new event tests passed against the original service.
Afterward, focused browser verification passed 57 tests / 103 assertions,
including append failure, serialization failure, queued events, lazy
subscription, consumer errors, resumed waiting, and unchanged string conversion
in unsafe-path diagnostics. Typecheck and scoped formatting passed.

The first combined Cargo run caught two rewritten CLI fixture failures: a Rust
constant reference had accidentally been inserted inside raw JSON. The worker
restored the original JSON and then audited 163 string nodes and parsed all 25
raw JSON literals across its ten files. Production parsing was unchanged.

The user explicitly requested handoff with full verification pending after the
rerun waited behind another project's active workspace suite. FYLO's queued
shell was terminated before it acquired the shared lock or started the rerun;
the other project and its lock were left untouched. No verification process
from this pass remains queued. The corrected native suite, strict workspace
Clippy, workspace formatting gate, and full browser suite must still run. The
earlier 192-native/75-browser results above belong to the previous wave, not to
these latest edits. Do not treat this wave as fully verified or release-ready.

Per-slice records:

- `docs/cohesion/updated-native-rules.md`
- `docs/cohesion/updated-config-rules.md`
- `docs/cohesion/updated-format-rules.md`
- `docs/cohesion/updated-browser-rules.md`

The existing shared Mac heavy-job queue is used for native verification; other
projects' running jobs are not interrupted. No commit, push, CI dispatch,
release, dependency install, or new background soak is authorized in this pass.

The scoped Graphify refresh covered 40 source/test files plus five documentation
sources. It preserved all 8,900 unrelated nodes, removed 28 stale scoped AST
nodes, and added 20 explicit semantic nodes. The refreshed graph has 9,527 nodes
and 23,227 edges across 406 communities. A before-image and audit are retained
under `graphify-out/updated-rules-*`. Legacy graph warnings remain; semantic
token cost is unmeasured, not zero. No large HTML visualization was generated.

To resume verification, first acquire the shared machine build slot, then run:

```sh
bun scripts/run-rust.mjs cargo test --workspace --all-targets --all-features --locked --quiet
bun scripts/run-rust.mjs cargo clippy --workspace --all-targets --all-features --locked --quiet -- -D warnings
bun scripts/run-rust.mjs cargo fmt --all --check
bun node_modules/typescript/bin/tsc --noEmit
bun test tests/browser/*.test.js --timeout 120000 --parallel=1
```

Use the machine-local ANNEX launcher for these commands on this Mac. Keep the
source snapshot stable until the combined checks finish.

### Resumed verification

The user subsequently authorized continuation. The corrected native workspace
suite passed all 246 tests. Strict workspace Clippy found two test-only defects:
a complex numeric-sample return type and a potentially truncating test size
conversion. Both were corrected without lint suppressions; Clippy, the affected
2 format and 26 configuration tests, workspace formatting, and typechecking
then passed. All 15 CLI configuration tests passed in the workspace run.

The full browser suite first exposed two FSA interop tests that hardcoded
`target/debug` despite ANNEX artifact routing. They now use `CargoArtifacts`
while preserving `FYLO_BINARY` overrides. After building the current debug
executable, all 126 browser tests / 391 assertions passed. These results close
the previous verification-pending handoff; they do not certify future edits.

The next bounded wave extracts queue records and claim validation, separates
schema responsibilities, and aligns previously extracted filenames/imports.
See `docs/cohesion/resumed-filename-rules.md`, `docs/cohesion/queue-rules.md`, and
`docs/cohesion/schema-rules.md`.

### Resumed wave completion

- Queue: 14 record/option types moved into matching files. Claim validation and
  its bounds belong to `QueueClaimOptions`. Fifteen added cases preserve default
  and wire envelopes, validation precedence, accepted limits, and rejection
  before storage creation. All 38 queue tests pass.
- Schema: 14 production types now separate paths, file reads, manifests,
  inspection/diagnosis, upgrade planning/execution, and CHEX lifecycle. Twenty-one
  characterization cases passed before and after the split; six further cases
  exercise upgrade order, rejected promises, outgoing data, warm-process reuse,
  respawn, and tolerant error responses. All 27 cases pass. Existing public
  interfaces and filesystem/protocol behavior are retained.
- Filename alignment: seven earlier production classes/types now have matching
  filenames, with consumer imports and facade aliases adjusted. Browser sample
  classes have matching helper files. The format contract verifier follows the
  extracted error-code file instead of the old monolithic facade.

The final stable source passed: 288 native workspace tests, strict all-target /
all-feature Clippy, a rebuilt CLI, 126 browser tests / 391 assertions, semantic
typechecking, warning-free workspace Rust documentation, workspace Rustfmt,
scoped Prettier, and `git diff --check`. Protocol verification covered 45
operations / 60 errors. Registered MC/DC verification passed 6 decisions / 27
conditions / 33 cases; this is not whole-engine MC/DC coverage.

The fresh browser LCOV/CRAP report has maximum 4 across 83 function records in
the five scoped browser production classes, below the limit of 15. Its global
scan still flags 36 functions outside this slice. Rust AST checks found no
explicit scoped structural violations in the 14 schema production files / 106
methods or 18 queue/renamed-type/fixture files / 27 methods. These checks do not
certify numerical Rust coverage or repository-wide compliance.

Read-only cross-review found no new behavior or security regression in the
queue records, schema files/execution, or root filename/artifact-path changes.
Existing file-check/open races were preserved, not hardened. Linux/Windows CI,
signed release artifacts and production soak qualification were not performed.
All changes remain local and uncommitted; no other project's job was stopped.

The scoped Graphify refresh extracted 89 source/test files and eight documentation
sources. It preserved 8,389 unrelated nodes, removed 147 stale scoped AST nodes,
and replaced 20 scoped semantic nodes with 24 current rationale/contract nodes.
The graph/report now contains 9,869 nodes / 23,736 edges / 497 communities.
Before-image and audit artifacts are retained under `graphify-out/resumed-rules-*`.
Legacy extraction warnings remain; semantic token cost is unmeasured. No large
HTML visualization or external labeling service was used.

## Remaining repository-wide work

This is not complete repository-wide compliance. Highest-impact remaining
modules include native write/version management, storage discovery, queues,
machine dispatch, query parsing/execution/index construction, the CLI, remaining
browser services, SDK shims, and build/release tools. They need the same
behavior-first refactoring and cross-platform verification; broad mechanical
rewrites would not substitute for those gates.

A lightweight Rust inventory scanned 49 source files and flagged 17 for further
review using only top-level type counts and explicit public control flow. The
largest concentrations remain storage `lib.rs` (27 types / 19 public control-flow
methods), `write.rs` (17 / 13), version management (17 / 7), queues (17 / 5),
query `lib.rs` (11 / 9), and engine `lib.rs` (12 / 8). This is a prioritization
aid, not an exhaustive rules audit; it does not certify numerical CRAP scores,
coverage, every macro, SDKs, or generated-code exemptions.

Internal source migrations: `decode_ttid` became `TtidTimestamps::decode`, format
and limit constants moved to `Document`/`DocumentLimits`, and
`validateMetadataRecord` became `MetadataRecord.validate`. These modules/crates
are implementation internals; SDK and machine-protocol entry points are unchanged.
