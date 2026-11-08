# AppContainer sample (Windows)

A packaged (MSIX) application runs in an AppContainer when its manifest
does not ask for full trust. [`AppxManifest.xml`](AppxManifest.xml) is such
a manifest, with no capabilities. This note covers the parts a manifest
cannot say. Not tested by FYLO's CI.

## Where the database lives

- **Simplest:** in the package's own `LocalState` folder
  (`ApplicationData.Current.LocalFolder`). Windows gives that folder to the
  package alone; nothing else is needed.
- **A shared folder** (for example `C:\ProgramData\ExampleApp`): grant the
  package's SID, and only it, modify rights, and remove inherited rights
  other users would get. The package SID comes from the package family
  name:

  ```powershell
  # CHANGE: the package name and the folder.
  Add-Type -Namespace Win32 -Name AppContainer -MemberDefinition @'
  [DllImport("userenv.dll", CharSet = CharSet.Unicode)]
  public static extern int DeriveAppContainerSidFromAppContainerName(string name, out IntPtr sid);
  [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern bool ConvertSidToStringSid(IntPtr sid, out string text);
  '@
  $family = (Get-AppxPackage Example.FyloApp).PackageFamilyName
  $pointer = [IntPtr]::Zero
  [Win32.AppContainer]::DeriveAppContainerSidFromAppContainerName($family, [ref]$pointer) | Out-Null
  $sid = $null
  [Win32.AppContainer]::ConvertSidToStringSid($pointer, [ref]$sid) | Out-Null
  icacls C:\ProgramData\ExampleApp /inheritance:r
  icacls C:\ProgramData\ExampleApp /grant "*${sid}:(OI)(CI)M" "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F"
  ```

  (`S-1-5-18` is SYSTEM and `S-1-5-32-544` Administrators.)

  FYLO needs read, write, create and rename in that folder: it opens the
  `.vdbx` file read-write with byte-range locks (`LockFileEx`), and a
  rekey or pack writes `<db>.vdbx.pack-staging*` beside it and renames it
  over the database.

## The identity's key

- **`tpm:<name>`** (phase 8): the key stays in the TPM and cannot be
  exported by anyone, the user's other processes included. Prefer it on
  machines with a TPM 2.0.
- **`cng:<name>`**: a per-user key of the Microsoft Software Key Storage
  Provider, protected by DPAPI. Any process of the same user can use it;
  the AppContainer limits only what this application can reach, not what
  others can. That is the limit the study records for unpackaged Windows
  applications (study section 6, Q3).

## What it does not do

An Administrator, or a process running as SYSTEM, can still read the
folder and the application's memory (study section 6).
