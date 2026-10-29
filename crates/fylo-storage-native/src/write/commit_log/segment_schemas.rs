use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use fylo_format::{LogRecord, LogRecordKind, LogScan};
use fylo_vfs::OpenOptions;
use serde::{Deserialize, Serialize};

use super::super::{durable_replace_owner_only, json_error, sibling_scratch, sync_parent};
use super::segment_rebuild::SegmentRebuild;
use crate::segments::{SegmentPaths, SegmentReader};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// The manifest of a schema record, kind 5 (ADR 0017 section 3).
///
/// The record's blob area holds the version exactly as the directory
/// registry would keep it in `<ordinal>.json`: storage passes its bytes
/// through unread, and records only the ordinal and the digest it needs to
/// order and check them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SchemaVersionManifest {
    pub(crate) format: String,
    pub(crate) ordinal: u32,
    pub(crate) sha256: String,
}

impl SchemaVersionManifest {
    pub(crate) const FORMAT: &'static str = "fylo.schema-version.v1";

    pub(crate) fn new(ordinal: u32, version: &[u8]) -> Self {
        Self {
            format: Self::FORMAT.into(),
            ordinal,
            sha256: crate::sha256_hex(version),
        }
    }

    /// The manifest of a schema record, checked against the version it
    /// carries.
    pub(crate) fn of(record: &LogRecord) -> Result<Self, NativeStorageError> {
        let manifest: Self = serde_json::from_slice(&record.manifest).map_err(|error| {
            SegmentSchemas::corrupt(&format!("schema record manifest: {error}"))
        })?;
        if manifest.format != Self::FORMAT || manifest.ordinal == 0 {
            return Err(SegmentSchemas::corrupt(
                "schema record manifest has an unknown format",
            ));
        }
        if crate::sha256_hex(&record.blobs) != manifest.sha256 {
            return Err(SegmentSchemas::corrupt(&format!(
                "schema version {} does not match its digest",
                manifest.ordinal
            )));
        }
        Ok(manifest)
    }
}

/// One committed schema version: where its record is, and what it holds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SchemaVersionEntry {
    pub(crate) ordinal: u32,
    pub(crate) lsn: u64,
    pub(crate) segment: u64,
    pub(crate) offset: u64,
    pub(crate) length: u32,
    pub(crate) sha256: String,
    /// The version's bytes, which are JSON and therefore UTF-8.
    pub(crate) version: String,
}

/// The schema registry of a segment collection, as its derived **schemas
/// snapshot** `segments/schemas.snapshot` keeps it (ADR 0017 section 3).
///
/// The schema records (kind 5) are the truth: the registry is every one of
/// them that no abort record cancelled, in ordinal order, and the head is
/// the newest. The snapshot holds that list with each version's bytes and
/// record location, so an open reads one small file instead of scanning
/// the whole log. A schema commit rewrites it before it publishes `stable`,
/// and a checkpoint records its digest, so recovery starts from the
/// snapshot the last checkpoint named and replays the schema records after
/// it. A snapshot that is missing, does not decode, or no longer matches
/// its records is rebuilt from the records.
///
/// The record locations are what keeps the records alive: compaction copies
/// every listed record into the segment it writes (keeping its sequence
/// number), and a segment that holds one is never removed as unreferenced.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SegmentSchemas {
    format: String,
    versions: Vec<SchemaVersionEntry>,
    /// Live documents per schema ordinal, keyed by the ordinal in decimal,
    /// for convergence tracking (ADR 0017 section 6). This release does not
    /// maintain counts: it writes none, carries over any it reads when it
    /// adds or moves a version, and a rebuild from records leaves them out
    /// to be recounted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    counts: Option<BTreeMap<String, u64>>,
}

/// Snapshots readers have decoded, keyed by path and checked against the
/// file's length and modification time. A snapshot is only ever replaced by
/// a rename, so a changed file shows a changed identity.
type ReaderCache = BTreeMap<PathBuf, (u64, Option<SystemTime>, Arc<SegmentSchemas>)>;

impl SegmentSchemas {
    pub(crate) const FORMAT: &'static str = "fylo.schemas-snapshot.v1";

    pub(crate) fn empty() -> Self {
        Self {
            format: Self::FORMAT.into(),
            versions: Vec::new(),
            counts: None,
        }
    }

    /// Ordinal of the head version, or `None` without a registry.
    pub(crate) fn head(&self) -> Option<u32> {
        self.versions.last().map(|entry| entry.ordinal)
    }

