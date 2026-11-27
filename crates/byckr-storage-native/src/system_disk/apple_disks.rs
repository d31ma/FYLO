use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::bsd_disk_name::BsdDiskName;
use super::disk_description::DiskDescription;
use super::disk_image_table::DiskImageTable;
use super::tool_runner::ToolRunner;

/// The disk that holds a path on macOS (ADR 0032 section 2): the whole disk
/// of the device `statfs` reports, which for APFS is the container. Disk
/// images are found through `/usr/bin/hdiutil info -plist`, at most once per
/// process (twice with the retry), and only for a root that is not on the
/// system disk.
pub(crate) struct AppleDisks;

impl AppleDisks {
    /// The whole disk of the device that holds the existing `path`.
    pub(crate) fn describe(path: &Path) -> Result<DiskDescription, String> {
        let stat = rustix::fs::statfs(path)
            .map_err(|error| format!("statfs of {} failed ({error})", path.display()))?;
        let device: Vec<u8> = stat
            .f_mntfromname
            .iter()
            .take_while(|&&character| character != 0)
            .map(|&character| character.to_ne_bytes()[0])
            .collect();
        let disk = BsdDiskName::whole_disk(&String::from_utf8_lossy(&device))?;
        Ok(DiskDescription::named(&disk))
    }

    /// The mount point of the filesystem that holds the existing `path`:
    /// `statfs`'s `f_mntonname`.
    pub(crate) fn mount_point(path: &Path) -> Result<PathBuf, String> {
        let stat = rustix::fs::statfs(path)
            .map_err(|error| format!("statfs of {} failed ({error})", path.display()))?;
        let mount: Vec<u8> = stat
            .f_mntonname
            .iter()
            .take_while(|&&character| character != 0)
            .map(|&character| character.to_ne_bytes()[0])
            .collect();
        Ok(PathBuf::from(String::from_utf8_lossy(&mount).into_owned()))
    }

    /// The file of the image that presents `disk`; none for a real disk.
    pub(crate) fn images(disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
        let table = Self::table()?;
        if table.presents_without_file(&disk.label) {
            return Err("hdiutil names no file for this disk image".to_owned());
        }
        Ok(table
            .backing_of(&disk.label)
            .map(Path::to_path_buf)
            .into_iter()
            .collect())
    }

    fn table() -> Result<&'static DiskImageTable, String> {
        static TABLE: OnceLock<Result<DiskImageTable, String>> = OnceLock::new();
        TABLE
            .get_or_init(|| ToolRunner::hdiutil().run(DiskImageTable::parse))
            .as_ref()
            .map_err(Clone::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::AppleDisks;
    use std::path::Path;

    #[test]
    fn the_temp_folder_and_the_boot_volume_share_the_system_disk() {
        let system = AppleDisks::describe(Path::new("/")).unwrap();
        let temp =
            AppleDisks::describe(&std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
        assert!(temp.shares_disk_with(&system), "{temp:?} {system:?}");
        assert!(system.label.starts_with("disk"), "{system:?}");
    }

    #[test]
    fn the_boot_volume_is_mounted_at_the_top_folder() {
        assert_eq!(
            AppleDisks::mount_point(Path::new("/")).unwrap(),
            Path::new("/")
        );
    }

    #[test]
    fn the_system_disk_is_not_an_image() {
        let system = AppleDisks::describe(Path::new("/")).unwrap();
        assert!(AppleDisks::images(&system).unwrap().is_empty());
    }
}
