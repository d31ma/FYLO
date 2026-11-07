use fylo_storage_native::{DocumentRekey, FieldKeys};

use crate::encryption::EncryptionReader;

/// The engine's part of a protected `fylo rekey` (ADR 0021 section 7):
/// each `$encrypted` value read with the field keys of the database being
/// rekeyed (and `fylo.encryption.key`'s, when the engine has it) and
/// written again under the new file's field key.
pub struct FieldRekey {
    encryption: EncryptionReader,
}

impl FieldRekey {
    pub(crate) fn new(encryption: EncryptionReader) -> FieldRekey {
        FieldRekey { encryption }
    }
}

impl DocumentRekey for FieldRekey {
    fn rekey_document(
        &self,
        collection: &str,
        body: &[u8],
        target: &FieldKeys,
    ) -> Result<Vec<u8>, String> {
        self.encryption.rekey_document(collection, body, target)
    }
}
