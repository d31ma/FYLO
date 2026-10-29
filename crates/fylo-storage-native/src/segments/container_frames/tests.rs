use std::path::PathBuf;

use fylo_format::{
    ContainerCodec, Crc32c, LogBodyCodec, LogFormatError, LogRecord, LogRecordKind, LogScan,
    LogSegmentHeader, LogSegmentKind,
};
use serde_json::json;

use super::ContainerFrames;
use crate::segments::{ContainerMedium, SegmentMedium, SegmentReader};
use crate::{
    CollectionKind, CollectionLayout, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, RootConfig, train_dictionary,
};

/// Where a frame's codec header byte sits, after the marker, length and
/// checksum.
const HEADER: usize = LogSegmentKind::MARKER_BYTES + LogRecord::FRAME_BYTES;

fn document(number: usize) -> String {
    json!({
        "id": format!("doc_{number:06}"),
        "status": "shipped",
        "city": "Halifax",
        "notes": "Leave it at the side door if nobody answers the bell.",
        "lines": (0..8).map(|line| json!({ "sku": format!("SKU-{line}"), "count": line })).collect::<Vec<_>>(),
    })
    .to_string()
}

fn record(blobs: &[u8]) -> LogRecord {
    LogRecord {
        lsn: 17,
        kind: LogRecordKind::Commit,
        generation: 3,
        manifest: br#"{"format":"fylo.segment.commit/v1","entries":[]}"#.to_vec(),
        blobs: blobs.to_vec(),
    }
}

fn frame(frames: &dyn LogBodyCodec, record: &LogRecord) -> Vec<u8> {
    LogSegmentKind::Documents
        .frame_with(record, frames)
        .unwrap()
}

/// A frame holding `stored` as its body, with a checksum that matches.
fn frame_of(stored: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xF5, 0x59, 0x52, 0x01];
    bytes.extend_from_slice(&u32::try_from(stored.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&Crc32c::checksum(stored).to_le_bytes());
    bytes.extend_from_slice(stored);
    bytes
}

#[test]
fn each_codec_round_trips_through_a_frame() {
    let frames = ContainerFrames::new(2, Vec::new(), 0);
    let large = record(document(1).repeat(4).as_bytes());
    let small = record(b"");

    let zstd = frame(&frames, &large);
    assert_eq!(zstd[HEADER], 1, "native writers write zstd");
    assert!(zstd.len() < LogSegmentKind::Documents.frame(&large).unwrap().len());
    let plain = frame(&frames, &small);
    assert_eq!(
        plain[HEADER], 0,
        "a body compression does not shrink is stored as is"
    );
    frames.write_lz4(true);
    let lz4 = frame(&frames, &large);
    assert_eq!(lz4[HEADER], 2, "the browser writes LZ4");
    frames.write_lz4(false);

    for (framed, expected) in [(&zstd, &large), (&plain, &small), (&lz4, &large)] {
        assert_eq!(
            LogSegmentKind::Documents.unframe_with(framed, &frames),
            Ok((expected.clone(), framed.len()))
        );
        // Another collection's frames, as a fresh process holds them, read
        // it too: nothing but the frame says how it was compressed.
        let other = ContainerFrames::new(9, Vec::new(), 0);
        assert_eq!(
            SegmentReader::decode(framed, &other).unwrap(),
            expected.clone()
        );
    }
}

#[test]
fn the_checksum_covers_the_stored_bytes() {
    let bodies = ContainerFrames::new(2, Vec::new(), 0);
    let large = record(document(2).repeat(3).as_bytes());
    let framed = frame(&bodies, &large);
    for index in LogSegmentKind::MARKER_BYTES + 4..framed.len() {
        let mut damaged = framed.clone();
        damaged[index] ^= 0x10;
        assert_eq!(
            LogSegmentKind::Documents.unframe_with(&damaged, &bodies),
            Err(LogFormatError::ChecksumMismatch),
            "byte {index}"
        );
        let error = SegmentReader::decode(&damaged, &bodies).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::CorruptDocument);
    }
}

#[test]
fn an_unknown_codec_is_refused_as_a_newer_format_never_as_damage() {
    let frames = ContainerFrames::new(2, Vec::new(), 0);
    let large = record(document(3).repeat(3).as_bytes());
    let mut stored = frame(&frames, &large)[HEADER..].to_vec();
    for header in [0x03, 0x0F, 0x41] {
        stored[0] = header;
        let unknown = frame_of(&stored);
        assert_eq!(
            LogSegmentKind::Documents.unframe_with(&unknown, &frames),
            Err(LogFormatError::UnsupportedVersion),
            "{header:#x}"
        );
        let error = SegmentReader::decode(&unknown, &frames).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
        assert!(error.to_string().contains("EFXDB_VERSION"), "{error}");

        // A scan stops there and says why, so recovery does not cut it off
        // as a torn tail.
        let mut segment = LogSegmentHeader {
            kind: LogSegmentKind::Documents,
            sequence: 1,
            first_lsn: 17,
        }
        .encode()
        .to_vec();
        segment.extend(&unknown);
        let scan = LogScan::segment_with(&segment, &frames).unwrap();
        assert!(scan.records.is_empty());
        assert_eq!(scan.stopped, Some(LogFormatError::UnsupportedVersion));
    }
}

