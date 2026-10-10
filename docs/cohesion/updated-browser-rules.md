# Updated browser rules

## Scope and acceptance

Apply the current shared rules to browser paths, metadata validation tests, and
the event journal/subscription service. Keep path behavior, journal bytes,
subscription timing, event order, cleanup, and public interfaces unchanged.
Keep generated/vendor files and all unrelated dirty work untouched.

## Plan

1. Establish event-journal behavior tests before moving code.
2. Isolate stream lifecycle in one type per file, with coordinating public
   methods and small private helpers. Name repeated path constants.
3. Split table loops in path/metadata tests into independent test cases.
4. Run focused browser checks, then root-coordinated combined verification.

## Progress

- Read the updated shared instructions and existing Graphify context.
- Two workers own disjoint native cache/lease and configuration slices.
- Other projects have heavy jobs on this host; heavy FYLO verification will
  use the existing shared machine queue rather than overlap those jobs.
- No commit, push, release, dependency install, or background soak is in scope.
- Four event tests passed against the original service before moving lifecycle
  handling to `BrowserEventStream`. Seven event cases now cover publication,
  buffering, lazy subscription, append/serialization failure, consumer failure,
  and resumed waiting.
- Path and metadata loop inputs are independent `test.each` cases. Test samples
  use associated constants, and filesystem locals use their full names.
- Focused verification: 57 tests / 103 assertions passed; TypeScript and scoped
  formatting passed. Structural inspection passed for four production browser
  classes / 55 methods. A second agent re-reviewed the corrections cleanly.
- Added an invalid-input-object regression to preserve string conversion in
  unsafe-path diagnostics while naming the shared message prefix.
- The user requested handoff with full verification pending. The queued rerun
  was cancelled before acquiring the shared build lock. See
  `docs/COHESION_REFACTOR.md` for the remaining verification commands.
- Resumed verification passed all 126 browser tests / 391 assertions after
  correcting two native-binary artifact paths in FSA interop tests. Typechecking
  and scoped Prettier checks passed. The newer filename/import alignment also
  passed those browser and typechecking gates.
