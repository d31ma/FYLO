# MC/DC qualification

Fylo applies unique-cause modified condition/decision coverage (MC/DC) to a
registered set of safety-critical Rust decisions. For every atomic condition,
the proof must contain two cases where only that condition changes and the
overall decision changes with it.

Run the gate locally with:

```sh
bun run rust:mcdc
```

The command validates `tests/mcdc/decisions.json`, confirms every registered
source marker and compiled proof test still exists, runs the proofs, rejects
unregistered `mcdc_` tests or missing reports, and independently rechecks every
condition pair in JavaScript. It writes the retained aggregate evidence to
`target/mcdc/report.json`.

## Current qualified scope

| Safety boundary      | Qualified decision                                             | Conditions |
| -------------------- | -------------------------------------------------------------- | ---------: |
| Authorization        | Public, owner, group, and other read paths                     |          4 |
| Concurrency          | Stable state and unchanged generation after a read             |          2 |
| Network import       | Supported private and special-use IPv4 classes                 |          7 |
| Queue recovery       | Stored-message format, identity, topic, and timestamp ordering |          4 |
| Queue leasing        | Receipt validity and attempt bounds                            |          4 |
| Transaction recovery | Manifest format, identity, operation, and capture invariants   |          6 |

The gate currently proves 27 independently effective conditions across six
decisions using 33 evaluated cases. The matrices execute the production
predicates and emit their actual condition vectors; they do not test a copied
boolean expression.

## Comparison with SQLite

SQLite publishes a stronger and much broader standard: its proprietary TH3
harness provides [100% branch and MC/DC coverage of its core][sqlite-testing],
and its coverage process uses `testcase()` instrumentation to expose condition
outcomes that branch coverage alone can miss. SQLite also rebuilds and reruns
the delivered configuration after coverage-specific instrumentation.

Fylo does not yet match SQLite. This gate is explicit matrix evidence for a
small safety-critical slice, while Fylo's LLVM coverage ratchet separately
measures whole-workspace line, function, and region execution. Rustc's
[documented source-coverage summary][rust-coverage] reports function,
instantiation, line, and region statistics, not an MC/DC statistic, so Fylo
uses executable unique-cause matrices and retained condition-pair evidence
rather than relabeling region coverage as MC/DC.

| Capability                      | SQLite published position                     | Fylo position after this gate                            |
| ------------------------------- | --------------------------------------------- | -------------------------------------------------------- |
| MC/DC scope                     | 100% of the core SQLite library               | Six registered safety-critical decisions                 |
| Criterion                       | Condition outcomes plus independent influence | Unique-cause independent influence                       |
| Evidence                        | TH3 plus coverage-build instrumentation       | Public manifest, Rust matrices, and retained JSON report |
| Whole-core claim                | Yes                                           | No                                                       |
| Ordinary branch/region coverage | Separate branch measurement                   | Separate LLVM line/function/region ratchet               |

The next ratchet is to inventory every compound decision in `fylo-format`,
`fylo-query`, `fylo-engine`, `fylo-storage-native`, and `fylo-machine`; either
register an independence proof or record a reviewed single-condition,
unreachable, or defensive-code disposition. Whole-core parity with SQLite can
be claimed only when that inventory is complete and the ordinary branch gate
also reaches 100% for the defined core configuration.

[sqlite-testing]: https://www.sqlite.org/testing.html#mcdc
[rust-coverage]: https://doc.rust-lang.org/rustc/instrument-coverage.html#interpreting-reports
