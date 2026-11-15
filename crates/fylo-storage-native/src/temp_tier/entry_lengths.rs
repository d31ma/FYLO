/// The variable lengths a [`super::document_entry::DocumentEntry`] header
/// records.
pub(crate) struct EntryLengths {
    pub(crate) collection: usize,
    pub(crate) identifier: usize,
    pub(crate) path: usize,
    pub(crate) payload: usize,
}

impl EntryLengths {
    pub(crate) const fn strings(&self) -> usize {
        self.collection + self.identifier + self.path
    }
}
