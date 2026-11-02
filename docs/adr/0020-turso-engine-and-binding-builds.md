# ADR 0020: Turso's engine and binding builds under the dependency and unsafe policy

- Status: **Proposed** (draft for the user, 2026-11-01)
- Date: **2026-11-01**
- Owners: **FYLO maintainers**
- Amends: ADR 0004 (unsafe code and dependencies). Follows ADR 0019
  (proposed), "FYLO through Turso's engine".

## Context

ADR 0019 builds FYLO's SQL layer on Turso's engine, `turso_core`, and ships
FYLO-built copies of Turso's own language-binding libraries (`fylo link`).
ADR 0004 forbids `unsafe` in FYLO's workspace and asks every dependency for
a documented purpose, features, license and posture. Turso's engine and its
binding crates contain `unsafe` code that FYLO did not write and will not
rewrite: a binding is by nature a foreign-function boundary.

## Decision

### 1. FYLO's own crates stay `unsafe`-free

- `unsafe_code = "forbid"` stays the workspace policy, and nothing in this
  ADR lowers it for a FYLO crate.
- FYLO reaches Turso only through safe Rust interfaces: internal virtual
  tables (`InternalVirtualTable`) rather than the `turso_ext` C extension
  interface, and tables rather than C-registered scalar functions.

### 2. `turso_core` is a vendored, pinned third-party dependency

- `vendor/turso_core` holds `turso_core` at the pinned release, from crates.io
  with its SHA-256 checked, plus FYLO's patch
  (`vendor/turso_core-<version>-fylo.patch`, behind a `fylo` feature).
  `scripts/vendor-turso-core.sh --check` proves the tree equals the release
  plus the patch.
- It is excluded from FYLO's workspace: FYLO's lints, tests and unsafe
  inventory do not cover it, and its own `unsafe` (649 occurrences of the
  keyword at 0.8.3) stays its upstream's responsibility. It is optional,
  built only by the `fylo` crate's `sql` feature.
- FYLO's patch adds no `unsafe` code.
- It must use FYLO's feature set for `regex` and `regex-syntax`, because
  Cargo unifies features and Turso's defaults widened CHEX's dialect.

### 3. Binding builds are a separate workspace with upstream `unsafe`

- `bindings/turso/build.sh` builds Turso's binding crates in Turso's own
  repository at the pinned tag, never in FYLO's workspace, against the same
  vendored `turso_core` and the `fylo` crate. Its patch
  (`bindings/turso/turso-<version>-bindings.patch`) adds two safe calls and
  two dependencies; it adds no `unsafe`.
- Those crates keep their upstream `unsafe` code, which is the C ABI or
  N-API boundary each SDK loads. At 0.8.3 the crates built there contain
  these occurrences of `unsafe`, beyond `turso_core` and third-party crates:

  | Crate | Role | `unsafe` | `extern "C"` |
  |---|---|---|---|
  | `turso_node` | the JavaScript addon (N-API through napi-rs) | 17 | 1 |
  | `turso_sdk_kit` | Turso's C ABI | 248 | 115 |
  | `turso_sync_sdk_kit` | the C ABI tursogo loads | 132 | 58 |
  | `turso_sdk_kit_macros` | ABI macros | 1 | 1 |
  | `turso_sync_engine` | sync engine | 8 | 0 |
  | `turso_ext` | extension ABI types | 120 | 49 |
  | `turso_macros` | engine macros | 56 | 42 |
  | `turso_parser` | SQL parser | 8 | 0 |

- FYLO does not add these to `docs/security/unsafe-inventory.md`: they are
  third-party, reviewed upstream, and pinned by commit. FYLO owns the build,
  the provenance and the conformance of the artifacts, not their `unsafe`.
- The binding workspace commits its `Cargo.lock`
  (`bindings/turso/Cargo.lock`), and `cargo deny` runs on it as on FYLO's.

### 4. What the user must accept

- **Exact pin:** one Turso version per FYLO release, moved deliberately; both
  patches rebase with it.
- **Third-party `unsafe` in shipped artifacts:** the `fylo link` libraries
  contain Turso's and napi-rs's `unsafe` code, as stock Turso SDKs do.
- **Feature unification inside artifacts:** Turso's JavaScript addon enables
  full-text search, whose `tantivy` turns on `regex`'s full `unicode` feature
  for that whole artifact, CHEX included: inside the addon CHEX also accepts
  Unicode property classes (`\p{…}`) that FYLO's CLI rejects. Schemas are
  pushed through the CLI, so this changes no stored schema; dropping
  full-text search would instead change stock behaviour for plain database
  paths.

## Consequences

- ADR 0004's dependency checklist applies to `turso_core` and the binding
  workspace's graph as to any direct dependency; ADR 0019 lists the licenses
  (all permissive, `turso_*` MIT).
- A Turso security fix means moving the pin: rebase the patches, rebuild,
  rerun the crash matrix and the `fylo link` conformance suite.
- Release provenance for the binding artifacts records the Turso commit, the
  FYLO commit and the patches (`fylo-link.json` beside each build).
