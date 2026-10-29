use std::path::Path;

use fylo_format::ContainerExtentKind;

use super::super::{NativeWriteRoot, failpoint};
use super::database_maintenance_report::{DatabaseCompactionReport, DatabaseSalvageReport};
use super::salvage_report::SalvageReport;
use super::segment_compactor::SegmentCompactor;
use super::segment_inspection::SegmentInspection;
use crate::container_allocator::ContainerTailExtent;
use crate::container_database::ContainerDatabase;
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// `fylo compact db.fxdb [--shrink]` and `fylo salvage db.fxdb --out
/// <root>` (ADR 0016 sections 6 and 11, `docs/fxdb-segment-backend.md`,
/// "Compaction and shrinking").
///
/// Compaction runs collection by collection, each as a directory root's
/// collection compacts (ADR 0015 section 7). A shrink then returns the
/// space it freed to the disk: with the file held as its only writer
/// process, the last extents in use move nearer the front, a segment by a
/// compaction of that segment alone and a derived run, a dictionary or the
/// map by a copy, each committed as it would be on its own; the free
/// extents at the end are cut off, and holes are punched over the free
/// extents left where the filesystem punches them.
impl NativeWriteRoot {
    /// Moves in a row that may leave the end of the extents in use where
    /// it was before a shrink stops.
    const SHRINK_STALLS: usize = 3;

    /// Compact every collection of this `.fxdb` database and, with
    /// `shrink`, give the space freed back to the disk.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_UNSUPPORTED` for a directory root, `EKEY` for an
    /// encrypted database opened without its key, `EROOTLOCKED` when
    /// another writer process keeps the file a shrink needs to itself
    /// or a collection a compaction needs, and what a collection's
    /// compaction returns. A compaction or shrink that stops leaves the
    /// database whole: the next opener settles what it left.
    pub fn compact_database(
        &self,
        shrink: bool,
    ) -> Result<DatabaseCompactionReport, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        let database = self.require_database("fylo compact <db.fxdb>")?;
        database.require_key()?;
        let mut report = DatabaseCompactionReport {
            file_bytes_before: database.file_bytes()?,
            ..DatabaseCompactionReport::default()
        };
        let mut run = || -> Result<(), NativeStorageError> {
            for name in database.collection_names() {
                let collection = self.root.collection(&name)?;
                let compacted = self.compact(&collection, None)?;
                report.collections.push((name, compacted));
            }
            if shrink {
                self.shrink_database(&database, &mut report)?;
            }
            Ok(())
        };
        if shrink {
            database.exclusively(run)?;
        } else {
            run()?;
        }
        report.file_bytes_after = database.file_bytes()?;
        Ok(report)
    }

    /// Move the last extents in use nearer the front while a free extent
    /// lies before them, cut the free extents at the end off, and punch
    /// holes over the free extents left.
    fn shrink_database(
        &self,
        database: &ContainerDatabase,
        report: &mut DatabaseCompactionReport,
    ) -> Result<(), NativeStorageError> {
        // Each move takes the lowest free extent, so the end of what is in
        // use comes nearer; a few moves in a row that leave it where it was
        // (a segment whose compaction opened a new active one at the end)
        // stop the shrink there.
        let (mut end, mut stalled) = (u64::MAX, 0);
        loop {
            // The extents released so far are free once the map that
            // releases them is durable, and only a free extent takes a copy.
            database.checkpoint_map()?;
            let Some(tail) = database.movable_tail()? else {
                break;
            };
            if tail.index < end {
                (end, stalled) = (tail.index, 0);
            } else if stalled == Self::SHRINK_STALLS {
                break;
            } else {
                stalled += 1;
            }
            if !self.relocate(database, tail)? {
                break;
            }
            report.extents_moved += 1;
        }
        failpoint("after-shrink-relocate")?;
        database.cut_free_tail()?;
        failpoint("after-shrink-truncate")?;
        report.extents_punched = database.punch_free_extents()?;
        Ok(())
    }

    /// Move `tail` nearer the front of the file. Returns `false` for an
    /// extent that does not move: the directory, or one of a collection no
    /// longer live.
    fn relocate(
        &self,
        database: &ContainerDatabase,
        tail: ContainerTailExtent,
    ) -> Result<bool, NativeStorageError> {
        if tail.kind == ContainerExtentKind::Map {
            // The map moves at the next checkpoint.
            return Ok(true);
        }
        let Some(owner) = database.collection_with_id(tail.owner) else {
            return Ok(false);
        };
        let collection = self.root.collection(&owner.header.name)?;
        match tail.kind {
            ContainerExtentKind::Segment => {
                self.compact(&collection, Some(tail.sequence))?;
            }
            ContainerExtentKind::Locations
            | ContainerExtentKind::Index
            | ContainerExtentKind::Schemas => {
                let _lock = self.lock_collection(&collection)?;
                if !database.medium(owner.block)?.relocate_run(tail.kind)? {
                    return Ok(false);
                }
            }
            ContainerExtentKind::Dictionary => {
                let _lock = self.lock_collection(&collection)?;
                database
                    .medium(owner.block)?
                    .relocate_dictionary(tail.index)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Write every readable document and schema version of this `.fxdb`
    /// database as a new directory root in the file layout at `out`, which
    /// must be absent or empty, stepping over damaged records by their
    /// sync markers and damaged segment extents by their headers. Runs no
    /// recovery and changes nothing in the database.
    ///
    /// The file's superblock, map and directory must still read: a file
    /// whose structures are damaged is not opened.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_UNSUPPORTED` for a directory root or a non-empty
    /// `out`, `EKEY` for an encrypted database opened without its key, or
    /// an I/O failure. What a failed salvage wrote is removed.
    pub fn salvage_database(
        &self,
        out: &Path,
    ) -> Result<DatabaseSalvageReport, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        let database = self.require_database("fylo salvage <db.fxdb>")?;
        database.require_key()?;
        let salvaged = Self::writing(Self::empty_output(out)?);
        let mut report = DatabaseSalvageReport::default();
        let outcome = database
            .collection_names()
            .into_iter()
            .try_for_each(|name| {
                let collection = self.root.collection(&name)?;
                let written = Self::salvage_into(&salvaged, &collection)?;
                report.collections.push((name, written));
                Ok(())
            });
        if let Err(error) = outcome {
            // `out` was empty, so everything in it is this salvage's.
            let _ = Self::clear_output(salvaged.root.path());
            return Err(error);
        }
        Ok(report)
    }

    /// Salvage one collection into `salvaged`. Only the schema versions up
    /// to the first one missing become its registry, which a directory
    /// root reads from its head down.
    fn salvage_into(
        salvaged: &Self,
        collection: &NativeCollection,
    ) -> Result<SalvageReport, NativeStorageError> {
        let (mut report, images, versions) = SegmentInspection::salvage_documents(collection)?;
        let registry: Vec<Vec<u8>> = versions
            .iter()
            .zip(1..)
            .take_while(|((ordinal, _), expected)| **ordinal == *expected)
            .map(|((_, (_, version)), _)| version.clone())
            .collect();
        report.schema_versions = (1..).take(registry.len()).collect();
        Self::write_unpacked(salvaged, collection, &images, &registry)?;
        Ok(report)
    }

    fn require_database(
        &self,
        command: &str,
    ) -> Result<std::sync::Arc<ContainerDatabase>, NativeStorageError> {
        self.root.database().cloned().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                format!("{command} takes a .fxdb database, and this root is a directory"),
            )
        })
    }
}
