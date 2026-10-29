use super::super::{CollectionWriteLock, NativeWriteRoot};
use super::compaction_report::CompactionReport;
use super::document_image::DocumentImage;
use super::file_export::FileExport;
use super::layout_migration_report::LayoutMigrationReport;
use super::salvage_report::SalvageReport;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_compactor::SegmentCompactor;
use super::segment_inspection::SegmentInspection;
use super::segment_rebuild::SegmentRebuild;
use super::segment_verification::SegmentVerification;
use crate::{GenerationStatus, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Whole-collection operations on a segment collection that run under the
/// collection lock instead of the transaction journal: a segment collection
/// never writes `state.json`, which stays the marker older releases refuse.
impl NativeWriteRoot {
    /// Rebuild the location table and the prefix index from segments.
    pub(crate) fn rebuild_segment_collection(
        &self,
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        let _lock = self.lock_collection(collection)?;
        self.settle_log(collection)?;
        let stored = collection.stored_generation()?;
        if stored.state != GenerationStatus::Stable {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::ConcurrentWrite,
                "segment collection is still writing after recovery",
            ));
        }
        let segment = Self::open_active_segment(collection, &stored)?;
        let generation = SegmentCheckpoint::bump_epoch(collection, &segment, stored.generation)?;
        SegmentRebuild::run(collection)?;
        SegmentCheckpoint::run(collection, segment, generation)?;
        Ok(())
    }

    /// Reclaim the space dead entries take in a segment collection's sealed
    /// segments (ADR 0015 section 7).
    ///
    /// # Errors
    ///
    /// Returns an error for a collection with the file layout, a damaged
    /// segment, lock contention, or an interrupted durable write, which the
    /// next opener recovers.
    pub fn compact_collection(&self, name: &str) -> Result<CompactionReport, NativeStorageError> {
        if !crate::NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        self.root.not_yet_in_database("compaction")?;
        let collection = self.root.collection(name)?;
        SegmentCompactor::require_segments(&collection)?;
        let _compaction = CollectionWriteLock::acquire_profiled(
            &self.root,
            &collection.path.join("locks"),
            "compaction.lock",
        )?;
        // A container medium closes when nothing holds it, and the copy in
        // progress is part of its open: keep it from preparation to
        // adoption.
        let _medium = collection.segment_medium()?;
        let Some(compactor) = self.prepare_compaction(&collection)? else {
            return Ok(CompactionReport::default());
        };
        compactor.copy(&collection)?;
        let _lock = self.lock_collection(&collection)?;
        let (stored, active) = self.settled_active(&collection)?;
        compactor.adopt(&collection, (&stored, active))
    }

    fn prepare_compaction(
        &self,
        collection: &NativeCollection,
    ) -> Result<Option<SegmentCompactor>, NativeStorageError> {
        let _lock = self.lock_collection(collection)?;
        let (stored, active) = self.settled_active(collection)?;
        SegmentCompactor::prepare(collection, &stored, active)
    }

    /// Recover the collection if needed and open its active segment.
    fn settled_active(
        &self,
        collection: &NativeCollection,
    ) -> Result<(crate::GenerationState, super::log_segment::LogSegment), NativeStorageError> {
        self.recover_log_if_needed(collection)?;
        let stored = collection.stored_generation()?;
        if stored.state != GenerationStatus::Stable {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::ConcurrentWrite,
                "segment collection is still writing after recovery",
            ));
        }
        let active = Self::open_active_segment(collection, &stored)?;
        Ok((stored, active))
    }

    /// Write a segment collection's documents as the per-document file
    /// tree under `out` (`docs/`, `.deleted/` and `index/`), without changing
    /// the collection. `out` must be absent or empty.
    ///
    /// # Errors
    ///
    /// Returns an error for a file collection, a non-empty `out`, or an I/O
    /// failure.
    pub fn export_collection(
        &self,
        name: &str,
        out: &std::path::Path,
    ) -> Result<LayoutMigrationReport, NativeStorageError> {
        if !crate::NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        let collection = self.root.collection(name)?;
        SegmentCompactor::require_segments(&collection)?;
        let root = Self::empty_output(out)?;
        let images = {
            let _lock = self.lock_collection(&collection)?;
            self.recover_locked(&collection)?;
            DocumentImage::read_collection(&collection)?
        };
        FileExport::build(&root, &root.canonical, &images, collection.shard_width())?;
        let deleted = images
            .iter()
            .filter(|image| image.state == crate::segments::LocationState::Deleted)
            .count();
        Ok(LayoutMigrationReport {
            collection: name.to_owned(),
            layout: crate::CollectionLayout::Files,
            documents: images.len() - deleted,
            deleted,
        })
    }

    /// Check every record of a segment collection and compare its location
    /// table with a rebuild.
    ///
    /// # Errors
    ///
    /// Returns an error for a file collection or an I/O failure. Defects in
    /// the data are reported, not returned as errors.
    pub fn verify_segments(&self, name: &str) -> Result<SegmentVerification, NativeStorageError> {
        if !crate::NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        let collection = self.root.collection(name)?;
        SegmentCompactor::require_segments(&collection)?;
        let _lock = self.lock_collection(&collection)?;
        self.recover_log_if_needed(&collection)?;
        SegmentInspection::verify(&collection)
    }

    /// Write every readable document of a damaged segment collection to the
    /// file tree under `out`, stepping past damaged records. Runs no
    /// recovery and changes nothing in the collection.
    ///
    /// # Errors
    ///
    /// Returns an error for a file collection, a non-empty `out`, or an I/O
    /// failure.
    pub fn salvage_collection(
        &self,
        name: &str,
        out: &std::path::Path,
    ) -> Result<SalvageReport, NativeStorageError> {
        if !crate::NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        self.root.not_yet_in_database("salvage")?;
        let collection = self.root.collection(name)?;
        SegmentCompactor::require_segments(&collection)?;
        Self::empty_output(out)?;
        SegmentInspection::salvage(&collection, out)
    }

    pub(super) fn empty_output(
        out: &std::path::Path,
    ) -> Result<crate::NativeRoot, NativeStorageError> {
        fylo_vfs::create_dir_all(out).map_err(NativeStorageError::io)?;
        let occupied = fylo_vfs::read_dir(out)
            .map_err(NativeStorageError::io)?
            .next()
            .is_some();
        if occupied {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                format!("output directory {} is not empty", out.display()),
            ));
        }
        crate::NativeRoot::open(out)
    }
}
