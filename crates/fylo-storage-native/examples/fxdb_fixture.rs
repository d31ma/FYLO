//! Write a small `.fxdb` file for tests and demos: two collections with
//! published generations, one segment extent, and one checkpoint.
//!
//! `cargo run -p fylo-storage-native --example fxdb_fixture -- <out.fxdb>`
//!
//! The file holds no records yet; it exercises the container structures
//! (superblock, map, directory) that `inspectDatabase` reports.

use fylo_format::{ContainerExtentKind, ContainerGenerationSlot, ContainerSuperblock};
use fylo_storage_native::ContainerFile;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: fxdb_fixture <out.fxdb>")?,
    );
    ContainerFile::create(&path, ContainerSuperblock::MIN_EXTENT_SHIFT)?;
    let mut container = ContainerFile::open_unchecked(&path)?;
    for (name, records) in [("users", 120_u64), ("orders", 450)] {
        let block = container
            .add_collection(name, br#"{"kind":"document"}"#)?
            .block;
        let segment = container.allocate(ContainerExtentKind::Segment, block + 1)?;
        container.publish_generation(
            block,
            ContainerGenerationSlot {
                generation: 1,
                writing: false,
                active_extent: segment.index,
                active_sequence: segment.sequence,
                committed_length: 0,
                next_lsn: records + 1,
                record_count: records,
            },
        )?;
    }
    container.sync()?;
    container.checkpoint()?;
    println!("{}", serde_json::to_string_pretty(&container.report()?)?);
    Ok(())
}
