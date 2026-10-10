# Native index cache cohesion

## Contract and scope

Separate `IndexInputs`, `CacheEntry`, and `IndexCache` without changing snapshot
validation, WAL replay, error text, cache admission, synchronization, or reader
lifetimes. Preserve all pre-existing tests and unrelated worktree changes.

## Plan and progress

1. Context and baseline: complete. Read shared/project instructions, the cohesion
   plan, existing Graphify report, and the cache implementation and callers.
   The coordinator confirmed native workspace tests and strict Clippy are green.
   Existing coverage checks file refresh, corruption, symlinks, admission,
   replacement, cross-root reuse, and poisoned locks.
2. Refactor: complete. The existing module path is an import-only facade;
   the three types have separate files below `index_cache/`. Public methods
   coordinate private validation, replay, admission, and replacement helpers.
   The storage crate root was not changed. Existing tests moved to a test-only
   file, preserving their assertions.
3. Behavior coverage: implemented. Added malformed/complete WAL replay,
   over-budget bypass retention (including WAL capacity), and corrupt snapshot
   rejection before a potentially repairing WAL. Extended the poisoning case
   to verify replacement after poisoning and the prior reader's lifetime.
4. Validation and review: complete.
   Pinned Rustfmt and `git diff --check` passed. Reviewed byte ordering, WAL
   record boundaries, error precedence/text, unchanged checked capacity sums,
   poisoned-lock recovery, out-of-lock build/destruction, and retained Arcs.
   Coordinator review requested a plain snapshot-key loop with named newline
   and nonempty-key helpers; this simplification is implemented without an
   additional intermediate collection.
   Heavy verification is serialized by the coordinator; this agent has not
   launched builds/tests independently.
   The coordinator's focused cache run passed all 12 tests before the final
   plain-loop review adjustment. Final combined native workspace verification
   passed all 192 tests, including all 78 storage tests and these regressions.
   Coordinator behavior/security and source-cohesion reviews accepted this
   slice. Strict Clippy's first run requested a by-value newline helper for the trivially
   copyable byte; that fix is applied without a suppression. Predicate closures
   only delegate to the helper. Final strict workspace Clippy passed, and all
   78 storage tests plus 26 machine tests passed again after that change.

## Recommended verification

- `bun scripts/run-rust.mjs cargo test -p fylo-storage-native index_cache`
- Full native workspace tests, including engine index-reuse concurrency tests.
- Strict workspace Clippy and the coordinator's focused cohesion AST audit.

Run all development commands through the shared workspace's Annex launcher.

## Risks to review

- Reject the original snapshot before replay, even if WAL could repair it.
- Ignore incomplete WAL tails and malformed/unknown complete records.
- Count vector capacities and merged bytes, not just input lengths.
- Build outside the lock; recover poisoned locks without changing error paths.
- Release the replaced entry after unlocking and preserve existing reader Arcs.
- Over-budget or corrupt inputs must not evict the last admitted entry.

No commits, dependency installation, CI, or global Graphify mutation are in scope.
