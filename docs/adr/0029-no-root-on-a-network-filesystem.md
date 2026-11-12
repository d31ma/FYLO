# ADR 0029: No root on a network filesystem

- Status: **Accepted** (the user chose "Yes, fix it", 2026-11-12)
- Date: **2026-11-12**
- Owners: **FYLO maintainers**
- Enforces: ADR 0003 ("Network shares and synchronized folders are
  unsupported unless their exact filesystem semantics pass a separate native
  qualification profile"). Keeps ADR 0010's explicit EFS profile as the one
  exception. Applies to the leases of ADR 0011 and ADR 0028.

## Context

ADR 0003 says one process owns a root, and that network shares are
unsupported. Nothing enforced the second rule. The root lease is a kernel
advisory lock on a file next to the root. On a network filesystem that lock
may bind only the processes of one machine.

Evidence (Mesail trial, 2026-11-12, develop 9d5d7f2):

- The root was on an NFSv3 export mounted on the Mac with `locallocks`
  (kestrel's `/Volumes/ANVIL/Builds/targets`, served by unfsd).
- kestrel held the root with `fylo exec --loop --exclusive-root`.
- At the same time the Mac's `fylo exec --invocation` also took the root and
  wrote to it.
- Both handshakes reported `rootLock {owned:true, kernelEnforced:true,
  remoteQualified:false}`. Two owners wrote one root.
- On ATLAS (NFSv4) the second owner got `EROOTLOCKED`, because that server
  forwards locks. FYLO cannot tell such mounts apart from the client.

On SMB the only refusal people saw was extended attributes failing with
`ENOTSUP`: an accident, not a rule.

## Decision

### 1. A native root on a network filesystem is refused

Before FYLO takes or opens a root, it asks the OS which filesystem holds the
path: one `statfs` (macOS, Linux) or one volume query (Windows). A missing
root is described by its nearest existing parent, the filesystem it would be
created on. When that filesystem is remote, FYLO refuses with the stable code
`EREMOTEROOT`, before it creates the root, the lock file or any other file.
The message names the path and the filesystem and says to use a disk attached
to this machine.

The check runs:

- when a root lease is taken (`fylo exec --loop`, `--exclusive-root`,
  `--invocation`, `--request`, queues, and the library's `RootLease`);
- when a folder root or a `.vdbx` database is opened (`NativeRoot`,
  `NativeWriteRoot`, and everything built on them);
- when a `.vdbx` database is created.

`EREMOTEROOT` is not retryable.

### 2. What is remote

- **macOS:** every mount whose `statfs` flags lack `MNT_LOCAL`, and the names
  `nfs`, `smbfs`, `webdav`, `afpfs`, `cifs` and `ftp` whatever the flags say.
  macFUSE mounts without the `local` option lack `MNT_LOCAL` and are remote.
- **Linux and Android:** the `f_type` magic numbers of NFS, CIFS, SMB2, the
  old smbfs, NCP, 9p, Ceph, AFS (both), Coda, Lustre, GPFS, OrangeFS,
  virtiofs and FUSE.
- **Windows:** a handle that reports a remote protocol (mapped network drives
  and shares), and every UNC path (`\\server\share`, `\\?\UNC\...`).
- A Linux magic number not in the list is reported as `unknown` and local. It
  is still never qualified for shared `.vdbx` byte-range locks (ADR 0016
  section 7).

**FUSE is remote.** A FUSE filesystem may keep its files on this disk
(ntfs-3g, bindfs) or on another machine (sshfs, rclone, s3fs, gcsfuse), and
the kernel does not say which. A lock on a remote FUSE mount binds only this
machine, which is the failure above, so FYLO refuses all of FUSE.

**virtiofs and 9p are remote.** They share a folder of another kernel (a
virtual machine's host, or Windows from WSL 2). Processes on that other
kernel do not see the locks taken here.

### 3. The EFS profile is the exception

`--filesystem-profile efs-sidecar-v1` (ADR 0010) is an explicit choice to run
a root on EFS or another NFS 4.1 server that forwards locks. It is admitted
on a remote filesystem exactly as before, and its qualification rules are
unchanged. The handshake reports what is known: `remote:true`, and
`kernelEnforced:false`, because a lock granted by this machine's kernel does
not prove the server enforces it for other machines.

### 4. The handshake reports the filesystem

`result.storage.rootLock` gains two fields:

- `filesystem`: the name the OS reports, such as `apfs`, `ext4`, `NTFS`,
  `nfs`, `nfs4.1` (Linux names NFS by version) or `unknown`; `null` when the
  session has no default root.
- `remote`: `true` or `false`; `null` when `filesystem` is `null`.

Before the lease is taken they describe the default root's path; after, the
filesystem the lease found. `kernelEnforced` is never `true` when `remote` is
`true`. No existing field is renamed or removed.

## Consequences

- A root on NFS, SMB, WebDAV, FUSE or a VM host share that opened before is
  refused now, for reading as well as writing. Move it to a local disk, or,
  for EFS, select the EFS profile.
- Docker Desktop and Colima on macOS share host folders with containers over
  virtiofs, so a root in such a bind mount is refused; keep the root in a
  Docker volume or on the container's own disk.
- The cost is one `statfs` per root open.

## Verification

- Unit tests map every listed name and magic number to remote or local, and
  check the message and the EFS exception
  (`crates/fylo-storage-native/src/byte_range_lock/filesystem.rs`,
  `src/root_filesystem.rs`).
- `crates/fylo-cli/tests/remote_root.rs` checks the handshake on a local root,
  and, opt-in, refuses real network mounts named in
  `FYLO_TEST_NETWORK_ROOTS`: a loop with and without `--exclusive-root`, an
  invocation, a one-shot request, an existing root, a new and a copied
  `.vdbx`, each with nothing written, and admits the EFS profile. On
  2026-11-12 it passed on an NFSv3 mount (unfsd, `locallocks`) and an NFSv4
  mount (CITADEL) from macOS.
