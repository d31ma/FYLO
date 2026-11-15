use std::path::{Path, PathBuf};

/// A folder in the temp folder for one test, removed when dropped.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let folder = std::env::temp_dir().join(crate::write::unique_name(name));
        std::fs::create_dir_all(&folder).expect("create the scratch folder");
        Self(folder)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
