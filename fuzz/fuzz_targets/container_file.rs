#![no_main]

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use fylo_format::{
    ContainerCheckpointSlot, ContainerCollectionHeader, ContainerExtentHeader, ContainerExtentKind,
    ContainerExtentState, ContainerGenerationSlot, ContainerSnapshotRef, ContainerSuperblock,
    Crc32c,
};
use fylo_storage_native::ContainerFile;
use libfuzzer_sys::fuzz_target;

const BLOCK: u64 = 4096;
const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const PROBE_BYTES: usize = 18;
const KINDS: [ContainerExtentKind; 7] = [
    ContainerExtentKind::Directory,
    ContainerExtentKind::Segment,
    ContainerExtentKind::Dictionary,
    ContainerExtentKind::Locations,
    ContainerExtentKind::Index,
    ContainerExtentKind::Map,
    ContainerExtentKind::Schemas,
];

/// A file image as its non-zero 4 KiB blocks: every `.fxdb` structure lies
/// within one block, and the 1 MiB extents are mostly holes.
#[derive(Clone)]
struct Image {
    length: u64,
    blocks: BTreeMap<u64, Box<[u8; BLOCK as usize]>>,
}

impl Image {
    fn block(&mut self, index: u64) -> &mut [u8; BLOCK as usize] {
        self.blocks
            .entry(index)
            .or_insert_with(|| Box::new([0; BLOCK as usize]))
    }

    fn write(&mut self, at: u64, bytes: &[u8]) {
        for (offset, byte) in (at..).zip(bytes) {
            self.block(offset / BLOCK)[(offset % BLOCK) as usize] = *byte;
        }
        self.length = self.length.max(at + bytes.len() as u64);
    }
}

/// The layout of the template container, for patches aimed at its parts.
struct Template {
    image: Image,
    regions: Vec<u64>,
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fylo-fuzz-container-file-{}-{name}.fxdb",
        std::process::id()
    ))
}

// A small but complete container: two collections, a sealed segment with a
// published generation and a checkpoint, a location snapshot, and a map that
// has been checkpointed once, so both superblock copies are valid.
fn template() -> &'static Template {
    static TEMPLATE: OnceLock<Template> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let path = scratch("template");
        let _ = std::fs::remove_file(&path);
        ContainerFile::create(&path, SHIFT).expect("create template");
        let mut container = ContainerFile::open_unchecked(&path).expect("open template");
        container
            .add_collection("users", br#"{"schema":1}"#)
            .expect("add users");
        container.add_collection("events", b"").expect("add events");
        let segment = container
            .allocate(ContainerExtentKind::Segment, 1)
            .expect("allocate segment");
        container
            .write_payload(segment.index, 0, b"{\"id\":1,\"name\":\"fuzz\"}")
            .expect("write segment");
        let locations = container
            .allocate(ContainerExtentKind::Locations, 1)
            .expect("allocate locations");
        container
            .write_payload(locations.index, 0, &[0x5a; 32])
            .expect("write locations");
        container
            .publish_generation(
                0,
                ContainerGenerationSlot {
                    generation: 1,
                    writing: false,
                    active_extent: segment.index,
                    active_sequence: segment.sequence,
                    committed_length: 24,
                    next_lsn: 2,
                    record_count: 1,
                    table_epoch: 0,
                },
            )
            .expect("publish generation");
        container
            .record_checkpoint(
                0,
                ContainerCheckpointSlot {
                    checkpoint: 1,
                    lsn: 1,
                    extent: segment.index,
                    extent_sequence: segment.sequence,
                    offset: 0,
                    locations: Some(ContainerSnapshotRef {
                        first_extent: locations.index,
                        extent_count: 1,
                        length: 32,
                        sha256: [7; 32],
                    }),
                    index: None,
                    schemas: None,
                    dictionary: None,
                },
            )
            .expect("record checkpoint");
        container.seal(segment.index, 24).expect("seal segment");
        container.checkpoint().expect("checkpoint map");
        container.sync().expect("sync template");

        let superblock = container.superblock().clone();
        let extent_bytes = superblock.extent_bytes();
        let payload = ContainerExtentHeader::PAYLOAD_OFFSET;
        let regions = vec![
            0,
            BLOCK,
            superblock.directory_extent * extent_bytes,
            superblock.directory_extent * extent_bytes + payload,
            superblock.map_first_extent * extent_bytes,
            superblock.map_first_extent * extent_bytes + payload,
            segment.index * extent_bytes,
            segment.index * extent_bytes + payload,
            locations.index * extent_bytes,
        ];
        drop(container);

        let bytes = std::fs::read(&path).expect("read template");
        let _ = std::fs::remove_file(&path);
        let mut image = Image {
            length: bytes.len() as u64,
            blocks: BTreeMap::new(),
        };
        for (index, chunk) in (0..).zip(bytes.chunks(BLOCK as usize)) {
            if chunk.iter().any(|byte| *byte != 0) {
                image.block(index)[..chunk.len()].copy_from_slice(chunk);
            }
        }
        Template { image, regions }
    })
}

