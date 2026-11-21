use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::disk_reader::DiskReader;
use super::disk_source::DiskSource;
use super::system_disk_refusal::SystemDiskRefusal;

/// The rule that a root's parent folder is a mount point (ADR 0033 section
/// 5.1, rule 1): the root folder sits directly in the top folder of a
/// drive.
pub(crate) struct VolumeRoot;

impl VolumeRoot {
    const VERBATIM_PREFIX: &'static str = r"\\?\";

    /// The mount point the root at `path` lives on: its parent's, which the
    /// handshake reports as `storage.volume`. `None` when FYLO cannot tell.
    pub(crate) fn of(path: &Path) -> Option<PathBuf> {
        DiskReader.mount_point(&Self::parent(path)).ok()
    }

    /// Admit the root at `path` when its parent folder is a mount point.
    ///
    /// # Errors
    ///
    /// Returns `NotVolumeRoot`, naming the nearest mount point, when the
    /// parent is not a mount point, and `CouldNotTell` when the mount
    /// point cannot be read.
    pub(crate) fn check(disks: &dyn DiskSource, path: &Path) -> Result<PathBuf, SystemDiskRefusal> {
        let parent = Self::parent(path);
        let mount = disks
            .mount_point(&parent)
            .map_err(|reason| SystemDiskRefusal::CouldNotTell { reason })?;
        Self::judge(parent, mount)
    }

    fn judge(parent: PathBuf, mount: PathBuf) -> Result<PathBuf, SystemDiskRefusal> {
        if Self::key(&parent) == Self::key(&mount) {
            return Ok(mount);
        }
        Err(SystemDiskRefusal::NotVolumeRoot { parent, mount })
    }

    /// The folder that holds `path`; the top folder `/` holds itself.
    fn parent(path: &Path) -> PathBuf {
        path.parent().unwrap_or(path).to_path_buf()
    }

    /// `path` as its parts, without a Windows `\\?\` prefix or a trailing
    /// separator, so `\\?\D:\` and `D:\` compare equal.
    fn key(path: &Path) -> Vec<String> {
        let text = path.to_string_lossy();
        let plain = text.strip_prefix(Self::VERBATIM_PREFIX).unwrap_or(&text);
        Path::new(plain)
            .components()
            .map(|part| Self::fold(part.as_os_str()))
            .collect()
    }

    /// Windows paths ignore case.
    #[cfg(windows)]
    fn fold(part: &OsStr) -> String {
        part.to_string_lossy().to_lowercase()
    }

    #[cfg(not(windows))]
    fn fold(part: &OsStr) -> String {
        part.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::VolumeRoot;
    use crate::system_disk::disk_description::DiskDescription;
    use crate::system_disk::disk_source::DiskSource;
    use crate::system_disk::system_disk_refusal::SystemDiskRefusal;
    use std::path::{Path, PathBuf};

    /// Mount points `/`, `/Volumes/Data` and `/mnt/share`.
    struct FakeMounts;

    impl DiskSource for FakeMounts {
        fn system(&self) -> Result<DiskDescription, String> {
            Ok(DiskDescription::named("system"))
        }

        fn describe(&self, _path: &Path) -> Result<DiskDescription, String> {
            Ok(DiskDescription::named("system"))
        }

        fn images(&self, _disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
            Ok(Vec::new())
        }

        fn remote(&self, _path: &Path) -> Result<bool, String> {
            Ok(false)
        }

        fn mount_point(&self, path: &Path) -> Result<PathBuf, String> {
            ["/Volumes/Data", "/mnt/share", "/"]
                .into_iter()
                .find(|mount| path.starts_with(mount))
                .map(PathBuf::from)
                .ok_or_else(|| "no mount".to_owned())
        }
    }

    struct BrokenMounts;

    impl DiskSource for BrokenMounts {
        fn system(&self) -> Result<DiskDescription, String> {
            Err("unused".to_owned())
        }

        fn describe(&self, _path: &Path) -> Result<DiskDescription, String> {
            Err("unused".to_owned())
        }

        fn images(&self, _disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
            Err("unused".to_owned())
        }

        fn remote(&self, _path: &Path) -> Result<bool, String> {
            Err("unused".to_owned())
        }

        fn mount_point(&self, _path: &Path) -> Result<PathBuf, String> {
            Err("statfs failed".to_owned())
        }
    }

    #[test]
    fn a_root_directly_below_a_mount_point_is_admitted() {
        let mount = VolumeRoot::check(&FakeMounts, Path::new("/Volumes/Data/mesail")).unwrap();
        assert_eq!(mount, PathBuf::from("/Volumes/Data"));
    }

    #[test]
    fn a_root_directly_below_a_network_mount_point_is_admitted() {
        let mount = VolumeRoot::check(&FakeMounts, Path::new("/mnt/share/mesail")).unwrap();
        assert_eq!(mount, PathBuf::from("/mnt/share"));
    }

    #[test]
    fn a_nested_root_is_refused_and_names_the_nearest_mount_point() {
        let refusal = VolumeRoot::check(&FakeMounts, Path::new("/Volumes/Data/roots/job/mesail"))
            .unwrap_err();
        assert_eq!(
            refusal,
            SystemDiskRefusal::NotVolumeRoot {
                parent: PathBuf::from("/Volumes/Data/roots/job"),
                mount: PathBuf::from("/Volumes/Data"),
            }
        );
    }

    #[test]
    fn a_root_that_is_itself_a_mount_point_is_refused() {
        let refusal = VolumeRoot::check(&FakeMounts, Path::new("/Volumes/Data")).unwrap_err();
        assert!(matches!(refusal, SystemDiskRefusal::NotVolumeRoot { .. }));
    }

    #[test]
    fn a_root_in_the_top_folder_passes_this_rule() {
        let mount = VolumeRoot::check(&FakeMounts, Path::new("/mesail")).unwrap();
        assert_eq!(mount, PathBuf::from("/"));
    }

    #[test]
    fn a_mount_point_that_cannot_be_read_cannot_be_told() {
        let refusal = VolumeRoot::check(&BrokenMounts, Path::new("/Volumes/Data/x")).unwrap_err();
        assert_eq!(
            refusal,
            SystemDiskRefusal::CouldNotTell {
                reason: "statfs failed".to_owned()
            }
        );
    }

    #[test]
    fn a_verbatim_prefix_and_a_trailing_separator_do_not_matter() {
        assert_eq!(
            VolumeRoot::key(Path::new(r"\\?\/mnt/share/")),
            VolumeRoot::key(Path::new("/mnt/share"))
        );
    }

    /// macOS firmlinks: the temp folder `/private/var/...` is on the mount
    /// `/System/Volumes/Data`, which is not a prefix of its path.
    #[test]
    fn the_volume_of_a_missing_root_in_the_temp_folder_is_read() {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = temp.join("fylo-volume-root-test").join("root");
        let volume = VolumeRoot::of(&root).unwrap();
        assert!(volume.is_absolute(), "{}", volume.display());
        assert_ne!(volume, temp.join("fylo-volume-root-test"));
    }
}
