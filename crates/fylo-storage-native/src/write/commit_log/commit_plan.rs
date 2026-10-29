use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fylo_vfs as fs;

use super::super::schema_counts::SchemaCountDelta;
use super::super::{CapturedAttribute, json_error, portable_path, safe_relative_path};
use super::blob_reference::BlobReference;
use super::commit_manifest::CommitManifest;
use super::commit_step::CommitStep;
use super::logged_plan::LoggedPlan;
use super::move_step::MoveStep;
use super::placed_record::PlacedRecord;
use super::record_image::RecordImage;
use super::step_applier::StepApplier;
use super::update_step::UpdateStep;
use crate::write::{NativeWriteRoot, failpoint};
use crate::{NativeCollection, NativeRoot, NativeStorageError, NativeStorageErrorCode};

/// Owner, group and mode a step sets, each only when present.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Ownership {
    pub(crate) mode: Option<u32>,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<u32>,
}

/// One mutation, described as post-images before any file changes.
///
/// A plan is built under the collection lock from the current files. It
/// becomes one commit record, and its pre-images stay in memory so the writer
/// can undo a failed apply.
#[derive(Debug)]
pub(crate) struct CommitPlan {
    operation: String,
    steps: Vec<CommitStep>,
    blobs: Vec<u8>,
    index_removed: BTreeSet<String>,
    index_added: BTreeSet<String>,
    pre_images: Vec<(PathBuf, Option<RecordImage>)>,
    touched: BTreeSet<PathBuf>,
}

impl CommitPlan {
    pub(crate) fn new(operation: &str) -> Self {
        Self {
            operation: operation.into(),
            steps: Vec::new(),
            blobs: Vec::new(),
            index_removed: BTreeSet::new(),
            index_added: BTreeSet::new(),
            pre_images: Vec::new(),
            touched: BTreeSet::new(),
        }
    }

    /// Create a record file that does not exist yet.
    pub(crate) fn create(
        &mut self,
        collection: &NativeCollection,
        path: &Path,
        content: (&[u8], Vec<CapturedAttribute>),
        ownership: Ownership,
    ) -> Result<(), NativeStorageError> {
        let (bytes, attributes) = content;
        let blob = self.add_blob(bytes);
        self.pre_images.push((path.to_owned(), None));
        self.push_update(
            collection,
            path,
            UpdateStep {
                path: String::new(),
                create: true,
                blob: Some(blob),
                attributes: Some(attributes),
                mode: ownership.mode,
                uid: ownership.uid,
                gid: ownership.gid,
                modified_ms: super::super::now_millis()?,
            },
        )
    }

    /// Replace the bytes of an existing record file in place. `before` is
    /// the file's current bytes and modification time, already read by the
    /// caller.
    pub(crate) fn overwrite(
        &mut self,
        collection: &NativeCollection,
        path: &Path,
        before: RecordImage,
        bytes: &[u8],
    ) -> Result<(), NativeStorageError> {
        self.pre_images.push((path.to_owned(), Some(before)));
        let blob = self.add_blob(bytes);
        self.push_update(
            collection,
            path,
            UpdateStep {
                path: String::new(),
                create: false,
                blob: Some(blob),
                attributes: None,
                mode: None,
                uid: None,
                gid: None,
                modified_ms: super::super::now_millis()?,
            },
        )
    }

    /// Replace the attributes, and optionally the ownership, of an existing
    /// record file, keeping its bytes and modification time.
    pub(crate) fn set_attributes(
        &mut self,
        collection: &NativeCollection,
        path: &Path,
        attributes: Vec<CapturedAttribute>,
        ownership: Ownership,
    ) -> Result<(), NativeStorageError> {
        let modified_ms = self.capture(path)?.modified_ms;
        self.push_update(
            collection,
            path,
            UpdateStep {
                path: String::new(),
                create: false,
                blob: None,
                attributes: Some(attributes),
                mode: ownership.mode,
                uid: ownership.uid,
                gid: ownership.gid,
                modified_ms,
            },
        )
    }

    /// Rename a record file to a destination proven absent.
    pub(crate) fn rename(
        &mut self,
        collection: &NativeCollection,
        from: &Path,
        to: &Path,
    ) -> Result<(), NativeStorageError> {
        self.touched.insert(from.to_owned());
        self.touched.insert(to.to_owned());
        self.steps.push(CommitStep::Move(MoveStep {
            from: Self::relative(collection, from)?,
            to: Self::relative(collection, to)?,
        }));
        Ok(())
    }

    /// Record the index keys the mutation removes and adds.
    pub(crate) fn index_delta(&mut self, before: &BTreeSet<String>, after: &BTreeSet<String>) {
        self.index_removed.extend(before.difference(after).cloned());
        self.index_added.extend(after.difference(before).cloned());
    }

    /// Bytes this plan would put in its record's blob area.
    pub(crate) fn blob_bytes(&self) -> usize {
        self.blobs.len()
    }

    pub(crate) fn steps(&self) -> &[CommitStep] {
        &self.steps
    }

