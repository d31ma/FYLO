use std::path::{Path, PathBuf};

/// Where the current OS user's temp cache folder is (ADR 0030 section 4.2).
///
/// | OS | Base folder | Cache folder |
/// | --- | --- | --- |
/// | macOS, iOS | `$TMPDIR`, already per user | `<base>/fylo-cache-v1` |
/// | Linux, Android | `$TMPDIR`, else `/tmp`, often shared | `<base>/fylo-cache-v1-<uid>` |
/// | Windows | `%TEMP%`, per user | `<base>\fylo-cache-v1` |
///
/// `--temp-cache-dir` replaces the base folder.
pub struct CacheLocation;

impl CacheLocation {
    const FOLDER: &'static str = "fylo-cache-v1";

    /// The cache folder below `base`, or below the OS temp folder.
    #[must_use]
    pub fn folder(base: Option<&Path>) -> PathBuf {
        Self::base(base).join(Self::folder_name())
    }

    /// The folder the cache folder is created in.
    #[must_use]
    pub fn base(base: Option<&Path>) -> PathBuf {
        base.map_or_else(std::env::temp_dir, Path::to_path_buf)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn folder_name() -> String {
        format!(
            "{}-{}",
            Self::FOLDER,
            super::folder_owner::FolderOwner::current().id()
        )
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn folder_name() -> String {
        Self::FOLDER.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::CacheLocation;
    use std::path::Path;

    #[test]
    fn the_folder_is_below_the_named_base() {
        let folder = CacheLocation::folder(Some(Path::new("base")));
        assert_eq!(folder.parent(), Some(Path::new("base")));
    }

    #[test]
    fn without_a_base_the_folder_is_in_the_os_temp_folder() {
        let folder = CacheLocation::folder(None);
        assert_eq!(folder.parent(), Some(std::env::temp_dir().as_path()));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_shared_temp_folder_gets_one_cache_folder_per_user() {
        let uid = rustix::process::geteuid().as_raw();
        let folder = CacheLocation::folder(Some(Path::new("/tmp")));
        assert_eq!(folder, Path::new(&format!("/tmp/fylo-cache-v1-{uid}")));
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    #[test]
    fn a_per_user_temp_folder_holds_the_plain_cache_folder() {
        let folder = CacheLocation::folder(Some(Path::new("base")));
        assert_eq!(folder.file_name().unwrap(), "fylo-cache-v1");
    }
}
