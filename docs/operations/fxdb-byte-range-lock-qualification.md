# `.fxdb` byte-range lock qualification

ADR 0016 section 7 lets several local processes share one `.fxdb` file through
OS byte-range locks, and only on a filesystem where the multi-process harness
has passed. Elsewhere a `.fxdb` opens with an exclusive lease. This page records
the harness, how to run it, and which filesystems have passed.

## The locks

`fylo_storage_native::ByteRangeLockFile` takes one byte, shared or exclusive,
with a non-waiting `try_lock` or a waiting `lock(byte, mode, timeout)`.

| Byte                       | Offset                          | Use                                                          |
| -------------------------- | ------------------------------- | ------------------------------------------------------------ |
| `LockByte::LEASE`          | 2⁶²                             | database lease: shared by local processes, exclusive for `fylo serve` |
| `LockByte::ALLOCATION`     | 2⁶² + 1                         | taking a free extent or growing the file                     |
| reserved                   | 2⁶² + 2 to 2⁶² + 4095           | further database-wide locks                                  |
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

**Do all I/O on a locked `.fxdb` through `ByteRangeLockFile::file()`.** On
macOS, opening and closing the file by path anywhere else in the process
releases its locks. The harness reports this case for each platform.

Waiting polls the non-waiting request with a backoff from 20 µs to 1 ms and
wakes at once on a release inside the process. A blocking kernel wait
(`F_OFD_SETLKW`, `F_SETLKW`, a blocking `LockFileEx`) has no timeout, and a
thread parked in it would hold a request the registry cannot see. The cost is
fairness: under heavy contention a waiter can lose several handoffs in a row
(the longest wait in the Linux load run below was 85 ms).

The `unsafe` calls are listed in `docs/security/unsafe-inventory.md`.

### Dependencies

ADR 0004 asks each new direct dependency to be documented. Both crates were
already in `Cargo.lock`, so the graph gains no crate.

| Crate                | Owner                 | Features                                                         | Why                                                                                   | License             | Target impact                          |
| -------------------- | --------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------- | ------------------- | -------------------------------------- |
| `libc` 0.2.189       | `fylo-storage-native` | none (`default-features = false`)                                | `struct flock` and the `F_OFD_*` commands differ per target; hand-written declarations would get one wrong | MIT or Apache-2.0   | Unix only; already linked via `errno`, `getrandom`, `rustix` |
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
4. **Close safety.** A holder that opens and drops a second
   `ByteRangeLockFile` keeps its locks (must pass). A holder that opens and
   closes the path with plain `std::fs` is reported, not judged.
5. **Control.** Two seconds of the load below with the locks switched off. It
   must show overlaps or lost updates, which proves the witness can see one on
   this filesystem.
6. **Load.** `procs` processes of `threads` threads, each thread with its own
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
fylo-lock-harness prepare --file /Volumes/ATLAS/locktest.fxdb    # once
fylo-lock-harness share --file /Volumes/ATLAS/locktest.fxdb --machines 2 --machine 0 \
    --start-at 1793000000 --sync --json 0.json                   # machine 0
fylo-lock-harness share --file <same file> --machines 2 --machine 1 \
    --start-at 1793000000 --sync --json 1.json                   # machine 1
fylo-lock-harness verify --file /Volumes/ATLAS/locktest.fxdb 0.json 1.json
```

`verify` sums every machine's entries and compares them with the counters in
the file.

## Qualification

| Filesystem                          | Machine                 | Date       | Result  | Kill release | Load (8 × 2, 10 s)                         | Path open+close drops locks |
| ----------------------------------- | ----------------------- | ---------- | ------- | ------------ | ------------------------------------------ | --------------------------- |
| ext4, Linux 6.18 (x86_64)           | cloud container         | 2026-10-29 | passed  | 1.1 ms       | 159,090 exclusive entries, 0 overlaps; control caught 246,052 | no                          |
| APFS, macOS                         | Mac Mini                | pending    |         |              |                                            |                             |
| NFS 4.1, local server (`efs:local`) | Linux client in Docker on the Mac Mini | pending |   |              |                                            |                             |
| NTFS, Windows                       | Surface                 | pending    |         |              |                                            |                             |
| SMB, ATLAS on CITADEL               | two machines            | spike 2, run by the design thread |  |       |                                            |                             |

A filesystem joins the list of those where `.fxdb` byte-range locks are
enabled only with a passed row here.
