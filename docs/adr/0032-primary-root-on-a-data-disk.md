# ADR 0032: A server root lives on a data disk, not the system disk

- Status: **Accepted** (2026-11-14, the hub's review; see "Review" at the
  end). Windows unsafe calls: accepted 2026-11-15, the user approved ("Yes,
  with review"); hub review of the SAFETY summary: <date, before merge>. The user chose the scope ("Servers only (Recommended)", option A)
  and the timing ("This FYLO release") the same day, through the hub.
- Date: **2026-11-14**
- Owners: **FYLO maintainers**
- Extends: ADR 0029 (no root on a network filesystem). Uses the same check
  points, the same error rules and the same handshake object.
- Related: ADR 0003 (one owner per root), ADR 0010 (EFS profile), ADR 0011
  and 0028 (invocation), ADR 0026 (mobile builds and the C ABI), ADR 0030
  (tier 2: the temp-folder cache; tier 3: query results in memory).

## Context

The user set three storage tiers for FYLO on 2026-11-14:

> "i'm think for FYLO the data has to come from a mounted source (primary
> disk). `/mnt` for linux, `/Volume` for mac and a drive letter that is not
> `C:/` from Windows. then secondary disk (frequently accessed document) has
> to be set in the `/tmp` location (equivalent in windows and mac). then
> memory for query results."

Tiers 2 and 3 are ADR 0030. This ADR is tier 1: where the root (the true
copy of the data) may live.

For a root on the system disk the user chose: "Refuse, allow an opt-out
(Recommended)". For the scope the user chose option A, "Servers only
(Recommended)": the rule applies to FYLO as a server or command-line
database; a root inside an app's own data folder is allowed; tests and
laptops use the opt-out.

Facts that decide the design:

1. A path prefix says nothing reliable. On macOS `/Volumes` holds local
   disks, network shares (HAVEN and ATLAS on this Mac are NFS) and, through
   `/Volumes/Macintosh HD`, the system volume itself. Firmlinks put `/Users`
   on the Data volume. On Linux the system disk can be bind-mounted under
   `/mnt`. On Windows the system drive is not always `C:`.
2. Measured on this Mac (the hub, 2026-11-14): `/` is `/dev/disk3s1s1` and
   `/System/Volumes/Data` is `/dev/disk3s5`. They are different volumes in
   one APFS container, `disk3`. `/Volumes/HANGAR` is `/dev/disk7s1`,
   container `disk7`. So comparing volumes (`st_dev`) is not enough; FYLO
   must compare the disk (on macOS the APFS container).
3. TACHYON runs FYLO only inside `ty` (`ty start`, `ty dev`, `ty preview`,
   `ty handler invoke`), with an absolute root from `application.json`. Its
   tests and CI lanes put roots in the OS temp folder, on the system disk.
   No TACHYON native app runs FYLO today.
4. Mesail keeps its gate and nightly test data on a local APFS sparse disk
   image (`~/Developer/AtlasFylo/mesail`) whose image file is on ATLAS
   (NFS). Its dev host keeps roots on the Lima VM's ext4 data disk, and its
   Lambdas use the EFS profile under `/mnt/data`.
5. On iOS and Android the system disk is the only disk.

## Decision

### 1. The rule

A native root must be on a **local disk that is not the system disk**,
unless one of the two exceptions in section 3 applies. A root on the system
disk is refused with the new stable code **`ESYSTEMDISK`**, before FYLO
creates the root, the lock file or any other file.

The rule runs at exactly the points of ADR 0029 section 1, in the same
check (`RootFilesystem::admit`), right after the network check:

