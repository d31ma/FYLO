use super::{ContainerWriteAccess, ContainerWriteOptions};
use crate::{ContainerError, ContainerFile, ContainerLease, ContainerLeaseTiming};
use std::path::PathBuf;
use std::time::Duration;

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn created(name: &str) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("app.fxdb");
    ContainerFile::create(&path, fylo_format::ContainerSuperblock::MIN_EXTENT_SHIFT)
        .expect("create container");
    (Database(dir), path)
}

const QUICK: ContainerWriteOptions = ContainerWriteOptions {
    busy_timeout: Duration::from_millis(100),
    exclusive: false,
    lease_timing: ContainerLeaseTiming {
        takeover_after: Duration::from_millis(400),
        poll: Duration::from_millis(10),
    },
};

const EXCLUSIVE: ContainerWriteOptions = ContainerWriteOptions {
    exclusive: true,
    ..QUICK
};

/// The tests below assume the shared mode where locks are qualified, which
/// is where these tests run in CI (ext4 or tmpfs on Linux).
fn qualified_here(path: &PathBuf) -> bool {
    let file = std::fs::File::open(path).expect("open");
    crate::FilesystemInfo::of(&file)
        .expect("filesystem")
        .byte_range_locks_qualified()
}

#[test]
fn writers_share_the_file_where_locks_are_qualified() {
    let (_guard, path) = created("fxdb-access-shared");
    if !qualified_here(&path) {
        return;
    }
    let (_first, first) = ContainerFile::open_writer(&path, QUICK).expect("first writer");
    let (_second, second) = ContainerFile::open_writer(&path, QUICK).expect("second writer");
    assert!(matches!(first, ContainerWriteAccess::SharedLocks { .. }));
    assert!(matches!(second, ContainerWriteAccess::SharedLocks { .. }));
    // An exclusive writer waits them out.
    assert!(matches!(
        ContainerFile::open_writer(&path, EXCLUSIVE),
        Err(ContainerError::Busy)
    ));
    // And leaves no held lease behind for them to trip over.
    assert_eq!(ContainerLease::held_generation(&path).expect("lease"), None);
}

#[test]
fn an_exclusive_writer_keeps_every_other_writer_out() {
    let (_guard, path) = created("fxdb-access-exclusive");
    let (_container, mut access) =
        ContainerFile::open_writer(&path, EXCLUSIVE).expect("exclusive writer");
    assert!(matches!(access, ContainerWriteAccess::Lease { .. }));
    access.assert_writable().expect("writable");
    access.renew().expect("renew");
    for options in [QUICK, EXCLUSIVE] {
        assert!(matches!(
            ContainerFile::open_writer(&path, options),
            Err(ContainerError::Busy)
        ));
    }
    // Readers are never kept out.
    ContainerFile::open_reader(&path).expect("reader");
    drop(access);
    let (_next, next) = ContainerFile::open_writer(&path, QUICK).expect("after release");
    if qualified_here(&path) {
        assert!(matches!(next, ContainerWriteAccess::SharedLocks { .. }));
    }
}

#[test]
fn a_lease_left_by_a_crash_is_cleared_by_the_next_writer() {
    let (_guard, path) = created("fxdb-access-crash");
    std::mem::forget(
        ContainerLease::acquire(&path, QUICK.lease_timing, Duration::ZERO).expect("lease"),
    );
    // Within the busy timeout the lease still looks live.
    assert!(matches!(
        ContainerFile::open_writer(&path, QUICK),
        Err(ContainerError::Busy)
    ));
    let patient = ContainerWriteOptions {
        busy_timeout: Duration::from_secs(5),
        ..QUICK
    };
    let (_container, _access) = ContainerFile::open_writer(&path, patient).expect("took over");
    if qualified_here(&path) {
        assert_eq!(ContainerLease::held_generation(&path).expect("lease"), None);
    }
}