fn reseal(bytes: &mut [u8]) {
    if bytes.iter().any(|byte| *byte != 0) {
        let checked = bytes.len() - 4;
        let checksum = Crc32c::checksum(&bytes[..checked]);
        bytes[checked..].copy_from_slice(&checksum.to_le_bytes());
    }
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}

/// What the superblock copies, as patched, say lives where.
struct Layout {
    extent_bytes: u64,
    directories: Vec<u64>,
    maps: Vec<(u64, u64)>,
}

fn layout(image: &Image) -> Layout {
    let mut layout = Layout {
        extent_bytes: 1 << SHIFT,
        directories: Vec::new(),
        maps: Vec::new(),
    };
    for copy in [0, 1] {
        let Some(block) = image.blocks.get(&copy) else {
            continue;
        };
        if block[..8] != ContainerSuperblock::MAGIC {
            continue;
        }
        let shift = block[24];
        if (ContainerSuperblock::MIN_EXTENT_SHIFT..=ContainerSuperblock::MAX_EXTENT_SHIFT)
            .contains(&shift)
            && copy == 0
        {
            layout.extent_bytes = 1 << shift;
        }
        layout.directories.push(read_u64(&block[..], 56));
        let map_count = u64::from(u32::from_le_bytes([
            block[72], block[73], block[74], block[75],
        ]));
        let map_first = read_u64(&block[..], 64);
        layout
            .maps
            .push((map_first, map_first.saturating_add(map_count)));
    }
    layout
}

// Recompute the checksum of every structure the patched layout names, so a
// patch reaches the decoder's field checks rather than only its checksum.
fn reseal_all(image: &mut Image) {
    let layout = layout(image);
    let blocks_per_extent = layout.extent_bytes / BLOCK;
    for (index, block) in &mut image.blocks {
        let (index, block) = (*index, &mut **block);
        if index < 2 {
            continue;
        }
        let extent = index / blocks_per_extent;
        if index % blocks_per_extent == 0 {
            reseal(&mut block[..ContainerExtentHeader::ENCODED_BYTES]);
        } else if layout.directories.contains(&extent) {
            let checkpoint = ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET;
            let generation = ContainerCollectionHeader::GENERATION_SLOTS_OFFSET;
            reseal(&mut block[..ContainerCollectionHeader::ENCODED_BYTES]);
            for slot in
                block[checkpoint..generation].chunks_mut(ContainerCheckpointSlot::ENCODED_BYTES)
            {
                reseal(slot);
            }
            for slot in block[generation..].chunks_mut(ContainerGenerationSlot::ENCODED_BYTES) {
                reseal(slot);
            }
        } else if layout
            .maps
            .iter()
            .any(|(first, end)| (*first..*end).contains(&extent))
        {
            reseal(&mut block[..]);
        }
    }
    for copy in [0, 1] {
        if let Some(block) = image.blocks.get_mut(&copy) {
            reseal(&mut block[..]);
        }
    }
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*first)
    }

    fn take(&mut self, length: usize) -> &[u8] {
        let (taken, rest) = self.0.split_at(length.min(self.0.len()));
        self.0 = rest;
        taken
    }

    fn u16(&mut self) -> u64 {
        let bytes = self.take(2);
        bytes
            .iter()
            .rev()
            .fold(0, |value, byte| value << 8 | u64::from(*byte))
    }

    fn u32(&mut self) -> u64 {
        let bytes = self.take(4);
        bytes
            .iter()
            .rev()
            .fold(0, |value, byte| value << 8 | u64::from(*byte))
    }
}

// Patch the template: writes into its named parts, at absolute offsets,
// small integers over fields, and truncation.
fn patched(template: &Template, patches: &[u8]) -> Image {
    let mut image = template.image.clone();
    let mut cursor = Cursor(patches);
    while let Some(op) = cursor.byte() {
        match op % 4 {
            0 | 3 => {
                let region = usize::from(cursor.byte().unwrap_or(0)) % template.regions.len();
                let at = template.regions[region] + cursor.u16();
                if op % 4 == 0 {
                    let length = usize::from(cursor.byte().unwrap_or(0) % 32) + 1;
                    let bytes = cursor.take(length).to_vec();
                    image.write(at, &bytes);
                } else {
                    let value = u64::from(cursor.byte().unwrap_or(0));
                    image.write(at, &value.to_le_bytes());
                }
            }
            1 => {
                let at = cursor.u32() % (template.image.length + (1 << SHIFT));
                let length = usize::from(cursor.byte().unwrap_or(0) % 32) + 1;
                let bytes = cursor.take(length).to_vec();
                image.write(at, &bytes);
            }
            _ => {
                image.length = cursor.u32() % (template.image.length + 1);
            }
        }
    }
    image
}