- when a root lease is taken (`fylo exec --loop`, `--exclusive-root`,
  `--invocation`, `--request`, queues, the library's `RootLease`);
- when a folder root or a `.vdbx` database is opened (`NativeRoot`,
  `NativeWriteRoot` and everything built on them);
- when a `.vdbx` database is created (`fylo create`, `pack --out`), and when
  `unpack --out` makes a folder root.

`export --out` and `salvage --out` write plain document trees, not roots,
and are not checked. The ADR 0030 temp cache (tier 2) is meant to live on
the system disk and is never a root, so it is not checked either.
Temp-tier code (ADR 0030 slices S2, S3 and later) opens its cache files
through `RootFilesystem::of` or plain file opens, never through
`RootFilesystem::admit` or a root open.

The order of checks is: network (ADR 0029, `EREMOTEROOT`), then system disk
(`ESYSTEMDISK`). A network root never reaches the second check, and the
exceptions in section 3 never admit a network root.

`ESYSTEMDISK` is not retryable.

**Where the refusal arrives.** Always as an error frame on standard output,
the same way as ADR 0029's `EREMOTEROOT`. It never exits with only a
message on stderr:

| Mode                           | Handshake                                                                                                                                      | First request that opens the root  |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- |
| `exec --loop` (plain)          | answers `ok`; `storage.rootLock.systemDisk` and `placement` say whether the root would be admitted (`placement:null` when it would be refused) | answers `ESYSTEMDISK` in its frame |
| `exec --loop --exclusive-root` | the lease is taken at start, so the first frame (the handshake's) is `ESYSTEMDISK`                                                             | (none)                             |
| `exec --request`               | a handshake alone answers `ok`, as in a plain loop; it opens no root                                                                           | answers `ESYSTEMDISK` in its frame |
| `exec --invocation`            | its one frame: `ESYSTEMDISK` when the request needs the root lease                                                                             | the same                           |

So a host can check a root before using it, from the handshake's
`systemDisk` and `placement`. `crates/fylo-cli/tests/system_disk.rs` checks
every row.

### 2. What "the system disk" is, per OS

FYLO describes two things the same way: the disk that holds the root (its
nearest existing parent, as in ADR 0029), and the disk that holds the
running OS. The root is on the system disk when the two are the same disk.
FYLO looks only at devices, never at path names.

**macOS.** `statfs` gives the device (`f_mntfromname`, such as
`/dev/disk3s5`). The disk is the whole disk of that device, the `diskN` part
of `/dev/diskNsM...`. For an APFS volume that is its APFS container, the
same answer DiskArbitration's `DADiskCopyWholeDisk` gives. The system disk
is the whole disk of `/` (the boot volume group's container). So:

- `/Users/...`, `/private/var/...` (the temp folder) and `/Volumes/Macintosh
HD/...` are all in the boot container: refused.
- A second APFS volume made in the boot container is refused too: it shares
  the system disk.
- An external disk, or a second internal disk (Mac Pro), is admitted.
- A device name that is not `/dev/disk...` on a local mount cannot be
  described: refused (fail closed, section 2.1).

Reading the BSD name needs no new unsafe code or framework link (ADR 0004);
`statfs` is already called for ADR 0029.

**Linux (not Android).** FYLO reads `/proc/self/mountinfo` and finds the
mount that holds the root's canonical path (the longest mount point that is
a parent) and the mount of `/`. The root is on the system disk when:

1. both mounts have the same device number (`major:minor` in mountinfo).
   This catches bind mounts of the system disk anywhere (also under `/mnt`)
   and btrfs subvolumes of the system filesystem; or
2. both mount sources are block devices, and their whole disks share a
   disk. The whole disk comes from `/sys/dev/block/<major>:<minor>`: a
   partition maps to its parent disk, and device-mapper or md devices (LVM,
   dm-crypt, RAID) map to the disks listed in `slaves/`, followed down; or
3. both are ZFS datasets of the same pool.

`stat`'s `st_dev` is not used: btrfs gives each subvolume its own.

In a container, `/` is the container's own root filesystem (often
`overlay`). FYLO cannot see the host's disks, so "the system disk" there
means the container's root filesystem: a root on it (the container's `/tmp`
included) is refused, and a root on a mounted volume is admitted.

**Memory filesystems are refused with the same code.** `tmpfs` and `ramfs`
are not disks; a root there is lost at reboot. (Many Linux systems mount
`/tmp` as tmpfs.) The opt-out admits them, for tests.

