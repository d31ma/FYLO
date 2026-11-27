use std::fmt;
use std::path::Path;

use super::NameSubject;
use crate::NativeStorageError;

/// The name of an app: the last folder name of its root (ADR 0033 section 2).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AppName(String);

impl AppName {
    /// Check a name against the name rule.
    ///
    /// # Errors
    ///
    /// Returns `EAPPNAME` when the name breaks the rule.
    pub fn parse(name: &str) -> Result<Self, NativeStorageError> {
        NameSubject::APP.check(name)?;
        Ok(Self(name.to_owned()))
    }

    /// The app name of a root: its last folder name. Pass the canonical
    /// root, so a symbolic link names the folder it points to.
    ///
    /// # Errors
    ///
    /// Returns `EAPPNAME` when the folder name breaks the rule, or when the
    /// root has no last folder name (a drive's top folder).
    pub fn from_root_folder(root: &Path) -> Result<Self, NativeStorageError> {
        Self::parse(&Self::folder_name(root))
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn folder_name(root: &Path) -> String {
        root.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

impl fmt::Display for AppName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::AppName;
    use std::path::Path;

    #[test]
    fn the_app_name_is_the_root_folder_name() {
        let name = AppName::from_root_folder(Path::new("/mnt/data/chat")).unwrap();
        assert_eq!(name.as_str(), "chat");
        assert_eq!(name.to_string(), "chat");
    }

    #[test]
    fn a_temporary_folder_name_is_refused() {
        let error = AppName::from_root_folder(Path::new("/tmp/.tmpAbC123")).unwrap_err();
        assert_eq!(error.code().as_str(), "EAPPNAME");
        assert!(
            error.message().contains("`.tmpAbC123`"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_drive_top_folder_has_no_app_name() {
        let error = AppName::from_root_folder(Path::new("/")).unwrap_err();
        assert_eq!(error.code().as_str(), "EAPPNAME");
        assert!(error.message().contains("too short"), "{}", error.message());
    }

    #[test]
    fn a_reserved_folder_name_is_refused() {
        let error = AppName::from_root_folder(Path::new("/Volumes/data/nul")).unwrap_err();
        assert!(
            error.message().contains("reserved name"),
            "{}",
            error.message()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_name_that_is_not_utf8_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let root = Path::new("/mnt").join(OsStr::from_bytes(b"ch\xffat"));
        let error = AppName::from_root_folder(&root).unwrap_err();
        assert_eq!(error.code().as_str(), "EAPPNAME");
    }
}
