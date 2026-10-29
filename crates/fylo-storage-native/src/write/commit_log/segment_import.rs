use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_format::{LogRecord, LogRecordKind};

use super::super::{NativeWriteRoot, durable_replace, json_error};
use super::document_image::DocumentImage;
use super::log_segment::LogSegment;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_layout::SegmentLayout;
use crate::segments::{
    DirectoryMedium, LocationState, LocationTable, SegmentBody, SegmentCommitManifest,
    SegmentEntry, SegmentLocation, SegmentMedium, SlotRecord,
};
use crate::{LogPosition, NativeRoot, NativeStorageError, NativeStorageErrorCode};

/// Builds a complete segment layout from document images, for a migration
/// from the file layout (ADR 0015 section 9).
///
/// Each document becomes one commit record, in identifier order, so the
/// result reads like a freshly compacted collection. The derived files are
/// written directly and a closing checkpoint record describes them.
pub(crate) struct SegmentImport;

impl SegmentImport {
    pub(crate) fn build(
        root: &NativeRoot,
        directory: &Path,
        images: &[DocumentImage],
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        SegmentLayout::create_empty(root, directory, generation)?;
        let medium = DirectoryMedium::of(directory);
        let mut segment = Self::open_first(&medium)?;
        let mut rows = Vec::with_capacity(images.len());
        let mut keys = BTreeSet::new();
        let mut ordered: Vec<&DocumentImage> = images.iter().collect();
        ordered.sort_by(|left, right| left.id.cmp(&right.id));
        for image in ordered {
            rows.push(Self::append(&mut segment, image, generation)?);
            if image.state == LocationState::Live {
                keys.extend(NativeWriteRoot::document_keys(&image.id, &image.body)?);
            }
        }
        medium.replace_location_snapshot(&LocationTable::from_rows(rows).encode_snapshot()?)?;
        Self::write_keys(directory, keys)?;
        Self::close(&medium, directory, segment, generation)
    }

    fn open_first(medium: &dyn SegmentMedium) -> Result<LogSegment, NativeStorageError> {
        let slot = medium.require_slot()?;
        let position = LogPosition {
            nonce: format!("{:016x}", slot.segment),
            length: slot.length,
            next_lsn: slot.next_lsn,
            records: slot.records,
        };
        LogSegment::open_segment_matching(medium, &position)?.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "new segment layout does not match its slot",
            )
        })
    }

    fn append(
        segment: &mut LogSegment,
        image: &DocumentImage,
        generation: u64,
    ) -> Result<SegmentLocation, NativeStorageError> {
        let entry = SegmentEntry {
            id: image.id.clone(),
            state: image.state,
            body: Some(SegmentBody {
                offset: 0,
                length: image.body.len() as u64,
                sha256: crate::sha256_hex(&image.body),
            }),
            attributes: Self::encoded_attributes(&image.attributes),
            modified_ms: image.modified_ms,
            mode: image.mode,
        };
        let added = if image.state == LocationState::Live {
            NativeWriteRoot::document_keys(&image.id, &image.body)?
        } else {
            BTreeSet::new()
        };
        let manifest = SegmentCommitManifest {
            format: SegmentCommitManifest::FORMAT.into(),
            operation: "migrate".into(),
            entries: vec![entry],
            index_removed: Vec::new(),
            index_added: added.into_iter().collect(),
        };
        let record = LogRecord {
            lsn: segment.next_lsn(),
            kind: LogRecordKind::Commit,
            generation,
            manifest: serde_json::to_vec(&manifest).map_err(|error| json_error(&error))?,
            blobs: image.body.clone(),
        };
        let encoded = segment.encode(&record)?;
        let location = SegmentLocation {
            id: image.id.clone(),
            segment: segment.sequence(),
            offset: segment.length(),
            length: u32::try_from(encoded.len()).map_err(|_| {
                NativeStorageError::new(
                    NativeStorageErrorCode::FileTooLarge,
                    "document record is too large",
                )
            })?,
            entry: 0,
            state: image.state,
            lsn: record.lsn,
        };
        segment.append(&encoded, 1)?;
        Ok(location)
    }

    pub(crate) fn encoded_attributes(
        attributes: &BTreeMap<String, Vec<u8>>,
    ) -> BTreeMap<String, String> {
        attributes
            .iter()
            .map(|(name, value)| (name.clone(), BASE64.encode(value)))
            .collect()
    }

    fn write_keys(directory: &Path, keys: BTreeSet<String>) -> Result<(), NativeStorageError> {
        let index = directory.join("index");
        let mut snapshot = Vec::new();
        for key in keys {
            snapshot.extend_from_slice(key.as_bytes());
            snapshot.push(b'\n');
        }
        durable_replace(&index.join("keys.snapshot"), &snapshot)?;
        durable_replace(&index.join("keys.wal"), b"")
    }

    /// Append the closing checkpoint, flush, and publish the stable slot.
    fn close(
        medium: &dyn SegmentMedium,
        directory: &Path,
        mut segment: LogSegment,
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        let manifest = SegmentCheckpoint::manifest(directory, medium)?;
        let record = LogRecord {
            lsn: segment.next_lsn(),
            kind: LogRecordKind::Checkpoint,
            generation,
            manifest: serde_json::to_vec(&manifest).map_err(|error| json_error(&error))?,
            blobs: Vec::new(),
        };
        let encoded = segment.encode(&record)?;
        segment.append(&encoded, 1)?;
        segment.full_flush()?;
        medium.publish_slot(&SlotRecord {
            generation,
            writing: false,
            segment: segment.sequence(),
            length: segment.length(),
            next_lsn: segment.next_lsn(),
            records: 0,
            locations_length: 0,
            table_epoch: 0,
        })?;
        medium.sync_slots()
    }
}
