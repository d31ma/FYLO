# Resumed verification and filename alignment

The resumed pass completed the pending native workspace suite, strict Clippy,
typechecking, formatting, and all 126 browser tests (391 assertions). Two
test-only Clippy defects were fixed without suppressions: a complex numeric
sample return type and an unchecked size conversion. Two FSA interop tests now
resolve the native executable using `CargoArtifacts`, honoring `CARGO_TARGET_DIR`
while preserving `FYLO_BINARY` overrides and ordinary-platform defaults.

The new filename rule is applied to the previously extracted production types:
`EqualityPlan`, `ApplicationConfig`, `IndexCache`, `BrowserPath`,
`BrowserMetadataStore`, `BrowserEventBus`, and `BrowserEventStream`. Module
facades preserve their reexports; internal consumers move with the files.
Browser test sample classes move into class-named helper files without changing
test discovery or assertions. This is a bounded slice, not a claim that every
remaining source or test filename in the repository already complies.

Final combined validation of this slice and the queue/schema slices passed:
288 native tests, strict workspace Clippy, 126 browser tests / 391 assertions,
typecheck, Rustdoc with warnings denied, Rustfmt, scoped Prettier, protocol
contracts, and the six registered MC/DC decisions. See the main cohesion record
for scope and caveats.
Public SDK APIs, protocol keys, persistence formats, and runtime behavior must
remain unchanged. All work remains local and uncommitted.

## Scoped quality evidence

After the filename/import edits, typecheck, all 126 browser tests / 391 assertions,
and scoped Prettier passed. A fresh LCOV browser run was evaluated by the existing
`scripts/crap-metric.mjs` with a threshold of 15. Across the five scoped production
classes (path, metadata store/record, event bus/stream), its 83 function records
(including nested callbacks) had maximum CRAP 4 and no violations. The same
repository-wide report flagged 36 functions outside this scope; its overall
exit status was failure. This is scoped evidence, not a repository-wide pass
and not Rust coverage certification.

A lightweight Rust AST check of the 14 queue record files and the three renamed
native types found no explicit structural violations across 17 files / 26
methods. It checks syntax, one type, matching filenames, module-level values,
parameter count, and explicit control flow; it is not a full macro or coverage
analysis. Strict storage Clippy passed after preserving the `cache` module alias
with `#[path]` to avoid Rust's module-inception lint without suppression.

The contract verification script still scanned only the format crate's old
monolithic `lib.rs`, so it incorrectly reported `EFORMAT_SIZE` missing. Its
format-error input now targets `format_error_code.rs`; all existing contract
assertions are retained. The verifier passes for 45 machine operations, 60
stable errors, and the version-1 format fixtures.
