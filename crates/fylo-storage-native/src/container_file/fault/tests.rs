//! A container write that fails taking extents or checkpointing the map
//! leaves the map in memory, the superblock in force and the file as they
//! were, so the next allocation and checkpoint work and the file verifies.

use std::path::{Path, PathBuf};

use fylo_format::{ContainerExtentKind, ContainerExtentMapPage, ContainerSuperblock};

use super::{ContainerFault, arm, pending};
use crate::{ContainerExtentCensus, ContainerFile};

const SEGMENT: ContainerExtentKind = ContainerExtentKind::Segment;
const LOCATIONS: ContainerExtentKind = ContainerExtentKind::Locations;

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A container of 1 MiB extents holding one collection, open to write,
/// and the collection's id.
fn created(name: &str) -> (Database, PathBuf, ContainerFile, u32) {
    let directory = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("test.vdbx");
    ContainerFile::create(&path, ContainerSuperblock::MIN_EXTENT_SHIFT).unwrap();
    let mut file = ContainerFile::open_unchecked(&path).unwrap();
    let id = file.add_collection("items", b"{}").unwrap().header.id;
    (Database(directory), path, file, id)
}

/// What a failure leaves as it was.
#[derive(Debug, PartialEq)]
struct State {
    pages: Vec<ContainerExtentMapPage>,
    census: ContainerExtentCensus,
    next_sequence: u64,
    unsaved: bool,
    superblock: ContainerSuperblock,
    file_bytes: u64,
}

impl State {
    fn of(file: &ContainerFile, path: &Path) -> Self {
        let allocator = file.allocator();
        Self {
            pages: allocator.map_pages(),
            census: allocator.census(),
            next_sequence: allocator.next_sequence(),
            unsaved: allocator.has_unsaved_changes(),
            superblock: file.superblock().clone(),
            file_bytes: std::fs::metadata(path).unwrap().len(),
        }
    }
}

/// Require that `fault`, armed, fails `run`, and that the failure leaves
/// `file` as it was.
fn fails_in_place<T: std::fmt::Debug>(
    (file, path): (&mut ContainerFile, &Path),
    fault: ContainerFault,
    run: impl FnOnce(&mut ContainerFile) -> Result<T, crate::ContainerError>,
) {
    let before = State::of(file, path);
    arm(fault);
    let outcome = run(file);
    assert!(outcome.is_err(), "{fault:?} fails it: {outcome:?}");
    assert!(!pending(), "{fault:?} was met");
    assert_eq!(State::of(file, path), before, "{fault:?} left it as it was");
    assert_eq!(
        ContainerFile::open_reader(path).unwrap().superblock(),
        &before.superblock,
        "{fault:?} left the superblock in force"
    );
}

fn verified(path: &Path) {
    let report = ContainerFile::open_reader(path).unwrap().verify().unwrap();
    assert!(report.ok && report.problems.is_empty(), "{report:?}");
}

#[test]
fn a_failed_allocation_takes_nothing() {
    for (fault, reused) in [
        (ContainerFault::Grow, false),
        (ContainerFault::ExtentHeader(SEGMENT), false),
        (ContainerFault::ClearExtent, true),
        (ContainerFault::ExtentHeader(SEGMENT), true),
    ] {
        let (_database, path, mut file, id) = created("vdbx-fault-allocate");
        if reused {
            // The checkpoint moves the map and frees the extent it held.
            file.checkpoint().unwrap();
            assert!(!file.allocator().free_extents().is_empty());
        }
        fails_in_place((&mut file, &path), fault, |file| file.allocate(SEGMENT, id));
        let sequence = file.allocator().next_sequence();
        let allocation = file.allocate(SEGMENT, id).unwrap();
        assert_eq!(
            (allocation.grows_file, allocation.sequence),
            (!reused, sequence)
        );
        file.checkpoint().unwrap();
        verified(&path);
    }
}

#[test]
fn a_failed_checkpoint_keeps_the_old_map_in_force() {
    for fault in [
        ContainerFault::Grow,
        ContainerFault::ExtentHeader(ContainerExtentKind::Map),
        ContainerFault::Map,
        ContainerFault::Superblock,
    ] {
        let (_database, path, mut file, id) = created("vdbx-fault-checkpoint");
        let allocation = file.allocate(SEGMENT, id).unwrap();
        fails_in_place((&mut file, &path), fault, ContainerFile::checkpoint);
        file.checkpoint().unwrap();
        verified(&path);
        let reopened = ContainerFile::open_reader(&path).unwrap();
        assert_eq!(
            reopened.allocator().owned(SEGMENT, id),
            vec![(allocation.index, allocation.sequence)]
        );
    }
}

#[test]
fn a_checkpoint_whose_flush_fails_keeps_both_maps_until_the_next() {
    let (_database, path, mut file, id) = created("vdbx-fault-flush");
    file.allocate(SEGMENT, id).unwrap();
    let before = file.superblock().clone();
    let old_map = before.map_first_extent;
    arm(ContainerFault::Flush);
    assert!(file.checkpoint().is_err());
    assert!(!pending());
    // The new superblock copy is written, so it is the one in force.
    let written = ContainerFile::open_reader(&path)
        .unwrap()
        .superblock()
        .clone();
    assert_eq!(file.superblock(), &written);
    assert_eq!(written.generation, before.generation + 1);
    // The old map is not reused before a checkpoint completes, in case the
    // copy that names it is the one a stop leaves in force.
    assert!(!file.allocator().free_extents().contains(&old_map));
    let taken = file.allocate(SEGMENT, id).unwrap();
    assert_ne!(taken.index, old_map);
    file.checkpoint().unwrap();
    assert!(file.allocator().free_extents().contains(&old_map));
    verified(&path);
    let reopened = ContainerFile::open_reader(&path).unwrap();
    assert_eq!(reopened.superblock(), file.superblock());
    assert_eq!(reopened.allocator().owned(SEGMENT, id).len(), 2);
}

#[test]
fn a_failed_snapshot_takes_nothing() {
    for fault in [
        ContainerFault::Grow,
        ContainerFault::ExtentHeader(LOCATIONS),
    ] {
        let (_database, path, mut file, id) = created("vdbx-fault-snapshot");
        let bytes = vec![7_u8; 3 << 20];
        fails_in_place((&mut file, &path), fault, |file| {
            file.write_snapshot(id, LOCATIONS, 1, &bytes)
        });
        let run = file.write_snapshot(id, LOCATIONS, 1, &bytes).unwrap();
        file.checkpoint().unwrap();
        verified(&path);
        assert_eq!(
            ContainerFile::open_reader(&path)
                .unwrap()
                .read_snapshot(id, LOCATIONS, 1, &run, u64::MAX)
                .unwrap(),
            bytes
        );
    }
}

#[test]
fn a_failed_shrink_keeps_the_tail() {
    for fault in [ContainerFault::Map, ContainerFault::Superblock] {
        let (_database, path, mut file, id) = created("vdbx-fault-shrink");
        let taken: Vec<u64> = (0..4)
            .map(|_| file.allocate(SEGMENT, id).unwrap().index)
            .collect();
        file.checkpoint().unwrap();
        for extent in taken {
            file.release(extent).unwrap();
        }
        file.checkpoint().unwrap();
        file.checkpoint().unwrap();
        fails_in_place((&mut file, &path), fault, ContainerFile::shrink_free_tail);
        assert!(file.shrink_free_tail().unwrap() > 0);
        verified(&path);
    }
}
