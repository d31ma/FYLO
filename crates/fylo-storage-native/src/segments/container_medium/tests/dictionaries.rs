//! Compression dictionaries of a container collection (ADR 0016 section 8):
//! training once the records reach 1 MiB, writing with the dictionary in
//! zstd and LZ4, reading after a reopen, refusing a record whose dictionary
//! is missing or damaged, retraining at compaction and releasing what no
//! record names, and the superblock's codec bits. The crash tests spawn this
//! test binary again with a failpoint, running [`a_dictionary_child_process`].

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fylo_format::{
    ContainerCodec, ContainerDictionaryHeader, ContainerExtentHeader, ContainerExtentKind,
    ContainerSuperblock, LogScan,
};
use serde_json::{Value, json};

use super::{ContainerMedium, Fixture, writer_at};
use crate::segments::SegmentMedium;
use crate::{
    ContainerFile, ContainerFrameCodec, ContainerStoredBody, DICTIONARY_TRAINING_BYTES,
    NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions,
};

const DICTIONARY: ContainerExtentKind = ContainerExtentKind::Dictionary;
const ROLE: &str = "FYLO_VDBX_DICTIONARY_CHILD_ROLE";
const ROOT: &str = "FYLO_VDBX_DICTIONARY_CHILD_ROOT";
const CHILD_TEST: &str =
    "segments::container_medium::tests::dictionaries::a_dictionary_child_process";

const FIRST: [&str; 8] = [
    "Amara", "Bastien", "Chiara", "Deepak", "Elena", "Farid", "Greta", "Hiroshi",
];
const LAST: [&str; 8] = [
    "Okafor",
    "Lindqvist",
    "Moreau",
    "Ramanathan",
    "Kowalski",
    "Haddad",
    "Nakamura",
    "Silva",
];
const CITIES: [&str; 6] = [
    "Halifax",
    "Saskatoon",
    "Thunder Bay",
    "Kelowna",
    "Moncton",
    "Sherbrooke",
];
const PRODUCTS: [(&str, &str); 8] = [
    ("KB-104", "Mechanical keyboard, brown switches"),
    ("MS-220", "Wireless mouse with USB-C receiver"),
    ("MN-270", "27 inch monitor, 1440p, matte panel"),
    ("HD-512", "Portable SSD, 512 GB, aluminium case"),
    ("CB-003", "Braided charging cable, two metres"),
    ("LS-090", "Adjustable laptop stand, walnut finish"),
    ("WC-108", "1080p webcam with privacy shutter"),
    ("HP-330", "Over-ear headphones, noise cancelling"),
];
const NOTES: [&str; 5] = [
    "Leave the parcel at the side door if nobody answers.",
    "Gift wrap please, and leave the receipt out of the box.",
    "Call ahead; the buzzer at the front entrance is broken.",
    "Deliver to the loading dock behind the building.",
    "",
];
/// Readings a shifted collection holds: past 1 MiB of records.
const READINGS: usize = 1600;
const SITES: [&str; 4] = ["north-yard", "pump-house", "greenhouse-2", "roof-array"];

/// A pseudo-random number for `number` and `salt`.
fn pick(number: usize, salt: usize, below: usize) -> usize {
    let mixed = (number as u64 + 1)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(17)
        ^ (salt as u64 + 1).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    usize::try_from(mixed.rotate_left(29) % below as u64).unwrap()
}

