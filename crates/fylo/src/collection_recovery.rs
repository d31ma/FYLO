use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::Path;

use fylo_storage_native::{
    NativeRoot, NativeStorageErrorCode, NativeWriteRoot, is_platform_companion,
};

use crate::Error;

/// Every collection of one root, each marked with whether it has a live data
/// directory, recovered the way `fylo exec` recovers them when it starts.
pub(crate) struct CollectionRecovery {
    names: BTreeMap<String, bool>,
}

impl CollectionRecovery {
    const DATA_DIRECTORIES: [&str; 2] = [".collections", ".buckets"];
    const CATALOG_DIRECTORY: &str = ".fylo-catalog";
    const CATALOG_COLLECTIONS: &str = "collections";
    const DESCRIPTOR_SUFFIX: &str = ".json";

    pub(crate) fn run(root: &NativeRoot, writer: &NativeWriteRoot) -> Result<(), Error> {
        let mut recovery = Self {
            names: BTreeMap::new(),
        };
        if let Some(names) = root.database_collection_names() {
            recovery
                .names
                .extend(names.into_iter().map(|name| (name, true)));
            return recovery.recover_all(writer);
        }
        recovery.add_data_directories(root.path())?;
        let catalog = root
            .catalog_root()
            .join(Self::CATALOG_DIRECTORY)
            .join(Self::CATALOG_COLLECTIONS);
        recovery.add_catalog_descriptors(&Self::entry_names(&catalog)?);
        recovery.recover_all(writer)
    }

    fn add_data_directories(&mut self, root: &Path) -> Result<(), Error> {
        for directory in Self::DATA_DIRECTORIES {
            let names = Self::entry_names(&root.join(directory))?;
            self.names
                .extend(names.into_iter().map(|name| (name, true)));
        }
        Ok(())
    }

    fn add_catalog_descriptors(&mut self, files: &[String]) {
        let names = files
            .iter()
            .filter_map(|file| file.strip_suffix(Self::DESCRIPTOR_SUFFIX));
        for name in names {
            self.names.entry(name.to_owned()).or_insert(false);
        }
    }

    fn entry_names(directory: &Path) -> Result<Vec<String>, Error> {
        let entries = match fylo_vfs::read_dir(directory) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            entries => entries.map_err(|error| Self::io_error(&error))?,
        };
        let mut names = Vec::new();
        for entry in entries {
            names.push(entry.map_err(|error| Self::io_error(&error))?.file_name());
        }
        Ok(Self::readable_names(names))
    }

    fn io_error(error: &std::io::Error) -> Error {
        Error::new(NativeStorageErrorCode::Io.as_str(), error.to_string())
    }

    fn readable_names(names: Vec<OsString>) -> Vec<String> {
        names
            .into_iter()
            .filter(|name| !is_platform_companion(name))
            .filter_map(|name| name.into_string().ok())
            .collect()
    }

    fn recover_all(&self, writer: &NativeWriteRoot) -> Result<(), Error> {
        for (name, has_live_directory) in &self.names {
            Self::recover_one(writer, name, *has_live_directory)?;
        }
        Ok(())
    }

    fn recover_one(
        writer: &NativeWriteRoot,
        name: &str,
        has_live_directory: bool,
    ) -> Result<(), Error> {
        match writer.recover_collection(name) {
            Err(error)
                if !has_live_directory && error.code() == NativeStorageErrorCode::NotFound =>
            {
                Ok(())
            }
            result => result.map(|_| ()).map_err(Error::from),
        }
    }
}
