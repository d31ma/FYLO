# `.vdbx` byte-range lock qualification

ADR 0016 section 7 lets several local processes share one `.vdbx` file through
OS byte-range locks, and only on a filesystem where the multi-process harness
has passed. FYLO is embedded, like SQLite, so nothing here relies on a server
process. On any other filesystem, including one that refuses byte-range locks
(SMB without lock support, FAT, exFAT, some FUSE filesystems), a `.vdbx` still
works under the lock-free lease (`ContainerLease`): one writer at a time,
readers never blocked. This page records the harness, how to run it, and which
filesystems have passed.

## Choosing locks or the lock-free lease

At open, FYLO checks two things, both in `fylo-storage-native`:

- `FilesystemInfo::of(file)` names the filesystem (`fstatfs` on Linux and
  macOS, `GetVolumeInformationByHandleW` and the remote-protocol query on
  Windows) and says whether it is remote.
  `byte_range_locks_qualified()` is true only for a filesystem with a passed
  row in the table below. Linux FUSE and unknown filesystems count as remote.
- `ByteRangeLockFile::probe()` takes and releases a reserved byte (2⁶² + 2).
  Any failure counts as "no locks".

| Qualified | Probe | What FYLO uses                         | Harness reports |
| --------- | ----- | -------------------------------------- | --------------- |
| yes       | yes   | byte-range locks, many writers         | qualified       |
| no        | yes   | lock-free lease, one writer at a time  | unqualified     |
| either    | no    | lock-free lease, one writer at a time  | lease fallback  |

Under the lock-free lease, the writing process calls
`ByteRangeLockFile::set_process_local(true)`, so its threads still exclude
each other on the writer and allocation bytes through the registry, with no
OS lock call.

## The locks

`fylo_storage_native::ByteRangeLockFile` takes one byte, shared or exclusive,
with a non-waiting `try_lock` or a waiting `lock(byte, mode, timeout)`.
`open(path)` opens the file for reading and writing; `open_read_only(path)`
works on a read-only file or mount too, shares the process's existing handle
when there is one, and may take only the lease byte, shared.

| Byte                       | Offset                          | Use                                                          |
| -------------------------- | ------------------------------- | ------------------------------------------------------------ |
| `LockByte::LEASE`          | 2⁶²                             | database lease: every process holds it shared while the file is open; `compact --shrink` and an in-place `pack --sealed` take it exclusive |
| `LockByte::ALLOCATION`     | 2⁶² + 1                         | taking a free extent or growing the file                     |
| probe                      | 2⁶² + 2                         | taken and released by `probe()`                              |
| reserved                   | 2⁶² + 3 to 2⁶² + 4095           | further database-wide locks                                  |
| `collection_writer(block)` | 2⁶² + 4096 + directory block    | one writer per collection                                    |

All of them lie far beyond any data offset, and taking them does not extend
the file.

| Platform    | Call                                         | Lock owner                     |
| ----------- | -------------------------------------------- | ------------------------------ |
| Linux       | `fcntl(F_OFD_SETLK)`                         | the open file description      |
| macOS       | `fcntl(F_SETLK)`                             | the process                    |
| Windows     | `LockFileEx(LOCKFILE_FAIL_IMMEDIATELY)`      | the handle                     |

Inside one process a registry decides, so the three behave the same:

- every `ByteRangeLockFile` for one file in a process shares one OS handle,
  found by device and file index;
- the registry counts holders per byte, so two threads, or two handles, in one
  process conflict as two processes do. On macOS the kernel alone would grant
  the second request, because the process already owns the byte;
- a duplicate descriptor opened while the file is locked is closed only once
  the process holds no lock on it, because on macOS closing any descriptor for
  the file drops every lock the process holds on it, as SQLite also guards
  against.

**Do all I/O on a locked `.vdbx` through `ByteRangeLockFile::file()`.** On
macOS, opening and closing the file by path anywhere else in the process
releases its locks. The harness reports this case for each platform.

