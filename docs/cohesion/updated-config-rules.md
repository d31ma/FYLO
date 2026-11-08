# Updated configuration rules

Resumed verification: all 15 CLI configuration tests passed in the 246-test
native workspace run. After replacing an unchecked test size conversion with
`usize::try_from`, strict workspace Clippy and all 26 configuration unit tests
passed. Typecheck and formatting also passed. The earlier pending handoff is
closed; subsequent refactoring has its own verification record.

## Scope and contract

Apply the updated shared instructions to application configuration and its machine
and CLI tests without changing accepted settings, secret handling, errors, or
validation order. The existing seven production types remain separate.

## Plan

- Name repeated keys, markers, validation messages, and numeric bounds with
  associated constants; replace abbreviated internal names with plain words.
- Keep production methods within four explicit parameters and retain the current
  public coordination/private decision boundaries.
- Give each previously loop-driven test input its own branch-free asserted case.
  Use fixture helpers for repeated CLI setup and execution.
- Have the root agent run serialized machine and CLI verification.

## Progress

- Read the shared and repository instructions and inspected every scoped file.
- Confirmed production signatures and type boundaries already fit the new limits.
- Production changes complete: associated constants cover repeated setting keys,
  the reference marker, shard limits, credential length, and path diagnostics.
  Internal source, directory, and character iterator names now use plain words.
- Machine tests now contain 26 explicit cases (previously 6); CLI configuration
  tests contain 15 explicit cases (previously 8). All prior table inputs remain
  covered, every case directly asserts outcomes, and no test case branches or
  loops. Shared CLI setup, process execution, and diagnostics use fixture helpers.
- Scoped Rust formatting passed. A repeated-string scan found no repeated string
  literals in the scoped production or test files after introducing fixture
  constants and a shared CLI success diagnostic.
- Reviewed signature sizes, production type boundaries, error precedence, lazy
  secret file access, literal quote handling, file limits, and redaction; no
  external API or file-format changes are intended.
- Machine and CLI runtime verification is delegated to the root agent's
  serialized native verification pass; no worker build or test job was launched.
- The first combined verification caught a test-fixture-only regression: textual
  constant extraction had replaced a quoted value inside the raw JSON override
  fixture. Restored the exact original JSON value; the production parser was
  unchanged. The coordinator will rerun native verification.
- Follow-up tree-sitter Rust audit covered all 10 scoped files and 163 string
  literal nodes: no Rust scoped references remain inside string contents, no
  syntax errors were found, and all 25 raw JSON literal nodes parse successfully.
