use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use fylo_format::{LogRecord, LogRecordKind, LogSalvage, LogSegmentKind};

use super::document_image::DocumentImage;
use super::file_export::FileExport;
use super::salvage_report::SalvageReport;
use super::segment_rebuild::SegmentRebuild;
use super::segment_schemas::{SchemaVersionManifest, SegmentSchemas};
use super::segment_verification::SegmentVerification;
use crate::segments::{
    LocationTable, RecordPosition, SegmentCommitManifest, SegmentDocument, SegmentLocation,
    SegmentMedium, SegmentStore, TableReach,
};
use crate::{NativeCollection, NativeRoot, NativeStorageError, NativeStorageErrorCode};

/// Reads a segment collection record by record, without trusting its
/// location table: `fylo verify` and `fylo salvage` (ADR 0015 section 8).
pub(crate) struct SegmentInspection;

/// Salvaged schema versions by ordinal: the sequence number of the record
/// that held each, and its bytes.
pub(crate) type SalvagedSchemas = BTreeMap<u32, (u64, Vec<u8>)>;

impl SegmentInspection {
    pub(crate) fn verify(
        collection: &NativeCollection,
    ) -> Result<SegmentVerification, NativeStorageError> {
        let medium = collection.segment_medium()?;
        let mut verification = SegmentVerification::default();
        let mut versions = Versions::default();
        for number in medium.segment_numbers()? {
            Self::verify_segment(&*medium, number, &mut verification, &mut versions)?;
        }
        // A medium without a location log replays the records after its
        // snapshot to build the table, so damage there is a defect too. The
        // table then stops at the damage, and the documents whose newest
        // version lay there are named as far as the damaged records show
        // them.
        let (rows, damage) = match Self::table(&medium) {
            Ok(loaded) => loaded,
            Err(error) if error.code() == NativeStorageErrorCode::CorruptDocument => {
                verification
                    .defects
                    .push(format!("cannot load locations: {error}"));
                return Ok(verification);
            }
            Err(error) => return Err(error),
        };
        if let Some(damage) = damage {
            verification.defects.push(format!(
                "locations stop at segment {} byte {}, where a record cannot be read",
                damage.segment, damage.offset
            ));
            let newest = versions.newest();
            for id in Self::lost_at(&*medium, damage, &newest)? {
                verification.defects.push(format!(
                    "the newest version of {id} is in a record that cannot be read"
                ));
            }
        }
        verification.documents = rows.len();
        let scans = match SegmentRebuild::scan_all(&*medium) {
            Ok(scans) => scans,
            Err(error) => {
                verification
                    .defects
                    .push(format!("cannot rebuild locations: {error}"));
                return Ok(verification);
            }
        };
        Self::verify_schemas(&*medium, &scans, &mut verification);
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
        let (mut report, images, versions) = Self::salvage_documents(collection)?;
        fylo_vfs::create_dir_all(out).map_err(NativeStorageError::io)?;
        let root = NativeRoot::open(out)?;
        FileExport::build(&root, &root.canonical, &images, collection.shard_width())?;
        report.schema_versions = Self::write_schemas(out, &versions)?;
        Ok(report)
    }

