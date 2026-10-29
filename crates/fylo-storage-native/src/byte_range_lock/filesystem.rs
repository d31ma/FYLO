//! Which filesystem holds a `.fxdb` file, so the lease can decide whether
//! byte-range locks there are trusted (ADR 0016 section 7).

use std::fs::File;
use std::io;

/// The filesystem a `.fxdb` file is on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemInfo {
    /// The name the OS reports: `ext4`, `apfs`, `NTFS`, `smbfs`, `nfs`,
    /// `exfat`, or `unknown`.
    pub name: String,
    /// Whether the file is reached over a network.
    pub remote: bool,
}

impl FilesystemInfo {
    /// Detect the filesystem of an open file.
    ///
    /// # Errors
    ///
    /// Returns the OS failure to describe the file's volume.
    pub fn of(file: &File) -> io::Result<Self> {
        detect(file)
    }

    /// Whether byte-range locks on this filesystem have passed the
    /// multi-process harness, so that several processes may share a file on
    /// it. Everywhere else the lock-free lease (ADR 0016 section 7) lets one
    /// writer at a time in, and readers in freely.
    ///
    /// The list grows only with a passed row in
    /// `docs/operations/fxdb-byte-range-lock-qualification.md`.
    pub fn byte_range_locks_qualified(&self) -> bool {
        QUALIFIED.iter().any(|&(os, name, remote)| {
            os == std::env::consts::OS && name == self.name && remote == self.remote
        })
    }
}

/// `(OS, filesystem name, remote)` for every filesystem that has passed.
const QUALIFIED: &[(&str, &str, bool)] = &[("linux", "ext4", false), ("linux", "tmpfs", false)];

#[cfg(target_os = "linux")]
fn detect(file: &File) -> io::Result<FilesystemInfo> {
    let stat = rustix::fs::fstatfs(file)?;
    let magic = magic(stat.f_type);
    let (name, remote) = match magic {
        0xEF53 => ("ext4", false),
        0x5846_5342 => ("xfs", false),
        0x9123_683E => ("btrfs", false),
        0x2FC1_2FC1 => ("zfs", false),
        0x0102_1994 => ("tmpfs", false),
        0x794C_7630 => ("overlay", false),
        0x4D44 => ("vfat", false),
        0x2011_BAB0 => ("exfat", false),
        0x7366_746E => ("ntfs3", false),
        0x6969 => ("nfs", true),
        0xFF53_4D42 => ("cifs", true),
        0xFE53_4D42 => ("smb2", true),
        0x0102_1997 => ("9p", true),
        0x00C3_6400 => ("ceph", true),
        0x6A65_6A63 => ("virtiofs", true),
        // FUSE may be anything, sshfs included: treat it as remote.
        0x6573_5546 => ("fuse", true),
        _ => ("unknown", true),
    };
    Ok(FilesystemInfo {
        name: name.to_owned(),
        remote,
    })
}

/// `f_type` is signed on some targets and unsigned on others; the magic
/// numbers are its low 32 bits.
#[cfg(target_os = "linux")]
fn magic<T>(value: T) -> u64
where
    i128: From<T>,
{
    u64::try_from(i128::from(value) & 0xFFFF_FFFF).expect("masked to 32 bits")
}

#[cfg(target_vendor = "apple")]
fn detect(file: &File) -> io::Result<FilesystemInfo> {
    let stat = rustix::fs::fstatfs(file)?;
    let name: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.to_ne_bytes()[0])
        .collect();
    Ok(FilesystemInfo {
        name: String::from_utf8_lossy(&name).into_owned(),
        remote: stat.f_flags & libc::MNT_LOCAL.cast_unsigned() == 0,
    })
}

#[cfg(all(unix, not(target_os = "linux"), not(target_vendor = "apple")))]
fn detect(_file: &File) -> io::Result<FilesystemInfo> {
    Ok(FilesystemInfo {
        name: "unknown".to_owned(),
        remote: true,
    })
}

#[cfg(windows)]
fn detect(file: &File) -> io::Result<FilesystemInfo> {
    let (name, remote) = super::os::filesystem(file)?;
    Ok(FilesystemInfo { name, remote })
}
