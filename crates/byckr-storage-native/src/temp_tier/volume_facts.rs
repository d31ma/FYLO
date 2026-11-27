use std::io;
use std::path::Path;

/// Facts about the volume that holds a path: its free space and its
/// identity, so the temp tier can tell whether a cache folder shares the
/// root's device (ADR 0030 section 4.1).
pub struct VolumeFacts;

impl VolumeFacts {
    /// Bytes an unprivileged user may still write on the volume of `path`.
    ///
    /// # Errors
    /// The OS error of describing the volume.
    #[cfg(unix)]
    pub fn free_bytes(path: &Path) -> io::Result<u64> {
        let facts = rustix::fs::statvfs(path)?;
        Ok(facts.f_bavail.saturating_mul(facts.f_frsize))
    }

    /// Bytes an unprivileged user may still write on the volume of `path`.
    ///
    /// # Errors
    /// The OS error of describing the volume.
    #[cfg(windows)]
    pub fn free_bytes(path: &Path) -> io::Result<u64> {
        super::windows_folder::WindowsFolder::free_bytes(path)
    }

    /// The identity of the volume that holds `path`: the device number on
    /// Unix, the volume serial number on Windows.
    ///
    /// # Errors
    /// The OS error of describing `path`.
    #[cfg(unix)]
    pub fn device(path: &Path) -> io::Result<u64> {
        use std::os::unix::fs::MetadataExt;

        Ok(std::fs::metadata(path)?.dev())
    }

    /// The identity of the volume that holds `path`: the device number on
    /// Unix, the drive of its canonical path on Windows (a share is named
    /// by its server and share).
    ///
    /// # Errors
    /// The OS error of describing `path`.
    #[cfg(windows)]
    pub fn device(path: &Path) -> io::Result<u64> {
        let canonical = std::fs::canonicalize(path)?;
        Ok(Self::drive_of(&canonical))
    }

    #[cfg(windows)]
    fn drive_of(path: &Path) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::hash::DefaultHasher::new();
        path.components().next().hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::VolumeFacts;

    #[test]
    fn the_temp_folder_has_free_space_and_a_device() {
        let temp = std::env::temp_dir();
        assert!(VolumeFacts::free_bytes(&temp).unwrap() > 0);
        assert_eq!(
            VolumeFacts::device(&temp).unwrap(),
            VolumeFacts::device(&temp).unwrap()
        );
    }
}
