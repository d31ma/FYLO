use crate::FieldKeys;

/// Writes a document's `$encrypted` values again under a collection's new
/// field key, for a protected `fylo rekey` (ADR 0021 section 7). Storage
/// does not know which fields a schema encrypts; the engine does, so it
/// supplies this.
pub trait DocumentRekey {
    /// `body`, a stored document of `collection`, with every `$encrypted`
    /// value read with the keys it was written with and written again with
    /// `target`'s newest key. A document with no such value comes back
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Why a value could not be read or written, or why the rewritten
    /// document does not read back as the old one did.
    fn rekey_document(
        &self,
        collection: &str,
        body: &[u8],
        target: &FieldKeys,
    ) -> Result<Vec<u8>, String>;
}
