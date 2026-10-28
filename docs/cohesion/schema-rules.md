# Schema cohesion rules

> **Superseded for CHEX (2026-10-28).** CHEX validation now runs in-process
> through the vendored `fylo-chex` crate. The external process lifecycle,
> transport, respawn, and fake-executable tests described below no longer
> exist. The rest of this record still describes the schema split.

## Scope and acceptance

Refactor native schema administration, validation and upgrade execution without
changing the `schema::SchemaTools` call interface, wire values, file handling,
error precedence, or external CHEX process lifecycle. Preserve the working-tree
source, including its existing uncommitted changes.

The dependency graph identifies the current SchemaTools community as the shared
boundary between manifest inspection, bounded file reads, CHEX and Boa. Split
those responsibilities into correctly named, single-type files under `schema/`.
Use associated constants, coordinative public methods and small private helpers.
Use typed serialization models instead of constructing response maps by hand.

## Progress

- Shared instructions and repository instructions read; graph context consulted.
- Root confirmed the full native/browser baseline, Clippy, format and typecheck.
- All 21 behavioral characterization tests passed against the original code.
- Split production code into 14 single-type files plus import-only facades.
- Inspection and diagnosis now serialize typed models with unchanged output.
- CHEX transport retains tolerant JSON response handling and resets only after
  transport failures, not validation rejection. Request field order is retained.
- The fixture is in its own correctly named file; every test case has assertions
  and no loops or branches.
- Six additional tests cover upgrade chain ordering, promise rejection, outbound
  request data, process reuse after validation rejection, transport-error respawn,
  and tolerant error-message handling. There are 27 cases in total.
- A separate agent reviewed both file/inspection and execution/validation slices
  read-only and found no behavioral or security regression.
- Root's structural audit passed 14 production files and 106 methods, with no
  explicit structural findings. This is not a measured Rust CRAP certification.
- No dependencies, external services, commits or releases are in scope.

## Verification

Root coordinates all heavy tests. New tests cover observable SchemaTools results
and real filesystem/Boa behavior. CHEX tests use a fake external executable at the
existing process boundary, not an in-process replacement.

The original 21 tests also passed after extraction. Compilation and Clippy found
ordinary signature/borrow and explicit-default issues; these were fixed before
the final combined run.

Root verified the final workspace on macOS:

- All 288 native tests passed, including all 27 schema cases.
- Workspace Clippy passed with warnings denied.
- CLI rebuilt successfully.
- All 126 browser tests passed with 391 assertions; typecheck passed.
- Rustdoc passed with warnings denied; workspace formatting passed.
- The protocol contract guard passed for 45 operations and 60 error codes.
- The existing MC/DC gate passed for six decisions, 27 conditions and 33 cases.
  That gate covers its declared decision set, not every newly extracted helper.

These checks are local macOS evidence, not Windows/Linux or release qualification.
The root agent owns the combined dependency graph refresh and final handoff.

## Preserved contracts

- `SchemaTools` remains re-exported from `schema`; engine call sites do not change.
- Missing manifests remain unversioned. Inspect explicitly emits null manifest
  and current fields, while unavailable optional per-version fields are omitted.
- Manifest unknown fields are omitted from inspection, and explicit null optional
  fields are omitted just as before. Hash errors remain non-blocking observations
  until diagnosis reports the corresponding mismatch.
- Diagnosis preserves issue ordering, repeated version findings, and JSON-quoted
  paths and next-version labels in existing message text.
- Upgrade validation preserves empty-list, unknown-source, missing-head and
  rolled-back-head error order. Documents already at the head remain unchanged,
  including an absent version stamp. Actual upgrades remove the old stamp before
  calling modules and stamp the head only after the complete chain succeeds.
- Boa keeps its root-confined module loader, fresh runtime per upgrade step,
  10,000,000-iteration limit, arrow-export normalization, promise waiting and
  object-result requirement. No runtime host capability is added.
- Flat schemas leave documents unstamped. Head validation removes the input
  version field before CHEX, then stamps the validated result. CHEX uses the same
  command arguments, inherited stderr, bounded response frame and child cleanup.
- File reads retain the existing regular-file/symlink checks and 16 MiB bound.
  This is not a new filesystem race-hardening or sandbox-security claim.

## Remaining scope

This completes only schema administration/validation/execution cohesion. The
separate encryption implementation, engine entry point and broader repository
cohesion backlog are not changed by this slice. No behavior, dependencies,
configuration, public documentation, release artifacts or platform support are
intentionally changed.