/// An order: distinct values in the shape every order shares, the kind of
/// document a dictionary is for.
pub(super) fn order(number: usize) -> Value {
    let lines: serde_json::Map<String, Value> = (0..=pick(number, 1, 4))
        .map(|line| {
            let (sku, title) = PRODUCTS[pick(number, 10 + line, PRODUCTS.len())];
            let line_item = json!({
                "sku": sku,
                "title": title,
                "quantity": 1 + pick(number, 20 + line, 3),
                "unitPriceCents": 1999 + 100 * pick(number, 30 + line, 90),
            });
            (format!("line{line}"), line_item)
        })
        .collect();
    let (first, last) = (
        FIRST[pick(number, 2, FIRST.len())],
        LAST[pick(number, 3, LAST.len())],
    );
    json!({
        "orderId": format!("ord_{:08x}", number * 7919 + 13),
        "customer": {
            "name": format!("{first} {last}"),
            "email": format!("{}.{}{}@example.com", first.to_lowercase(), last.to_lowercase(), number % 97),
            "city": CITIES[pick(number, 4, CITIES.len())],
            "postcode": format!("B{}{} {}", pick(number, 5, 10), (b'A' + u8::try_from(pick(number, 6, 26)).unwrap()) as char, pick(number, 7, 900) + 100),
        },
        "status": (["placed", "packed", "shipped", "delivered"][pick(number, 8, 4)]),
        "placedAt": format!("2026-{:02}-{:02}T{:02}:{:02}:00Z", 1 + pick(number, 9, 12), 1 + pick(number, 11, 28), pick(number, 12, 24), pick(number, 13, 60)),
        "lines": lines,
        "notes": NOTES[pick(number, 14, NOTES.len())],
    })
}

/// A sensor reading: another collection's shape, sharing almost nothing
/// with an order.
fn reading(number: usize) -> Value {
    json!({
        "sensor": format!("s-{:04}", pick(number, 1, 400)),
        "site": SITES[pick(number, 2, SITES.len())],
        "firmware": format!("v2.{}.{}", pick(number, 3, 4), pick(number, 4, 10)),
        "samples": (0..6).map(|sample| (format!("t{}", sample * 600), json!({
            "celsius": format!("{}.{}", 10 + pick(number, 10 + sample, 15), pick(number, 20 + sample, 10)),
            "relativeHumidity": 30 + pick(number, 30 + sample, 50),
            "battery": (["ok", "low"][usize::from(pick(number, 40 + sample, 20) == 0)]),
        }))).collect::<serde_json::Map<String, Value>>(),
        "alarm": pick(number, 5, 30) == 0,
    })
}

fn identifier(number: usize) -> String {
    format!("4VRNF6{:05}", 60_000 + number)
}

