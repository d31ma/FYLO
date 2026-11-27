//! Which filesystem holds a `.vdbx` file or a root, so the lease can decide
//! whether byte-range locks there are trusted (ADR 0016 section 7), and a root
//! on a network filesystem is refused (ADR 0029).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// The filesystem a `.vdbx` file is on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemInfo {
    /// The name the OS reports: `ext4`, `apfs`, `NTFS`, `smbfs`, `nfs`,
    /// `exfat`, or `unknown`.
    pub name: String,
    /// Whether the file is reached over a network, or served by another
    /// kernel (FUSE, a virtual machine's host share), so that locks taken
    /// here do not stop a writer somewhere else.
    pub remote: bool,
}

impl FilesystemInfo {
    const PROBE_PREFIX: &'static str = ".fylo-filesystem-probe";

    /// `(magic, name, remote)` for every Linux filesystem FYLO names. FUSE may be
    /// anything, sshfs and rclone included, so it is remote; so is a virtual
    /// machine's host share (virtiofs, 9p), whose files another kernel writes.
    const LINUX_MAGIC: &[(u64, &str, bool)] = &[
        (0xEF53, "ext4", false),
        (0xF2F5_2010, "f2fs", false),
        (0x5846_5342, "xfs", false),
        (0x9123_683E, "btrfs", false),
        (0x2FC1_2FC1, "zfs", false),
        (0x0102_1994, "tmpfs", false),
        (0x794C_7630, "overlay", false),
        (0x4D44, "vfat", false),
        (0x2011_BAB0, "exfat", false),
        (0x7366_746E, "ntfs3", false),
        (0xCA45_1A4E, "bcachefs", false),
        (0x6969, "nfs", true),
        (0xFF53_4D42, "cifs", true),
        (0xFE53_4D42, "smb2", true),
        (0x517B, "smbfs", true),
        (0x564C, "ncpfs", true),
        (0x0102_1997, "9p", true),
        (0x00C3_6400, "ceph", true),
        (0x5346_414F, "afs", true),
        (0x6B41_4653, "kafs", true),
        (0x7375_7245, "coda", true),
        (0x0BD0_0BD0, "lustre", true),
        (0x4750_4653, "gpfs", true),
        (0x2003_0528, "orangefs", true),
        (0x6A65_6A63, "virtiofs", true),
        (0x6573_5546, "fuse", true),
    ];

