use serde_json::{Map, Value};

use crate::NativeStorageError;

/// What happens to a document body between storage and the mutation that
/// rewrites it.
///
/// Storage computes some post-images itself: a shallow patch merges into the
/// stored body, and SQL `INSERT` and `UPDATE` build bodies from a statement.
/// The engine owns schema validation and field encryption, so it hands storage
/// a codec for those mutations. `decode` turns a stored body into the fields
/// the mutation works on (decrypting declared fields), and `encode` turns the
/// post-image into the bytes to store (validating, stamping `_v`, encrypting
/// and encoding). ADR 0017 section 1.
pub trait DocumentCodec {
    /// Fields a mutation should see for a stored body of `collection`.
    ///
    /// # Errors
    ///
    /// Returns an error when the body cannot be decoded, for example a
    /// declared field that does not decrypt.
    fn decode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, NativeStorageError>;

    /// Canonical bytes to store for the post-image `fields` of `collection`.
    ///
    /// # Errors
    ///
    /// Returns an error when the post-image is rejected or cannot be encoded.
    fn encode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Vec<u8>, NativeStorageError>;
}

/// The codec for a writer with no schema or encryption: bodies pass through
/// and are only checked against the document format.
pub struct PlainDocumentCodec;

impl DocumentCodec for PlainDocumentCodec {
    fn decode(
        &self,
        _collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, NativeStorageError> {
        Ok(fields)
    }

    fn encode(
        &self,
        _collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Vec<u8>, NativeStorageError> {
        super::encode_document_fields(fields)
    }
}
