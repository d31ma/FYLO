use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use fylo_format::{LogRecordKind, LogSalvage};

use super::document_image::DocumentImage;
use super::file_export::FileExport;
use super::salvage_report::SalvageReport;
use super::segment_rebuild::SegmentRebuild;
use super::segment_verification::SegmentVerification;
use crate::segments::{
    LocationTable, SegmentCommitManifest, SegmentDocument, SegmentLocation, SegmentPaths,
    SegmentStore,
};
use crate::{NativeCollection, NativeRoot, NativeStorageError};

/// Reads a segment collection record by record, without trusting its
/// location table: `fylo verify` and `fylo salvage` (ADR 0015 section 8).
pub(crate) struct SegmentInspection;

impl SegmentInspection {
    pub(crate) fn verify(
        collection: &NativeCollection,
    ) -> Result<SegmentVerification, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let mut verification = SegmentVerification::default();
        for number in paths.segment_numbers()? {
            Self::verify_segment(&paths, number, &mut verification)?;
        }
        let store = SegmentStore::open_for_writer(&collection.path)?;
        let rows: Vec<SegmentLocation> = store.table().rows().into_iter().cloned().collect();
        verification.documents = rows.len();
        let scans = match SegmentRebuild::scan_all(&paths) {
            Ok(scans) => scans,
            Err(error) => {
                verification
                    .defects
                    .push(format!("cannot rebuild locations: {error}"));
                return Ok(verification);
            }
        };
        let rebuilt = LocationTable::from_rows(Self::winners(&scans)?);
        for (row, rebuilt) in Self::pairs(&rows, &rebuilt) {
            verification.defects.push(format!(
                "location of {} differs from a rebuild: {} against {}",
                row.map_or("?", |row| row.id.as_str()),
                Self::describe(row),
                Self::describe(rebuilt),
            ));
        }
        Ok(verification)
    }

    pub(crate) fn salvage(
        collection: &NativeCollection,
        out: &Path,
    ) -> Result<SalvageReport, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let mut report = SalvageReport::default();
        let mut salvaged = Vec::new();
        for number in paths.segment_numbers()? {
            let bytes = fylo_vfs::read(paths.segment(number)).map_err(NativeStorageError::io)?;
            match LogSalvage::segment(&bytes) {
                Ok(salvage) => {
                    for (start, end) in &salvage.damaged {
                        report.damaged.push(format!("{number}:{start}-{end}"));
                    }
                    salvaged.push((number, salvage));
                }
                Err(error) => report
                    .damaged
                    .push(format!("{number}:0-{} ({error})", bytes.len())),
            }
        }
        let newest = Self::salvaged(&salvaged)?;
        report.lost = Self::lost(collection, &newest);
        let images = newest
            .into_values()
            .filter(|(_, document)| document.entry.state != crate::segments::LocationState::Absent)
            .map(|(_, document)| DocumentImage::from_segment(document))
            .collect::<Result<Vec<_>, _>>()?;
        report.recovered = images.len();
        fylo_vfs::create_dir_all(out).map_err(NativeStorageError::io)?;
        let root = NativeRoot::open(out)?;
        FileExport::build(&root, &root.canonical, &images, collection.shard_width())?;
        Ok(report)
    }

    fn verify_segment(
        paths: &SegmentPaths,
        number: u64,
        verification: &mut SegmentVerification,
    ) -> Result<(), NativeStorageError> {
        let bytes = fylo_vfs::read(paths.segment(number)).map_err(NativeStorageError::io)?;
        verification.segments += 1;
        let salvage = match LogSalvage::segment(&bytes) {
            Ok(salvage) => salvage,
            Err(error) => {
                verification
                    .defects
                    .push(format!("segment {number}: {error}"));
                return Ok(());
            }
        };
        for (start, end) in &salvage.damaged {
            verification.defects.push(format!(
                "segment {number}: bytes {start}-{end} hold no readable record"
            ));
        }
        for (offset, record) in &salvage.records {
            verification.records += 1;
            if record.kind == LogRecordKind::Commit {
                Self::verify_bodies(number, (*offset, record), verification)?;
            }
        }
        Ok(())
    }

    fn verify_bodies(
        number: u64,
        (offset, record): (u64, &fylo_format::LogRecord),
        verification: &mut SegmentVerification,
    ) -> Result<(), NativeStorageError> {
        let manifest = SegmentCommitManifest::decode(&record.manifest)?;
        for entry in &manifest.entries {
            let Some(body) = &entry.body else {
                continue;
            };
            let start = usize::try_from(body.offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(usize::try_from(body.length).unwrap_or(usize::MAX));
            let digest = record.blobs.get(start..end).map(crate::sha256_hex);
            if digest.as_deref() != Some(body.sha256.as_str()) {
                verification.defects.push(format!(
                    "segment {number} offset {offset}: body of {} does not match its digest",
                    entry.id
                ));
            }
        }
        Ok(())
    }

    fn winners(
        scans: &[(u64, fylo_format::LogScan)],
    ) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let aborted = SegmentRebuild::aborted(scans.iter().map(|(_, scan)| scan))?;
        let mut winners: BTreeMap<String, SegmentLocation> = BTreeMap::new();
        for (number, scan) in scans {
            let lengths = SegmentRebuild::frame_lengths(scan);
            for ((offset, record), length) in scan.records.iter().zip(lengths) {
                if record.kind != LogRecordKind::Commit || aborted.contains(&record.lsn) {
                    continue;
                }
                for row in SegmentRebuild::rows_of(*number, (*offset, length), record)? {
                    Self::keep_newest(&mut winners, row);
                }
            }
        }
        Ok(winners.into_values().collect())
    }

    fn keep_newest(winners: &mut BTreeMap<String, SegmentLocation>, row: SegmentLocation) {
        let newer = winners
            .get(&row.id)
            .is_none_or(|current| (row.lsn, row.segment) >= (current.lsn, current.segment));
        if newer {
            winners.insert(row.id.clone(), row);
        }
    }

    /// Rows whose sequence number or state disagrees between the table and
    /// a rebuild. A copy in another segment with the same sequence number is
    /// the same document and agrees.
    fn pairs<'a>(
        rows: &'a [SegmentLocation],
        rebuilt: &'a LocationTable,
    ) -> Vec<(Option<&'a SegmentLocation>, Option<&'a SegmentLocation>)> {
        let mut ids: BTreeSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        ids.extend(rebuilt.rows().into_iter().map(|row| row.id.as_str()));
        let table: BTreeMap<&str, &SegmentLocation> =
            rows.iter().map(|row| (row.id.as_str(), row)).collect();
        ids.into_iter()
            .map(|id| (table.get(id).copied(), rebuilt.get(id)))
            .filter(|(row, rebuilt)| {
                row.map(|row| (row.lsn, row.state.code()))
                    != rebuilt.map(|row| (row.lsn, row.state.code()))
            })
            .collect()
    }

    fn describe(row: Option<&SegmentLocation>) -> String {
        row.map_or_else(
            || "none".into(),
            |row| format!("sequence {} state {:?}", row.lsn, row.state),
        )
    }

    /// The newest readable entry per document, with its sequence number.
    fn salvaged(
        salvaged: &[(u64, LogSalvage)],
    ) -> Result<BTreeMap<String, (u64, SegmentDocument)>, NativeStorageError> {
        let mut aborted = BTreeSet::new();
        for (_, salvage) in salvaged {
            for (_, record) in &salvage.records {
                if record.kind == LogRecordKind::Abort
                    && let Ok(manifest) = serde_json::from_slice::<
                        super::abort_manifest::AbortManifest,
                    >(&record.manifest)
                {
                    aborted.insert(manifest.aborts);
                }
            }
        }
        let mut newest: BTreeMap<String, (u64, SegmentDocument)> = BTreeMap::new();
        for (number, salvage) in salvaged {
            for (offset, record) in &salvage.records {
                if record.kind != LogRecordKind::Commit || aborted.contains(&record.lsn) {
                    continue;
                }
                Self::collect(&mut newest, (*number, *offset), record)?;
            }
        }
        Ok(newest)
    }

    fn collect(
        newest: &mut BTreeMap<String, (u64, SegmentDocument)>,
        (number, offset): (u64, u64),
        record: &fylo_format::LogRecord,
    ) -> Result<(), NativeStorageError> {
        let manifest = SegmentCommitManifest::decode(&record.manifest)?;
        for (index, entry) in manifest.entries.iter().enumerate() {
            let location = SegmentLocation {
                id: entry.id.clone(),
                segment: number,
                offset,
                length: 0,
                entry: u16::try_from(index).unwrap_or(u16::MAX),
                state: entry.state,
                lsn: record.lsn,
            };
            let newer = newest
                .get(&entry.id)
                .is_none_or(|(lsn, _)| record.lsn >= *lsn);
            if newer {
                let document = SegmentDocument::from_record(&location, record)?;
                newest.insert(entry.id.clone(), (record.lsn, document));
            }
        }
        Ok(())
    }

    /// Identifiers the location table names at a sequence number no
    /// salvaged entry reached. Empty when the table itself is unreadable.
    fn lost(
        collection: &NativeCollection,
        newest: &BTreeMap<String, (u64, SegmentDocument)>,
    ) -> Vec<String> {
        let Ok(store) = SegmentStore::open_for_writer(&collection.path) else {
            return Vec::new();
        };
        store
            .table()
            .rows()
            .into_iter()
            .filter(|row| newest.get(&row.id).is_none_or(|(lsn, _)| *lsn < row.lsn))
            .map(|row| row.id.clone())
            .collect()
    }
}
