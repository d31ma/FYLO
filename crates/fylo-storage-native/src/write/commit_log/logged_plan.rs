use std::collections::BTreeSet;
use std::path::PathBuf;

use super::super::schema_counts::SchemaCountDelta;
use super::placed_record::PlacedRecord;
use crate::{NativeCollection, NativeRoot, NativeStorageError};

/// A mutation planned as post-images, ready to become one commit record.
///
/// A file collection's plan writes files when applied; a segment
/// collection's plan only records where its entries landed.
pub(crate) trait LoggedPlan {
    /// The paths, real or virtual, that no other member of a busy period
    /// may touch until this plan is applied.
    fn touched(&self) -> &BTreeSet<PathBuf>;

    /// The canonical JSON manifest of the commit record.
    fn manifest(&self) -> Result<Vec<u8>, NativeStorageError>;

    /// The blob area of the commit record.
    fn blobs(&self) -> &[u8];

    /// Bring the collection up to date with the durable record. The flag in
    /// the error says whether the index delta was already appended.
    fn apply(
        &self,
        root: &NativeRoot,
        collection: &NativeCollection,
        placed: &PlacedRecord,
    ) -> Result<(), (NativeStorageError, bool)>;

    /// How this plan, once applied, changed the stored `_v` of the live
    /// documents of a collection that keeps convergence counts (ADR 0017
    /// section 6). A plan of a collection that keeps none changes nothing.
    fn schema_count_delta(
        &self,
        _collection: &NativeCollection,
    ) -> Result<SchemaCountDelta, NativeStorageError> {
        Ok(SchemaCountDelta::default())
    }

    /// Undo a failed apply from the in-memory pre-images.
    fn undo(
        &self,
        root: &NativeRoot,
        collection: &NativeCollection,
        index_applied: bool,
    ) -> Result<(), NativeStorageError>;
}