    /// The bytes of version `ordinal`, if it is committed.
    pub(crate) fn version(&self, ordinal: u32) -> Option<&[u8]> {
        self.versions
            .iter()
            .find(|entry| entry.ordinal == ordinal)
            .map(|entry| entry.version.as_bytes())
    }

    pub(crate) fn entries(&self) -> &[SchemaVersionEntry] {
        &self.versions
    }

    /// Segments that hold a schema record the registry lists.
    pub(crate) fn segments(&self) -> BTreeSet<u64> {
        self.versions.iter().map(|entry| entry.segment).collect()
    }

    /// The entry a schema record at `(segment, offset, length)` makes.
    pub(crate) fn entry_of(
        (segment, offset, length): (u64, u64, u32),
        record: &LogRecord,
    ) -> Result<SchemaVersionEntry, NativeStorageError> {
        let manifest = SchemaVersionManifest::of(record)?;
        let version = String::from_utf8(record.blobs.clone()).map_err(|_| {
            Self::corrupt(&format!(
                "schema version {} is not UTF-8 JSON",
                manifest.ordinal
            ))
        })?;
        Ok(SchemaVersionEntry {
            ordinal: manifest.ordinal,
            lsn: record.lsn,
            segment,
            offset,
            length,
            sha256: manifest.sha256,
            version,
        })
    }

    /// The registry with `entry` added as its new head. A replayed entry the
    /// registry already holds at the same sequence number changes nothing.
    pub(crate) fn with(mut self, entry: SchemaVersionEntry) -> Result<Self, NativeStorageError> {
        if let Some(held) = self
            .versions
            .iter()
            .find(|held| held.ordinal == entry.ordinal)
        {
            if held.lsn == entry.lsn && held.sha256 == entry.sha256 {
                return Ok(self);
            }
            return Err(Self::corrupt(&format!(
                "schema version {} was committed twice",
                entry.ordinal
            )));
        }
        if entry.ordinal != self.head().unwrap_or(0) + 1 {
            return Err(Self::corrupt(&format!(
                "schema version {} does not follow version {}",
                entry.ordinal,
                self.head().unwrap_or(0)
            )));
        }
        self.versions.push(entry);
        Ok(self)
    }