**Windows.** FYLO finds the volume of the root's nearest existing parent
(`GetVolumePathNameW`, then `GetVolumeNameForVolumeMountPointW`, giving
`\\?\Volume{GUID}\`) and the volume of the Windows folder
(`GetWindowsDirectoryW`; this is `%SystemDrive%`, never a hard-coded `C:`).
The root is on the system disk when the volumes are the same, or when the
physical disk numbers of the two volumes share a disk
(`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`). So `D:` on a second disk is
admitted; `D:` as a second partition of the system disk is refused; a
`SUBST` drive or a folder junction that leads to the system volume is
refused. A volume with no disk extents (the query fails) is judged by the
volume comparison alone. A volume with more extents than FYLO reads (64; a
spanned or striped dynamic volume answers `ERROR_MORE_DATA` with a partial
list) is "could not tell": FYLO never compares a partial list (the hub's
review of the SAFETY summary, 2026-11-15).

**iOS and Android.** The rule does not apply: there is no other disk.
These builds always run in app mode (section 3.2).

**The browser engine** (OPFS) is not affected.

#### 2.1 When FYLO cannot tell

If FYLO cannot describe the root's disk or the system disk (an unreadable
mountinfo, a device name it does not know), it refuses with `ESYSTEMDISK`
and says it could not tell. The opt-out admits such a root.

#### 2.2 Disk images

A disk image counts as the disk it presents, **but the system-disk check
follows the image to its backing file** (the hub's review, confirmed by the
user for Mesail's case). Otherwise any `.dmg`, `.sparseimage`, loop file or
`.vhdx` kept on the system disk would get around the rule.

When the root's disk is a disk image, FYLO finds the image's backing file
and applies the system-disk test to that file's disk:

| Backing file is on       | Result                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| the system disk          | refused, `ESYSTEMDISK` ("is on a disk image whose file `<image>` is on the system disk")                                                                     |
| another local disk       | admitted, `placement:"dataDisk"`                                                                                                                             |
| a network share          | admitted, `placement:"dataDisk"`, no warning                                                                                                                 |
| a place FYLO cannot find | refused, `ESYSTEMDISK` ("is on a disk image (`<disk>`, `<image path if known>`) and FYLO could not tell which disk holds the image file"), as in section 2.1 |

A network-backed image is admitted on purpose. The volume FYLO sees is a
local filesystem with local locks, and macOS attaches an image read-write on
one machine at a time, so ADR 0029's two-owner failure cannot happen through
it. Its durability is that of the image file's storage. For Mesail's image on
ATLAS that is CITADEL's async NFS export, a risk the user accepted for HAVEN
and ATLAS on 2026-11-11. The user approved this case.

An image inside an image is followed down, at most 4 levels; deeper is
"could not tell".

How the backing file is found:

- **macOS:** FYLO runs `/usr/bin/hdiutil info -plist` (absolute path,
  no shell, no arguments from input) and reads each attached image's
  `image-path` and the `dev-entry` of its `system-entities`. The root's disk
  is an image when its `diskN` is one of those entries (an APFS image lists
  both the image disk and its container, for example Mesail's image as
  `disk8` and `disk9`). This adds no unsafe code. It runs at most once per
  process, only when the root's disk is not the system disk and the opt-out
  is off, and its answer is kept for the process. Measured on this Mac:
  10 to 20 ms. The user chose this way over IOKit ("A: Apple's hdiutil tool
  (Recommended)", through the hub, 2026-11-15), so no unsafe code is added.
    - FYLO runs it only by its absolute path `/usr/bin/hdiutil`, never
      through `PATH`, so a fake `hdiutil` earlier in `PATH` cannot answer.
    - It has a time limit of 5 seconds. On timeout FYLO kills the child and
      waits for it.
    - **One retry, with a longer limit.** On a timeout, a non-zero exit, or
      output FYLO cannot parse, FYLO runs it once more with a 15-second limit
      (at most 20 seconds in all). A busy Mac (load 40 to 90 during batch
      rechecks) can slow a small child process for a few seconds; one failure
      alone must not fail a gate. The retry is logged on stderr: `FYLO
warning: hdiutil <reason>; trying again (ADR 0032).`
    - **One answer per process.** The parsed answer (all attached images and
      their disks) is kept for the life of the process, so a process that
      opens many roots runs `hdiutil` at most twice in all. Nothing is kept
      on disk: a cached answer would be one more thing that could wrongly
      admit a root.
    - If the retry also fails, the root is refused with `ESYSTEMDISK` ("could
      not tell"), and the message names the root's disk and the reason
      (`hdiutil timed out after 15 s`, `hdiutil exited with <code>`, `hdiutil
output could not be read`).
- **Linux:** a loop device (`/dev/loopN`, major 7) has its backing file in
  `/sys/block/loopN/loop/backing_file`. Device-mapper or md devices built on
  loop devices are followed through `slaves/` to the loops. A loop whose
  backing file was deleted (`(deleted)`) is "could not tell".
- **Windows:** a volume on a virtual disk (VHD, VHDX, ISO) is found through
  the virtual disk API: `GetStorageDependencyInformation` on the volume's
  disk gives the backing file's path (`DependentVolumeRelativePath` on its
  host volume), which is then checked like any root path.

The disk-image check never turns a refusal into an admission except as the
table says, and it never admits a network root (the root itself on a share is
still `EREMOTEROOT`; only an image's file may be on a share).

### 3. The two exceptions

#### 3.1 The opt-out: allow the system disk

One explicit setting, in three forms. Any one of them turns it on:

| Form                 | Name                     | Values                                                                                                    |
| -------------------- | ------------------------ | --------------------------------------------------------------------------------------------------------- |
| CLI flag             | `--allow-system-disk`    | no value                                                                                                  |
| Environment variable | `FYLO_ALLOW_SYSTEM_DISK` | `1` or `true` turn it on; unset, empty, `0` or `false` leave it off; any other value is an error at start |
| Config key           | `fylo.allowSystemDisk`   | `true` or `false`                                                                                         |

- The flag is a global option, like `--root`: it works with every command
  that opens a root, including `exec --loop`, `exec --request` and
  `exec --invocation`.
- The environment variable is read by the library at the root check, so it
  reaches every mode of the CLI and every library open (`NativeRoot`,
  `RootLease`, the `fylo` crate) without code in the host.
- The library also takes it as an explicit option (section 5).
- It admits a local root on the system disk and on a memory filesystem. It
  never admits a network root (ADR 0029 is unchanged); a test checks this.
- **When it is on, FYLO says so.** The first time in a process that the
  setting is read as on, FYLO prints one line on stderr (never on stdout,
  which carries the machine protocol):

    ```text
    FYLO warning: system-disk check disabled by <source> (ADR 0032); a root on the system disk is allowed.
    ```

    `<source>` is `--allow-system-disk`, `FYLO_ALLOW_SYSTEM_DISK=1` or
    `fylo.allowSystemDisk` (the first that is on, in that order). The
    handshake also shows it, as `placement:"systemDiskAllowed"`. So nobody
    runs a server with the opt-out on without seeing it.

- **Note for contributors:** the repository's `.cargo/config.toml` turns the
  opt-out on for every cargo command in a checkout (section 7), which
  includes `cargo run` of the `fylo` server. That is intended for
  development, and the stderr line above shows it. `CONTRIBUTING.md` says
  this, and says to run a real server from a built release binary, not from
  `cargo run` in a checkout.

#### 3.2 App mode: a root in the app's own data folder

An app that keeps FYLO data in its own per-app data folder declares it:

| Form                 | Name                                                                    | Value        |
| -------------------- | ----------------------------------------------------------------------- | ------------ |
| CLI flag             | `--app-id <id>`                                                         | the app's id |
| Environment variable | `FYLO_APP_ID`                                                           | the app's id |
| Config key           | `fylo.appId`                                                            | the app's id |
| Library              | `RootPlacement::AppOwned(AppId)`, installed for the process (section 5) | the app's id |

The flag wins over the environment variable, which wins over the config key.

**The app id is validated before it is used** (it becomes part of a folder
path). It is the app's bundle id or package name, in reverse-DNS style:

- only ASCII letters, digits, `.`, `-` and `_`;
- 1 to 255 bytes;
- no `/`, no `\`, no `..` anywhere, and no leading `.`;
- no empty part between dots, and it does not end with a `.`.

Any other value is an error before any path is built: the CLI stops at
start with a message on stderr and exit code 1, and a library host that
reads it from the environment gets `ESYSTEMDISK` ("cannot be checked") at
the root check. After
FYLO joins the id to the OS folder, it canonicalizes both the app folder and
the root's nearest existing parent (symbolic links, and on Windows junctions
and mount points, resolved) and admits the root only when the root is inside
the app folder, compared by path components. A link or junction that leads
out of the app folder is refused.

FYLO then **computes the app's folder from the OS**, never from the root
path, and admits the root only when its canonical path is inside that folder
(compared by path components, after both are canonicalized):

| OS                   | App-owned folder                                                                                                                                                                                                                                                                                                                                                                                                                   |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| macOS, not sandboxed | `<home>/Library/Application Support/<id>`, `<home>` as the OS reports it to the process (Rust's `std::env::home_dir`: `HOME`, else the user database)                                                                                                                                                                                                                                                                              |
| macOS, sandboxed     | the app's container, `<home>/Library/Containers/<id>/Data`                                                                                                                                                                                                                                                                                                                                                                         |
| Windows              | `<LocalAppData>\<id>` and `<RoamingAppData>\<id>` (`%LOCALAPPDATA%` and `%APPDATA%`), both from `SHGetKnownFolderPath` (`FOLDERID_LocalAppData`, `FOLDERID_RoamingAppData`), not from the environment; for a packaged (MSIX) app also their redirected folders under `<LocalAppData>\Packages\<package family>\LocalCache\` (`Local\<id>`, `Roaming\<id>`). A roaming folder redirected to a network share is still `EREMOTEROOT`. |
| Linux                | `$XDG_DATA_HOME/<id>`; when `XDG_DATA_HOME` is unset, empty or not absolute, `<home>/.local/share/<id>` (Flatpak sets `XDG_DATA_HOME` to the app's own folder)                                                                                                                                                                                                                                                                     |
| iOS                  | the app's container (`$HOME`, which iOS sets to it)                                                                                                                                                                                                                                                                                                                                                                                |
| Android              | the app's internal data folder, `/data/user/<n>/<id>` or `/data/data/<id>`; when no id is given, any folder under those two whose owner is the app's own user id                                                                                                                                                                                                                                                                   |

- On iOS and Android app mode is automatic: the mobile builds always run in
  it, and the id is optional.
- On desktop it is explicit: without an app id, a root in an app folder is
  just a root on the system disk, and is refused.
- A root in app mode that is outside the app's folder is refused with
  `ESYSTEMDISK`, and the message names the folder FYLO expected.
- App mode never admits a network root: an app folder in a network home
  directory is still `EREMOTEROOT`; a test checks this.
- If the app folder is on a data disk (a moved home directory), the root is
  admitted by the main rule anyway.

No TACHYON native app runs FYLO today, so app mode is tested by FYLO's own
tests, one per OS folder rule (section 7).

### 4. The refusal message

```text
FYLO root <path> is on the system disk (<disk>). A FYLO server keeps its
data on another local disk: a disk under /Volumes on macOS, a mounted disk
such as /mnt/<disk> on Linux, or a drive other than the Windows drive on
Windows. To use the system disk anyway (tests, CI, a laptop), pass
--allow-system-disk, set FYLO_ALLOW_SYSTEM_DISK=1 or set
fylo.allowSystemDisk to true. An app that keeps its data in its own data
folder passes --app-id <id> (FYLO_APP_ID, fylo.appId).
```

The message is one line in the error frame; it is wrapped here only for
reading. `<disk>` is `disk3` (macOS), `8:0 sda` (Linux),
`\\?\Volume{...}\ (disk 0)` (Windows). Variants of the first sentence: "is on
a memory file system (tmpfs)"; "could not tell which disk holds it (<reason>)";
in app mode, "is outside the folder of app <id> (<folder>)". The rest of the
message is the same in every variant, so a host can map `ESYSTEMDISK` alone.

### 5. Library API

`fylo-storage-native` adds, in `src/system_disk/`:

```rust
pub enum RootPlacement {
    /// The default: data disk only.
    DataDisk,
    /// The opt-out, and which setting turned it on.
    AllowSystemDisk(OptOutSource),
    /// App mode.
    AppOwned(AppId),
}

pub struct PlacementSettings {
    pub allow_flag: bool,
    pub allow_config: bool,
    pub app_id_flag: Option<String>,
    pub app_id_config: Option<String>,
}
```

- `RootPlacement::from_settings(&settings)` resolves the flags and the
  configuration with the environment (section 3); `.install()` makes the
  result the placement of every root the process opens. The CLI does this
  once, when it loads its configuration.
- A host that installs nothing gets `RootPlacement::current()`, which reads
  `FYLO_ALLOW_SYSTEM_DISK` and `FYLO_APP_ID` alone. So every library open
  (`NativeRoot`, `NativeWriteRoot`, `RootLease`, the `fylo` crate) and the C
  ABI (ADR 0026) follow the environment without code in the host; a C ABI
  host on a desktop sets `FYLO_APP_ID` for app mode.
- `RootFilesystem::admit(path, profile)` keeps its signature and runs the
  rule after the network check. `RootFilesystem::describe(path)` reports
  the same without refusing, for the handshake, and
  `RootFilesystem::placement()` returns `PlacementReport { system_disk,
placement }`.
- The OS lookups sit behind one internal trait, so the decision (system
  disk, memory filesystem, the three disk-image cases, "could not tell") is
  unit-tested on every OS with fake disks.

### 6. The handshake

`result.storage.rootLock` gains two fields, next to ADR 0029's `filesystem`
and `remote`:

- `systemDisk`: `true`, `false`, or `null` when FYLO did not look it up (no
  default root, a remote root, the opt-out, app mode) or could not tell;
- `placement`: why the root is admitted: `"dataDisk"`,
  `"systemDiskAllowed"` (the opt-out), `"appOwned"`, or `null` when there is
  no default root.

No existing field is renamed or removed. The machine protocol stays `v1`.
`ESYSTEMDISK` is added to `api/errors/v1.json` (not retryable) and to the
website's error pages.

### 7. Tests, CI and local lanes

Every place that opens roots in a temp folder turns the opt-out on **from
the repository**, so a lane needs no setup of its own:

| Where                                                                                               | How                                                                                                                                                        |
| --------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo test`, `cargo nextest` (all crates, also on the Linux and Windows lanes and the nightly run) | new `.cargo/config.toml` in the repository: `[env] FYLO_ALLOW_SYSTEM_DISK = "1"`                                                                           |
| `bun test` (`tests/browser`, `tests/interop`, every language client the interop tests start)        | new `bunfig.toml`: `[test] preload = ["./tests/setup/allow-system-disk.js"]`, which sets `process.env.FYLO_ALLOW_SYSTEM_DISK ??= "1"`; children inherit it |
| `scripts/*.mjs` verify scripts that start `fylo` on temp roots                                      | set it in their spawn environment (one shared helper)                                                                                                      |
| GitHub workflows (`ci.yml`, `rust-nightly.yml`, `publish.yml`)                                      | workflow-level `env: FYLO_ALLOW_SYSTEM_DISK: "1"`                                                                                                          |
| Local nightly (`nightly-full`, FYLO product script) and the Linux (Colima) and Windows VM gates     | covered by the two files above; the nightly product script also exports it, so a run outside the repository's cargo and bun config is covered too          |

Tests of the rule itself force it on, because the repository, CI and the
nightly run (Platform sets `FYLO_ALLOW_SYSTEM_DISK=1` there) all turn the
opt-out on in the parent environment:

- unit tests pass `RootPlacement` explicitly, with fake disk descriptions,
  so the environment is never read;
- CLI tests start `fylo` with `env_remove("FYLO_ALLOW_SYSTEM_DISK")` and
  `env_remove("FYLO_APP_ID")`, with no `--allow-system-disk` or `--app-id`,
  and with `--config` pointing at a test `application.json` that sets neither
  key (so a stray `./application.json` cannot turn it on);
- one test proves this: with `FYLO_ALLOW_SYSTEM_DISK=1` set in the test
  process itself, the child that runs with the cleared environment is still
  refused with `ESYSTEMDISK`, and its handshake has no `systemDiskAllowed`.

New tests:

- Unit (`crates/fylo-storage-native/src/system_disk/`): the macOS BSD-name
  rule (`/dev/disk3s1s1` and `/dev/disk3s5` same disk; `/dev/disk7s1` not);
  Linux mountinfo rules (same device, bind mount, btrfs, partition parent,
  LVM over dm-crypt, ZFS pool, overlay root in a container, tmpfs);
  Windows (same volume, same physical disk, other disk, failed extents);
  the environment values; the message text; the order with
  `EREMOTEROOT`.
- App mode, one test per OS folder rule: macOS unsandboxed and sandboxed,
  Windows plain and packaged, Linux with and without `XDG_DATA_HOME`, iOS,
  Android with id and by owner. Each checks one root inside (admitted), one
  beside the folder (refused) and one through a symbolic link that leads
  out (refused), and on Windows one through a junction that leads out
  (refused). The folder functions take the OS values as input, so all
  rules are tested on every OS; the real OS lookup is tested on its own OS.
- App id validation: `../x`, `a/../b`, `a/b`, `a\b`, `..`, `.hidden`, `a..b`,
  `a.`, the empty id, a 256-byte id and a non-ASCII id are each refused
  before any path is built; `com.example.app`, `com.example-app_2` and a
  255-byte id are accepted.
- Network roots are never admitted: an NFS (or, opt-in, any
  `FYLO_TEST_NETWORK_ROOTS`) root with the opt-out on is `EREMOTEROOT`; an
  app-mode root whose app folder is on a network share is `EREMOTEROOT`. The
  unit form runs everywhere with fake filesystem descriptions; the real form
  runs where ADR 0029's network tests run.
- The opt-out's stderr line: printed once, on stderr, naming the source that
  turned it on (flag, environment variable, config key); never on stdout;
  not printed when the opt-out is off.
- Disk images, the three cases the hub set: an image whose file is on the
  system disk (refused), on another local disk (admitted) and on a network
  share (admitted), plus "could not tell" (refused, the message names the
  image). The `hdiutil` reading: a first timeout followed by a good answer
  admits (the retry), and the retry's stderr line is printed; two timeouts
  (a stand-in program that sleeps past short test limits) refuse as "could
  not tell" and leave no child running; unparsable output and a non-zero
  exit, twice, refuse the same way; a process that opens several roots runs
  the stand-in once; a fake
  `hdiutil` placed first in `PATH`, which would answer "not an image", is
  ignored, because only `/usr/bin/hdiutil` runs (the runner takes the
  program path and limit as inputs, so tests can point it at the stand-ins,
  while the production path is a constant that a test asserts). Unit tests
  use recorded `hdiutil info -plist` output, Linux sysfs
  trees and Windows dependency answers; real tests: on macOS an image made by
  the test in the temp folder (system disk, refused) and Mesail's ATLAS
  image (network, admitted, opt-in); on Linux a loop device on a file in the
  VM's root disk (refused) and on its data disk (admitted), where the gate
  can attach loops.
- CLI (`crates/fylo-cli/tests/system_disk.rs`): a temp root is refused in
  every mode with nothing written; the flag, the environment variable and the
  config key each admit it; the handshake fields; app mode on the host OS.
  Opt-in, like ADR 0029: roots named in `FYLO_TEST_DATA_DISK_ROOTS` must be
  admitted with `placement:"dataDisk"`. On this Mac: Mesail's sparse image
  (`~/Developer/AtlasFylo/mesail`, the network-backed image case) and a
  folder on HANGAR. On Linux in Colima: the VM's data disk. On Windows: a
  small VHDX attached as a second drive by the gate script (needs Platform's
  OK for the VM change).
- The Mesail cases: the sparse image (admitted), a `$TMPDIR` root (refused,
  admitted with the opt-out), an ext4 data disk that is not the VM's root
  disk (admitted), and the EFS profile under `/mnt/data` (admitted).

### 8. Versioning: how the break is marked

FYLO uses CalVer (`YY.WW.DD`), which has no major number. A breaking change
is marked the way 26.45.07 marked the removal of POSTIX:

- the release's CHANGELOG entry has a `### Breaking Changes` section, and
  this change is its first item;
- the release notes start with that section and the migration note below;
- the stable error code is new and documented, so hosts can match it;
- the machine protocol and the error catalog stay `v1`, because only fields
  and a code are added.

## Migration note

Who must act before upgrading:

1. **A server whose root is on the system disk.** Move the root to a data
   disk:
    - stop every process that uses the root;
    - a `.vdbx` database is one file: copy it;
    - a folder root: copy it with its extended attributes (macOS `ditto
<old> <new>`, Linux `cp -a` or `rsync -aHAX`, Windows `robocopy <old>
<new> /E /COPY:DATSO /DCOPY:DAT`);
    - point `fylo.root` (or `--root`) at the new place, and run `fylo verify
<collection>` for each collection.

    Or keep it there and set `fylo.allowSystemDisk: true` (or the flag, or
    the environment variable).

2. **Tests, CI and laptops.** Set `FYLO_ALLOW_SYSTEM_DISK=1` in the test or
   CI environment.
3. **Desktop apps** that keep data in their own data folder: pass the app
   id (`--app-id`, `FYLO_APP_ID`, `fylo.appId`, or the library and C ABI
   option). Mobile apps need nothing.
4. **Linux roots on tmpfs:** move them to a disk, or use the opt-out.

Known users on 2026-11-14:

- **TACHYON:** `ty dev`, `ty preview`, `ty handler invoke`, its tests, CI
  lanes and nightly set `FYLO_ALLOW_SYSTEM_DISK=1`. `ty start` does not: a
  real server on the system disk turns on the setting itself. TACHYON maps
  `ESYSTEMDISK` to its message TY2160.
- **Mesail:** gate and nightly data on the sparse image needs nothing;
  `$TMPDIR` fallback roots need the opt-out; the dev host and Lambdas need
  nothing.

## CHANGELOG entry (draft)

```markdown
### Breaking Changes

- **A root on the system disk is refused (ADR 0032).** FYLO keeps its data
  on a local disk that is not the system disk: a disk under `/Volumes` on
  macOS, a mounted disk on Linux, a drive other than the Windows drive on
  Windows. A root on the system disk (also the OS temp folder, and on Linux
  `tmpfs`) is refused with the new code `ESYSTEMDISK` before anything is
  written, in every `fylo exec` mode, the other commands and the library.
  FYLO compares disks, not paths: on macOS the APFS container, on Linux the
  device and its whole disk, on Windows the volume and its physical disk.
  A disk image counts as its disk, but an image whose file is on the system
  disk is refused too (on macOS FYLO asks `/usr/bin/hdiutil`).
  To keep using the system disk (tests, CI, a laptop), pass
  `--allow-system-disk`, set `FYLO_ALLOW_SYSTEM_DISK=1` or set
  `fylo.allowSystemDisk` to `true`; FYLO then prints one warning line on
  stderr saying the check is off. A desktop app that keeps its data in its
  own data folder passes `--app-id <id>` (`FYLO_APP_ID`, `fylo.appId`);
  iOS and Android apps need nothing. Network roots stay refused
  (`EREMOTEROOT`, ADR 0029); the EFS profile works as before. See the
  migration note in ADR 0032.
- **The handshake reports where the root is.** `storage.rootLock` gains
  `systemDisk` and `placement` (`dataDisk`, `systemDiskAllowed`,
  `appOwned`).
```

## Consequences

- Every existing root on the system disk stops opening until it is moved or
  the opt-out is set. On most laptops that is every root.
- A Mac or PC with one disk needs the opt-out, or an external disk, to run
  FYLO as a server.
- The cost is one more device lookup per root open: on Linux one read of
  `/proc/self/mountinfo` (and sysfs for block devices); on Windows two
  volume queries, and for a virtual disk one dependency query; on macOS one
  `statfs` of `/` per process, plus one `hdiutil info -plist` (10 to 20 ms)
  per process when the root is not on the system disk. With the opt-out on,
  none of these run.
- **Unsafe code (ADR 0004):** Linux and macOS add none. Windows adds calls
  through `windows-sys` to the volume, disk-extent, virtual-disk and
  known-folder functions named above. std has no volume API, so the Windows
  rule cannot be built without them. The user allowed them ("Yes, with
  review (Recommended)", through the hub, 2026-11-15) on these terms:
    - all of them live in one small module
      (`crates/fylo-storage-native/src/system_disk/windows.rs`), and no other
      file gains unsafe code;
    - every unsafe block has a `// SAFETY:` note saying why its pointers,
      buffer sizes and handles are valid;
    - `docs/security/unsafe-inventory.md` gets a row for each unsafe
      function in the module, each citing ADR 0032;
    - its tests run in `windows-vm`;
    - the module's SAFETY summary goes to the hub for review before merge.

## Review (the hub, 2026-11-14)

Approved. The hub's answers:

1. Compare disks, not volumes: yes.
2. Refuse `tmpfs` and `ramfs`: yes.
3. Disk images: changed. The check follows an image to its backing file; an
   image file on the system disk is refused; on another local disk or a
   network share it is admitted (section 2.2; the user approved Mesail's
   case).
4. Fail closed when the disk cannot be told, with a message that names the
   opt-out: yes.
5. The Windows second-disk test: yes in principle; Platform is asked for a
   VHDX that the gate script attaches and detaches, never touching the real
   system disk.

Required changes, all applied: the app id is validated against path escapes
(section 3.2); the opt-out prints a visible stderr line, and the repository
opt-out's effect on `cargo run` is stated here and in `CONTRIBUTING.md`
(section 3.1); tests prove network roots are never admitted by the opt-out
or by app mode (section 7). The CHANGELOG "Breaking Changes" section and the
migration note are the right mark for FYLO's CalVer.

The user then chose `hdiutil` over IOKit for macOS disk images (2026-11-15).
The hub added: run it only as `/usr/bin/hdiutil`, with a 5-second limit, and
refuse as "could not tell" on a timeout or unreadable output (section 2.2).
Nothing is open.
