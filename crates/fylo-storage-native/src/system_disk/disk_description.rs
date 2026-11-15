use std::path::PathBuf;

/// The disk that holds a path, as the OS describes it (ADR 0032 section 2).
///
/// `keys` name the disk: on macOS the APFS container or whole disk
/// (`disk3`); on Linux the mount's device number, its whole disks and its
/// ZFS pool; on Windows the volume and its physical disks. Two paths are on
/// the same disk when their descriptions share a key. `images` are the
/// files of the disk images the disk is made from, when FYLO already knows
/// them (Linux loop devices); macOS and Windows find them only when asked.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DiskDescription {
    pub(crate) label: String,
    pub(crate) keys: Vec<String>,
    pub(crate) memory: bool,
    pub(crate) images: Vec<PathBuf>,
}

impl DiskDescription {
    /// A disk known by one key, which is also its name in messages.
    pub(crate) fn named(key: &str) -> Self {
        Self {
            label: key.to_owned(),
            keys: vec![key.to_owned()],
            memory: false,
            images: Vec::new(),
        }
    }

    /// Whether this disk and `other` share a disk.
    pub(crate) fn shares_disk_with(&self, other: &Self) -> bool {
        self.keys.iter().any(|key| other.keys.contains(key))
    }
}
