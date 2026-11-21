use std::path::PathBuf;

/// The folder FYLO's tests create their roots in: `FYLO_TEST_ROOT` when it
/// is set (the macOS test image on HANGAR, made by
/// `scripts/ci/test-image.sh run`), else the OS temporary folder.
pub struct TestFolder;

impl TestFolder {
    /// The environment variable that names the folder.
    pub const VARIABLE: &'static str = "FYLO_TEST_ROOT";

    /// The folder to create test roots in.
    ///
    /// # Panics
    ///
    /// Panics when `FYLO_TEST_ROOT` names something that is not a folder, so
    /// a test never falls back to the OS temporary folder by mistake.
    #[must_use]
    pub fn path() -> PathBuf {
        Self::existing(Self::configured().unwrap_or_else(std::env::temp_dir))
    }

    fn configured() -> Option<PathBuf> {
        std::env::var_os(Self::VARIABLE)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn existing(folder: PathBuf) -> PathBuf {
        assert!(
            folder.is_dir(),
            "FYLO test image not attached: {} names {}, which is not a folder; run tests through scripts/ci/test-image.sh run",
            Self::VARIABLE,
            folder.display()
        );
        folder
    }
}

#[cfg(test)]
mod tests {
    use super::TestFolder;

    #[test]
    fn the_folder_exists() {
        assert!(TestFolder::path().is_dir());
    }

    #[test]
    fn the_folder_is_the_variable_when_set() {
        let expected = std::env::var_os(TestFolder::VARIABLE)
            .filter(|value| !value.is_empty())
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
        assert_eq!(TestFolder::path(), expected);
    }
}