fn exercise(container: &ContainerFile, probe: &[u8]) {
    let report = container.report().expect("report reads only the length");
    assert_eq!(report.extent_count, container.superblock().extent_count);
    assert_eq!(report.collections.len(), container.collections().len());
    let _ = serde_json::to_vec(&report).expect("a report serializes");

    let capacity = container.payload_capacity();
    let mut small = [0_u8; 16];
    for collection in container.collections() {
        let owner = collection.header.id;
        if let Some(slot) = &collection.generation {
            let _ = container.expect_extent(
                slot.active_extent,
                ContainerExtentKind::Segment,
                owner,
                slot.active_sequence,
            );
            let _ = container.read_payload(
                slot.active_extent,
                slot.committed_length.saturating_sub(16),
                &mut small,
            );
        }
        if let Some(slot) = &collection.checkpoint {
            let _ = container.expect_extent(
                slot.extent,
                ContainerExtentKind::Segment,
                owner,
                slot.extent_sequence,
            );
            let _ = container.read_payload(slot.extent, slot.offset, &mut small);
            for (kind, snapshot) in [
                (ContainerExtentKind::Locations, slot.locations),
                (ContainerExtentKind::Index, slot.index),
                (ContainerExtentKind::Schemas, slot.schemas),
            ] {
                if let Some(snapshot) = snapshot {
                    let _ = container.expect_extent(snapshot.first_extent, kind, owner, 0);
                    let _ = container.read_payload(
                        snapshot.first_extent,
                        snapshot.length.saturating_sub(16),
                        &mut small,
                    );
                }
            }
            if let Some((extent, _, length)) = slot.dictionary {
                let _ = container.expect_extent(extent, ContainerExtentKind::Dictionary, owner, 0);
                let _ = container.read_payload(
                    extent,
                    u64::from(length).saturating_sub(16),
                    &mut small,
                );
            }
        }
    }

    for page in container.allocator().map_pages().iter().take(2) {
        for (extent, state) in (page.first_extent..).zip(page.entries.iter().take(16)) {
            if let ContainerExtentState::Owned {
                kind,
                owner,
                sequence,
            }
            | ContainerExtentState::Sealed {
                kind,
                owner,
                sequence,
                ..
            } = *state
            {
                let _ = container.expect_extent(extent, kind, owner, sequence);
            }
            let _ = container.read_payload(extent, 0, &mut small);
            let _ = container.read_payload(extent, capacity - 16, &mut small);
            assert!(
                container
                    .read_payload(extent, capacity - 15, &mut small)
                    .is_err(),
                "read past the end of an extent's payload"
            );
        }
    }

    let mut probe = Cursor(probe);
    let extent = probe.u32() | probe.u32() << 32;
    let offset = probe.u32() | probe.u32() << 32;
    let length = usize::from(probe.byte().unwrap_or(0) % 64);
    let kind = KINDS[usize::from(probe.byte().unwrap_or(0)) % KINDS.len()];
    let _ = container.read_payload(extent, offset, &mut vec![0; length]);
    let _ = container.expect_extent(extent, kind, 0, offset);
}

// A `.fxdb` file is untrusted input to whatever opens it, as a SQLite file
// is: opening and inspecting arbitrary or damaged bytes must fail cleanly,
// without panicking, hanging, or allocating more than the file justifies.
fuzz_target!(|input: &[u8]| {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    let path = PATH.get_or_init(|| scratch("input"));
    let Some((&flags, rest)) = input.split_first() else {
        return;
    };
    let (probe, body) = rest.split_at(rest.len().min(PROBE_BYTES));

    let image = if flags & 1 == 0 {
        let mut image = Image {
            length: 0,
            blocks: BTreeMap::new(),
        };
        image.write(0, body);
        image
    } else {
        patched(template(), body)
    };
    let mut image = image;
    if flags & 2 != 0 {
        reseal_all(&mut image);
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .expect("open scratch file");
    file.set_len(image.length).expect("size scratch file");
    for (index, block) in &image.blocks {
        let at = index * BLOCK;
        if at < image.length {
            let end = usize::try_from((image.length - at).min(BLOCK)).unwrap_or(0);
            file.write_all_at(&block[..end], at)
                .expect("write scratch file");
        }
    }
    drop(file);

    if let Ok(container) = ContainerFile::open_reader(path) {
        assert!(!container.is_writable());
        exercise(&container, probe);
    }
    if let Ok(container) = ContainerFile::open_unchecked(path) {
        assert!(container.is_writable());
        exercise(&container, probe);
    }
});
