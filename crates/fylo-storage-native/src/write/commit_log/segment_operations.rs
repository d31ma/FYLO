use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_format::TtidTimestamps;
use fylo_query::StructuredQuery;
use serde_json::{Map, Value};

use super::super::{
    NativeWriteRoot, SqlMutationResult, SqlMutationResultKind, WriteAccess, WriteActor,
    encode_document_fields, merge_top_level, now_millis, parse_document_fields,
    require_write_access,
};
use super::commit_plan::Ownership;
use super::logged_writer::Planned;
use super::segment_plan::SegmentPlan;
use crate::segments::{LocationState, SegmentDocument, SegmentEntry, SegmentStore};
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Live documents a SQL mutation matched, each with its parsed fields.
type SegmentMatches = Vec<(SegmentDocument, Map<String, Value>)>;

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
        (metadata, access): (&BTreeMap<String, Value>, WriteAccess),
    ) -> Result<(), NativeStorageError> {
        let after = Self::document_keys(identifier, canonical)?;
        self.commit_logged(collection, || {
            let store = SegmentStore::open_for_writer(collection.segment_medium())?;
            let previous = store.table().get(identifier).cloned();
            if previous.is_some() {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "document identifier already exists",
                ));
            }
            let (attributes, mode) =
                Self::segment_initial_attributes(collection, metadata, access)?;
            let entry = SegmentEntry {
                id: identifier.to_owned(),
                state: LocationState::Live,
                body: None,
                attributes,
                modified_ms: now_millis()?,
                mode,
            };
            let mut plan = SegmentPlan::new("put-document");
            plan.push(&collection.path, (entry, Some(canonical)), None)?;
            plan.index_delta(&BTreeSet::new(), &after);
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn patch_document_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        actor: Option<&WriteActor>,
        mut patch: impl FnMut(&[u8]) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            require_write_access(stored.access()?, actor)?;
            let canonical = patch(&stored.body)?;
            let mut plan = SegmentPlan::new("patch-document");
            plan.index_delta(
                &Self::document_keys(identifier, &stored.body)?,
                &Self::document_keys(identifier, &canonical)?,
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
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            require_write_access(stored.access()?, actor)?;
            let mut plan = SegmentPlan::new("delete-document");
            plan.index_delta(
                &Self::document_keys(identifier, &stored.body)?,
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
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let store = SegmentStore::open_for_writer(collection.segment_medium())?;
            if store.location(identifier, LocationState::Live).is_some() {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "cannot restore a record that already exists",
                ));
            }
            let deleted = store.document(identifier, LocationState::Deleted)?;
            require_write_access(deleted.access()?, actor)?;
            let mut plan = SegmentPlan::new("restore-document");
            plan.index_delta(
                &BTreeSet::new(),
                &Self::document_keys(identifier, &deleted.body)?,
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
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            require_write_access(stored.access()?, actor)?;
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

    pub(crate) fn set_record_access_segmented(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        access: WriteAccess,
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        Self::require_posix_access()?;
        self.commit_logged(collection, || {
            let stored = Self::live_document(collection, identifier)?;
            let descriptor = stored.access()?;
            require_write_access(descriptor, actor)?;
            let (uid, gid) = match descriptor {
                Some(descriptor) => (descriptor.uid, descriptor.gid),
                None => Self::segment_owner(collection)?,
            };
            let ownership = Ownership {
                mode: Some(access.mode.unwrap_or(Self::DEFAULT_ACCESS_MODE)),
                uid: Some(access.uid.unwrap_or(uid)),
                gid: Some(access.gid.unwrap_or(gid)),
            };
            let mut entry = stored.entry.clone();
            entry.attributes.insert(
                crate::ACCESS_XATTR.into(),
                Self::access_attribute(ownership)?,
            );
            entry.mode = ownership.mode;
            let mut plan = SegmentPlan::new("set-access");
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
        actor: Option<&WriteActor>,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        self.commit_logged(collection, || {
            let mut plan = SegmentPlan::new("update-many");
            let mut identifiers = Vec::new();
            for (stored, mut fields) in Self::segment_matches(collection, query, actor)? {
                merge_top_level(&mut fields, changes);
                let canonical = encode_document_fields(fields)?;
                plan.index_delta(
                    &Self::document_keys(&stored.entry.id, &stored.body)?,
                    &Self::document_keys(&stored.entry.id, &canonical)?,
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
        actor: Option<&WriteActor>,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        self.commit_logged(collection, || {
            let mut plan = SegmentPlan::new("delete-many");
            let mut identifiers = Vec::new();
            for (stored, _) in Self::segment_matches(collection, query, actor)? {
                plan.index_delta(
                    &Self::document_keys(&stored.entry.id, &stored.body)?,
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
        SegmentStore::open_for_writer(collection.segment_medium())?
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
    /// parsed fields. Any match the actor may not change fails the whole
    /// mutation, as the file layout does.
    fn segment_matches(
        collection: &NativeCollection,
        query: &StructuredQuery,
        actor: Option<&WriteActor>,
    ) -> Result<SegmentMatches, NativeStorageError> {
        let store = SegmentStore::open_for_writer(collection.segment_medium())?;
        let limit = query.limit().filter(|limit| *limit > 0);
        let mut matches = Vec::new();
        for batch in store
            .ids(LocationState::Live)
            .chunks(Self::SEGMENT_SCAN_BATCH)
        {
            for stored in store.documents(batch, LocationState::Live)? {
                if let Some(fields) = Self::segment_match(&stored, query, actor)? {
                    matches.push((stored, fields));
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
        actor: Option<&WriteActor>,
    ) -> Result<Option<Map<String, Value>>, NativeStorageError> {
        let fields = parse_document_fields(&stored.body)?;
        let timestamps = TtidTimestamps::decode(&stored.entry.id).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::InvalidDocumentId,
                format!("document TTID is invalid: {error}"),
            )
        })?;
        if !query.matches(&fields, timestamps.created_at, stored.entry.modified_ms) {
            return Ok(None);
        }
        require_write_access(stored.access()?, actor)?;
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
        collection: &NativeCollection,
        metadata: &BTreeMap<String, Value>,
        access: WriteAccess,
    ) -> Result<(BTreeMap<String, String>, Option<u32>), NativeStorageError> {
        let mut attributes = BTreeMap::new();
        for (name, value) in metadata {
            Self::set_metadata_attribute(&mut attributes, name, value)?;
        }
        if access.is_empty() {
            return Ok((attributes, None));
        }
        Self::require_posix_access()?;
        let (uid, gid) = Self::segment_owner(collection)?;
        let ownership = Ownership {
            mode: Some(access.mode.unwrap_or(Self::DEFAULT_ACCESS_MODE)),
            uid: Some(access.uid.unwrap_or(uid)),
            gid: Some(access.gid.unwrap_or(gid)),
        };
        attributes.insert(
            crate::ACCESS_XATTR.into(),
            Self::access_attribute(ownership)?,
        );
        Ok((attributes, ownership.mode))
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

    /// The owner and group a record of this collection is filed under when
    /// a write names neither: the owner a file this root's writer creates
    /// gets.
    #[cfg(unix)]
    fn segment_owner(collection: &NativeCollection) -> Result<(u32, u32), NativeStorageError> {
        collection.segment_medium().owner()
    }

    #[cfg(not(unix))]
    fn segment_owner(_collection: &NativeCollection) -> Result<(u32, u32), NativeStorageError> {
        Err(Self::posix_only())
    }

    // The signature mirrors the platforms without POSIX identities.
    #[allow(clippy::unnecessary_wraps)]
    #[cfg(unix)]
    fn require_posix_access() -> Result<(), NativeStorageError> {
        Ok(())
    }

    #[cfg(not(unix))]
    fn require_posix_access() -> Result<(), NativeStorageError> {
        Err(Self::posix_only())
    }

    #[cfg(not(unix))]
    fn posix_only() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "UID/GID/mode access control is available only on POSIX platforms",
        )
    }
}
