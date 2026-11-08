# Updated native coding rules

## Scope and acceptance

Apply the updated shared instructions to the existing index-cache and root-lease slices without changing public APIs, persisted records, lock lifetimes, cache admission, replay ordering, or error precedence. Other native storage modules remain outside this slice.

## Plan

1. Give log-record positions, metadata versions, and repeated fixture values named associated constants.
2. Replace the checked size fold with a simple loop and expand internal abbreviations while preserving wire names.
3. Split the loop-bearing lease test into separate assertion-bearing cases.
4. Review production method cohesion, signatures, dependency direction, and security-sensitive behavior; send focused validation commands to the coordinator.

## Progress

- Read the current shared instructions, repository instructions, graph report, and all assigned files.
- Existing production signatures already satisfy the four-parameter bound. The cache and lease slices keep their existing downward dependency direction.
- Implementation complete: named log byte positions and repeated cache-fixture values; replaced the size fold with a checked loop; expanded internal process/lock-file names while retaining the serialized `pid` field.
- Lease tests now live in `lease/tests.rs`. Thirteen separate cases cover acquisition, lock lifetime, owner-record validation, aliases, restored records, and the unchanged process-ID wire name. Every test case contains assertions and no branch or loop.
- Index-cache coverage remains twelve cases with the same assertions and inputs.
- Manual structural review found no signature above four parameters, no production file with multiple types, no public control-flow decisions, and no new dependency cycle. Byte accounting, malformed-log handling, error ordering, symlink rejection, and last-handle unlock remain unchanged.
- Targeted Rust formatting and `git diff --check` passed. Heavy validation and the combined dependency-graph refresh belong to the coordinating agent; no independent Cargo job was launched.

## Verification commands

Run through the ANNEX launcher and the shared machine queue:

```sh
bun scripts/run-rust.mjs cargo test -p fylo-storage-native --all-targets --locked --quiet
bun scripts/run-rust.mjs cargo clippy -p fylo-storage-native --all-targets --all-features --locked --quiet -- -D warnings
```

The coordinator authorized the surgical rename of crate-internal `IndexInputs.wal` to `log`, including its initializer in storage `lib.rs`. Private maximum constants now spell out `MAXIMUM`. Persisted `pid`, `keys.wal`, and error strings deliberately remain compatible.

The user requested handoff with full verification pending. The first combined
run stopped at two CLI fixture failures in another slice; those fixtures were
corrected. The queued rerun was cancelled before acquiring the shared lock.
The resumed pass subsequently passed all 246 native workspace tests, strict
workspace Clippy, and formatting, closing that pending verification.
