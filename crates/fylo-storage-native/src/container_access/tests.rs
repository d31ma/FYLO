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
    let path = dir.join("app.vdbx");
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
    let (_guard, path) = created("vdbx-access-shared");
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
    let (_guard, path) = created("vdbx-access-exclusive");
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
    let (_guard, path) = created("vdbx-access-crash");
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

/// Rename a container with another key salt over `path`, as `fylo rekey`
/// commits, and return its salt.
fn rekeyed(path: &PathBuf) -> [u8; 32] {
    let staging = path.with_extension("staging");
    ContainerFile::create_encrypted(
        &staging,
        fylo_format::ContainerSuperblock::MIN_EXTENT_SHIFT,
        b"a secret generated for this test, never a real one",
    )
    .expect("create the new file");
    std::fs::rename(&staging, path).expect("rename over");
    ContainerFile::open_reader(path)
        .expect("reader")
        .superblock()
        .key_salt
}

#[test]
fn a_writer_that_waited_through_a_rekey_writes_the_new_file() {
    // Locks where they are qualified, and the lease, with a takeover time
    // the wait never reaches.
    for exclusive in [false, true] {
        let (_guard, path) = created("vdbx-access-rekeyed");
        let options = ContainerWriteOptions {
            exclusive,
            lease_timing: ContainerLeaseTiming::default(),
            ..QUICK
        };
        // The rekey's hold on the file, which keeps every other writer out.
        let (old, mut access) = ContainerFile::open_writer(&path, options).expect("rekey");
        access
            .hold_exclusively(options.busy_timeout)
            .expect("exclusive");
        let waiting = std::thread::spawn({
            let path = path.clone();
            let patient = ContainerWriteOptions {
                busy_timeout: Duration::from_secs(20),
                ..options
            };
            move || ContainerFile::open_writer(&path, patient)
        });
        // The waiting writer opens the old file, then waits for the right
        // to write it, which it gets only after the rename.
        std::thread::sleep(Duration::from_millis(300));
        let salt = rekeyed(&path);
        drop((old, access));
        let (file, _access) = waiting.join().expect("thread").expect("waited");
        assert_eq!(file.superblock().key_salt, salt, "exclusive: {exclusive}");
        assert!(file.is_named_by(&path).expect("named"));
    }
}

#[test]
fn a_file_is_named_by_its_path_until_a_rekey_replaces_it() {
    let (_guard, path) = created("vdbx-access-named");
    let old = ContainerFile::open_reader(&path).expect("reader");
    assert!(old.is_named_by(&path).expect("named"));
    rekeyed(&path);
    assert!(!old.is_named_by(&path).expect("replaced"));
    assert!(
        !old.is_named_by(&path.with_extension("absent"))
            .expect("absent")
    );
}
