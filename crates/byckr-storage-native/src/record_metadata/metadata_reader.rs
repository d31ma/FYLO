use std::collections::BTreeMap;
use std::path::Path;

use byckr_vfs::File;

use crate::record_attributes::RecordAttributes;
use crate::{NativeRoot, NativeStorageError};

/// Reads a record's metadata from extended attributes and its sidecar file,
/// with the record binding checked.
pub(crate) struct MetadataReader;

impl MetadataReader {
    pub(crate) fn read(
        root: &NativeRoot,
        file: &File,
        path: &Path,
    ) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        let attributes = RecordAttributes::read(root, file, path)?;
        Ok(BTreeMap::clone(&attributes))
    }
}
