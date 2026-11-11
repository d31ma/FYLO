use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_format::TtidTimestamps;
use fylo_query::StructuredQuery;
use serde_json::{Map, Value};

use super::super::document_codec::{DocumentCodec, DocumentPolicy};
use super::super::{
    DocumentWrite, NativeWriteRoot, SqlMutationResult, SqlMutationResultKind, merge_top_level,
    now_millis, parse_document_fields,
};
use super::logged_writer::Planned;
use super::segment_plan::SegmentPlan;
use crate::segments::{LocationState, SegmentDocument, SegmentEntry, SegmentStore};
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Live documents a SQL mutation matched, each with its parsed fields.
/// Each match with its fields, and whether those fields are already decoded.
type SegmentMatches = Vec<(SegmentDocument, Map<String, Value>, bool)>;

/// Document mutations of a segment collection (ADR 0015).
///
/// Each mirrors its counterpart in `logged_operations.rs`: the same checks,
/// in the same order, with the same errors. Every plan reads the writer's
/// view of the location table, which includes busy-period members that have
/// applied but not yet published.
impl NativeWriteRoot {
    const SEGMENT_SCAN_BATCH: usize = 256;

    pub(crate) fn put_document_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        canonical: &[u8],
        metadata: &BTreeMap<String, Value>,
    ) -> Result<(), NativeStorageError> {
        let after = Self::document_keys(collection.word_fields(), identifier, canonical)?;
        self.commit_logged(collection, || {
            let store = SegmentStore::open_for_writer(collection.segment_medium()?)?;
            let previous = store.table().get(identifier).cloned();
            if previous.is_some() {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "document identifier already exists",
                ));
            }
            let entry = SegmentEntry {
                id: identifier.to_owned(),
                state: LocationState::Live,
                body: None,
                attributes: Self::segment_initial_attributes(metadata)?,
                modified_ms: now_millis()?,
                mode: None,
            };
            let mut plan = SegmentPlan::new("put-document");
            plan.push(&collection.path, (entry, Some(canonical)), None)?;
            plan.index_delta(&BTreeSet::new(), &after);
            Ok((Some(plan), ()))
        })
    }

    /// [`Self::commit_document_writes_logged`] for a segment collection:
    /// every write goes into one plan, so the batch commits or fails whole.
    pub(crate) fn commit_document_writes_segmented(
        &self,
        collection: &NativeCollection,
        writes: &[DocumentWrite],
        bodies: &[Option<Vec<u8>>],
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let store = SegmentStore::open_for_writer(collection.segment_medium()?)?;
            let mut plan = SegmentPlan::new("write-documents");
            for (write, body) in writes.iter().zip(bodies) {
                let change = (write, body.as_deref());
                Self::push_document_write(&mut plan, collection, &store, change)?;
            }
            Ok((Some(plan), ()))
        })
    }

    fn push_document_write(
        plan: &mut SegmentPlan,
        collection: &NativeCollection,
        store: &SegmentStore,
        (write, body): (&DocumentWrite, Option<&[u8]>),
    ) -> Result<(), NativeStorageError> {
        let identifier = write.identifier();
        match (write, body) {
            (DocumentWrite::Put { .. }, Some(canonical)) => {
                Self::push_new_document(plan, collection, store, (identifier, canonical))
            }
            (DocumentWrite::Replace { .. }, Some(canonical)) => {
                Self::push_replaced_document(plan, collection, store, (identifier, canonical))
            }
            (DocumentWrite::Delete { .. }, None) => {
                let stored = store.document(identifier, LocationState::Live)?;
                plan.index_delta(
                    &Self::document_keys(collection.word_fields(), identifier, &stored.body)?,
                    &BTreeSet::new(),
                );
                Self::push_state_change(plan, collection, stored, LocationState::Deleted)
            }
            _ => Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptDocument,
                "a batch write is missing its body",
            )),
        }
    }

    fn push_new_document(
        plan: &mut SegmentPlan,
        collection: &NativeCollection,
        store: &SegmentStore,
        (identifier, canonical): (&str, &[u8]),
    ) -> Result<(), NativeStorageError> {
        if store.table().get(identifier).is_some() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "document identifier already exists",
            ));
        }
        let entry = SegmentEntry {
            id: identifier.to_owned(),
            state: LocationState::Live,
            body: None,
            attributes: Self::segment_initial_attributes(&BTreeMap::new())?,
            modified_ms: now_millis()?,
            mode: None,
        };
        plan.index_delta(
            &BTreeSet::new(),
            &Self::document_keys(collection.word_fields(), identifier, canonical)?,
        );
        plan.push(&collection.path, (entry, Some(canonical)), None)
    }

    fn push_replaced_document(
        plan: &mut SegmentPlan,
        collection: &NativeCollection,
        store: &SegmentStore,
        (identifier, canonical): (&str, &[u8]),
    ) -> Result<(), NativeStorageError> {
        let stored = store.document(identifier, LocationState::Live)?;
        plan.index_delta(
            &Self::document_keys(collection.word_fields(), identifier, &stored.body)?,
            &Self::document_keys(collection.word_fields(), identifier, canonical)?,
        );
        let mut entry = stored.entry.clone();
        entry.modified_ms = now_millis()?;
        plan.push(
            &collection.path,
            (entry, Some(canonical)),
            Some(stored.location),
        )
    }

    pub(crate) fn patch_document_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        mut patch: impl FnMut(&[u8]) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            let canonical = patch(&stored.body)?;
            let mut plan = SegmentPlan::new("patch-document");
            plan.index_delta(
                &Self::document_keys(collection.word_fields(), identifier, &stored.body)?,
                &Self::document_keys(collection.word_fields(), identifier, &canonical)?,
            );
            let mut entry = stored.entry.clone();
            entry.modified_ms = now_millis()?;
            plan.push(
                &collection.path,
                (entry, Some(&canonical)),
                Some(stored.location),
            )?;
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn delete_document_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            let mut plan = SegmentPlan::new("delete-document");
            plan.index_delta(
                &Self::document_keys(collection.word_fields(), identifier, &stored.body)?,
                &BTreeSet::new(),
            );
            Self::push_state_change(&mut plan, collection, stored, LocationState::Deleted)?;
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn restore_document_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let store = SegmentStore::open_for_writer(collection.segment_medium()?)?;
            if store.location(identifier, LocationState::Live).is_some() {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "cannot restore a record that already exists",
                ));
            }
            let deleted = store.document(identifier, LocationState::Deleted)?;
            let mut plan = SegmentPlan::new("restore-document");
            plan.index_delta(
                &BTreeSet::new(),
                &Self::document_keys(collection.word_fields(), identifier, &deleted.body)?,
            );
            Self::push_state_change(&mut plan, collection, deleted, LocationState::Live)?;
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn write_record_metadata_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        (record, replace): (&Map<String, Value>, bool),
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            let mut record = record.clone();
            if replace {
                for name in crate::decode_custom_metadata(&stored.attributes()?)?.keys() {
                    record.entry(name.clone()).or_insert(Value::Null);
                }
            }
            let mut entry = stored.entry.clone();
            let updated_at = Self::next_segment_meta_updated_at(&stored)?;
            for (name, value) in &record {
                Self::set_metadata_attribute(&mut entry.attributes, name, value)?;
            }
            entry.attributes.insert(
                crate::META_UPDATED_XATTR.into(),
                BASE64.encode(updated_at.to_string()),
            );
            let mut plan = SegmentPlan::new("set-metadata");
            plan.push(
                &collection.path,
                (entry, Some(&stored.body)),
                Some(stored.location),
            )?;
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn sql_update_segmented(
        &self,
        collection: &NativeCollection,
        (query, changes): (&StructuredQuery, &Map<String, Value>),
        policy: &DocumentPolicy<'_>,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        self.commit_logged(collection, || {
            policy.restart();
            let mut plan = SegmentPlan::new("update-many");
            let mut identifiers = Vec::new();
            for (stored, fields, decoded) in Self::segment_matches(collection, query, policy)? {
                // Decrypted and validated, then encrypted again, as the file
                // layout's update does: merging into the stored form would
                // write a declared field in the clear, or keep ciphertext a
                // writer without keys could not have produced.
                let mut fields = if decoded {
                    fields
                } else {
                    policy.decode(&collection.name, fields)?
                };
                merge_top_level(&mut fields, changes);
                let canonical = policy.encode(&collection.name, fields)?;
                plan.index_delta(
                    &Self::document_keys(collection.word_fields(), &stored.entry.id, &stored.body)?,
                    &Self::document_keys(collection.word_fields(), &stored.entry.id, &canonical)?,
                );
                let mut entry = stored.entry.clone();
                entry.modified_ms = now_millis()?;
                identifiers.push(entry.id.clone());
                plan.push(
                    &collection.path,
                    (entry, Some(&canonical)),
                    Some(stored.location),
                )?;
            }
            Ok(Self::segment_result(
                plan,
                SqlMutationResultKind::Update,
                identifiers,
            ))
        })
    }

    pub(crate) fn sql_delete_segmented(
        &self,
        collection: &NativeCollection,
        query: &StructuredQuery,
        codec: &dyn DocumentCodec,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        self.commit_logged(collection, || {
            let mut plan = SegmentPlan::new("delete-many");
            let mut identifiers = Vec::new();
            for (stored, _, _) in Self::segment_matches(collection, query, codec)? {
                plan.index_delta(
                    &Self::document_keys(collection.word_fields(), &stored.entry.id, &stored.body)?,
                    &BTreeSet::new(),
                );
                identifiers.push(stored.entry.id.clone());
                Self::push_state_change(&mut plan, collection, stored, LocationState::Deleted)?;
            }
            Ok(Self::segment_result(
                plan,
                SqlMutationResultKind::Delete,
                identifiers,
            ))
        })
    }

    fn live_document(
        collection: &NativeCollection,
        identifier: &str,
    ) -> Result<SegmentDocument, NativeStorageError> {
        SegmentStore::open_for_writer(collection.segment_medium()?)?
            .document(identifier, LocationState::Live)
    }

    /// Move a document between live and deleted, keeping its body,
    /// attributes, mode and time, as the file layout's rename does.
    fn push_state_change(
        plan: &mut SegmentPlan,
        collection: &NativeCollection,
        stored: SegmentDocument,
        state: LocationState,
    ) -> Result<(), NativeStorageError> {
        let mut entry = stored.entry.clone();
        entry.state = state;
        plan.push(
            &collection.path,
            (entry, Some(&stored.body)),
            Some(stored.location),
        )
    }

    /// Live documents matching `query`, in identifier order, each with its
    /// parsed fields. Where `codec` says decoding changes what a query sees,
    /// the query runs against decoded fields.
    fn segment_matches(
        collection: &NativeCollection,
        query: &StructuredQuery,
        codec: &dyn DocumentCodec,
    ) -> Result<SegmentMatches, NativeStorageError> {
        let decoded = codec.decodes_for_query(collection.name())?;
        let store = SegmentStore::open_for_writer(collection.segment_medium()?)?;
        let limit = query.limit().filter(|limit| *limit > 0);
        let mut matches = Vec::new();
        for batch in store
            .ids(LocationState::Live)
            .chunks(Self::SEGMENT_SCAN_BATCH)
        {
            for stored in store.documents(batch, LocationState::Live)? {
                let decode = |fields| {
                    if decoded {
                        codec.decode(collection.name(), fields)
                    } else {
                        Ok(fields)
                    }
                };
                if let Some(fields) = Self::segment_match(&stored, query, decode)? {
                    matches.push((stored, fields, decoded));
                }
                if limit.is_some_and(|limit| matches.len() >= limit) {
                    return Ok(matches);
                }
            }
        }
        Ok(matches)
    }

    fn segment_match(
        stored: &SegmentDocument,
        query: &StructuredQuery,
        decode: impl FnOnce(Map<String, Value>) -> Result<Map<String, Value>, NativeStorageError>,
    ) -> Result<Option<Map<String, Value>>, NativeStorageError> {
        let fields = decode(parse_document_fields(&stored.body)?)?;
        let timestamps = TtidTimestamps::decode(&stored.entry.id).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::InvalidDocumentId,
                format!("document TTID is invalid: {error}"),
            )
        })?;
        if !query.matches(&fields, timestamps.created_at, stored.entry.modified_ms) {
            return Ok(None);
        }
        Ok(Some(fields))
    }

    fn segment_result(
        plan: SegmentPlan,
        kind: SqlMutationResultKind,
        identifiers: Vec<String>,
    ) -> Planned<SqlMutationResult, SegmentPlan> {
        let result = SqlMutationResult {
            kind,
            affected: identifiers.len(),
            identifiers,
        };
        if plan.is_empty() {
            return (None, result);
        }
        (Some(plan), result)
    }

    fn segment_initial_attributes(
        metadata: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, String>, NativeStorageError> {
        let mut attributes = BTreeMap::new();
        for (name, value) in metadata {
            Self::set_metadata_attribute(&mut attributes, name, value)?;
        }
        Ok(attributes)
    }

    fn next_segment_meta_updated_at(stored: &SegmentDocument) -> Result<u64, NativeStorageError> {
        let previous = stored
            .attributes()?
            .get(crate::META_UPDATED_XATTR)
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(now_millis()?.max(previous.saturating_add(1)))
    }
}