    /// A UNC path, and its long form; the local device prefixes also start with
    /// two backslashes but name this machine.
    const WINDOWS_UNC_PREFIX: &str = r"\\";
    const WINDOWS_UNC_LONG_PREFIX: &str = r"\\?\UNC\";
    const WINDOWS_DEVICE_PREFIXES: [&str; 2] = [r"\\?\", r"\\.\"];

    /// `f_fstypename` values that are network filesystems on Apple systems.
    /// `MNT_LOCAL` already marks them; the list keeps them remote should a
    /// mount claim to be local.
    const APPLE_REMOTE: &[&str] = &["nfs", "smbfs", "webdav", "afpfs", "cifs", "ftp"];

    /// Detect the filesystem of an open file.
    ///
    /// # Errors
    ///
    /// Returns the OS failure to describe the file's volume.
    pub fn of(file: &File) -> io::Result<Self> {
        detect(file)
    }

    /// Detect the filesystem of `directory`, through a probe file this
    /// creates in it and removes again.
    ///
    /// # Errors
    ///
    /// Returns the OS failure to create, describe or remove the probe file.
    pub fn of_directory(directory: &Path) -> io::Result<Self> {
        Self::of_probe(&directory.join(crate::write::unique_name(Self::PROBE_PREFIX)))
    }

    /// Detect the filesystem that holds `path`, without writing anything.
    /// A missing path is described by its nearest existing parent, the
    /// filesystem it would be created on.
    ///
    /// # Errors
    ///
    /// Returns the OS failure to resolve or describe the path's volume.
    pub fn of_path(path: &Path) -> io::Result<Self> {
        let absolute = std::path::absolute(path)?;
        detect_path(Self::nearest_existing(&absolute)?)
    }

    fn nearest_existing(path: &Path) -> io::Result<&Path> {
        path.ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    /// The filesystem a Linux `f_type` magic number names. NFS gets its
    /// version from the mount table afterwards. A magic number not listed is
    /// `unknown` and local: it is never qualified for shared locks, but a
    /// root on it is not refused.
    #[must_use]
    pub fn from_linux_magic(magic: u64) -> Self {
        let (name, remote) = Self::linux_entry(magic);
        Self {
            name: name.to_owned(),
            remote,
        }
    }

    fn linux_entry(magic: u64) -> (&'static str, bool) {
        for &(number, name, remote) in Self::LINUX_MAGIC {
            if number == magic {
                return (name, remote);
            }
        }
        ("unknown", false)
    }

    /// The filesystem an Apple `statfs` describes: `name` is
    /// `f_fstypename`, and `local` whether `MNT_LOCAL` is set. A name in
    /// the network list is remote even with `MNT_LOCAL` set.
    #[must_use]
    pub fn from_apple(name: &str, local: bool) -> Self {
        Self {
            name: name.to_owned(),
            remote: Self::apple_remote(name, local),
        }
    }

    fn apple_remote(name: &str, local: bool) -> bool {
        !local || Self::APPLE_REMOTE.contains(&name)
    }

    /// Whether a Windows path names a network share by itself: a UNC path
    /// (`\\server\share`, `\\?\UNC\server\share`). A mapped drive letter
    /// is found remote from its open handle instead.
    #[must_use]
    pub fn windows_path_is_remote(path: &str) -> bool {
        Self::is_unc(path)
    }

    fn is_unc(path: &str) -> bool {
        if path.starts_with(Self::WINDOWS_UNC_LONG_PREFIX) {
            return true;
        }
        path.starts_with(Self::WINDOWS_UNC_PREFIX)
            && !Self::WINDOWS_DEVICE_PREFIXES
                .iter()
                .any(|prefix| path.starts_with(prefix))
    }

    fn of_probe(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let detected = detect(&file);
        drop(file);
        std::fs::remove_file(path)?;
        detected
    }

    /// Whether byte-range locks on this filesystem have passed the
    /// multi-process harness, so that several processes may share a file on
    /// it. Everywhere else the lock-free lease (ADR 0016 section 7) lets one
    /// writer at a time in, and readers in freely.
    ///
    /// The list grows only with a passed row in
    /// `docs/operations/vdbx-byte-range-lock-qualification.md`.
    pub fn byte_range_locks_qualified(&self) -> bool {
        QUALIFIED.iter().any(|&(os, name, remote)| {
            os == std::env::consts::OS && name == self.name && remote == self.remote
        })
    }
}

/// `(OS, filesystem name, remote)` for every filesystem that has passed.
const QUALIFIED: &[(&str, &str, bool)] = &[
    ("android", "ext4", false),
    ("linux", "ext4", false),
    ("linux", "tmpfs", false),
    ("macos", "apfs", false),
    ("windows", "NTFS", false),
];

#[cfg(any(target_os = "linux", target_os = "android"))]
fn detect(file: &File) -> io::Result<FilesystemInfo> {
    use std::os::unix::fs::MetadataExt;
    let stat = rustix::fs::fstatfs(file)?;
    Ok(linux_info(magic(stat.f_type), || {
        file.metadata().ok().map(|metadata| metadata.dev())
    }))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn detect_path(path: &Path) -> io::Result<FilesystemInfo> {
    use std::os::unix::fs::MetadataExt;
    let stat = rustix::fs::statfs(path)?;
    Ok(linux_info(magic(stat.f_type), || {
        std::fs::metadata(path).ok().map(|metadata| metadata.dev())
    }))
}

/// The filesystem for a magic number; an NFS mount is named by its version
/// from the mount table, found through the device `dev` returns.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn linux_info(magic: u64, dev: impl FnOnce() -> Option<u64>) -> FilesystemInfo {
    let info = FilesystemInfo::from_linux_magic(magic);
    if info.name != "nfs" {
        return info;
    }
    FilesystemInfo {
        name: dev().and_then(nfs_name).unwrap_or(info.name),
        remote: true,
    }
}

/// One NFS magic number covers every version, so the mount table tells them
/// apart: `nfs4.1` for an NFS 4.1 mount, and `nfs-local-lock` for a mount
/// whose locks stay on this client (`local_lock=posix` or `all`, or `nolock`),
/// which is never qualified.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn nfs_name(dev: u64) -> Option<String> {
    let device = format!("{}:{}", rustix::fs::major(dev), rustix::fs::minor(dev));
    let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    nfs_name_in(&table, &device)
}

/// The NFS name for `device` (`major:minor`) in a `/proc/self/mountinfo`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn nfs_name_in(table: &str, device: &str) -> Option<String> {
    let line = table
        .lines()
        .find(|line| line.split(' ').nth(2) == Some(device))?;
    let (mount, filesystem) = line.split_once(" - ")?;
    let options = mount
        .split(' ')
        .nth(5)
        .into_iter()
        .chain(filesystem.split(' ').nth(2))
        .flat_map(|options| options.split(','))
        .collect::<Vec<_>>();
    if options
        .iter()
        .any(|option| matches!(*option, "nolock" | "local_lock=posix" | "local_lock=all"))
    {
        return Some("nfs-local-lock".to_owned());
    }
    let version = options
        .iter()
        .find_map(|option| option.strip_prefix("vers="))?;
    Some(format!("nfs{version}"))
}

