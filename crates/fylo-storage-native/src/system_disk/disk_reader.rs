use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::app_folders::{AppFolderInputs, AppPlatform};
use super::disk_description::DiskDescription;
use super::disk_source::DiskSource;

/// The real answers to [`DiskSource`] on the OS FYLO runs on (ADR 0032
/// section 2): macOS through `statfs` and `hdiutil`, Linux through
/// `/proc/self/mountinfo` and `/sys`, Windows through its volume calls.
/// Other operating systems cannot tell, so a root there needs the opt-out.
pub(crate) struct DiskReader;

impl DiskReader {
    /// iOS and Android have no other disk: the rule runs in app mode there.
    pub(crate) const MOBILE: bool = cfg!(any(target_os = "ios", target_os = "android"));

    /// The OS whose app-folder rules apply.
    pub(crate) const fn platform() -> AppPlatform {
        if cfg!(target_os = "ios") {
            AppPlatform::Ios
        } else if cfg!(target_os = "android") {
            AppPlatform::Android
        } else if cfg!(target_vendor = "apple") {
            AppPlatform::MacOs
        } else if cfg!(windows) {
            AppPlatform::Windows
        } else {
            AppPlatform::Linux
        }
    }

    /// The OS values app folders are built from.
    pub(crate) fn app_folder_inputs() -> AppFolderInputs {
        let (local_app_data, roaming_app_data) = Self::windows_app_data();
        AppFolderInputs {
            home: std::env::home_dir(),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            local_app_data,
            roaming_app_data,
        }
    }

    /// Whether this process's user owns `path`'s nearest existing folder.
    #[cfg(unix)]
    pub(crate) fn owned_by_process(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let owner = path
            .ancestors()
            .find_map(|ancestor| std::fs::metadata(ancestor).ok())
            .map(|metadata| metadata.uid());
        owner == Some(rustix::process::geteuid().as_raw())
    }

    #[cfg(not(unix))]
    pub(crate) fn owned_by_process(_path: &Path) -> bool {
        false
    }

    #[cfg(windows)]
    fn windows_app_data() -> (Option<PathBuf>, Option<PathBuf>) {
        super::windows_disks::WindowsDisks::app_data_folders()
    }

    #[cfg(not(windows))]
    fn windows_app_data() -> (Option<PathBuf>, Option<PathBuf>) {
        (None, None)
    }

    /// The canonical nearest existing folder of `path`: the place a missing
    /// root would be created.
    fn existing(path: &Path) -> Result<PathBuf, String> {
        let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
        let existing = absolute
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or_else(|| format!("no part of {} exists", path.display()))?;
        std::fs::canonicalize(existing).map_err(|error| error.to_string())
    }

    #[cfg(target_os = "macos")]
    fn describe_existing(path: &Path) -> Result<DiskDescription, String> {
        super::apple_disks::AppleDisks::describe(path)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn describe_existing(path: &Path) -> Result<DiskDescription, String> {
        let table = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|error| format!("/proc/self/mountinfo cannot be read ({error})"))?;
        super::linux_disks::LinuxDisks::new(&table, Path::new("/sys")).describe(path)
    }

    #[cfg(windows)]
    fn describe_existing(path: &Path) -> Result<DiskDescription, String> {
        super::windows_disks::WindowsDisks::describe(path)
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        windows
    )))]
    fn describe_existing(_path: &Path) -> Result<DiskDescription, String> {
        Err("FYLO cannot tell disks apart on this operating system".to_owned())
    }

    #[cfg(windows)]
    fn system_disk() -> Result<DiskDescription, String> {
        super::windows_disks::WindowsDisks::system()
    }

    #[cfg(not(windows))]
    fn system_disk() -> Result<DiskDescription, String> {
        Self::describe_existing(Path::new("/"))
    }

    #[cfg(target_os = "macos")]
    fn images_of(disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
        super::apple_disks::AppleDisks::images(disk)
    }

    #[cfg(windows)]
    fn images_of(disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
        super::windows_disks::WindowsDisks::images(disk)
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the macOS and Windows forms can fail, and all forms share one signature"
    )]
    fn images_of(disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
        Ok(disk.images.clone())
    }
}

impl DiskSource for DiskReader {
    /// Read once per process: the system disk does not move.
    fn system(&self) -> Result<DiskDescription, String> {
        static SYSTEM: OnceLock<Result<DiskDescription, String>> = OnceLock::new();
        SYSTEM.get_or_init(Self::system_disk).clone()
    }

    fn describe(&self, path: &Path) -> Result<DiskDescription, String> {
        Self::describe_existing(&Self::existing(path)?)
    }

    fn images(&self, disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
        Self::images_of(disk)
    }

    fn remote(&self, path: &Path) -> Result<bool, String> {
        crate::FilesystemInfo::of_path(path)
            .map(|filesystem| filesystem.remote)
            .map_err(|error| {
                format!(
                    "the filesystem of {} cannot be read ({error})",
                    path.display()
                )
            })
    }
}