#[test]
fn a_frame_names_its_dictionary_and_needs_it_to_be_read() {
    let samples: Vec<Vec<u8>> = (0..400)
        .map(|number| document(number).into_bytes())
        .collect();
    let dictionary = train_dictionary(7, &samples).expect("the samples train a dictionary");
    let bodies = ContainerFrames::new(2, vec![dictionary], 7);
    let large = record(document(500).as_bytes());
    let framed = frame(&bodies, &large);
    assert_eq!(framed[HEADER], 0x11, "zstd with a dictionary id");
    assert_eq!(framed[HEADER + 1..HEADER + 5], 7_u32.to_le_bytes());
    assert_eq!(SegmentReader::decode(&framed, &bodies).unwrap(), large);
    let without = ContainerFrames::new(2, Vec::new(), 0);
    let error = SegmentReader::decode(&framed, &without).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptDocument);
}

/// A root whose segment collection `items` keeps its segments in a
/// `.fxdb` container of 1 MiB extents.
struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&path).unwrap();
        let fixture = Self(std::fs::canonicalize(path).unwrap());
        fixture
            .writer()
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0)
            .unwrap()
            .with_config(RootConfig {
                layout: CollectionLayout::Segments,
                ..RootConfig::default()
            })
            .with_segment_container()
    }

    fn directory(&self) -> PathBuf {
        self.0.join(".collections/items")
    }

    fn container(&self) -> ContainerMedium {
        ContainerMedium::of(&self.directory()).unwrap().unwrap()
    }

    /// The codec of every record in every segment.
    fn codecs(&self) -> Vec<ContainerCodec> {
        let medium = self.container();
        let mut codecs = Vec::new();
        for number in medium.segment_numbers().unwrap() {
            let bytes = medium.require_segment(number).unwrap();
            let scan = LogScan::segment_with(&bytes, &*medium.frame_bodies()).unwrap();
            for (offset, _) in scan.records {
                let header = bytes[usize::try_from(offset).unwrap() + HEADER];
                codecs.push(ContainerCodec::from_code(header & 0x0F).unwrap());
            }
        }
        codecs
    }

    fn read(&self, number: usize) -> serde_json::Value {
        let bytes = NativeRoot::open(&self.0)
            .unwrap()
            .collection("items")
            .unwrap()
            .read_document(&identifier(number))
            .unwrap()
            .bytes;
        serde_json::from_slice(&bytes).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        ContainerMedium::forget(&self.directory());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 50_000 + number)
}

/// A document of about 5 KiB that compresses with either codec: half
/// repeated text, half not.
fn padded(number: usize, round: usize) -> serde_json::Value {
    json!({
        "name": format!("user{number}"),
        "round": round,
        "notes": "Leave it at the side door if nobody answers the bell. ".repeat(40),
        "noise": fylo_test_support::incompressible_text(u64::try_from(number * 1000 + round).unwrap(), 2048),
    })
}

fn put(writer: &NativeWriteRoot, number: usize) {
    writer
        .put_document(
            "items",
            &identifier(number),
            padded(number, 0).to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn patch(writer: &NativeWriteRoot, number: usize) {
    writer
        .patch_document(
            "items",
            &identifier(number),
            padded(number, 1).to_string().as_bytes(),
            None,
        )
        .unwrap();
}

#[test]
fn a_collection_written_natively_is_compressed_and_reads_back_after_a_reopen() {
    let fixture = Fixture::create("fxdb-frames-native");
    let writer = fixture.writer();
    for number in 0..20 {
        put(&writer, number);
    }
    ContainerMedium::forget(&fixture.directory());
    for number in 0..20 {
        assert_eq!(fixture.read(number), padded(number, 0));
    }
    let codecs = fixture.codecs();
    assert!(!codecs.contains(&ContainerCodec::Lz4), "{codecs:?}");
    assert!(
        codecs
            .iter()
            .filter(|codec| **codec == ContainerCodec::Zstd)
            .count()
            >= 20,
        "{codecs:?}"
    );
    let file = std::fs::read(fixture.directory().join(ContainerMedium::FILE_NAME)).unwrap();
    let phrase = b"if nobody answers the bell. Leave it";
    assert!(
        !file.windows(phrase.len()).any(|window| window == phrase),
        "document bodies are stored compressed"
    );
}

#[test]
fn lz4_records_read_natively_and_compaction_rewrites_them_as_zstd() {
    let fixture = Fixture::create("fxdb-frames-lz4");
    let writer = fixture.writer();
    let medium = fixture.container();
    medium.frames().write_lz4(true);
    for number in 0..250 {
        put(&writer, number);
    }
    medium.frames().write_lz4(false);
    for number in 0..200 {
        patch(&writer, number);
    }
    assert!(
        fixture.codecs().contains(&ContainerCodec::Lz4),
        "the browser's records are LZ4"
    );
    drop(medium);
    ContainerMedium::forget(&fixture.directory());
    for number in [0, 120, 249] {
        assert_eq!(
            fixture.read(number),
            padded(number, usize::from(number < 200))
        );
    }

    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    let codecs = fixture.codecs();
    assert!(!codecs.contains(&ContainerCodec::Lz4), "{codecs:?}");
    for number in [0, 210, 249] {
        assert_eq!(
            fixture.read(number),
            padded(number, usize::from(number < 200))
        );
    }
}