/// `f_type` is signed on some targets and unsigned on others; the magic
/// numbers are its low 32 bits.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn magic<T>(value: T) -> u64
where
    i128: From<T>,
{
    u64::try_from(i128::from(value) & 0xFFFF_FFFF).expect("masked to 32 bits")
}

#[cfg(target_vendor = "apple")]
fn detect(file: &File) -> io::Result<FilesystemInfo> {
    Ok(apple_info(&rustix::fs::fstatfs(file)?))
}

#[cfg(target_vendor = "apple")]
fn detect_path(path: &Path) -> io::Result<FilesystemInfo> {
    Ok(apple_info(&rustix::fs::statfs(path)?))
}

#[cfg(target_vendor = "apple")]
fn apple_info(stat: &rustix::fs::StatFs) -> FilesystemInfo {
    let name: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.to_ne_bytes()[0])
        .collect();
    FilesystemInfo::from_apple(
        &String::from_utf8_lossy(&name),
        stat.f_flags & libc::MNT_LOCAL.cast_unsigned() != 0,
    )
}

/// Elsewhere the filesystem is not known. It is never qualified for shared
/// locks, and a root on it is not refused.
#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android")),
    not(target_vendor = "apple")
))]
fn detect(_file: &File) -> io::Result<FilesystemInfo> {
    Ok(FilesystemInfo {
        name: "unknown".to_owned(),
        remote: false,
    })
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android")),
    not(target_vendor = "apple")
))]
fn detect_path(_path: &Path) -> io::Result<FilesystemInfo> {
    Ok(FilesystemInfo {
        name: "unknown".to_owned(),
        remote: false,
    })
}

#[cfg(windows)]
fn detect(file: &File) -> io::Result<FilesystemInfo> {
    let (name, remote) = super::os::filesystem(file)?;
    Ok(FilesystemInfo { name, remote })
}

