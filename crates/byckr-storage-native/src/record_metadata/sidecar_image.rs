/// The final state of a record's sidecar file in a [`super::MetadataPlan`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SidecarImage {
    /// The sidecar file stays as it is.
    Keep,
    /// The sidecar file is replaced by these bytes.
    Replace(Vec<u8>),
    /// The sidecar file is removed.
    Remove,
}
