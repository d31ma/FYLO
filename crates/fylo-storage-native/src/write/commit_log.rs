//! The collection write-ahead log (ADR 0010).

mod abort_manifest;
mod blob_reference;
mod checkpoint_manifest;
mod collection_guard;
mod commit_manifest;
mod commit_plan;
mod commit_queue;
mod commit_step;
mod log_checkpoint;
mod log_recovery;
mod log_segment;
mod logged_operations;
mod logged_writer;
mod move_step;
mod record_image;
mod step_applier;
mod update_step;

pub(crate) use collection_guard::CollectionGuard;
pub(crate) use commit_queue::CommitQueue;

/// A file's modification time in whole Unix milliseconds.
pub(crate) fn modified_ms(metadata: &fylo_vfs::Metadata) -> Result<u64, crate::NativeStorageError> {
    record_image::RecordImage::modified_ms_of(metadata)
}
