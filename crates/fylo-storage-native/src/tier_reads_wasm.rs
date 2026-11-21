use crate::index_cache::IndexInputs;
use crate::index_limit::IndexLimit;
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

    pub(crate) fn index_inputs(&self, log_limit: u64) -> Result<IndexInputs, NativeStorageError> {
        let log = self.index_log(log_limit)?;
        Ok(IndexInputs {
            snapshot: self
                .root
                .read_file(&self.index_snapshot_path(), IndexLimit::INDEX_BYTES)?,
            log,
        })
    }

    #[expect(
        clippy::unused_self,
        clippy::unnecessary_wraps,
        reason = "the browser engine never has a temp tier; the signature matches the native one"
    )]
    pub(crate) const fn index_inputs_for_ranges(
        &self,
    ) -> Result<Option<IndexInputs>, NativeStorageError> {
        Ok(None)
    }
}
