use crate::FileStamp;

/// The EFS profile's manifest file as one root read saw it.
pub(crate) struct ManifestSeen {
    pub(crate) opened: Option<FileStamp>,
    pub(crate) checked: Option<FileStamp>,
    pub(crate) bytes: Option<Vec<u8>>,
}
