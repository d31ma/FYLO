use crate::{GenerationState, NativeCollection, NativeStorageError, StoredBytes};

/// The browser engine never reads through a temp tier (ADR 0030 section 2).
impl NativeCollection {
    #[expect(
        clippy::unused_self,
        reason = "the browser engine never has a temp tier"
    )]
    pub(crate) const fn reads_through_tier(&self) -> bool {
        false
    }

    pub(crate) fn observed_generation(&self) -> Result<GenerationState, NativeStorageError> {
        self.stored_generation()
    }

    pub(crate) fn read_live_document(
        &self,
        identifier: &str,
    ) -> Result<StoredBytes, NativeStorageError> {
        self.read_document_at(&self.path.join("docs"), identifier)
    }
}
