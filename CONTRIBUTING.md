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
