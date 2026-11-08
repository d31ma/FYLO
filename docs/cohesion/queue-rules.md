# Queue records and claim options

## Contract

Preserve queue APIs, serialized envelopes and field order, validation order and
messages, durable transitions, receipt checks, read budgets, and path safeguards.
Keep the existing storage crate reexports unchanged.

## Progress

1. Read the shared and repository instructions and queue Graphify community.
   The coordinator confirmed a green native/browser baseline before edits.
2. Complete: extracted eight public records/options and six internal records/budget types to
   files named after their types. Keep internal types and fields parent-only.
   Move claim defaults and validation limits into `QueueClaimOptions`, whose
   visible validation methods coordinate single-check private helpers.
3. Complete: added 15 real `NativeQueue::claim` boundary and envelope/default
   tests. Existing queue behavior and MC/DC tests remain. The new invalid-option
   tests verify error precedence and that no queue layout is created. Both lower
   and upper accepted option boundaries are exercised. Each new case directly
   asserts and has no branch or loop. A test-only `QueueClaimSamples` fixture owns
   repeated limit values and error text. The rejection helper returns the actual
   error and layout state after cleanup; assertions live in each test case.
4. Scoped pinned Rustfmt and whitespace checks passed. A declaration comparison
   confirmed all 14 moved records retain derives, serde attributes, field types
   and field order. Parent-only visibility replaces private fields solely to
   retain their original queue-module access. A normalized body comparison found
   all 94 retained functions unchanged except the intended option delegation and
   constant paths. The coordinator ran all 38 queue tests successfully. An initial
   test-only failure was corrected to assert the existing `EQUEUE_LIMIT: ` Display
   prefix, not to change errors. Strict storage-crate Clippy also passed in the
   coordinator's run. All 38 queue tests passed again after the direct-assertion
   and test-fixture adjustment. The coordinator's final workspace checks passed:
   288 native tests, strict workspace Clippy, Rustfmt and warning-free Rustdoc.
   Registered MC/DC verification passed all six decisions, including both queue
   decisions. This is not whole-queue MC/DC or numerical Rust CRAP certification.

## Review

The extracted production files each contain one matching type, with associated
constants and no new module variables or free helpers. Claim validation remains
message count, visibility timeout, attempt count, then response budget; visible
methods coordinate private one-check helpers. New private constant names spell
out maximum, minimum and milliseconds. Public field names stay unchanged.
No receipt generation, durable write sequence, filesystem access checks, lease
lifetime, consumer-state transitions, or serialized field visibility changed.
No new dependencies, commits, pushes, or background jobs were introduced.

## Deliberately remaining

`NativeQueue`, `ConsumerState`, `DeliveryState`, receipt encoding, and persistence
helpers remain a later queue slice. Existing public signatures with more than
four arguments are retained to avoid an unrelated breaking API change.
This pass is not a claim that the whole queue module satisfies every new rule.
