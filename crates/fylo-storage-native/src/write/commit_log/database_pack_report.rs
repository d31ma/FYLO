/// What `fylo pack` or `fylo unpack` carried over (ADR 0016 section 11).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatabasePackReport {
    /// Every collection copied, in the order it was copied.
    pub collections: Vec<PackedCollection>,
    /// The blocks of a sealed pack (`fylo pack --sealed`); `None` for any
    /// other pack, and for an unpack.
    pub sealed: Option<crate::ContainerSealedReport>,
}

/// One collection `fylo pack` or `fylo unpack` copied.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackedCollection {
    /// The collection's name.
    pub collection: String,
    /// Live documents carried over.
    pub documents: usize,
    /// Soft-deleted documents carried over.
    pub deleted: usize,
    /// Versions of its schema registry carried over (ADR 0017 section 3).
    pub schema_versions: usize,
}
