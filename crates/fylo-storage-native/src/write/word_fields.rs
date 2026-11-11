use fylo_query::normalize_field_path;
use serde_json::Value;

use super::NativeWriteRoot;
use crate::{CollectionKind, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Declaring a collection's word fields (ADR 0023 section 1).
impl NativeWriteRoot {
    /// Most word fields one collection may declare.
    pub const MAX_WORD_FIELDS: usize = 16;
    /// Longest word field path, in bytes.
    pub const MAX_WORD_FIELD_BYTES: usize = 256;

    /// Replace `collection_name`'s word fields with `fields` and return them
    /// slash-separated, as they are stored. The collection's index is
    /// rebuilt with the new list under the collection lock before the
    /// descriptor names it, so the index is complete for every listed field
    /// from the moment it is listed. An empty list clears the word keys.
    ///
    /// # Errors
    ///
    /// Returns `EQUERY_INVALID` for a list that is too long, a field that is
    /// empty, too long, repeated or encrypted, or a file collection; and the
    /// rebuild's errors otherwise.
    pub fn set_word_fields(
        &self,
        collection_name: &str,
        fields: &[String],
        encrypted: &[String],
    ) -> Result<Vec<String>, NativeStorageError> {
        self.root.assert_profile_owned()?;
        let normalized = Self::normalized_word_fields(fields)?;
        let collection = self.root.collection(collection_name)?;
        Self::require_document_collection(&collection)?;
        let recorded = self
            .root
            .recorded_encrypted_fields(collection_name)?
            .unwrap_or_default();
        Self::refuse_encrypted(&normalized, encrypted)?;
        Self::refuse_encrypted(&normalized, &recorded)?;
        let _lock = self.lock_collection(&collection)?;
        let updated = collection.with_word_fields(normalized.clone());
        self.rebuild_locked(&updated)?;
        self.replace_descriptor_field(&updated, "words", Value::from(normalized.clone()))?;
        Ok(normalized)
    }

    /// Rebuild `collection_name`'s index when any of `fields` is a word field
    /// whose word keys an older release left incomplete (ADR 0023 section
    /// 8). Returns whether it rebuilt.
    ///
    /// # Errors
    ///
    /// Returns the rebuild's errors.
    pub fn repair_word_fields(
        &self,
        collection_name: &str,
        fields: &[String],
    ) -> Result<bool, NativeStorageError> {
        self.root.assert_profile_owned()?;
        let collection = self.root.collection(collection_name)?;
        if !Self::needs_repair(&collection, fields)? {
            return Ok(false);
        }
        let _lock = self.lock_collection(&collection)?;
        // Check again under the lock: another writer may have rebuilt it.
        if !Self::needs_repair(&collection, fields)? {
            return Ok(false);
        }
        self.rebuild_locked(&collection)?;
        Ok(true)
    }

    fn needs_repair(
        collection: &NativeCollection,
        fields: &[String],
    ) -> Result<bool, NativeStorageError> {
        let wanted: Vec<&String> = fields
            .iter()
            .filter(|field| collection.word_fields().contains(field))
            .collect();
        if wanted.is_empty() {
            return Ok(false);
        }
        let complete = collection.complete_word_fields()?;
        Ok(wanted.iter().any(|field| !complete.contains(field)))
    }

    fn rebuild_locked(&self, collection: &NativeCollection) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.rebuild_segment_collection_locked(collection);
        }
        self.rebuild_file_collection_locked(collection)
    }

    fn normalized_word_fields(fields: &[String]) -> Result<Vec<String>, NativeStorageError> {
        if fields.len() > Self::MAX_WORD_FIELDS {
            return Err(Self::invalid_word_fields(format!(
                "a collection may have at most {} word fields",
                Self::MAX_WORD_FIELDS
            )));
        }
        let normalized: Vec<String> = fields
            .iter()
            .map(|field| normalize_field_path(field))
            .collect();
        Self::require_valid_paths(&normalized)?;
        Ok(normalized)
    }

    fn require_valid_paths(fields: &[String]) -> Result<(), NativeStorageError> {
        for (index, field) in fields.iter().enumerate() {
            Self::require_valid_path(field)?;
            if fields[..index].contains(field) {
                return Err(Self::invalid_word_fields(format!(
                    "word field \"{field}\" is listed twice"
                )));
            }
        }
        Ok(())
    }

    fn require_valid_path(field: &str) -> Result<(), NativeStorageError> {
        if field.is_empty() || field.len() > Self::MAX_WORD_FIELD_BYTES {
            return Err(Self::invalid_word_fields(format!(
                "a word field must be a path of 1 to {} bytes",
                Self::MAX_WORD_FIELD_BYTES
            )));
        }
        Ok(())
    }

    fn refuse_encrypted(fields: &[String], encrypted: &[String]) -> Result<(), NativeStorageError> {
        let is_encrypted = |field: &String| {
            encrypted
                .iter()
                .any(|path| normalize_field_path(path) == *field)
        };
        let Some(field) = fields.iter().find(|field| is_encrypted(field)) else {
            return Ok(());
        };
        Err(Self::invalid_word_fields(format!(
            "word field \"{field}\" is encrypted; its words would be stored in plain text"
        )))
    }

    fn require_document_collection(
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        if collection.kind() == CollectionKind::Document {
            return Ok(());
        }
        Err(Self::invalid_word_fields(
            "word fields need a document collection".to_owned(),
        ))
    }

    fn invalid_word_fields(message: String) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::InvalidQuery, message)
    }
}
