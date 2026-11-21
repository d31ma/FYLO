# Contributing to FYLO

FYLO stores user data directly on disk. Correctness, compatibility,
recoverability, and bounded resource use take priority over feature volume.

## Prerequisites

- Bun from `.bun-version`;
- Rust from `rust-toolchain.toml`;
- for browser engine builds, binaryen's `wasm-opt` from `.binaryen-version`
  (`brew install binaryen`, or `sh scripts/install-binaryen.sh`, which checks
  a pinned digest);
- Git;
- a local test root outside synchronized production data.

## Local gate

Run the smallest relevant test first, then:

```bash
bun install --frozen-lockfile
bun run contracts:verify
bun run query:verify
bun run predicate:verify
bun run sql:verify
bun run rust:fmt
bun run rust:clippy
bun run rust:clippy:wasm
bun run rust:test
bun run rust:deny
bun run rust:interop:readonly
bun run typecheck
bun run test
bun run test:interop
```

### Targeted merges: only the affected crates

A merge into `develop` gates only the crates the change touches, plus every
workspace crate that depends on them. The full workspace runs in the nightly
run on `develop` and in the release gates. To get the crate list:

```bash
bun scripts/ci/affected-crates.mjs --check
```

It compares the branch, including uncommitted and untracked files, with its
merge base on `origin/develop` (`--base <ref>` picks another base) and prints
one of:

- the crate names, one per line;
- `none`: no Rust crate changed, so skip the cargo gates;
- `full`: run the full workspace. This happens when a `Cargo.toml`,
  `Cargo.lock`, `build.rs`, `rust-toolchain.toml`, `.cargo/`, `.config/`
  (nextest), `deny.toml`, `rustfmt.toml` or the shared test harness
  `crates/fylo-test-support/` changed, or when a changed file outside every
  crate affects Rust (a `.rs` file, `VERSION`, `tests/fixtures/`, `vendor/`,
  `fuzz/`). Stderr names each file that caused it.

`--check` also runs `cargo tree --invert` for each changed crate and fails when
its list differs from the script's. `--args` prints the same choice as cargo
flags (`-p a -p b`, or `--workspace` for `full`), for example:

```bash
bun ./scripts/run-rust.mjs cargo clippy $(bun scripts/ci/affected-crates.mjs --args) --all-targets --all-features --locked -- -D warnings
bun ./scripts/run-rust.mjs cargo nextest run $(bun scripts/ci/affected-crates.mjs --args) --all-targets --all-features --locked
bun ./scripts/run-rust.mjs cargo doc $(bun scripts/ci/affected-crates.mjs --args) --all-features --no-deps --locked
```

Leave the command output unquoted, so the shell passes each flag on its own.
Run `cargo fmt --all --check` as before. Name the crate list in the merge
report so anyone can rerun the same gates. The script's tests are
`bun test tests/interop/affected-crates.test.js`.

### The system-disk opt-out in a checkout

FYLO refuses a root on the system disk
([ADR 0032](docs/adr/0032-primary-root-on-a-data-disk.md)), and the tests open
their roots in the temp folder, on the system disk. So the repository turns
the opt-out (`FYLO_ALLOW_SYSTEM_DISK=1`) on for its own commands:
`.cargo/config.toml` for every cargo command, `bunfig.toml` (through
`tests/setup/allow-system-disk.js`) for every bun command,
`playwright.config.mjs` for the browser tests, and the GitHub workflows for
CI. This includes `cargo run` of the `fylo` server in a checkout, which then
prints `FYLO warning: system-disk check disabled by FYLO_ALLOW_SYSTEM_DISK=1`
on stderr. Run a real server from a built release binary, not from
`cargo run` in a checkout. Tests of the rule itself start `fylo` with the
variable removed (`crates/fylo-cli/tests/system_disk.rs`). In Bun, a value the
preload file sets does not reach `Bun.spawn` unless the call passes
`env: { ...process.env }`, so a test that starts `fylo` (or a client that
starts it) passes its environment that way.

Browser/Wasm changes also run `bun run build:web:wasm` and the compiled-module
browser corpus. Transaction, platform, backup, and release changes require
their dedicated qualification profiles.

### Running the tests in a container

The tests keep their databases, and the lease's host markers, under the
temporary directory (`TMPDIR`). The lease and recovery tests need byte-range
locks there, which FYLO trusts only on the filesystems listed in
[the lock qualification](docs/operations/vdbx-byte-range-lock-qualification.md)
(ext4, tmpfs, APFS and NTFS). A container's `/tmp` is usually overlayfs, where
FYLO falls back to lease files, so those tests stop with "this test needs
byte-range locks". Point `TMPDIR` at a volume instead (a named Docker volume is
usually ext4 on a Linux engine), and turn core dumps off, because the crash tests abort
child processes on purpose and each abort would otherwise leave a `core` file
in its working directory:

```bash
docker run --ulimit core=0 -v fylo-tmp:/fylo-tmp -e TMPDIR=/fylo-tmp ...
```

In a shell already inside the container, `ulimit -c 0` and
`export TMPDIR=<a directory on the volume>` do the same.

## Change requirements

- Start behavior changes with an observable failing test.
- Keep changes vertical across contracts, implementation, negative paths,
  documentation, compatibility, and rollback.
- Do not add empty crates or speculative public abstractions.
- Preserve stored bytes and public behavior unless an accepted ADR/RFC defines
  the migration and downgrade path.
- Update schemas, fixtures, CLI/machine behavior, clients, and docs together.
- Add failpoints and crash/recovery evidence around durability changes.
- Add threat-model coverage for paths, permissions, encryption, credentials,
  object storage, protocol frames, subprocesses, and supply chain.
- Never weaken or skip an integrity/security test merely to make CI pass.

## Pull requests

Describe:

1. the invariant and behavior changed;
2. failure modes and compatibility impact;
3. tests, filesystems, browsers, and operating systems used;
4. migration, operations, release, and rollback implications.

Generated benchmark output, build products, fuzz artifacts, coverage, and test
roots stay uncommitted. Retain qualifying evidence in CI/release artifacts.

## Licensing

FYLO is MIT licensed. Unless stated otherwise, intentional contributions are
submitted under the same license. The project does not currently require a
separate CLA.