/// A directory opens on Windows only with backup semantics; a UNC path is
/// remote whatever its handle reports.
#[cfg(windows)]
fn detect_path(path: &Path) -> io::Result<FilesystemInfo> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let (name, remote) = super::os::filesystem(&file)?;
    let unc = FilesystemInfo::windows_path_is_remote(&path.to_string_lossy());
    Ok(FilesystemInfo {
        name,
        remote: remote || unc,
    })
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::nfs_name_in;

    const TABLE: &str = "\
22 1 0:21 / / rw,relatime - ext4 /dev/vda rw
40 22 0:45 / /mnt/nfs rw,relatime - nfs4 server:/ rw,vers=4.1,rsize=1048576,hard,proto=tcp,local_lock=none,addr=172.18.0.2
41 22 0:46 / /mnt/old rw,relatime - nfs server:/old rw,vers=3,nolock,proto=tcp
42 22 0:47 / /mnt/local rw,relatime - nfs4 server:/ rw,vers=4.2,local_lock=all
";

    #[test]
    fn nfs_mounts_are_named_by_version_and_local_locking() {
        assert_eq!(nfs_name_in(TABLE, "0:45").as_deref(), Some("nfs4.1"));
        assert_eq!(
            nfs_name_in(TABLE, "0:46").as_deref(),
            Some("nfs-local-lock")
        );
        assert_eq!(
            nfs_name_in(TABLE, "0:47").as_deref(),
            Some("nfs-local-lock")
        );
        assert_eq!(nfs_name_in(TABLE, "9:9"), None);
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::FilesystemInfo;

    fn linux(magic: u64) -> (String, bool) {
        let info = FilesystemInfo::from_linux_magic(magic);
        (info.name, info.remote)
    }

    #[test]
    fn linux_network_filesystems_are_remote() {
        assert_eq!(linux(0x6969), ("nfs".to_owned(), true));
        assert_eq!(linux(0xFF53_4D42), ("cifs".to_owned(), true));
        assert_eq!(linux(0xFE53_4D42), ("smb2".to_owned(), true));
        assert_eq!(linux(0x517B), ("smbfs".to_owned(), true));
        assert_eq!(linux(0x564C), ("ncpfs".to_owned(), true));
        assert_eq!(linux(0x0102_1997), ("9p".to_owned(), true));
        assert_eq!(linux(0x00C3_6400), ("ceph".to_owned(), true));
        assert_eq!(linux(0x5346_414F), ("afs".to_owned(), true));
        assert_eq!(linux(0x6B41_4653), ("kafs".to_owned(), true));
        assert_eq!(linux(0x7375_7245), ("coda".to_owned(), true));
        assert_eq!(linux(0x0BD0_0BD0), ("lustre".to_owned(), true));
        assert_eq!(linux(0x4750_4653), ("gpfs".to_owned(), true));
        assert_eq!(linux(0x2003_0528), ("orangefs".to_owned(), true));
    }

    #[test]
    fn linux_fuse_and_virtual_machine_shares_are_remote() {
        assert_eq!(linux(0x6573_5546), ("fuse".to_owned(), true));
        assert_eq!(linux(0x6A65_6A63), ("virtiofs".to_owned(), true));
    }

    #[test]
    fn linux_local_filesystems_are_local() {
        assert_eq!(linux(0xEF53), ("ext4".to_owned(), false));
        assert_eq!(linux(0xF2F5_2010), ("f2fs".to_owned(), false));
        assert_eq!(linux(0x5846_5342), ("xfs".to_owned(), false));
        assert_eq!(linux(0x9123_683E), ("btrfs".to_owned(), false));
        assert_eq!(linux(0x2FC1_2FC1), ("zfs".to_owned(), false));
        assert_eq!(linux(0x0102_1994), ("tmpfs".to_owned(), false));
        assert_eq!(linux(0x794C_7630), ("overlay".to_owned(), false));
        assert_eq!(linux(0x4D44), ("vfat".to_owned(), false));
        assert_eq!(linux(0x2011_BAB0), ("exfat".to_owned(), false));
        assert_eq!(linux(0x7366_746E), ("ntfs3".to_owned(), false));
        assert_eq!(linux(0xCA45_1A4E), ("bcachefs".to_owned(), false));
    }

    #[test]
    fn an_unlisted_linux_magic_is_unknown_and_local() {
        assert_eq!(linux(0x1234_5678), ("unknown".to_owned(), false));
    }

    #[test]
    fn apple_network_filesystems_are_remote() {
        for name in ["nfs", "smbfs", "webdav", "afpfs", "cifs", "ftp"] {
            assert!(FilesystemInfo::from_apple(name, false).remote, "{name}");
            assert!(FilesystemInfo::from_apple(name, true).remote, "{name}");
        }
    }

    #[test]
    fn an_apple_mount_without_mnt_local_is_remote() {
        let info = FilesystemInfo::from_apple("macfuse", false);
        assert_eq!(info.name, "macfuse");
        assert!(info.remote);
    }

    #[test]
    fn apple_local_filesystems_are_local() {
        for name in ["apfs", "hfs", "msdos", "exfat"] {
            assert!(!FilesystemInfo::from_apple(name, true).remote, "{name}");
        }
    }

    #[test]
    fn windows_unc_paths_are_remote() {
        assert!(FilesystemInfo::windows_path_is_remote(
            r"\\server\share\root"
        ));
        assert!(FilesystemInfo::windows_path_is_remote(
            r"\\?\UNC\server\share\root"
        ));
        assert!(FilesystemInfo::windows_path_is_remote(
            r"\\wsl$\Ubuntu\root"
        ));
    }

    #[test]
    fn windows_drive_and_device_paths_are_not_remote_by_name() {
        assert!(!FilesystemInfo::windows_path_is_remote(r"C:\data\root"));
        assert!(!FilesystemInfo::windows_path_is_remote(r"\\?\C:\data\root"));
        assert!(!FilesystemInfo::windows_path_is_remote(r"\\.\C:\data\root"));
    }

    #[test]
    fn a_missing_path_is_described_by_its_nearest_existing_parent() {
        let missing = std::env::temp_dir().join("fylo-filesystem-missing/a/b");
        let parent = FilesystemInfo::of_path(&std::env::temp_dir()).unwrap();
        assert_eq!(FilesystemInfo::of_path(&missing).unwrap(), parent);
        assert!(!missing.exists());
    }
}
