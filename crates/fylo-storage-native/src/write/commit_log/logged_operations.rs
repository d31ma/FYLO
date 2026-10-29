use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_format::{Document, DocumentLimits, LogRecord};
use fylo_query::{IndexLookupValue, StructuredQuery, index_entries_for_document};
use serde_json::{Map, Value};

use super::super::{
    CapturedAttribute, DocumentCodec, NativeWriteRoot, SqlMutationResult, SqlMutationResultKind,
    WriteAccess, WriteActor, capture_attributes, deleted_document_path, json_error,
    matching_documents, merge_top_level, next_meta_updated_at, record_target, require_write_access,
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
    const DEFAULT_ACCESS_MODE: u32 = 0o600;

    pub(crate) fn put_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        canonical: &[u8],
        (metadata, access): (&BTreeMap<String, Value>, WriteAccess),
    ) -> Result<(), NativeStorageError> {
        let after = Self::document_keys(identifier, canonical)?;
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
            let (attributes, ownership) = self.initial_attributes(&target, metadata, access)?;
            let mut plan = CommitPlan::new("put-document");
            plan.create(collection, &target, (canonical, attributes), ownership)?;
            plan.index_delta(&BTreeSet::new(), &after);
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn patch_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        actor: Option<&WriteActor>,
        mut patch: impl FnMut(&[u8]) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = collection.read_document(identifier)?;
            require_write_access(stored.access, actor)?;
            let canonical = patch(&stored.bytes)?;
            let before = Self::document_keys(identifier, &stored.bytes)?;
            let after = Self::document_keys(identifier, &canonical)?;
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
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let stored = collection.read_document(identifier)?;
            require_write_access(stored.access, actor)?;
            let target = deleted_document_path(collection, identifier);
            Self::require_no_tombstone(&target)?;
            let mut plan = CommitPlan::new("delete-document");
            plan.rename(collection, &stored.path, &target)?;
            plan.index_delta(
                &Self::document_keys(identifier, &stored.bytes)?,
                &BTreeSet::new(),
            );
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn restore_document_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            Self::require_no_live_record(collection, identifier)?;
            let deleted = collection.read_deleted_document(identifier)?;
            require_write_access(deleted.access, actor)?;
            let target = collection
                .path
                .join("docs")
                .join(crate::shard_of(identifier, collection.shard_width()))
                .join(format!("{identifier}.json"));
            let mut plan = CommitPlan::new("restore-document");
            plan.rename(collection, &deleted.path, &target)?;
            plan.index_delta(
                &BTreeSet::new(),
                &Self::document_keys(identifier, &deleted.bytes)?,
            );
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn write_record_metadata_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        (record, replace): (&Map<String, Value>, bool),
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let (target, access) = record_target(collection, identifier)?;
            require_write_access(access, actor)?;
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

    pub(crate) fn set_record_access_logged(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        access: WriteAccess,
        actor: Option<&WriteActor>,
    ) -> Result<(), NativeStorageError> {
        self.commit_logged(collection, || {
            let (target, descriptor) = record_target(collection, identifier)?;
            require_write_access(descriptor, actor)?;
            let (uid, gid) = Self::file_owner(&target)?;
            let ownership = Ownership {
                mode: Some(access.mode.unwrap_or(Self::DEFAULT_ACCESS_MODE)),
                uid: Some(access.uid.unwrap_or(uid)),
                gid: Some(access.gid.unwrap_or(gid)),
            };
            let mut attributes = Self::attribute_map(capture_attributes(&target)?);
            attributes.insert(
                crate::ACCESS_XATTR.into(),
                Self::access_attribute(ownership)?,
            );
            let mut plan = CommitPlan::new("set-access");
            plan.set_attributes(
                collection,
                &target,
                Self::attribute_list(attributes),
                ownership,
            )?;
            Ok((Some(plan), ()))
        })
    }

    pub(crate) fn sql_update_logged(
        &self,
        collection: &NativeCollection,
        (query, changes): (&StructuredQuery, &Map<String, Value>),
        actor: Option<&WriteActor>,
        codec: &dyn DocumentCodec,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        self.commit_logged(collection, || {
            let documents = matching_documents(collection, query, actor)?;
            let mut plan = CommitPlan::new("update-many");
            let mut identifiers = Vec::with_capacity(documents.len());
            for document in documents {
                let stored = collection.read_document(&document.identifier)?;
                let mut fields = codec.decode(&collection.name, document.fields)?;
                merge_top_level(&mut fields, changes);
                let canonical = codec.encode(&collection.name, fields)?;
                plan.index_delta(
                    &Self::document_keys(&document.identifier, &stored.bytes)?,
                    &Self::document_keys(&document.identifier, &canonical)?,
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
        actor: Option<&WriteActor>,
    ) -> Result<SqlMutationResult, NativeStorageError> {
        self.commit_logged(collection, || {
            let documents = matching_documents(collection, query, actor)?;
            let mut plan = CommitPlan::new("delete-many");
            let mut identifiers = Vec::with_capacity(documents.len());
            for document in documents {
                let target = deleted_document_path(collection, &document.identifier);
                Self::require_no_tombstone(&target)?;
                let stored = collection.read_document(&document.identifier)?;
                plan.rename(collection, &document.path, &target)?;
                plan.index_delta(
                    &Self::document_keys(&document.identifier, &stored.bytes)?,
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
        &self,
        target: &Path,
        metadata: &BTreeMap<String, Value>,
        access: WriteAccess,
    ) -> Result<(Vec<CapturedAttribute>, Ownership), NativeStorageError> {
        let mut attributes = BTreeMap::new();
        for (name, value) in metadata {
            Self::set_metadata_attribute(&mut attributes, name, value)?;
        }
        if access.is_empty() {
            return Ok((Self::attribute_list(attributes), Ownership::default()));
        }
        let (uid, gid) = match (access.uid, access.gid) {
            (Some(uid), Some(gid)) => (uid, gid),
            _ => self.creator_identity(target)?,
        };
        let ownership = Ownership {
            mode: Some(access.mode.unwrap_or(Self::DEFAULT_ACCESS_MODE)),
            uid: Some(access.uid.unwrap_or(uid)),
            gid: Some(access.gid.unwrap_or(gid)),
        };
        attributes.insert(
            crate::ACCESS_XATTR.into(),
            Self::access_attribute(ownership)?,
        );
        Ok((Self::attribute_list(attributes), ownership))
    }

    /// The owner and group a file this process creates beside `target` gets,
    /// which is what the journal path records when a put names neither.
    fn creator_identity(&self, target: &Path) -> Result<(u32, u32), NativeStorageError> {
        let parent = target.parent().ok_or_else(|| {
            NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "target has no parent")
        })?;
        super::super::ensure_directory(&self.root, parent)?;
        let probe = super::super::sibling_scratch(target);
        fylo_vfs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&probe)
            .map_err(NativeStorageError::io)?;
        let owner = Self::file_owner(&probe);
        fylo_vfs::remove_file(&probe).map_err(NativeStorageError::io)?;
        owner
    }

    #[cfg(unix)]
    fn file_owner(path: &Path) -> Result<(u32, u32), NativeStorageError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fylo_vfs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        Ok((metadata.uid(), metadata.gid()))
    }

    #[cfg(not(unix))]
    fn file_owner(_path: &Path) -> Result<(u32, u32), NativeStorageError> {
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "UID/GID/mode access control is available only on POSIX platforms",
        ))
    }

    fn access_attribute(ownership: Ownership) -> Result<String, NativeStorageError> {
        let descriptor = crate::AccessDescriptor {
            version: 1,
            uid: ownership.uid.unwrap_or_default(),
            gid: ownership.gid.unwrap_or_default(),
            mode: ownership.mode.unwrap_or(Self::DEFAULT_ACCESS_MODE),
        };
        let encoded = serde_json::to_vec(&descriptor).map_err(|error| json_error(&error))?;
        Ok(BASE64.encode(encoded))
    }

    fn set_metadata_attribute(
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
        identifier: &str,
        bytes: &[u8],
    ) -> Result<BTreeSet<String>, NativeStorageError> {
        let document = Document::parse(bytes, DocumentLimits::default()).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptDocument,
                format!("document is invalid during index update: {error}"),
            )
        })?;
        index_entries_for_document(identifier, document.fields(), |_, value| {
            Ok::<_, NativeStorageError>(IndexLookupValue::plain(value))
        })
    }
}
