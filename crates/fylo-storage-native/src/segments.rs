//! Append-only document segments (ADR 0015): the read side, and the formats
//! the write side shares with it.

mod container_frames;
mod container_medium;
mod directory_medium;
mod generation_slots;
mod index_delta;
mod location_state;
mod location_table;
mod positioned_read;
mod record_position;
mod segment_body;
mod segment_checkpoint_manifest;
mod segment_commit_manifest;
mod segment_document;
mod segment_entry;
mod segment_location;
mod segment_medium;
mod segment_paths;
mod segment_reader;
mod segment_store;
mod slot_record;
mod table_cache;
mod table_reach;

pub(crate) use container_frames::{ContainerFrames, DictionarySource};
pub(crate) use container_medium::{ContainerMedium, ContainerOpen, SlotWriter};
pub(crate) use directory_medium::{DirectoryMedium, FileSegment};
use generation_slots::GenerationSlots;
pub(crate) use index_delta::IndexDelta;
pub(crate) use location_state::LocationState;
pub(crate) use location_table::LocationTable;
use positioned_read::PositionedRead;
pub(crate) use record_position::RecordPosition;
pub(crate) use segment_body::SegmentBody;
pub(crate) use segment_checkpoint_manifest::SegmentCheckpointManifest;
pub(crate) use segment_commit_manifest::SegmentCommitManifest;
pub(crate) use segment_document::SegmentDocument;
pub(crate) use segment_entry::SegmentEntry;
pub(crate) use segment_location::SegmentLocation;
pub(crate) use segment_medium::{MediumIdentity, SegmentHandle, SegmentMedium, SnapshotStamp};
use segment_paths::SegmentPaths;
pub(crate) use segment_reader::SegmentReader;
pub(crate) use segment_store::SegmentStore;
pub(crate) use slot_record::SlotRecord;
pub(crate) use table_cache::TableCache;
pub(crate) use table_reach::TableReach;

/// The medium of the segment collection in directory `collection`: its
/// `.fxdb` container when the directory holds one, else `segments/`.
pub(crate) fn medium_of(
    collection: &std::path::Path,
) -> Result<std::sync::Arc<dyn SegmentMedium>, crate::NativeStorageError> {
    if !crate::NativeCollection::SEGMENTS_SUPPORTED {
        return Err(crate::NativeStorageError::new(
            crate::NativeStorageErrorCode::Unsupported,
            "this build does not carry the segment layout",
        ));
    }
    Ok(match ContainerMedium::of(collection)? {
        Some(medium) => std::sync::Arc::new(medium),
        None => DirectoryMedium::shared(collection),
    })
}

impl SlotRecord {
    /// A placeholder generation state for a collection whose slots are both
    /// invalid.
    pub(crate) const LOST_TRANSACTION: &'static str = "log-segment-lost";
}
