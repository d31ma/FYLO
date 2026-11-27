/// The variable lengths a [`super::document_entry::DocumentEntry`] header
/// records.
pub(crate) struct EntryLengths {
    pub(crate) collection: usize,
    pub(crate) identifier: usize,
    pub(crate) path: usize,
    pub(crate) payload: usize,
    pub(crate) manifest: usize,
}

impl EntryLengths {
    pub(crate) const fn strings(&self) -> usize {
        self.collection + self.identifier + self.path
    }

    /// The payload and the manifest together, or `None` when they would
    /// overflow.
    pub(crate) const fn body(&self) -> Option<usize> {
        self.payload.checked_add(self.manifest)
    }
}
