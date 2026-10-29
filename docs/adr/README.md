# FYLO Architecture Decision Records

Architecture Decision Records capture decisions that are expensive, risky, or
incompatible to reverse. They explain why a decision was made, the constraints
it protects, and the evidence required to keep it.

## Status vocabulary

- **Proposed**: open for review and not yet authoritative.
- **Accepted**: authoritative for new work.
- **Superseded**: replaced by a later ADR; retained for historical context.
- **Rejected**: considered but not selected.
- **Withdrawn**: accepted, then reversed and removed; retained for historical
  context.
- **Deprecated**: still present during a documented migration window.

Accepted ADRs are immutable except for status, supersession links, and
corrections that do not change the decision. A material change requires a new
ADR.

## Index

| ADR                                                         | Status   | Decision                                    |
| ----------------------------------------------------------- | -------- | ------------------------------------------- |
| [0001](0001-rust-native-engine-and-portable-wasm-kernel.md) | Accepted | Rust native engine and portable Wasm kernel |
| [0002](0002-compatibility-first-strangler-migration.md)     | Accepted | Compatibility-first strangler migration     |
| [0003](0003-native-and-browser-storage-boundaries.md)       | Accepted | Native and browser storage boundaries       |
| [0004](0004-unsafe-and-dependency-policy.md)                | Accepted | Unsafe-code and dependency policy           |
| [0005](0005-narrow-wasm-linear-memory-boundary.md)          | Accepted | Narrow Wasm linear-memory boundary          |
| [0006](0006-shard-records-by-the-trailing-creation-characters.md) | Accepted | Trailing-creation-character sharding   |
| [0007](0007-filesystem-only-native-storage.md)              | Accepted | Filesystem-only native storage              |
| [0008](0008-fylo-wasm-browser-and-mobile-engine.md)         | Accepted | Portable full-engine Wasm artifact          |
| [0009](0009-embedded-serverless-queue.md)                   | Accepted | Embedded durable serverless queue           |
| [0012](0012-opt-in-network-server.md)                       | Withdrawn | Opt-in network server for one root (removed; FYLO is embedded) |
| [0013](0013-collection-write-ahead-log.md)                  | Accepted | Collection write-ahead log with group commit |
| [0014](0014-in-memory-hot-set.md)                           | Accepted | In-memory hot set for exclusive embedded sessions |
| [0016](0016-single-file-fxdb-database.md)                   | Accepted | Single-file `.fxdb` databases               |
| [0017](0017-schemas-in-the-database.md)                     | Accepted | Schemas in the database, validated at the commit |
| [0018](0018-json-only-schema-migrations.md)                 | Accepted | JSON-only schema migrations                 |

## When another ADR is required

Create an ADR before changing:

- the authoritative storage medium;
- disk, transaction, index, backup, or machine-protocol formats;
- durability or recovery guarantees;
- native/browser responsibility boundaries;
- multi-writer or distributed-coordination assumptions;
- cryptographic or key-custody boundaries;
- unsafe-code policy;
- supported platform or release-evidence policy;
- whether Rust crates become stable public APIs.

Public behavior also requires an RFC with compatibility, migration, security,
operations, and rollback analysis.