fn put(writer: &NativeWriteRoot, number: usize, document: &Value) {
    writer
        .put_document(
            "items",
            &identifier(number),
            document.to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn read(fixture: &Fixture, number: usize) -> Value {
    let bytes = fixture
        .collection()
        .read_document(&identifier(number))
        .unwrap()
        .bytes;
    serde_json::from_slice(&bytes).unwrap()
}

/// Bytes of commit record bodies the collection holds, up to the training
/// threshold, as training counts them.
fn record_bytes(container: &ContainerMedium) -> usize {
    container.samples(false).unwrap().iter().map(Vec::len).sum()
}

/// Put orders from `from` until the collection's records reach `bytes`.
/// Returns the next number.
fn put_orders_until(
    writer: &NativeWriteRoot,
    container: &ContainerMedium,
    from: usize,
    bytes: usize,
) -> usize {
    let mut number = from;
    while record_bytes(container) < bytes {
        for _ in 0..50 {
            put(writer, number, &order(number));
            number += 1;
        }
    }
    number
}

/// Each frame's (codec, dictionary id, stored bytes) of every segment.
fn frames(container: &ContainerMedium) -> Vec<(ContainerCodec, u32, usize)> {
    let mut found = Vec::new();
    for number in container.segment_numbers().unwrap() {
        let bytes = container.require_segment(number).unwrap();
        let (bodies, _) = LogScan::stored_bodies(&bytes, &*container.frame_bodies()).unwrap();
        for (_, stored) in bodies {
            let parsed = ContainerStoredBody::parse(stored).unwrap();
            found.push((parsed.codec, parsed.dictionary_id, stored.len()));
        }
    }
    found
}

impl Fixture {
    fn file_reader(&self) -> ContainerFile {
        ContainerFile::open_reader(&self.file()).unwrap()
    }

    /// The dictionary the newest checkpoint names, as (extent, id, length).
    fn named_dictionary(&self) -> Option<(u64, u32, u32)> {
        let container = self.container();
        let file = self.file_reader();
        file.read_checkpoint(container.shared.block)
            .unwrap()
            .and_then(|slot| slot.dictionary)
    }

    /// The collection's dictionaries the map shows, as (extent, id).
    fn dictionaries(&self) -> Vec<(u64, u32)> {
        let collection = self.container().shared.collection;
        self.file_reader().dictionary_extents(collection).unwrap()
    }
}

#[test]
fn a_collection_trains_its_dictionary_once_its_records_reach_a_mebibyte() {
    let fixture = Fixture::create("vdbx-dictionary-train");
    let writer = fixture.writer();
    let container = fixture.container();
    let mut number = put_orders_until(&writer, &container, 0, DICTIONARY_TRAINING_BYTES / 2);
    writer.rebuild_collection("items").unwrap();
    assert_eq!(
        fixture.named_dictionary(),
        None,
        "half a mebibyte trains nothing"
    );
    assert!(fixture.dictionaries().is_empty());
    assert!(frames(&container).iter().all(|(_, id, _)| *id == 0));

    number = put_orders_until(&writer, &container, number, DICTIONARY_TRAINING_BYTES);
    let trained_at = number;
    writer.rebuild_collection("items").unwrap();
    let (extent, id, length) = fixture
        .named_dictionary()
        .expect("the first checkpoint past 1 MiB trains a dictionary");
    assert_eq!(id, 1);
    assert!(
        length > 1024 && length as usize <= crate::MAX_DICTIONARY_BYTES,
        "{length}"
    );
    assert_eq!(fixture.dictionaries(), vec![(extent, 1)]);

    for next in number..number + 200 {
        put(&writer, next, &order(next));
    }
    let written = frames(&container);
    let with: Vec<_> = written.iter().filter(|(_, id, _)| *id == 1).collect();
    assert!(
        with.len() >= 200,
        "records after training name the dictionary"
    );
    assert!(
        with.iter()
            .all(|(codec, _, _)| *codec == ContainerCodec::Zstd)
    );

    // The dictionary multiplies the ratio on documents like these.
    let bodies: Vec<Vec<u8>> = (trained_at..trained_at + 200)
        .map(|number| order(number).to_string().into_bytes())
        .collect();
    let raw: usize = bodies.iter().map(Vec::len).sum();
    let stored = |dictionary| {
        let mut codec = ContainerFrameCodec::new(dictionary).unwrap();
        bodies
            .iter()
            .map(|body| codec.encode(body).unwrap().bytes.len())
            .sum::<usize>()
    };
    let dictionary = fixture
        .file_reader()
        .read_dictionary(container.shared.collection, extent)
        .unwrap();
    let (without, with) = (stored(None), stored(Some(dictionary)));
    let ratio = |stored: usize| {
        f64::from(u32::try_from(raw).unwrap()) / f64::from(u32::try_from(stored).unwrap())
    };
    eprintln!(
        "{} documents, {raw} bytes: zstd {:.2}x, zstd with the dictionary {:.2}x",
        bodies.len(),
        ratio(without),
        ratio(with)
    );
    assert!(ratio(with) > 1.5 * ratio(without), "{without} {with}");

    // A new process reads every record, loading the dictionary they name.
    drop(container);
    fixture.crash();
    for number in [0, trained_at - 1, trained_at, number + 199] {
        assert_eq!(read(&fixture, number), order(number), "{number}");
    }
    let report = fixture.file_reader().report().unwrap();
    assert_eq!(report.codecs, vec!["zstd"]);
    assert_eq!(report.collections[0].dictionary, Some(1));
    assert_eq!(report.collections[0].dictionaries, 1);
    let verified = fixture.file_reader().verify().unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
}

#[test]
fn lz4_records_use_the_dictionary_too_and_compaction_rewrites_them_as_zstd() {
    let fixture = Fixture::create("vdbx-dictionary-lz4");
    let writer = fixture.writer();
    let container = fixture.container();
    let mut number = put_orders_until(&writer, &container, 0, DICTIONARY_TRAINING_BYTES);
    writer.rebuild_collection("items").unwrap();
    assert_eq!(fixture.named_dictionary().map(|(_, id, _)| id), Some(1));
    assert_eq!(
        fixture.file_reader().superblock().incompat,
        ContainerSuperblock::INCOMPAT_CODEC_ZSTD
    );

    // A writer that writes LZ4, as the browser does, lists it before it
    // writes any.
    drop(container);
    fixture.crash();
    let container = fixture.container();
    container.frames().write_lz4(true);
    let lz4_from = number;
    for _ in 0..100 {
        put(&writer, number, &order(number));
        number += 1;
    }
    assert_eq!(
        fixture.file_reader().superblock().incompat,
        ContainerSuperblock::INCOMPAT_CODEC_ZSTD | ContainerSuperblock::INCOMPAT_CODEC_LZ4
    );
    assert_eq!(
        fixture.file_reader().report().unwrap().codecs,
        vec!["zstd", "lz4"]
    );
    let lz4: Vec<_> = frames(&container)
        .into_iter()
        .filter(|(codec, _, _)| *codec == ContainerCodec::Lz4)
        .collect();
    assert!(lz4.len() >= 100, "{lz4:?}");
    assert!(
        lz4.iter().all(|(_, id, _)| *id == 1),
        "LZ4 names the dictionary"
    );
    container.frames().write_lz4(false);
    drop(container);
    fixture.crash();
    for number in [lz4_from, lz4_from + 50, number - 1] {
        assert_eq!(read(&fixture, number), order(number), "{number}");
    }

    for number in 0..lz4_from {
        writer
            .delete_document("items", &identifier(number), None)
            .unwrap();
    }
    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    let container = fixture.container();
    let written = frames(&container);
    assert!(
        written
            .iter()
            .all(|(codec, _, _)| *codec != ContainerCodec::Lz4),
        "{written:?}"
    );
    for number in [lz4_from, lz4_from + 50, number - 1] {
        assert_eq!(read(&fixture, number), order(number), "{number}");
    }
}

#[test]
fn a_record_whose_dictionary_is_missing_or_damaged_is_refused_clearly() {
    let fixture = Fixture::create("vdbx-dictionary-damage");
    let writer = fixture.writer();
    let container = fixture.container();
    let mut number = put_orders_until(&writer, &container, 0, DICTIONARY_TRAINING_BYTES);
    writer.rebuild_collection("items").unwrap();
    let (extent, _, _) = fixture.named_dictionary().unwrap();
    let after = number;
    for _ in 0..20 {
        put(&writer, number, &order(number));
        number += 1;
    }
    drop(container);
    fixture.crash();

    // Flip a byte of the stored dictionary.
    let path = fixture.file();
    let mut bytes = std::fs::read(&path).unwrap();
    let at = extent * (1 << ContainerMedium::TEST_EXTENT_SHIFT)
        + ContainerExtentHeader::PAYLOAD_OFFSET
        + ContainerDictionaryHeader::ENCODED_BYTES as u64
        + 100;
    bytes[usize::try_from(at).unwrap()] ^= 0x20;
    std::fs::write(&path, &bytes).unwrap();

    let error = fixture
        .collection()
        .read_document(&identifier(after + 3))
        .unwrap_err();
    assert_eq!(
        error.code(),
        NativeStorageErrorCode::CorruptDocument,
        "{error}"
    );
    assert!(error.to_string().contains("dictionary"), "{error}");
    let verified = fixture.file_reader().verify().unwrap();
    assert!(!verified.ok);
    let codes: Vec<&str> = verified
        .problems
        .iter()
        .map(|problem| problem.code)
        .collect();
    assert!(codes.contains(&"dictionaryDamaged"), "{codes:?}");
    assert!(codes.contains(&"dictionaryMissing"), "{codes:?}");
    // Recovery refuses rather than cut the records that name it off as a
    // torn tail.
    let recovered = writer.recover_collection("items");
    assert!(recovered.is_err() || fixture.collection().document_ids().unwrap().len() == number);

    // A dictionary gone from the map is missing too.
    bytes[usize::try_from(at).unwrap()] ^= 0x20;
    std::fs::write(&path, &bytes).unwrap();
    fixture.crash();
    assert_eq!(read(&fixture, after + 3), order(after + 3));
    {
        let mut file = ContainerFile::open_unchecked(&path).unwrap();
        let collection = file.collections()[0].header.id;
        file.release_dictionary(collection, extent).unwrap();
        file.checkpoint().unwrap();
    }
    fixture.crash();
    let error = fixture
        .collection()
        .read_document(&identifier(after + 3))
        .unwrap_err();
    assert!(error.to_string().contains("dictionary"), "{error}");
    let codes: Vec<&str> = fixture
        .file_reader()
        .verify()
        .unwrap()
        .problems
        .iter()
        .map(|problem| problem.code)
        .collect();
    assert!(codes.contains(&"dictionaryMissing"), "{codes:?}");
}

/// Write orders past 1 MiB and train, then readings past 1 MiB, and delete
/// every order, so a compaction finds the newest records better served by
/// another dictionary.
fn shifted(fixture: &Fixture) -> (NativeWriteRoot, usize) {
    let writer = fixture.writer();
    let container = fixture.container();
    let orders = put_orders_until(&writer, &container, 0, DICTIONARY_TRAINING_BYTES);
    writer.rebuild_collection("items").unwrap();
    assert_eq!(fixture.named_dictionary().map(|(_, id, _)| id), Some(1));
    for number in orders..orders + READINGS {
        put(&writer, number, &reading(number));
    }
    for number in 0..orders {
        writer
            .delete_document("items", &identifier(number), None)
            .unwrap();
    }
    (writer, orders)
}

/// Compact until nothing is left to compact: a collection of 1 MiB
/// extents compacts a few segments at a time.
fn compact_fully(writer: &NativeWriteRoot) {
    for _ in 0..12 {
        if writer
            .compact_collection("items")
            .unwrap()
            .segments_compacted
            == 0
        {
            return;
        }
    }
    panic!("compaction never finished");
}

#[test]
fn compaction_retrains_and_releases_the_dictionary_no_record_names() {
    let fixture = Fixture::create("vdbx-dictionary-retrain");
    let (writer, orders) = shifted(&fixture);
    let (old_extent, _) = fixture.dictionaries()[0];

    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    compact_fully(&writer);
    let (extent, id, _) = fixture.named_dictionary().unwrap();
    assert_eq!(id, 2, "the readings train a better dictionary");
    let container = fixture.container();
    let ids: std::collections::BTreeSet<u32> =
        frames(&container).iter().map(|(_, id, _)| *id).collect();
    assert!(!ids.contains(&1), "{ids:?}");
    assert_eq!(
        fixture.dictionaries(),
        vec![(extent, 2)],
        "dictionary 1 is released"
    );
    assert_ne!(
        container.holds(old_extent).map(|(kind, _, _)| kind),
        Some(DICTIONARY)
    );
    drop(container);
    fixture.crash();
    for number in [orders, orders + 600, orders + READINGS - 1] {
        assert_eq!(read(&fixture, number), reading(number), "{number}");
    }
    assert!(fixture.collection().read_document(&identifier(3)).is_err());
    let verified = fixture.file_reader().verify().unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");

    // Compacting again keeps the dictionary: a new one would not be 10%
    // better.
    writer.compact_collection("items").unwrap();
    assert_eq!(fixture.named_dictionary().map(|(_, id, _)| id), Some(2));
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `rebuild`: checkpoint the collection, whose records reach 1 MiB, with
///   a failpoint that stops the process after the checkpoint trains.
/// - `compact`: compact the collection with a failpoint that stops it
///   part way.
#[test]
fn a_dictionary_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT).unwrap());
    let writer = writer_at(&root);
    let outcome = match role.as_str() {
        "rebuild" => writer.rebuild_collection("items").map(drop),
        "compact" => writer.compact_collection("items").map(drop),
        other => panic!("unknown child role {other}"),
    };
    panic!("the failpoint did not stop the process: {outcome:?}");
}

