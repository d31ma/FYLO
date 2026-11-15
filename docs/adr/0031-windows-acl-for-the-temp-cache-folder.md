# ADR 0031: Windows ACL for the temp cache folder

- Status: **Accepted** 2026-11-14: user approved option D ('yes, D'); hub
  review 2026-11-14 (atomic create with security attributes required)
- Date: **2026-11-14**
- Owners: **FYLO maintainers**
- Serves: ADR 0030 section 4.2 (the temp cache folder is private to the OS
  user). Adds one unsafe boundary under ADR 0004.

## Context

ADR 0030 keeps a read cache in the OS temp folder. The cache holds stored
document records, so it must be private to the OS user on every OS:

- created so that only the current user can reach it;
- an existing folder used only when it belongs to the current user and
  nobody else can reach it;
- never a link or reparse point.

On Unix the standard library does all of this: `DirBuilderExt::mode(0o700)`,
`MetadataExt::uid()` and the mode bits, and `O_NOFOLLOW` opens.

On Windows access is a security descriptor: an owner and a DACL (a list of
access entries). The standard library cannot read or set either. It has no
API for an owner, a SID, an ACL or a security descriptor; `Permissions` on
Windows is only the read-only attribute. A new folder inherits the DACL of
its parent, and `%TEMP%`'s DACL grants SYSTEM and Administrators too. No
crate already in FYLO's dependency graph offers a safe wrapper. A child
process (`icacls`, PowerShell `Get-Acl`) was rejected: slow (hundreds of
milliseconds), dependent on external tools, and its text output is
localized and fragile to parse.

Reparse points need no unsafe code: `MetadataExt::file_attributes()` and
`OpenOptionsExt::custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)` are in the
standard library.

## Decision

`crates/fylo-storage-native/src/temp_tier/windows_folder.rs` (Windows only)
is the one place in the temp tier with unsafe code. It does three
operations, each through Win32 calls `windows-sys` declares:

1. **Create the folder private** (`create_private`): one
   `CreateDirectoryW` call whose `SECURITY_ATTRIBUTES` carry a protected
   DACL with one entry, full control for the current user, inherited by
   every file and folder below (`D:P(A;OICI;FA;;;<user SID>)`). The folder
   is private from its first moment: it never has its parent's DACL, so no
   other account can open it in between, and there is no second call that a
   junction swapped in between could redirect. The call fails when anything
   already has that name.
   - `OpenProcessToken` and `GetTokenInformation(TokenUser)` read the
     current user's SID; `CloseHandle` closes the token.
   - `ConvertSidToStringSidW` turns the SID into text for the SDDL string.
   - `ConvertStringSecurityDescriptorToSecurityDescriptorW` builds the
     descriptor that `CreateDirectoryW` takes.
   - `LocalFree` frees what the system allocated.
2. **Check an existing folder** (`is_private`): it is used only when
   - its owner is the current user, or the token's default owner (an
     elevated administrator's new files belong to Administrators;
     `GetTokenInformation(TokenOwner)`), compared with `EqualSid`; and
   - its DACL is not null and every access-allowed entry names the current
     user. Deny entries are allowed; any other entry type is refused.
     `GetNamedSecurityInfoW` reads the owner and DACL, `GetAclInformation`
     the entry count, and `GetAce` each entry.
3. **Free space** (`free_bytes`): `GetDiskFreeSpaceExW`, the space the
   current user may still use, for the budget cap and the 64 MiB rule of
   ADR 0030 sections 4.1 and 4.5. The standard library has no free-space
   API on any OS (Unix uses `rustix::fs::statvfs`, which is safe).

FYLO never changes the owner or DACL of any folder: it sets a DACL only in
the call that creates a folder. Folders and files FYLO creates below the
cache folder (`PrivateFolder::create_folders` and `create_file`) get their
protection only by inheritance from the protected cache folder; on Windows
they are created with plain standard-library calls.
A folder that fails the check turns the temp tier off for that process, and
`fylo cache clear` refuses it (both leave it unchanged).

