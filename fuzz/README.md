# FYLO Rust Fuzzing

Install the pinned Rust nightly selected by release engineering and
`cargo-fuzz`, then run:

```bash
cargo fuzz run query_snapshot -- -max_len=2097152
cargo fuzz run portable_inputs -- -max_len=1048576
cargo fuzz run container_structures -- -max_len=8192
cargo fuzz run container_codec -- -max_len=16384 -malloc_limit_mb=64
cargo fuzz run container_cipher -- -max_len=4096
cargo fuzz run container_file -- -max_len=4096
```

The target sends arbitrary snapshot and query bytes through the same bounded
safe kernel used by the browser Wasm adapter. The `container_*` targets treat
a `.vdbx` file as untrusted input, the way any reader of a SQLite file must:
`container_structures` decodes each on-disk structure, `container_codec`
decodes and round-trips record bodies under a 64 KiB limit with and without a
dictionary, `container_cipher` opens and round-trips encrypted units, and
`container_file` opens, inspects and reads raw or patched containers, with
checksums recomputed when the first input byte asks for it. Crashes, panics, hangs, and
sanitizer findings are release-blocking. Minimized regressions belong in the
versioned fixture corpus. The scheduled `rust-nightly.yml` workflow runs both
targets with pinned tooling and retains the corpus, exact toolchain identity,
and any findings for 90 days.
