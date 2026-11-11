use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_format::{Document, DocumentLimits, LogRecord};
use fylo_query::{IndexLookupValue, StructuredQuery, index_entries_with_words};
use serde_json::{Map, Value};

use super::super::{
    CapturedAttribute, DocumentCodec, DocumentPolicy, DocumentWrite, NativeWriteRoot,
    SqlMutationResult, SqlMutationResultKind, capture_attributes, deleted_document_path,
    json_error, matching_documents, merge_top_level, next_meta_updated_at, record_target,
};
use super::commit_plan::{CommitPlan, Ownership};
use super::logged_writer::Planned;
use super::record_image::RecordImage;
use crate::{
    ExpectedType, NativeCollection, NativeStorageError, NativeStorageErrorCode,
    path_exists_no_follow,
};

/// Document mutations planned as post-images and committed through the log.
///
/// Each mirrors its journal counterpart in `write.rs`: the same checks, in
/// the same order, with the same errors.
impl NativeWriteRoot {
    /// Largest blob area a plan may build before falling back to the journal.
    const LOG_BLOB_BUDGET: usize = LogRecord::MAX_BODY_BYTES - 1024 * 1024;
    const OVER_BUDGET: &'static str = "mutation exceeds the write-ahead log record limit";

    pub(crate) fn put_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        canonical: &[u8],
        metadata: &BTreeMap<String, Value>,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.put_document_segmented(collection, identifier, canonical, metadata);
        }
        let after = Self::document_keys(collection.word_fields(), identifier, canonical)?;
        self.commit_logged(collection, || {
            let target = collection
                .path
                .join("docs")
                .join(crate::shard_of(identifier, collection.shard_width()))
                .join(format!("{identifier}.json"));
            if path_exists_no_follow(&target)? {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "document identifier already exists",
                ));
            }
            self.verify_parent(&target)?;
            let attributes = Self::initial_attributes(metadata)?;
            let mut plan = CommitPlan::new("put-document");
            plan.create(
                collection,
                &target,
                (canonical, attributes),
                Ownership::default(),
            )?;
            plan.index_delta(&BTreeSet::new(), &after);
            Ok((Some(plan), ()))
        })
    }

    /// Plan every write of a batch into one commit, with each body already
    /// in canonical form (`bodies[i]` for `writes[i]`, `None` for a delete).
    /// Commit a batch through the collection's own layout.
    pub(crate) fn commit_document_writes_to(
        &self,
        collection: &NativeCollection,
        writes: &[DocumentWrite],
        bodies: &[Option<Vec<u8>>],
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.commit_document_writes_segmented(collection, writes, bodies);
        }
        self.commit_document_writes_logged(collection, writes, bodies)
    }

    pub(crate) fn commit_document_writes_logged(
        &self,
        collection: &NativeCollection,
        writes: &[DocumentWrite],
        bodies: &[Option<Vec<u8>>],
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let mut plan = CommitPlan::new("write-documents");
            for (write, body) in writes.iter().zip(bodies) {
                let identifier = write.identifier();
                match (write, body) {
                    (DocumentWrite::Put { .. }, Some(canonical)) => {
                        let target = collection
                            .path
                            .join("docs")
                            .join(crate::shard_of(identifier, collection.shard_width()))
                            .join(format!("{identifier}.json"));
                        if path_exists_no_follow(&target)? {
                            return Err(NativeStorageError::new(
                                NativeStorageErrorCode::CorruptMetadata,
                                "document identifier already exists",
                            ));
                        }
                        self.verify_parent(&target)?;
                        let attributes = Self::initial_attributes(&BTreeMap::new())?;
                        plan.create(
                            collection,
                            &target,
                            (canonical, attributes),
                            Ownership::default(),
                        )?;
                        plan.index_delta(
                            &BTreeSet::new(),
                            &Self::document_keys(collection.word_fields(), identifier, canonical)?,
                        );
                    }
                    (DocumentWrite::Replace { .. }, Some(canonical)) => {
                        let stored = collection.read_document(identifier)?;
                        let before = Self::document_keys(
                            collection.word_fields(),
                            identifier,
                            &stored.bytes,
                        )?;
                        let image = RecordImage::contents(stored.bytes, stored.modified_millis);
                        plan.overwrite(collection, &stored.path, image, canonical)?;
                        plan.index_delta(
                            &before,
                            &Self::document_keys(collection.word_fields(), identifier, canonical)?,
                        );
                    }
                    (DocumentWrite::Delete { .. }, None) => {
                        let stored = collection.read_document(identifier)?;
                        let target = deleted_document_path(collection, identifier);
                        Self::require_no_tombstone(&target)?;
                        plan.rename(collection, &stored.path, &target)?;
                        plan.index_delta(
                            &Self::document_keys(
                                collection.word_fields(),
                                identifier,
                                &stored.bytes,
                            )?,
                            &BTreeSet::new(),
                        );
                    }
                    _ => {
                        return Err(NativeStorageError::new(
                            NativeStorageErrorCode::CorruptDocument,
                            "a batch write is missing its body",
                        ));
                    }
                }
                Self::require_within_budget(&plan)?;
            }
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn patch_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        mut patch: impl FnMut(&[u8]) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.patch_document_segmented(collection, identifier, patch);
        }
        self.commit_logged(collection, || {
            let stored = collection.read_document(identifier)?;
            let canonical = patch(&stored.bytes)?;
            let before = Self::document_keys(collection.word_fields(), identifier, &stored.bytes)?;
            let after = Self::document_keys(collection.word_fields(), identifier, &canonical)?;
            let image = RecordImage::contents(stored.bytes, stored.modified_millis);
            let mut plan = CommitPlan::new("patch-document");
            plan.overwrite(collection, &stored.path, image, &canonical)?;
            plan.index_delta(&before, &after);
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn delete_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.delete_document_segmented(collection, identifier);
        }
        self.commit_logged(collection, || {
            let stored = collection.read_document(identifier)?;
            let target = deleted_document_path(collection, identifier);
            Self::require_no_tombstone(&target)?;
            let mut plan = CommitPlan::new("delete-document");
            plan.rename(collection, &stored.path, &target)?;
            plan.index_delta(
                &Self::document_keys(collection.word_fields(), identifier, &stored.bytes)?,
                &BTreeSet::new(),
            );
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn restore_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.restore_document_segmented(collection, identifier);
        }
        self.commit_logged(collection, || {
            Self::require_no_live_record(collection, identifier)?;
            let deleted = collection.read_deleted_document(identifier)?;
            let target = collection
                .path
                .join("docs")
                .join(crate::shard_of(identifier, collection.shard_width()))
                .join(format!("{identifier}.json"));
            let mut plan = CommitPlan::new("restore-document");
            plan.rename(collection, &deleted.path, &target)?;
            plan.index_delta(
                &BTreeSet::new(),
                &Self::document_keys(collection.word_fields(), identifier, &deleted.bytes)?,
            );
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn write_record_metadata_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        (record, replace): (&Map<String, Value>, bool),
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return self.write_record_metadata_segmented(collection, identifier, (record, replace));
        }
        self.commit_logged(collection, || {
            let target = record_target(collection, identifier)?;
            let mut record = record.clone();
            if replace {
                for name in collection.read_custom_metadata(identifier)?.keys() {
                    record.entry(name.clone()).or_insert(Value::Null);
                }
            }
            let updated_at = next_meta_updated_at(&self.root, &target)?;
            let mut attributes = Self::attribute_map(capture_attributes(&target)?);
            for (name, value) in &record {
                Self::set_metadata_attribute(&mut attributes, name, value)?;
            }
            attributes.insert(
                crate::META_UPDATED_XATTR.into(),
                BASE64.encode(updated_at.to_string()),
            );
            let mut plan = CommitPlan::new("set-metadata");
            plan.set_attributes(
                collection,
                &target,
                Self::attribute_list(attributes),
                Ownership::default(),
            )?;
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn sql_update_logged(
        &self,
        collection: &NativeCollection,
        (query, changes): (&StructuredQuery, &Map<String, Value>),
        policy: &DocumentPolicy<'_>,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        if collection.uses_segments() {
            return self.sql_update_segmented(collection, (query, changes), policy);
        }
        self.commit_logged(collection, || {
            policy.restart();
            let documents = matching_documents(collection, query, policy)?;
            let mut plan = CommitPlan::new("update-many");
            let mut identifiers = Vec::with_capacity(documents.len());
            for mut document in documents {
                let stored = collection.read_document(&document.identifier)?;
                let mut fields = document.decoded_fields(&collection.name, policy)?;
                merge_top_level(&mut fields, changes);
                let canonical = policy.encode(&collection.name, fields)?;
                plan.index_delta(
                    &Self::document_keys(
                        collection.word_fields(),
                        &document.identifier,
                        &stored.bytes,
                    )?,
                    &Self::document_keys(
                        collection.word_fields(),
                        &document.identifier,
                        &canonical,
                    )?,
                );
                let image = RecordImage::contents(stored.bytes, stored.modified_millis);
                plan.overwrite(collection, &document.path, image, &canonical)?;
                Self::require_within_budget(&plan)?;
                identifiers.push(document.identifier);
            }
            let result = SqlMutationResult {
                kind: SqlMutationResultKind::Update,
                affected: identifiers.len(),
                identifiers,
            };
            Ok(Self::nonempty(plan, result))
        })
    }

    pub(crate) fn sql_delete_logged(
        &self,
        collection: &NativeCollection,
        query: &StructuredQuery,
        codec: &dyn DocumentCodec,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        if collection.uses_segments() {
            return self.sql_delete_segmented(collection, query, codec);
        }
        self.commit_logged(collection, || {
            let documents = matching_documents(collection, query, codec)?;
            let mut plan = CommitPlan::new("delete-many");
            let mut identifiers = Vec::with_capacity(documents.len());
            for document in documents {
                let target = deleted_document_path(collection, &document.identifier);
                Self::require_no_tombstone(&target)?;
                let stored = collection.read_document(&document.identifier)?;
                plan.rename(collection, &document.path, &target)?;
                plan.index_delta(
                    &Self::document_keys(
                        collection.word_fields(),
                        &document.identifier,
                        &stored.bytes,
                    )?,
                    &BTreeSet::new(),
                );
                identifiers.push(document.identifier);
            }
            let result = SqlMutationResult {
                kind: SqlMutationResultKind::Delete,
                affected: identifiers.len(),
                identifiers,
            };
            Ok(Self::nonempty(plan, result))
        })
    }

    /// Whether an error asks the caller to run the mutation on the journal.
    pub(crate) fn exceeds_log_budget(error: &NativeStorageError) -> bool {
        error.code() == NativeStorageErrorCode::FileTooLarge
            && error.to_string().contains(Self::OVER_BUDGET)
    }

    fn nonempty<T>(plan: CommitPlan, value: T) -> Planned<T> {
        if plan.steps().is_empty() {
            return (None, value);
        }
        (Some(plan), value)
    }

    fn require_within_budget(plan: &CommitPlan) -> Result<(), NativeStorageError> {
        if plan.blob_bytes() > Self::LOG_BLOB_BUDGET {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                Self::OVER_BUDGET,
            ));
        }
        Ok(())
    }

    fn require_no_tombstone(target: &Path) -> Result<(), NativeStorageError> {
        if path_exists_no_follow(target)? {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "retained document tombstone already exists",
            ));
        }
        Ok(())
    }

    fn require_no_live_record(
        collection: &NativeCollection,
        identifier: &str,
    ) -> Result<(), NativeStorageError> {
        match record_target(collection, identifier) {
            Ok(_) => Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "cannot restore a record that already exists",
            )),
            Err(error) if error.code() == NativeStorageErrorCode::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn verify_parent(&self, target: &Path) -> Result<(), NativeStorageError> {
        let parent = target.parent().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "document target has no parent",
            )
        })?;
        if path_exists_no_follow(parent)? {
            self.root.verify_path(parent, ExpectedType::Directory)?;
        }
        Ok(())
    }

    fn initial_attributes(
        metadata: &BTreeMap<String, Value>,
    ) -> Result<Vec<CapturedAttribute>, NativeStorageError> {
        let mut attributes = BTreeMap::new();
        for (name, value) in metadata {
            Self::set_metadata_attribute(&mut attributes, name, value)?;
        }
        Ok(Self::attribute_list(attributes))
    }

    pub(crate) fn set_metadata_attribute(
        attributes: &mut BTreeMap<String, String>,
        name: &str,
        value: &Value,
    ) -> Result<(), NativeStorageError> {
        let attribute = format!("{}{name}", crate::META_XATTR_PREFIX);
        if value.is_null() {
            attributes.remove(&attribute);
            return Ok(());
        }
        let encoded = serde_json::to_vec(value).map_err(|error| json_error(&error))?;
        attributes.insert(attribute, BASE64.encode(encoded));
        Ok(())
    }

    fn attribute_map(attributes: Vec<CapturedAttribute>) -> BTreeMap<String, String> {
        attributes
            .into_iter()
            .map(|attribute| (attribute.name, attribute.value))
            .collect()
    }

    fn attribute_list(attributes: BTreeMap<String, String>) -> Vec<CapturedAttribute> {
        attributes
            .into_iter()
            .map(|(name, value)| CapturedAttribute { name, value })
            .collect()
    }

    pub(crate) fn document_keys(
        word_fields: &[String],
        identifier: &str,
        bytes: &[u8],
    ) -> Result<BTreeSet<String>, NativeStorageError> {
        let document = Document::parse(bytes, DocumentLimits::default()).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptDocument,
                format!("document is invalid during index update: {error}"),
            )
        })?;
        index_entries_with_words(identifier, document.fields(), word_fields, |_, value| {
            Ok::<_, NativeStorageError>(IndexLookupValue::plain(value))
        })
    }
}