Waiting polls the non-waiting request with a backoff from 20 µs to 1 ms and
wakes at once on a release inside the process. A blocking kernel wait
(`F_OFD_SETLKW`, `F_SETLKW`, a blocking `LockFileEx`) has no timeout, and a
thread parked in it would hold a request the registry cannot see. The cost is
fairness: under heavy contention a waiter can lose several handoffs in a row
(the longest wait in the Linux load runs below was under 100 ms, and 6.0 s on
Windows, with no request reaching the harness's 10 s timeout).

The `unsafe` calls are listed in `docs/security/unsafe-inventory.md`.

### Dependencies

ADR 0004 asks each new direct dependency to be documented. Both crates were
already in `Cargo.lock`, so the graph gains no crate.

| Crate                | Owner                 | Features                                                         | Why                                                                                   | License             | Target impact                          |
| -------------------- | --------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------- | ------------------- | -------------------------------------- |
| `libc` 0.2.189       | `fylo-storage-native` | none (`default-features = false`)                                | `struct flock` and the `F_OFD_*` commands differ per target; hand-written declarations would get one wrong | MIT or Apache-2.0   | Unix only; already linked via `errno`, `getrandom`, `rustix` |
| `rustix` 1.1.4 (now on all Unix, was Apple only) | `fylo-storage-native` | `fs` | safe `fstatfs` for `FilesystemInfo` | Apache-2.0 WITH LLVM-exception, Apache-2.0 or MIT | already linked |
| `windows-sys` 0.61.2 | `fylo-storage-native` | `Win32_Foundation`, `Win32_Storage_FileSystem`, `Win32_System_IO` | `LockFileEx`, `UnlockFileEx`, `GetFileInformationByHandle`                            | MIT or Apache-2.0   | Windows only; already linked via `errno`, `tokio` |

Removal path: `std` offers only whole-file locks (`File::lock`), and `rustix`
only whole-file `fcntl` locks. If either gains byte ranges, the module can drop
its `unsafe` and both direct dependencies.

## The harness

`fylo-lock-harness` is a binary of `fylo-storage-native`.

```sh
cargo build --release -p fylo-storage-native --bin fylo-lock-harness
target/release/fylo-lock-harness qualify --dir <directory on the filesystem> \
    --procs 8 --threads 2 --seconds 10 --label "<filesystem>" --json result.json
```

It creates a 32 KiB scratch file in the directory and runs:

1. **Beyond the end.** Takes the lease, allocation and writer bytes 0 and
   2³²−1, and checks that the file's length and data are unchanged.
2. **Conflict matrix.** A child process holds a byte; the harness checks that
   a shared lease admits another shared holder and refuses an exclusive one,
   that an exclusive lease refuses both, that allocation and writer 5 are
   refused while writer 6 and the lease stay free, and that a waiting request
   is granted once the child releases.
3. **Release.** A child holding lease, allocation and writer 0 exits normally,
   then another is killed (`SIGKILL`, `TerminateProcess`); the harness times
   how long until all three bytes are free.
4. **Read-only.** A file without write permission opens with
   `open_read_only`, reads, and takes the lease shared; a read-only process's
   shared lease refuses an exclusive request from another process.
5. **Close safety.** A holder that opens and drops a second
   `ByteRangeLockFile`, writable or read-only, keeps its locks (must pass). A holder that opens and
   closes the path with plain `std::fs` is reported, not judged.
6. **Control.** Two seconds of the load below with the locks switched off. It
   must show overlaps or lost updates, which proves the witness can see one on
   this filesystem.
7. **Load.** `procs` processes of `threads` threads, each thread with its own
   handle, for `seconds`. Each round takes the lease shared (30 %) or exclusive
   (5 %), the allocation byte (15 %) or a random writer byte (50 %), by
   `try_lock` a quarter of the time and by a waiting `lock` otherwise. An
   exclusive holder checks that an owner word in the file's data is free on
   entry and still its own on exit, and increments a counter beside it; a lease
   holder also checks that no shared holder's flag is set. Afterwards each
   counter must equal the number of entries the holders reported, so an
   overlap or a lost update fails the run.

The exit status is 1 when any check fails. `cargo test -p fylo-storage-native`
runs a two-second version.

Across machines (spike 2), every machine runs part of the load on one shared
file, with `--sync` so each holder flushes its data before releasing, and
`--start-at` so they start together:

```sh
fylo-lock-harness prepare --file /Volumes/ATLAS/locktest.vdbx    # once
fylo-lock-harness share --file /Volumes/ATLAS/locktest.vdbx --machines 2 --machine 0 \
    --start-at 1793000000 --sync --json 0.json                   # machine 0
fylo-lock-harness share --file <same file> --machines 2 --machine 1 \
    --start-at 1793000000 --sync --json 1.json                   # machine 1
fylo-lock-harness verify --file /Volumes/ATLAS/locktest.vdbx 0.json 1.json
```

`verify` sums every machine's entries and compares them with the counters in
the file.

On the local NFS 4.1 server, `bun run vdbx:locks:nfs` builds the Linux harness
in Docker, starts the same kernel nfsd as `bun run efs:local`, runs `qualify`
from one client, and then `share` from two clients against one file. Pass
`--cache-dir` to keep the build cache off the Docker disk image (a cache on a
slow network share has broken builds; Docker's own volumes are the safer
default), `--export ext4` to serve a loop-mounted ext4 image instead of tmpfs,
and `--repeat N` to run the two-client load N times. With `bun run`, pass the
flags without a `--`.

## Qualification

| Filesystem                          | Machine                 | Date       | Result  | Kill release | Load (8 × 2, 10 s)                         | Path open+close drops locks |
| ----------------------------------- | ----------------------- | ---------- | ------- | ------------ | ------------------------------------------ | --------------------------- |
| ext4, Linux 6.18 (x86_64)           | cloud container         | 2026-10-29 | passed  | 1.1 ms       | 159,090 exclusive entries, 0 overlaps; control caught 246,052 | no                          |
| tmpfs, Linux 6.18 (x86_64)          | cloud container         | 2026-10-29 | passed  | 1.2 ms       | 157,772 exclusive entries, 0 overlaps; control caught 245,836 | no                          |
| ext4 stress (16 × 4, 8 writers, 60 s) | cloud container       | 2026-10-29 | passed  |              | 1,144,672 exclusive entries, 0 overlaps; longest wait 471 ms | |
| APFS, macOS 27.2                    | Mac Mini (M4, 10 cores) | 2026-10-29 | passed  | 1.1 ms       | 207,669 exclusive entries, 0 overlaps; control caught 306,385; longest wait 91 ms | **yes**, as expected        |
| SMB 3.1.1 (`smbfs`, CITADEL HAVEN share), macOS | Mac Mini   | 2026-10-29 | lease fallback | | the probe lock was refused, so the lock checks are skipped | |
| NFS 4.1, local nfsd, one client     | Linux 7.0 client in Docker on the Mac Mini | 2026-10-29 | passed | 1.1 ms | 35,120 exclusive entries, 0 overlaps; control caught 21,230; longest wait 257 ms | no |
| NFS 4.1, local nfsd, two clients    | two Linux clients in Docker on the Mac Mini | 2026-10-29 | passed, one unexplained overlap | | 10 of 10 runs passed (15 s, 4 × 2 per client, `--sync`): ext4 export 45,428 exclusive entries, tmpfs export 101,418, 0 overlaps; one earlier tmpfs run had 1 overlap and 1 lost update in 23,700 entries | |
| NTFS, Windows 11 26H2 (ARM64)      | Surface (Snapdragon X, 10 cores) | 2026-10-29 | passed | 1.6 ms | 34,244 exclusive entries, 0 overlaps; control caught 84,300; longest wait 6.0 s | no |
| SMB 3.1.1 (`smbfs`, CITADEL ATLAS share), macOS | Mac Mini, two writer processes | 2026-10-29 | lease fallback | lease taken over 65 s after `kill -9` | the probe lock was refused, so the lock checks are skipped; writers 2 and 3 got `EBUSY` after 5.0 s while writer 1 held the lease; a reader ran 23,333 checked queries meanwhile with 0 failures | |
| exFAT, macOS 27.2 (FSKit, 2 GiB image on HAVEN) | Mac Mini (M4, 10 cores) | 2026-10-29 | passed; not in `QUALIFIED`, so a writer uses the lease | 1.3 ms | 239,431 exclusive entries, 0 overlaps; control caught 357,022; longest wait 70 ms | **yes**, as expected |
| NTFS, Windows 11 26H2 (ARM64), second drive (F:) | Surface (Snapdragon X, 10 cores) | 2026-10-29 | passed | 1.2 ms | 34,047 exclusive entries, 0 overlaps; control caught 82,499; longest wait 3.3 s | no |

On the ATLAS share, writing a `.vdbx` first needed two fixes, both on
develop: the lease file is flushed with `fsync`, because `smbfs` refuses
`F_FULLFSYNC` (`0a65795`), and `fylo pack` falls back to a rename when the
filesystem refuses hard links with any error (`9f666cd`). Numbers and the
full sequence are in `docs/QUERY_RELEASE_QUALIFICATION.md`, "Single-file
`.vdbx` databases on ATLAS".

### Database checks on exFAT and NTFS

Beyond the lock harness, each of these filesystems ran a whole database
through the release CLI and the spike harness
(`/Volumes/FORGE/Logs/FYLO/fxdb-atlas/scripts/fxdbcheck.rs`), at develop
`70d468b`. The checks:

- **CLI round trip:** `create`, `createCollection`, three `INSERT`s, then
  `SELECT`, `find`, `get`, `DELETE`, `verify` and `inspect`.
- **Pack and unpack:** pack a 2,000-document root (two collections) onto
  the filesystem, verify it, unpack it there, and check every document.
- **Compaction:** delete half the documents, run `compact` and then
  `compact --shrink`, and verify the database with the oracle on the 1,000
  documents left.
- **Writers:** the two-writer sequence on a packed 5,000-document database.
- **Crash test:** three rounds of `kill -9` on a writer in the middle of its
  puts, then a read, `verify`, and the next writer.

| Check | exFAT, macOS 27.2 | NTFS, Windows 11 (F:) |
| ----- | ----------------- | --------------------- |
| What FYLO picks | lease, lease byte held exclusive (probe granted, unqualified) | byte-range locks (qualified) |
| CLI round trip | passed | passed |
| `fylo pack` onto the filesystem | passed (10.7 s for 2 × 2,000 documents) | passed (3.4 s) |
| `fylo unpack` onto the filesystem | **fails**: directory root, see below | passed, all 2,000 documents match |
| `compact`, then `compact --shrink` | passed; 112 → 120 → 112 MiB, 0 extents punched | passed; 112 → 120 → 112 MiB, 1 extent punched |
| `verify` after shrinking | ok | ok |
| Second and third writer while the first holds the file | `EBUSY` after 5.0 s each | both wrote at once (73 ms, 51 ms) |
| Reader during the writers | 30,545 checked queries, 0 failures, longest 28 ms | 14,961 checked queries, 0 failures, longest 538 ms |
| Next writer after the holder closed | 41 ms | 34 ms |
| Next writer after `kill -9` of the holder | `EBUSY` at 0 s and 35 s; took the lease over at 65 s | at once (12 ms) |
| Crash test, 3 rounds | 0 acknowledged puts lost, `verify` ok every round; next writer after 61–64 s | 0 acknowledged puts lost, `verify` ok every round; next writer after 22–27 ms |
| Size of a 3-document database | 40 MiB, all allocated | 40 MiB |

What the volumes offered, from direct probes:

- **exFAT (macOS FSKit):** `fsync`, `F_FULLFSYNC` and a directory `fsync`
  work. Byte-range locks work, with the harness result above. There are no
  sparse files: a 64 MiB `ftruncate` allocated 64 MiB. `F_PUNCHHOLE` returns
  `ENOTTY`, which `70d468b` treats as "cannot punch". `link` returns
  `ENOTSUP`, so pack takes its rename path (`9f666cd`). macOS keeps extended
  attributes in AppleDouble `._<name>` files beside the database and its
  lease (`._rt.vdbx`, `._rt.vdbx.lease.5`). They are harmless to a `.vdbx`
  file.
- **NTFS:** byte-range locks are qualified. Hole punching works: compaction
  marks the file sparse and frees the extent.

Known failures:

- **Directory roots on exFAT under macOS** (not a `.vdbx` failure; a fix is
  in progress). macOS writes AppleDouble `._<name>` files next to a root's
  shard directories. Building a directory root there then fails with
  `ENATIVE_UNSAFE_PATH: document shard is not a regular directory:
  …/.collections/items/docs/._0`, and `fylo unpack` onto exFAT fails with
  `ENATIVE_METADATA: layout copy failed verification: document 4VRNF520000
  differs in the copy`.
- **`cargo test -p fylo-storage-native` does not compile on Windows.** The
  test helper `copy_frame` in
  `src/segments/container_medium/tests/encryption.rs:402` imports
  `std::os::unix::fs::FileExt`, which fails with `E0433` and then `E0599`
  for `read_exact_at` and `write_all_at`. All nine integration test targets
  of the crate pass on Windows, among them `vdbx_pack` (10) and `vdbx_root`
  (8). So does `cargo test -p fylo-cli --test vdbx_database` (12).
- **The ENOTTY corruption, fixed in `70d468b`.** Before that commit,
  `ENOTTY` from `F_PUNCHHOLE` on exFAT failed `clear_extent` after the
  extent had already been allocated. The first `createCollection` on a
  fresh database failed. Later writes reported `ENATIVE_CONCURRENT_WRITE`,
  reads reported "segment collection has no valid generation slot", and
  `fylo verify` found an `extentHeaderMismatch`. `70d468b` makes the punch
  fall back to writing zeros. A failed clear still leaves the extent
  allocated, so another filesystem error at that point could leave the same
  mismatch.

Logs and raw JSON: `/Volumes/FORGE/Logs/FYLO/fxdb-atlas/exfat/` and
`/Volumes/FORGE/Logs/FYLO/fxdb-atlas/ntfs/`. The run at `7f7d1ec`, before
the ENOTTY fix, is in `exfat-7f7d1ec/`.

A filesystem joins `QUALIFIED` in
`crates/fylo-storage-native/src/byte_range_lock/filesystem.rs`, the list of
those where `.vdbx` byte-range locks are enabled, only with a passed row here.
A filesystem whose probe fails is recorded as "lease fallback": it is
supported, through the lock-free lease.

On Linux every NFS version has one `fstatfs` magic number, so FYLO reads the
mount table: an NFS 4.1 mount is named `nfs4.1`, and a mount whose locks stay
on the client (`nolock`, `local_lock=posix` or `local_lock=all`) is named
`nfs-local-lock` and is never qualified. The Linux NFS client answers some
cross-client conflicts with `EDEADLK` even to a non-waiting request; FYLO
treats it as a conflict and retries.

NFS stays unqualified. Across two clients, exclusion held in ten runs on two
exports, but one earlier run saw a holder read a counter one behind the last
holder's flushed write, which is what a client page cache that missed the
other client's change looks like. Byte-range locks on NFS would also need
every reader to revalidate its cache after each lock, which FYLO does not
promise. On NFS a `.vdbx` uses the lock-free lease.