/// Run [`a_dictionary_child_process`] as `role`, stopped at `failpoint`.
fn stopped(role: &str, root: &Path, failpoint: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(ROOT, root)
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && !printed.contains("did not stop"),
        "child {role} at {failpoint}:\n{printed}"
    );
}

#[test]
fn a_crash_after_the_training_checkpoint_recovers_with_the_dictionary() {
    let fixture = Fixture::create("vdbx-dictionary-crash-train");
    let writer = fixture.writer();
    let container = fixture.container();
    let number = put_orders_until(&writer, &container, 0, DICTIONARY_TRAINING_BYTES);
    assert_eq!(fixture.named_dictionary(), None);
    drop(container);
    fixture.crash();

    stopped("rebuild", &fixture.0, "after-checkpoint-flush");
    assert_eq!(fixture.named_dictionary().map(|(_, id, _)| id), Some(1));
    writer.recover_collection("items").unwrap();
    for next in number..number + 20 {
        put(&writer, next, &order(next));
    }
    let container = fixture.container();
    assert!(frames(&container).iter().any(|(_, id, _)| *id == 1));
    drop(container);
    fixture.crash();
    for number in [0, number - 1, number + 19] {
        assert_eq!(read(&fixture, number), order(number), "{number}");
    }
    let verified = fixture.file_reader().verify().unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
}

