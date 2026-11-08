# Root lease cohesion

## Contract and scope

Separate `RootLease`, `RootLeaseInner`, and `LeaseMetadata` into one-type files.
Preserve the public root reexport, canonical root/sentinel identity, platform
lock fallback, metadata field order and errors, owner checks, and last-Arc
kernel unlock. No storage crate-root or unrelated module changes are planned.

## Plan and progress

1. Context: complete. Read the current lease module and existing tests. Native
   workspace baseline was green earlier in the coordinator's run. The cache
   slice has been accepted in review and its pre-final-tweak 12 tests passed.
2. Extraction: complete and reviewed. `lease.rs` is an import-only
   facade with preserved test code. The public lease coordinates shared
   ownership; its inner value acquires and releases the sentinel; metadata owns
   record creation, bounded reading, and serialization. Three production type
   files live under `lease/`; the storage crate root is unchanged.
3. Behavior coverage: implemented. Both existing acquisition tests remain.
   Added last-clone lock retention/release; replaced, future-version, malformed,
   oversized, missing, and non-file ownership records; canonical symlink alias
   contention; and symlinked owner-record rejection. Restoring the original
   metadata restores ownership validation, matching the existing contract.
4. Verification: complete. Pinned Rustfmt and `git diff --check` pass. Source review
   confirms the original two `AsRef` calls, filesystem/lock/write ordering,
   error text, metadata field order, native/wasm fallback behavior, and final
   Arc unlock. The coordinator's dependent machine/CLI run passed (6 unit and
   8 CLI tests) after initial extraction. Final combined native workspace
   verification passed all 192 tests, including all 78 storage tests and the
   new lease cases. This agent ran no builds or tests independently.
   Coordinator semantic review accepted the extraction. Minimal tool-read
   `# Errors` rustdoc was retained on the public fallible lease methods for
   strict Clippy. Coordinator behavior/security and source-cohesion reviews
   accepted this slice. Final strict workspace Clippy passed; all 78 storage
   tests plus 26 machine tests passed again after the final cache-only lint fix.

## Recommended verification

- `bun scripts/run-rust.mjs cargo test -p fylo-storage-native lease`
- Full native workspace tests and strict Clippy.
- Focused cohesion AST audit of the three new production type files.

Run all development commands through the shared workspace's Annex launcher.

## Preservation checklist

- Create a missing root before canonicalizing it; sentinel remains beside root.
- A lock conflict still describes a readable recorded owner, else the root.
- Unsupported kernel locking is reported as non-enforced, not silently enforced.
- Metadata serialization order, timestamp timing, write/truncate, and sync stay.
- Symlink, non-file, oversized, invalid, mismatched-version/owner records fail
  ownership checks without trusting their contents.
- Clones retain the same sentinel until the final inner Arc is dropped.

No release actions, dependencies, or global graph mutations are included.