    /// How this plan, once applied, changed the stored `_v` of live
    /// documents (ADR 0017 section 6): a body it writes arrives with its
    /// blob and leaves with its pre-image, and a document it moves into or
    /// out of the live tree is read where the move left it.
    fn live_version_delta(
        &self,
        collection: &NativeCollection,
    ) -> Result<SchemaCountDelta, NativeStorageError> {
        let mut delta = SchemaCountDelta::default();
        let mut written: BTreeMap<PathBuf, &[u8]> = BTreeMap::new();
        for step in &self.steps {
            match step {
                CommitStep::Update(update) => {
                    let relative = safe_relative_path(&update.path)?;
                    let Some(blob) = update.blob.as_ref() else {
                        continue;
                    };
                    if SchemaCountDelta::is_live_document(&relative) {
                        written.insert(collection.path.join(relative), self.blob(blob)?);
                    }
                }
                CommitStep::Move(rename) => {
                    let from =
                        SchemaCountDelta::is_live_document(&safe_relative_path(&rename.from)?);
                    let to = safe_relative_path(&rename.to)?;
                    if from == SchemaCountDelta::is_live_document(&to) {
                        continue;
                    }
                    let moved =
                        fs::read(collection.path.join(&to)).map_err(NativeStorageError::io)?;
                    if from {
                        delta.remove(&moved)?;
                    } else {
                        delta.add(&moved)?;
                    }
                }
            }
        }
        for (path, after) in written {
            let before = self
                .pre_images
                .iter()
                .find(|(captured, _)| *captured == path)
                .and_then(|(_, image)| image.as_ref());
            if let Some(before) = before {
                delta.remove(&before.bytes)?;
            }
            delta.add(after)?;
        }
        Ok(delta)
    }

    fn blob(&self, blob: &BlobReference) -> Result<&[u8], NativeStorageError> {
        usize::try_from(blob.offset)
            .ok()
            .zip(usize::try_from(blob.length).ok())
            .and_then(|(offset, length)| self.blobs.get(offset..offset.checked_add(length)?))
            .ok_or_else(|| {
                NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "commit blob lies outside its plan",
                )
            })
    }

    /// The canonical JSON manifest this plan commits.
    pub(crate) fn manifest(&self) -> Result<Vec<u8>, NativeStorageError> {
        let manifest = CommitManifest {
            format: CommitManifest::FORMAT.into(),
            operation: self.operation.clone(),
            steps: self.steps.clone(),
            index_removed: self.index_removed.iter().cloned().collect(),
            index_added: self.index_added.iter().cloned().collect(),
        };
        serde_json::to_vec(&manifest).map_err(|error| json_error(&error))
    }

    fn capture(&mut self, path: &Path) -> Result<RecordImage, NativeStorageError> {
        let image = RecordImage::read(path)?.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::NotFound,
                "record changed before it could be planned",
            )
        })?;
        self.pre_images.push((path.to_owned(), Some(image.clone())));
        Ok(image)
    }

    fn push_update(
        &mut self,
        collection: &NativeCollection,
        path: &Path,
        mut step: UpdateStep,
    ) -> Result<(), NativeStorageError> {
        step.path = Self::relative(collection, path)?;
        self.touched.insert(path.to_owned());
        self.steps.push(CommitStep::Update(step));
        Ok(())
    }

    fn add_blob(&mut self, bytes: &[u8]) -> BlobReference {
        let offset = self.blobs.len() as u64;
        self.blobs.extend_from_slice(bytes);
        BlobReference {
            offset,
            length: bytes.len() as u64,
            sha256: crate::sha256_hex(bytes),
        }
    }

    fn relative(collection: &NativeCollection, path: &Path) -> Result<String, NativeStorageError> {
        let relative = path.strip_prefix(&collection.path).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "log step escapes its collection",
            )
        })?;
        portable_path(relative)
    }
}

impl LoggedPlan for CommitPlan {
    fn schema_count_delta(
        &self,
        collection: &NativeCollection,
    ) -> Result<SchemaCountDelta, NativeStorageError> {
        self.live_version_delta(collection)
    }

    fn touched(&self) -> &BTreeSet<PathBuf> {
        &self.touched
    }

    fn manifest(&self) -> Result<Vec<u8>, NativeStorageError> {
        Self::manifest(self)
    }

    fn blobs(&self) -> &[u8] {
        &self.blobs
    }

    fn apply(
        &self,
        root: &NativeRoot,
        collection: &NativeCollection,
        _placed: &PlacedRecord,
    ) -> Result<(), (NativeStorageError, bool)> {
        let applier = StepApplier::live(root, &collection.path);
        for step in &self.steps {
            applier
                .apply(step, &self.blobs)
                .map_err(|error| (error, false))?;
        }
        let delta = NativeWriteRoot::index_lines(&self.index_removed, &self.index_added);
        NativeWriteRoot::append_index(collection, &delta).map_err(|error| (error, true))?;
        failpoint("after-log-apply-step").map_err(|error| (error, true))
    }

    fn undo(
        &self,
        root: &NativeRoot,
        collection: &NativeCollection,
        index_applied: bool,
    ) -> Result<(), NativeStorageError> {
        StepApplier::live(root, &collection.path).undo(&self.steps, &self.pre_images)?;
        if index_applied {
            let inverse = NativeWriteRoot::index_lines(&self.index_added, &self.index_removed);
            NativeWriteRoot::append_index(collection, &inverse)?;
        }
        Ok(())
    }
}
