# OS hardening samples for protected databases

These are **samples**, not shipped policy. They show how a host's
administrator can add the kernel's own access control around one application
that uses a protected `.vdbx` database (ADR 0021, option E of
[the access-control study](../access-control-design.md#45-e-mandatory-access-control-profiles)).
FYLO does not install them, test them in CI or depend on them. Copy one,
change the paths and names marked `CHANGE`, and test it on your own host,
in complain or audit mode first.

## What a profile adds

A protected database already makes its records unreadable and unforgeable
for every process that holds no granted key (study section 6). Where the
key is a software key (`file:`, `creds:`, `cng:`), same-user malware can
still read the key, or the application's memory, and act as the
application. A mandatory access control profile closes that gap on a
server by naming the **one executable** that may:

1. read the identity's private key (a `file:` key, or the `creds:`
   credential systemd hands the unit);
2. open the database file for reading and writing, and create its staging
   files beside it (`<db>.vdbx.pack-staging*`, `<db>.vdbx.seal-staging*`)
   when the application itself runs `rekey` or `pack`;
3. be traced (`ptrace`) or have its memory read: no other process may.

Everything else stays as FYLO needs it: FYLO opens no listener and no
network connection, writes only the database file (and its staging files
during a rekey or pack), and coordinates writers with byte-range locks
inside the file (`fcntl` on Unix, `LockFileEx` on Windows).

Run the application under **its own OS user** as well (the study's answer
to Q3). The profile then protects the key and the file from other programs
of that user and from other users; the file mode (`0600`) protects them
from other users even without a profile.

## The samples

| Platform | File | Mechanism |
| --- | --- | --- |
| Linux (Ubuntu, Debian, SUSE) | [`apparmor/usr.local.bin.example-app`](apparmor/usr.local.bin.example-app) | AppArmor profile for the application's executable |
| Linux (Fedora, RHEL) | [`selinux/example_app.te`](selinux/example_app.te), [`selinux/example_app.fc`](selinux/example_app.fc) | SELinux policy module: a domain for the application and file types for its key and database |
| Windows | [`appcontainer/AppxManifest.xml`](appcontainer/AppxManifest.xml), [`appcontainer/README.md`](appcontainer/README.md) | A packaged (MSIX) application in an AppContainer, with the database folder's ACL granted to the package alone |

macOS needs no sample: an application signed with the hardened runtime,
whose identity is a `keychain:` key or an `enclave:` key, is already
covered by the keychain's access list and by `task_for_pid` restrictions
(study section 6). A sandboxed macOS application keeps its database in its
container.

## What they do not do

- They do not stop root or Administrator, or a kernel exploit (study
  section 6).
- They do not stop the application itself from misusing its own grant;
  the policy inside the database does that.
- They are not tested by FYLO's CI. The AppArmor and SELinux samples
  follow each tool's documented syntax; check them with
  `apparmor_parser -Q -K <profile>` and `checkmodule -M -m`, then load them
  in complain (AppArmor) or permissive-domain (SELinux) mode and watch the
  audit log before you enforce them.
