//! The collection write-ahead log (ADR 0010).

mod abort_manifest;
mod blob_reference;
mod checkpoint_manifest;
mod collection_guard;
mod commit_manifest;
mod commit_plan;
mod commit_queue;
mod commit_step;
mod compaction_manifest;
mod compaction_report;
mod document_image;
mod file_export;
mod layout_migration;
mod layout_migration_report;
mod log_checkpoint;
mod log_recovery;
mod log_segment;
mod logged_operations;
mod logged_plan;
mod logged_writer;
mod move_step;
mod placed_record;
mod record_image;
mod salvage_report;
mod segment_checkpoint;
mod segment_compactor;
mod segment_import;
mod segment_inspection;
mod segment_layout;
mod segment_maintenance;
mod segment_operations;
mod segment_plan;
mod segment_rebuild;
mod segment_recovery;
mod segment_replay;
mod segment_verification;
mod step_applier;
mod update_step;

pub(crate) use collection_guard::CollectionGuard;
pub(crate) use commit_queue::CommitQueue;
pub use compaction_report::CompactionReport;
pub use layout_migration_report::LayoutMigrationReport;
pub use salvage_report::SalvageReport;
pub(crate) use segment_layout::SegmentLayout;
pub use segment_verification::SegmentVerification;

/// A file's modification time in whole Unix milliseconds.
pub(crate) fn modified_ms(metadata: &fylo_vfs::Metadata) -> Result<u64, crate::NativeStorageError> {
    record_image::RecordImage::modified_ms_of(metadata)
}
