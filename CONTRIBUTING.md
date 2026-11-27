# Contributing to BYCKR

BYCKR stores user data directly on disk. Correctness, compatibility,
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

### Development builds of `develop`

Products that use BYCKR (TACHYON, Mesail) test against `develop` before a
release. `scripts/ci/publish-dev-build.mjs` builds the `fylo` executable for
one `develop` commit on the maintainers' build Mac and publishes it:

```bash
bun scripts/ci/publish-dev-build.mjs <develop commit> --gate-log <file> [--contains <commit>]...
```

Name the log of the BYCKR gates that passed for the commit with `--gate-log`,
or pass `--ungated` to publish a commit whose gates have not run; exactly one
is required. `--contains` records an earlier commit whose changes the build
includes (it must be an ancestor of the commit).

It writes one folder per commit, under `$YCKR_DEV_BUILDS_ROOT` (default
`~/Developer/AnvilTargets/fylo-builds`):

```text
<full sha>/macos-arm64/fylo        <full sha>/macos-arm64/fylo.sha256
<full sha>/linux-arm64/fylo        <full sha>/linux-arm64/fylo.sha256
<full sha>/windows-arm64/fylo.exe  <full sha>/windows-arm64/fylo.sha256
<full sha>/manifest.json
```

- Each build is a release-profile `build-executable.mjs` build with build kind
  `development-compiled`, so `fylo version` reports
  `<newest release>-develop.<short sha>` and the exact commit.
- macOS builds natively; Linux builds in an Ubuntu 24.04 container in Colima
  and must need glibc 2.39 or older; Windows builds natively in the ARM64
  test VM. Each build runs as its own `mac-heavy` job (coordinator `fylo`),
  the Linux and Windows ones under their VM's lock.
- `fylo.sha256` uses `shasum`'s format. `manifest.json` records the commit,
  the commits it contains, whether it is gated and the gate log, the build
  time, the version, the Rust compiler, and for each platform the Rust target
  triple, the file's SHA-256 and size (and the glibc version Linux needs). It
  is written last.
- The commit must be on `origin/develop`. All three builds must pass and agree
  on the version and the Rust compiler; otherwise nothing is published. The
  script exits 0 when it published the commit or the commit was already
  published, and 1 on any failure.
- A stopped run removes its temporary folder and lock.
- The folder appears in one rename from a temporary folder, so a reader sees a
  whole build or nothing. A published folder is never replaced: running the
  script again for the same commit does nothing. Only one publisher runs at a
  time (`.publish.lock` in the builds root).
- The builds root is refused on the production volumes HAVEN and ATLAS.

Logs go to `$LOG_ROOT/BYCKR/publish-<time>-<short sha>/`. The script's tests are
`bun test tests/interop/publish-dev-build.test.js`.

### The system-disk opt-out in a checkout

BYCKR refuses a root on the system disk
([ADR 0032](docs/adr/0032-primary-root-on-a-data-disk.md)), and the tests open
their roots in the temp folder, on the system disk. So the repository turns
the opt-out (`YCKR_ALLOW_SYSTEM_DISK=1`) on for its own commands:
`.cargo/config.toml` for every cargo command, `bunfig.toml` (through
`tests/setup/allow-system-disk.js`) for every bun command,
`playwright.config.mjs` for the browser tests, and the GitHub workflows for
CI. This includes `cargo run` of the `fylo` server in a checkout, which then
prints `BYCKR warning: system-disk check disabled by YCKR_ALLOW_SYSTEM_DISK=1`
on stderr. Run a real server from a built release binary, not from
`cargo run` in a checkout. Tests of the rule itself start `fylo` with the
variable removed (`crates/fylo-cli/tests/system_disk.rs`). In Bun, a value the
preload file sets does not reach `Bun.spawn` unless the call passes
`env: { ...process.env }`, so a test that starts `fylo` (or a client that
starts it) passes its environment that way.

Browser/Wasm changes also run `bun run build:web:wasm` and the compiled-module
browser corpus. Transaction, platform, backup, and release changes require
their dedicated qualification profiles.

### Test roots on the macOS test image

On the maintainers' Mac, tests open their roots in BYCKR's macOS test image,
not on the system disk ([ADR 0033](docs/adr/0033-primary-layout-on-a-data-drive.md)
section 12). Run a test command through the image helper, inside a
`mac-heavy` or jobq job:

```bash
scripts/ci/test-image.sh run --owner <name> -- cargo nextest run --workspace --all-targets --all-features --locked
```