    /// Every readable document of a damaged segment collection, as the
    /// newest version a salvaged record holds, and every readable schema
    /// version by ordinal, with what was damaged and lost; nothing is
    /// written. A segment that cannot be read at all, as one whose extent
    /// header in a `.vdbx` file no longer names it, is reported damaged
    /// whole and stepped over.
    pub(crate) fn salvage_documents(
        collection: &NativeCollection,
    ) -> Result<(SalvageReport, Vec<DocumentImage>, SalvagedSchemas), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let mut report = SalvageReport::default();
        let mut salvaged = Vec::new();
        for number in medium.segment_numbers()? {
            let bytes = match medium.require_segment(number) {
                Ok(bytes) => bytes,
                Err(error) => {
                    let end = medium.max_segment_bytes();
                    report.damaged.push(format!("{number}:0-{end} ({error})"));
                    continue;
                }
            };
            match LogSalvage::segment_with(&bytes, &*medium.frame_bodies()) {
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
        report.lost = Self::lost(&medium, &newest)?;
        let versions = Self::salvaged_schemas(&salvaged);
        let images = newest
            .into_values()
            .filter(|(_, document)| document.entry.state != crate::segments::LocationState::Absent)
            .map(|(_, document)| DocumentImage::from_segment(document))
            .collect::<Result<Vec<_>, _>>()?;
        report.recovered = images.len();
        Ok((report, images, versions))
    }

    fn verify_segment(
        medium: &dyn SegmentMedium,
        number: u64,
        verification: &mut SegmentVerification,
        versions: &mut Versions,
    ) -> Result<(), NativeStorageError> {
        let bytes = medium.require_segment(number)?;
        verification.segments += 1;
        let salvage = match LogSalvage::segment_with(&bytes, &*medium.frame_bodies()) {
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
            versions.note(record);
            if record.kind == LogRecordKind::Commit {
                Self::verify_bodies(number, (*offset, record), verification)?;
            }
            if record.kind == LogRecordKind::Schema
                && let Err(error) = SchemaVersionManifest::of(record)
            {
                verification
                    .defects
                    .push(format!("segment {number} offset {offset}: {error}"));
            }
        }
        Ok(())
    }

    /// Compare the schemas snapshot with a rebuild from the schema records.
    fn verify_schemas(
        medium: &dyn SegmentMedium,
        scans: &[(u64, fylo_format::LogScan)],
        verification: &mut SegmentVerification,
    ) {
        let rebuilt = match SegmentSchemas::rebuild(scans) {
            Ok(rebuilt) => rebuilt,
            Err(error) => {
                verification
                    .defects
                    .push(format!("cannot rebuild the schema registry: {error}"));
                return;
            }
        };
        verification.schema_versions = rebuilt.entries().len();
        let listed = |schemas: &SegmentSchemas| -> Vec<(u32, u64, String)> {
            schemas
                .entries()
                .iter()
                .map(|entry| (entry.ordinal, entry.lsn, entry.sha256.clone()))
                .collect()
        };
        match SegmentSchemas::read(medium) {
            Ok(Some(Some(snapshot))) if listed(&snapshot) == listed(&rebuilt) => {}
            Ok(Some(Some(snapshot))) => verification.defects.push(format!(
                "schemas snapshot lists versions up to {} but the schema records hold up to {}",
                snapshot.head().unwrap_or(0),
                rebuilt.head().unwrap_or(0)
            )),
            Ok(None) if rebuilt.head().is_none() => {}
            Ok(None) => verification
                .defects
                .push("schemas snapshot is missing".to_owned()),
            Ok(Some(None)) => verification
                .defects
                .push("schemas snapshot cannot be read".to_owned()),
            Err(error) => verification
                .defects
                .push(format!("schemas snapshot cannot be read: {error}")),
        }
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

    /// The table's rows, and where it stops at a record that cannot be
    /// read: a medium without a location log builds its table from the
    /// records after the snapshot, and one that cannot be read there ends
    /// it (`LocationTable::load_salvaging`). With a location log the table
    /// does not depend on the records.
    fn table(
        medium: &Arc<dyn SegmentMedium>,
    ) -> Result<(Vec<SegmentLocation>, Option<RecordPosition>), NativeStorageError> {
        match SegmentStore::open_for_writer(Arc::clone(medium)) {
            Ok(store) => Ok((store.table().rows().into_iter().cloned().collect(), None)),
            Err(error)
                if error.code() == NativeStorageErrorCode::CorruptDocument
                    && !medium.has_location_log() =>
            {
                let slot = medium.require_slot()?;
                let reach = TableReach::published(&**medium, &slot);
                let table = LocationTable::load_salvaging(&**medium, reach)?;
                let rows = table.rows().into_iter().cloned().collect();
                Ok((rows, table.damage()))
            }
            Err(error) => Err(error),
        }
    }

    /// Every readable schema version no readable abort record cancelled, by
    /// ordinal. A version committed twice keeps its newest record.
    fn salvaged_schemas(salvaged: &[(u64, LogSalvage)]) -> SalvagedSchemas {
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
        let mut versions: BTreeMap<u32, (u64, Vec<u8>)> = BTreeMap::new();
        for (_, salvage) in salvaged {
            for (_, record) in &salvage.records {
                if record.kind != LogRecordKind::Schema || aborted.contains(&record.lsn) {
                    continue;
                }
                let Ok(manifest) = SchemaVersionManifest::of(record) else {
                    continue;
                };
                let newer = versions
                    .get(&manifest.ordinal)
                    .is_none_or(|(lsn, _)| record.lsn >= *lsn);
                if newer {
                    versions.insert(manifest.ordinal, (record.lsn, record.blobs.clone()));
                }
            }
        }
        versions
    }

    /// Write salvaged schema versions as the directory registry's files.
    fn write_schemas(
        out: &Path,
        versions: &SalvagedSchemas,
    ) -> Result<Vec<u32>, NativeStorageError> {
        if versions.is_empty() {
            return Ok(Vec::new());
        }
        let directory = out.join(".fylo-schemas");
        fylo_vfs::create_dir_all(&directory).map_err(NativeStorageError::io)?;
        for (ordinal, (_, version)) in versions {
            fylo_vfs::write(directory.join(format!("{ordinal}.json")), version)
                .map_err(NativeStorageError::io)?;
        }
        Ok(versions.keys().copied().collect())
    }

    /// Identifiers whose newest version no salvaged entry reached: those
    /// the location table names at a later sequence number, and, where the
    /// table stops at damage, those the damaged records name. Empty when
    /// the table itself is unreadable.
    fn lost(
        medium: &Arc<dyn SegmentMedium>,
        newest: &BTreeMap<String, (u64, SegmentDocument)>,
    ) -> Result<Vec<String>, NativeStorageError> {
        let Ok((rows, damage)) = Self::table(medium) else {
            return Ok(Vec::new());
        };
        let mut lost: BTreeSet<String> = rows
            .iter()
            .filter(|row| newest.get(&row.id).is_none_or(|(lsn, _)| *lsn < row.lsn))
            .map(|row| row.id.clone())
            .collect();
        if let Some(damage) = damage {
            let newest = newest
                .iter()
                .map(|(id, (lsn, _))| (id.clone(), *lsn))
                .collect();
            lost.extend(Self::lost_at(&**medium, damage, &newest)?);
        }
        Ok(lost.into_iter().collect())
    }

    /// Documents named by a damaged record of a document segment at or
    /// after `damage` that no readable record after it writes again.
    ///
    /// The location log a directory collection keeps names such documents
    /// whatever the damage. Without one, only a damaged record whose
    /// manifest still reads names them: its checksum fails, so the
    /// manifest is read unchecked, and only identifiers that are canonical
    /// TTIDs are taken from it. A record whose manifest is damaged too
    /// names nothing, and its byte range is all the report shows.
    fn lost_at(
        medium: &dyn SegmentMedium,
        damage: RecordPosition,
        newest: &BTreeMap<String, u64>,
    ) -> Result<Vec<String>, NativeStorageError> {
        let mut lost = BTreeSet::new();
        for number in medium.segment_numbers()? {
            if number < damage.segment {
                continue;
            }
            let bytes = medium.require_segment(number)?;
            let Ok(salvage) = LogSalvage::segment_with(&bytes, &*medium.frame_bodies()) else {
                continue;
            };
            if salvage.header.kind != LogSegmentKind::Documents {
                continue;
            }
            for (start, end) in &salvage.damaged {
                let at = RecordPosition {
                    segment: number,
                    offset: *start,
                };
                if at < damage {
                    continue;
                }
                // Records of a document segment are in sequence order, so
                // a version written again after the damage has a sequence
                // number above every record before it.
                let before = salvage
                    .records
                    .iter()
                    .filter(|(offset, _)| offset < start)
                    .map(|(_, record)| record.lsn)
                    .max()
                    .unwrap_or_else(|| salvage.header.first_lsn.saturating_sub(1));
                let range = usize::try_from(*start).unwrap_or(usize::MAX)
                    ..usize::try_from(*end).unwrap_or(usize::MAX);
                for id in Self::damaged_ids(medium, bytes.get(range).unwrap_or_default()) {
                    if newest.get(&id).is_none_or(|lsn| *lsn <= before) {
                        lost.insert(id);
                    }
                }
            }
        }
        Ok(lost.into_iter().collect())
    }

    /// The document identifiers a damaged frame's manifest still names.
    fn damaged_ids(medium: &dyn SegmentMedium, frame: &[u8]) -> Vec<String> {
        let bodies = medium.frame_bodies();
        let Some(manifest) = frame
            .get(LogSegmentKind::MARKER_BYTES..)
            .and_then(|frame| LogRecord::unchecked_manifest_with(frame, &*bodies))
        else {
            return Vec::new();
        };
        SegmentCommitManifest::decode(&manifest)
            .map(|manifest| {
                manifest
                    .entries
                    .into_iter()
                    .map(|entry| entry.id)
                    .filter(|id| crate::validate_canonical_ttid(id).is_ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The newest committed sequence number of every document a verify read,
/// without those an abort record cancelled.
#[derive(Default)]
struct Versions {
    written: Vec<(String, u64)>,
    aborted: BTreeSet<u64>,
}

impl Versions {
    fn note(&mut self, record: &LogRecord) {
        match record.kind {
            LogRecordKind::Commit => {
                if let Ok(manifest) = SegmentCommitManifest::decode(&record.manifest) {
                    self.written.extend(
                        manifest
                            .entries
                            .into_iter()
                            .map(|entry| (entry.id, record.lsn)),
                    );
                }
            }
            LogRecordKind::Abort => {
                if let Ok(manifest) =
                    serde_json::from_slice::<super::abort_manifest::AbortManifest>(&record.manifest)
                {
                    self.aborted.insert(manifest.aborts);
                }
            }
            _ => {}
        }
    }

    fn newest(&self) -> BTreeMap<String, u64> {
        let mut newest: BTreeMap<String, u64> = BTreeMap::new();
        for (id, lsn) in &self.written {
            if self.aborted.contains(lsn) {
                continue;
            }
            let entry = newest.entry(id.clone()).or_insert(*lsn);
            *entry = (*entry).max(*lsn);
        }
        newest
    }
}