The volume check of ADR 0030 rule 4.1(4) uses the drive of the folder's
canonical path, which needs no unsafe code.

### SAFETY argument

Every unsafe block has a `SAFETY:` comment in the code. In short:

- **Inputs.** The `SECURITY_ATTRIBUTES` passed to `CreateDirectoryW` is
  fully initialized, states its own size, and points at a live descriptor
  that is freed only after the call. Every path and SDDL string is NUL-terminated UTF-16 built by
  `encode_wide` and kept alive for the whole call. Null is passed only for
  outputs and inputs the call documents as optional (group, SACL, owner on
  set, sizes).
- **Outputs.** Every output is an exclusively borrowed local slot or struct
  of exactly the type and size the call writes, and it outlives the call.
- **Memory the system allocates** (the descriptors from
  `GetNamedSecurityInfoW` and the SDDL conversion, the SID text) is freed
  exactly once with `LocalFree`, after the last use of every pointer into it
  (owner SID, DACL, ACEs). The work in between runs in a closure, so no
  pointer escapes.
- **The token** is opened with `TOKEN_QUERY` on the current process's
  pseudo handle (which needs no closing) and closed exactly once.
- **Token information** is read into a `Vec<u64>` (8-byte aligned) at least
  the size the first `GetTokenInformation` call reports. The `TOKEN_USER` or
  `TOKEN_OWNER` read at its start, and the SID it points to, are used only
  while that buffer lives.
- **ACEs** are read only at an index below the DACL's entry count; every ACE
  starts with an `ACE_HEADER`; the SID at `SidStart` is read only for the
  access-allowed type, whose layout is `ACCESS_ALLOWED_ACE`.
- **The SID text** is read up to its NUL terminator, never past it.
- No call is asynchronous and none keeps a pointer after it returns.

Miri cannot run foreign calls; Clippy runs on the Windows target.

### Tests

In `temp_tier::private_folder` (run in `windows-vm`, ARM64 Windows 11):

- a folder is private as it is created: right after `create_private`, its
  DACL grants only the current user and `is_private` is true, and a second
  `create_private` fails with `AlreadyExists`;
- a new folder made through `PrivateFolder` grants only the current user;
- folders and files created below it inherit that DACL;
- a folder created with the inherited `%TEMP%` DACL (wider: SYSTEM and
  Administrators) is refused and left unchanged;
- a junction in place of the folder is refused;
- a folder another account owns is not the user's: `%SystemRoot%`
  (owned by TrustedInstaller) fails the owner check and the whole check,
  while a folder the test created passes the owner check. On Unix the
  foreign-owner case also runs with an injected user id.

### Unsafe inventory

A new row in `docs/security/unsafe-inventory.md`, "Windows private temp
cache folder", lists every function with unsafe code in
`windows_folder.rs` and cites this ADR. `fylo-storage-native` denies unsafe
code outside `byte_range_lock/os.rs`, `container_file/punch.rs` and
`temp_tier/windows_folder.rs`.

## Consequences

- The temp cache folder is private on Windows as on Unix.
- One more unsafe boundary to review, Windows only: 387 lines with doc
  comments and SAFETY comments.
- `windows-sys` gains the `Win32_Security`, `Win32_Security_Authorization`
  and `Win32_System_Threading` features (the crate is already in the
  graph).

## Alternatives rejected

- **No DACL on Windows, rely on `%TEMP%` being per user** (choice C): the
  folder would grant SYSTEM and Administrators, and a folder under
  `--temp-cache-dir` could be open to anyone. The cache holds user data.
- **`icacls` and PowerShell child processes** (choice B): slow, needs
  external tools, localized output.
- **Create the folder, then set its DACL** (`SetNamedSecurityInfoW`): the
  first draft. Between the two calls the folder has its parent's DACL, so
  another account could open a handle and keep it, and the path could be
  swapped for a junction whose target would get the DACL.
- **Build the ACL by hand** (`InitializeAcl`, `AddAccessAllowedAceEx`): the
  same number of unsafe calls plus manual buffer sizing; the SDDL string is
  shorter and easier to review.
