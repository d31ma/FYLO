use crate::NativeStorageError;
use fylo_vfs as fs;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;

#[derive(Debug)]
pub(super) struct DirectoryListing {
    names: BTreeSet<OsString>,
}

impl DirectoryListing {
    pub(super) fn read(directory: &Path) -> Result<Self, NativeStorageError> {
        let mut names = BTreeSet::new();
        for entry in fs::read_dir(directory).map_err(NativeStorageError::io)? {
            names.insert(entry.map_err(NativeStorageError::io)?.file_name());
        }
        Ok(Self { names })
    }

    pub(super) fn contains(&self, name: &OsStr) -> bool {
        self.names.contains(name)
    }

    pub(super) fn len(&self) -> usize {
        self.names.len()
    }
}
