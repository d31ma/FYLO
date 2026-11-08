use fylo_vfs::{File, Metadata};
use std::path::Path;

/// Outcome of opening a path in one call that refuses a symbolic link in any
/// component, so the kernel checks the whole path atomically.
///
/// `Unavailable` covers every case the caller must hand to the
/// component-by-component walk instead: a platform without such an open, a
/// link, a permission error, or anything else that walk reports precisely.
#[cfg_attr(
    not(target_vendor = "apple"),
    expect(
        dead_code,
        reason = "only Apple platforms open a whole path in one call"
    )
)]
pub(crate) enum BeneathOpen {
    Opened(File, Metadata),
    Missing,
    Unavailable,
}

impl BeneathOpen {
    pub(crate) const SUPPORTED: bool = cfg!(target_vendor = "apple");

    #[cfg(target_vendor = "apple")]
    const NO_LINK_IN_ANY_COMPONENT: i32 = 0x2000_0000;
    #[cfg(target_vendor = "apple")]
    const DO_NOT_BLOCK: i32 = 0x0000_0004;
    #[cfg(target_vendor = "apple")]
    const DIRECTORY_ONLY: i32 = 0x0010_0000;

    #[cfg(target_vendor = "apple")]
    pub(crate) fn attempt(path: &Path, directory: bool) -> Self {
        use std::os::unix::fs::OpenOptionsExt;

        let flags =
            Self::NO_LINK_IN_ANY_COMPONENT | Self::DO_NOT_BLOCK | Self::directory_flag(directory);
        Self::from_open(
            fylo_vfs::OpenOptions::new()
                .read(true)
                .custom_flags(flags)
                .open(path),
        )
    }

    #[cfg(not(target_vendor = "apple"))]
    pub(crate) fn attempt(_path: &Path, _directory: bool) -> Self {
        Self::Unavailable
    }

    #[cfg(target_vendor = "apple")]
    const fn directory_flag(directory: bool) -> i32 {
        if directory { Self::DIRECTORY_ONLY } else { 0 }
    }

    #[cfg(target_vendor = "apple")]
    fn from_open(result: std::io::Result<File>) -> Self {
        match result {
            Ok(file) => Self::with_metadata(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::Missing,
            Err(_) => Self::Unavailable,
        }
    }

    #[cfg(target_vendor = "apple")]
    fn with_metadata(file: File) -> Self {
        match file.metadata() {
            Ok(metadata) => Self::Opened(file, metadata),
            Err(_) => Self::Unavailable,
        }
    }
}
