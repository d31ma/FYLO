# Updated document-format coding rules

## Acceptance and scope

Keep the exported Rust types, constants, serde field names, encoded JSON, error messages, and validation order stable. Apply the updated rules to `crates/fylo-format/src` and its existing tests only.

## Plan and progress

- Read every format source/test and the existing graph's format/compatibility context.
- Name timestamp length/range/position/radix bounds and repeated sentinel/error values; spell out private millisecond constants. Keep public compatibility names unchanged.
- Replace test-case branches and loops with explicit cases or fixture methods returning assertable results. Preserve every golden fixture and all 4,096 generated float inputs and ten repeated encoding cycles.
- Move repeated test literals into fixture-associated constants; keep serde attribute literals required by the derive API.
- No heavy validation here. The coordinator runs combined Rust gates and refreshes the dependency graph after source stabilizes.

## Completed changes

- Timestamp parsing now names every numeric bound in a condition, segment positions, radix, rounding divisor, placeholder, and repeated invalid-segment message. Private timestamp bounds spell out milliseconds and minimum/maximum. Public type names and document-limit constants are unchanged.
- Every format test case is branch-free and loop-free with an explicit assertion. Eight error-code representations and all prior invalid identifiers remain covered as explicit cases. Golden document and metadata cases still run in full through a fixture helper with unchanged expected values.
- Numeric fixtures retain the original deterministic bit sequence, finite-value filter, original-value comparisons, and ten successive parse/encode checks; test bodies compare generated actual/expected sequences.
- Repeated test values use fixture-associated constants. JSON fixture bytes and serde attribute names are unchanged.
- Targeted Rust formatting and `git diff --check` passed. Combined Cargo verification is queued by the coordinator; no independent heavy command was launched.

## Focused verification

```sh
bun scripts/run-rust.mjs cargo test -p fylo-format --all-targets --locked --quiet
bun scripts/run-rust.mjs cargo clippy -p fylo-format --all-targets --all-features --locked --quiet -- -D warnings
```

Use the machine-local ANNEX launcher and shared heavy-job queue. This slice does not certify repository-wide compliance or numerical coverage/CRAP scores.

The user requested handoff with full verification pending. The first combined
run stopped at two CLI fixture failures, subsequently corrected. The queued
rerun was cancelled before acquiring the shared lock. Current format tests and
Clippy subsequently passed during the resumed verification: all 246 native
workspace tests passed, followed by strict workspace Clippy after simplifying
the numeric fixture result type. Both numeric roundtrip tests passed again.
