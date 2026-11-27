use std::collections::BTreeMap;

use byckr_vfs::File;

use crate::FileStamp;

/// An EFS record's manifest as a read verified it: its values, its open
/// file, and the file's stamp taken right after it was opened, before
/// anything was read from it (ADR 0030 section 3.2).
pub(crate) struct VerifiedManifest {
    pub(crate) values: BTreeMap<String, Vec<u8>>,
    pub(crate) file: File,
    pub(crate) opened: Option<FileStamp>,
}
