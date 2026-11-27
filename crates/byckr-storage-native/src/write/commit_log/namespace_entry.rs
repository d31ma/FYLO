use byckr_format::LogRecordKind;

/// One record to append to a namespace log, with the failpoints around its
/// flush when it is a commit.
pub(crate) struct NamespaceEntry<'a> {
    pub(crate) kind: LogRecordKind,
    pub(crate) manifest: Vec<u8>,
    pub(crate) blobs: Vec<u8>,
    pub(crate) failpoints: Option<(&'a str, &'a str)>,
}
