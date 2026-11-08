# Browser path cohesion

## Plan

Move browser path utilities into one `BrowserPath` class, preserving normalized
path results and traversal rejection. Update only imports and calls in its
eight first-party consumers. Keep browser code independent of Node path APIs.
Add direct boundary tests before refactoring; run the existing browser suite and
typecheck afterward. Generated bundles and vendor files stay untouched.

## Progress

- Inspected all eight consumers and existing path implementation.
- Existing browser baseline is green (72 tests, 318 assertions).
- Added explicit normalization, relative lookup, and containment contracts:
  three tests / 51 assertions passed before and after refactoring.
- Replaced free functions with one `BrowserPath` class. Public methods coordinate
  normalization, path selection and containment checks; private decisions and
  iteration are separated. Migrated 39 references in eight consumers using
  imported-symbol-aware edits; generated artifacts remain untouched.
- Differential validation: 7,070 checks matched the original implementation,
  including non-string arguments and generated parent/relative path cases.
- Typecheck found and caught an obsolete internal `join` re-export in the memory
  adapter (no repository consumers and never part of the public browser entry
  point); it was removed along with a missing array type annotation.
- Full browser suite passed: 75 tests, 369 assertions.
- TypeScript typecheck passed. Focused AST checks passed for the path class and
  previously refactored metadata classes (65 methods across three classes).
- No storage layout, path escaping policy, or generated release assets changed.

## Coordination

Root owns this slice and serializes heavy verification for the swarm. Native
index-cache and machine application-config workers own independent Rust files.