#[test]
fn a_dictionary_a_crash_left_unnamed_is_skipped_and_released_by_compaction() {
    let fixture = Fixture::create("vdbx-dictionary-crash-unnamed");
    let writer = fixture.writer();
    let container = fixture.container();
    let collection = container.shared.collection;
    let number = put_orders_until(&writer, &container, 0, DICTIONARY_TRAINING_BYTES);
    let samples = container.samples(false).unwrap();
    drop(container);
    fixture.crash();
    // What a writer that stopped between writing the dictionary and the
    // checkpoint slot that names it leaves: a dictionary the map shows and
    // no slot or record names.
    let orphan = {
        let mut file = ContainerFile::open_unchecked(&fixture.file()).unwrap();
        let mut trained = crate::train_dictionary(1, &samples).unwrap();
        trained.id = 1;
        let (extent, _, _) = file.write_dictionary(collection, &trained).unwrap();
        file.checkpoint().unwrap();
        extent
    };
    let verified = fixture.file_reader().verify().unwrap();
    assert!(verified.ok, "{verified:?}");
    assert_eq!(
        verified
            .problems
            .iter()
            .map(|problem| problem.code)
            .collect::<Vec<_>>(),
        vec!["dictionaryUnused"]
    );

    writer.rebuild_collection("items").unwrap();
    assert_eq!(
        fixture.named_dictionary().map(|(_, id, _)| id),
        Some(2),
        "a new dictionary takes the next id"
    );
    for next in number..number + 50 {
        put(&writer, next, &order(next));
    }
    for gone in 0..number / 2 {
        writer
            .delete_document("items", &identifier(gone), None)
            .unwrap();
    }
    compact_fully(&writer);
    let held: Vec<u32> = fixture.dictionaries().iter().map(|(_, id)| *id).collect();
    assert!(!held.contains(&1), "{held:?} (orphan in extent {orphan})");
    assert!(held.contains(&2), "{held:?}");
    fixture.crash();
    assert_eq!(read(&fixture, number + 10), order(number + 10));
}