- `run` attaches the image when it is not attached
  (`/Volumes/HANGAR/fylo-test/fylo-macos-test.sparsebundle`, at
  `~/Developer/FyloTestImage`; it is made once, 8 GiB maximum). It gives the
  command its own folder `<mount>/roots/<owner>-<pid>` as `YCKR_TEST_ROOT`
  and removes the folder when the command ends.
- Rust tests take their folder from `fylo_test_support::TestFolder::path()`,
  bun tests from `testFolder()` in `tests/helpers/root.js`. Without
  `YCKR_TEST_ROOT` (GitHub CI, the VMs, a laptop) both use the OS temp
  folder. `TMPDIR` never changes. Files that are not roots, such as the
  container lease markers and the temp tier, stay in the OS temp folder.
- There is no fallback. When the image cannot be used, the helper prints
  `BYCKR test image not attached: <reason>` and exits 1.
- `scripts/ci/test-image.sh status` shows the image; `detach` works only when
  no job and no process uses it; `compact` (only on a detached image) gives
  freed space back to the HANGAR SSD.

### Running the tests in a container

The tests keep their databases, and the lease's host markers, under the
temporary directory (`TMPDIR`). The lease and recovery tests need byte-range
locks there, which BYCKR trusts only on the filesystems listed in
[the lock qualification](docs/operations/vdbx-byte-range-lock-qualification.md)
(ext4, tmpfs, APFS and NTFS). A container's `/tmp` is usually overlayfs, where
BYCKR falls back to lease files, so those tests stop with "this test needs
byte-range locks". Point `TMPDIR` at a volume instead (a named Docker volume is
usually ext4 on a Linux engine), and turn core dumps off, because the crash tests abort
child processes on purpose and each abort would otherwise leave a `core` file
in its working directory:

```bash
docker run --ulimit core=0 -v fylo-tmp:/fylo-tmp -e TMPDIR=/fylo-tmp ...
```

In a shell already inside the container, `ulimit -c 0` and
`export TMPDIR=<a directory on the volume>` do the same.

### Test disks

BYCKR's tests open their roots under `YCKR_TEST_ROOT` when it is set
([ADR 0033](docs/adr/0033-primary-layout-on-a-data-drive.md) section 12): a
job folder on a test disk that is not the boot disk, so the drive rule is
tested for real. Each system has its own disk, and a disk image is never
attached to two systems at once. Every script below makes the job folder
`<disk>/roots/<owner>-<host>-<pid>`, checks it with a write probe and 1 GiB of
free space, removes job folders older than 24 hours whose process is gone,
and removes its own job folder on every exit path. When the disk cannot be
used it stops with `BYCKR test disk not ready: <reason>` and never falls back
to another folder.

| Where | Test disk | Script |
| ----- | --------- | ------ |
| The build Mac, macOS lane | sparse APFS image on the HANGAR SSD | `scripts/ci/test-image.sh run -- <command>` |
| The build Mac, Linux lane (Colima) | the Docker volume `fylo-test-volume` at `/data`, on Colima's data disk | `scripts/ci/linux-lane.sh -- <command>` inside `mac-heavy --vm=colima` |
| The build Mac, windows-vm | the reusable raw image `/Volumes/HANGAR/fylo-test/windows-vm.raw` (made by Platform), attached only by `testvm` as the VM's second NVMe disk (serial `TEST-DISK`), formatted NTFS on first use, drive `T:` | `scripts/ci/windows-lane.sh -- <script.ps1>` inside `mac-heavy --vm=windows` |
| Laptops and GitHub runners (Linux, macOS) | a sparse ext4 or APFS image the script makes on a disk that is not the boot disk (GitHub Linux: `/mnt`) | `scripts/ci/test-disk.sh make --folder <dir> --mount <dir>`, then `scripts/ci/test-disk.sh run --mount <dir> -- <command>` |
| Laptops and GitHub runners (Windows) | a dynamically expanding VHDX on a disk other than the Windows disk (GitHub: `D:`) | `scripts/ci/test-disk.ps1 -Action Make -Folder D:\fylo-test -Letter T`, then `-Action Run -Letter T -Script <file.ps1>` |

`make` refuses a folder on the boot disk, on HAVEN or ATLAS (production
volumes), and the build Mac's lane folders (`/Volumes/HANGAR/fylo-test`,
`/Volumes/HANGAR/test-disks`). On Linux it needs `sudo` for the loop mount.
`detach --mount <dir> --folder <dir> --delete` (`-Action Detach -Folder
<dir> -Delete` on Windows) removes the image when it is no longer needed.

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

BYCKR is MIT licensed. Unless stated otherwise, intentional contributions are
submitted under the same license. The project does not currently require a
separate CLA.