    /// Point every version whose record a compacted segment copied at the
    /// copy. `copies` maps a sequence number to the copy's segment, offset
    /// and length.
    pub(crate) fn relocated(mut self, copies: &BTreeMap<u64, (u64, u64, u32)>) -> Self {
        for entry in &mut self.versions {
            if let Some((segment, offset, length)) = copies.get(&entry.lsn) {
                entry.segment = *segment;
                entry.offset = *offset;
                entry.length = *length;
            }
        }
        self
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, NativeStorageError> {
        let mut bytes = serde_json::to_vec(self).map_err(|error| json_error(&error))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Decode a snapshot, or `None` when it is not one this release wrote
    /// or its versions do not hold together.
    fn decode(bytes: &[u8]) -> Option<Self> {
        let snapshot: Self = serde_json::from_slice(bytes).ok()?;
        let consistent = snapshot.format == Self::FORMAT
            && snapshot.versions.iter().enumerate().all(|(index, entry)| {
                u32::try_from(index + 1).is_ok_and(|ordinal| ordinal == entry.ordinal)
                    && crate::sha256_hex(entry.version.as_bytes()) == entry.sha256
            });
        consistent.then_some(snapshot)
    }

    /// The snapshot file's bytes, or `None` when there is none.
    pub(crate) fn snapshot_bytes(
        paths: &SegmentPaths,
    ) -> Result<Option<Vec<u8>>, NativeStorageError> {
        match fylo_vfs::read(paths.schemas_snapshot()) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// The snapshot's digest, or `None` when there is none, for a
    /// checkpoint record.
    pub(crate) fn digest(paths: &SegmentPaths) -> Result<Option<String>, NativeStorageError> {
        Ok(Self::snapshot_bytes(paths)?.map(|bytes| crate::sha256_hex(&bytes)))
    }

    /// The snapshot as the file holds it: `Ok(None)` when it is missing,
    /// `Ok(Some(None))` when it does not decode.
    #[allow(clippy::option_option)]
    pub(crate) fn read(paths: &SegmentPaths) -> Result<Option<Option<Self>>, NativeStorageError> {
        Ok(Self::snapshot_bytes(paths)?.map(|bytes| Self::decode(&bytes)))
    }

    /// The snapshot, if it is exactly the one a checkpoint recorded as
    /// `digest` (`None` meaning no snapshot at all).
    pub(crate) fn checkpointed(
        paths: &SegmentPaths,
        digest: Option<&str>,
    ) -> Result<Option<Self>, NativeStorageError> {
        let bytes = Self::snapshot_bytes(paths)?;
        let current = bytes.as_deref().map(crate::sha256_hex);
        if current.as_deref() != digest {
            return Ok(None);
        }
        Ok(match bytes {
            None => Some(Self::empty()),
            Some(bytes) => Self::decode(&bytes),
        })
    }

    /// Rebuild the registry from every schema record in `scans`, skipping
    /// those an abort record cancelled. Two records of one version with the
    /// same sequence number are a record and its compacted copy.
    pub(crate) fn rebuild(scans: &[(u64, LogScan)]) -> Result<Self, NativeStorageError> {
        let aborted = SegmentRebuild::aborted(scans.iter().map(|(_, scan)| scan))?;
        let mut found: BTreeMap<u32, SchemaVersionEntry> = BTreeMap::new();
        for (number, scan) in scans {
            let lengths = SegmentRebuild::frame_lengths(scan);
            for ((offset, record), length) in scan.records.iter().zip(lengths) {
                if record.kind != LogRecordKind::Schema || aborted.contains(&record.lsn) {
                    continue;
                }
                let entry = Self::entry_of((*number, *offset, length), record)?;
                match found.get(&entry.ordinal) {
                    Some(held) if held.lsn != entry.lsn || held.sha256 != entry.sha256 => {
                        return Err(Self::corrupt(&format!(
                            "schema version {} was committed twice",
                            entry.ordinal
                        )));
                    }
                    Some(held) if held.segment > entry.segment => {}
                    _ => {
                        found.insert(entry.ordinal, entry);
                    }
                }
            }
        }
        found.into_values().try_fold(Self::empty(), Self::with)
    }

    /// Rebuild the registry by scanning every segment the collection holds.
    pub(crate) fn rebuild_from(paths: &SegmentPaths) -> Result<Self, NativeStorageError> {
        Self::rebuild(&SegmentRebuild::scan_all(paths)?)
    }

    /// The registry a writer works from, under the collection lock: the
    /// snapshot when every version it lists is still the record at its
    /// location, otherwise a rebuild from the records. `descriptor_head` is
    /// the head the catalog descriptor names, which follows the records and
    /// can lag them but never lead them.
    pub(crate) fn load(
        paths: &SegmentPaths,
        descriptor_head: Option<u32>,
    ) -> Result<Self, NativeStorageError> {
        match Self::read(paths)? {
            Some(Some(snapshot)) if snapshot.confirmed(paths) => {
                snapshot.reconciled(paths, descriptor_head)
            }
            _ => Self::rebuild_from(paths)?.require_at_least(descriptor_head),
        }
    }

    /// The registry a reader works from: the snapshot as it is, decoded once
    /// per change of the file. A missing or unreadable snapshot is rebuilt
    /// from the records without being written, which only a writer does;
    /// a missing one under a descriptor that names no registry is a
    /// collection created before snapshots existed, which a writer's next
    /// checkpoint gives one, and reads as empty.
    pub(crate) fn for_reader(
        paths: &SegmentPaths,
        descriptor_head: Option<u32>,
    ) -> Result<Arc<Self>, NativeStorageError> {
        let path = paths.schemas_snapshot();
        let identity = match fylo_vfs::metadata(&path) {
            Ok(metadata) => Some((metadata.len(), metadata.modified().ok())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(NativeStorageError::io(error)),
        };
        let Some((length, modified)) = identity else {
            if descriptor_head.is_none() {
                return Ok(Arc::new(Self::empty()));
            }
            return Ok(Arc::new(
                Self::rebuild_from(paths)?.require_at_least(descriptor_head)?,
            ));
        };
        let cached = Self::reader_cache()
            .get(&path)
            .filter(|(cached_length, cached_modified, _)| {
                *cached_length == length && *cached_modified == modified
            })
            .map(|(_, _, snapshot)| Arc::clone(snapshot));
        let snapshot = match cached {
            Some(snapshot) => snapshot,
            None => match Self::read(paths)? {
                Some(Some(snapshot)) => {
                    let snapshot = Arc::new(snapshot);
                    Self::reader_cache().insert(path, (length, modified, Arc::clone(&snapshot)));
                    snapshot
                }
                _ => {
                    return Ok(Arc::new(
                        Self::rebuild_from(paths)?.require_at_least(descriptor_head)?,
                    ));
                }
            },
        };
        if snapshot.head() >= descriptor_head {
            return Ok(snapshot);
        }
        Ok(Arc::new(
            Self::rebuild_from(paths)?.require_at_least(descriptor_head)?,
        ))
    }

    /// This registry, or a rebuild when the descriptor names a newer head
    /// than it has: the snapshot is then older than its records.
    pub(crate) fn reconciled(
        self,
        paths: &SegmentPaths,
        descriptor_head: Option<u32>,
    ) -> Result<Self, NativeStorageError> {
        if self.head() >= descriptor_head {
            return Ok(self);
        }
        Self::rebuild_from(paths)?.require_at_least(descriptor_head)
    }

    fn require_at_least(self, descriptor_head: Option<u32>) -> Result<Self, NativeStorageError> {
        if self.head() >= descriptor_head {
            return Ok(self);
        }
        Err(Self::corrupt(&format!(
            "the collection descriptor names schema version {} but the collection's records hold only {}",
            descriptor_head.unwrap_or(0),
            self.head().unwrap_or(0)
        )))
    }

    /// Whether every listed version is still the schema record at its
    /// location.
    fn confirmed(&self, paths: &SegmentPaths) -> bool {
        let reader = SegmentReader::new(paths.clone());
        self.versions.iter().all(|entry| {
            usize::try_from(entry.length)
                .ok()
                .and_then(|length| reader.span(entry.segment, entry.offset, length).ok())
                .and_then(|frame| SegmentReader::decode(&frame).ok())
                .is_some_and(|record| {
                    record.kind == LogRecordKind::Schema
                        && record.lsn == entry.lsn
                        && SchemaVersionManifest::of(&record).is_ok_and(|manifest| {
                            manifest.ordinal == entry.ordinal && manifest.sha256 == entry.sha256
                        })
                })
        })
    }

    /// Write the snapshot durably.
    pub(crate) fn write(&self, paths: &SegmentPaths) -> Result<(), NativeStorageError> {
        durable_replace_owner_only(&paths.schemas_snapshot(), &self.encode()?)
    }

    /// Write the snapshot unless the file already holds exactly it.
    /// Returns whether it wrote.
    pub(crate) fn write_if_changed(
        &self,
        paths: &SegmentPaths,
    ) -> Result<bool, NativeStorageError> {
        let encoded = self.encode()?;
        if Self::snapshot_bytes(paths)?.as_deref() == Some(encoded.as_slice()) {
            return Ok(false);
        }
        durable_replace_owner_only(&paths.schemas_snapshot(), &encoded)?;
        Ok(true)
    }

    /// Put the snapshot back as it was before a schema commit or compaction
    /// that is being rolled back: `previous` holds its bytes, or `None` when
    /// there was none. This is a compensating step, so it passes no
    /// failpoint: an injected failure must not also stop the rollback.
    pub(crate) fn restore(
        paths: &SegmentPaths,
        previous: Option<&[u8]>,
    ) -> Result<(), NativeStorageError> {
        let path = paths.schemas_snapshot();
        if Self::snapshot_bytes(paths)?.as_deref() == previous {
            return Ok(());
        }
        let Some(bytes) = previous else {
            fylo_vfs::remove_file(&path).map_err(NativeStorageError::io)?;
            return sync_parent(&path);
        };
        let scratch = sibling_scratch(&path);
        let written = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&scratch).map_err(NativeStorageError::io)?;
            file.write_all(bytes).map_err(NativeStorageError::io)?;
            crate::sync_handle(&file).map_err(NativeStorageError::io)?;
            fylo_vfs::rename(&scratch, &path).map_err(NativeStorageError::io)?;
            sync_parent(&path)
        })();
        if written.is_err() {
            let _ = fylo_vfs::remove_file(&scratch);
        }
        written
    }

    fn reader_cache() -> std::sync::MutexGuard<'static, ReaderCache> {
        static CACHE: OnceLock<Mutex<ReaderCache>> = OnceLock::new();
        CACHE
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn corrupt(detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("schema registry is corrupt: {detail}"),
        )
    }
}