#[test]
fn a_crash_during_a_retraining_compaction_recovers_and_the_next_one_releases() {
    for failpoint in ["after-compaction-copy", "after-compaction-record"] {
        let fixture = Fixture::create("vdbx-dictionary-crash-compact");
        let (writer, orders) = shifted(&fixture);
        fixture.crash();
        stopped("compact", &fixture.0, failpoint);
        writer.recover_collection("items").unwrap();
        for number in [orders, orders + READINGS - 1] {
            assert_eq!(read(&fixture, number), reading(number), "{failpoint}");
        }
        let verified = fixture.file_reader().verify().unwrap();
        assert!(verified.ok, "{failpoint}: {verified:?}");

        compact_fully(&writer);
        let (_, id, _) = fixture.named_dictionary().unwrap();
        assert!(id >= 2, "{failpoint}");
        let held: Vec<u32> = fixture.dictionaries().iter().map(|(_, id)| *id).collect();
        assert_eq!(held, vec![id], "{failpoint}");
        fixture.crash();
        for number in [orders, orders + 600, orders + READINGS - 1] {
            assert_eq!(read(&fixture, number), reading(number), "{failpoint}");
        }
        let verified = fixture.file_reader().verify().unwrap();
        assert!(
            verified.ok && verified.problems.is_empty(),
            "{failpoint}: {verified:?}"
        );
    }
}
