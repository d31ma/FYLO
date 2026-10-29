use super::{ByteRangeLockFile, FilesystemInfo, LOCK_RANGE_START, LockByte, LockMode};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn database(name: &str) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("test.fxdb");
    std::fs::write(&path, b"fxdb").expect("create database file");
    (Database(dir), path)
}

#[test]
fn reserved_bytes_sit_at_and_above_two_to_the_sixty_two() {
    assert_eq!(LockByte::LEASE.offset(), 1 << 62);
    assert_eq!(LockByte::ALLOCATION.offset(), LOCK_RANGE_START + 1);
    assert_eq!(
        LockByte::collection_writer(0).offset(),
        LOCK_RANGE_START + 4096
    );
    assert_eq!(
        LockByte::collection_writer(u32::MAX).offset(),
        LOCK_RANGE_START + 4096 + u64::from(u32::MAX)
    );
    assert_eq!(LockByte::collection_writer(7).to_string(), "writer 7");
}

#[test]
fn an_exclusive_byte_excludes_every_other_holder_in_the_process() {
    let (_dir, path) = database("brl-exclusive");
    let first = ByteRangeLockFile::open(&path).expect("open");
    let second = ByteRangeLockFile::open(&path).expect("open again");
    let held = first
        .try_lock(LockByte::LEASE, LockMode::Exclusive)
        .expect("lock")
        .expect("free byte");
    for handle in [&first, &second] {
        for mode in [LockMode::Exclusive, LockMode::Shared] {
            assert!(
                handle
                    .try_lock(LockByte::LEASE, mode)
                    .expect("try")
                    .is_none()
            );
        }
    }
    drop(held);
    assert!(
        second
            .try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("lock")
            .is_some()
    );
}

#[test]
fn shared_holders_stack_and_hold_off_an_exclusive_one_until_the_last_leaves() {
    let (_dir, path) = database("brl-shared");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let a = file
        .try_lock(LockByte::LEASE, LockMode::Shared)
        .expect("lock")
        .expect("free");
    let b = file
        .try_lock(LockByte::LEASE, LockMode::Shared)
        .expect("lock")
        .expect("shared again");
    assert!(
        file.try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("try")
            .is_none()
    );
    drop(a);
    assert!(
        file.try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("try")
            .is_none()
    );
    b.unlock().expect("unlock");
    assert!(
        file.try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("lock")
            .is_some()
    );
}

#[test]
fn bytes_are_independent() {
    let (_dir, path) = database("brl-independent");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let bytes = [
        LockByte::LEASE,
        LockByte::ALLOCATION,
        LockByte::collection_writer(0),
        LockByte::collection_writer(1),
        LockByte::collection_writer(2047),
    ];
    let guards: Vec<_> = bytes
        .iter()
        .map(|&byte| {
            file.try_lock(byte, LockMode::Exclusive)
                .expect("lock")
                .expect("independent byte")
        })
        .collect();
    assert_eq!(guards.len(), bytes.len());
}

#[test]
fn locks_beyond_the_end_leave_the_file_and_its_data_alone() {
    let (_dir, path) = database("brl-data");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let _lease = file
        .try_lock(LockByte::LEASE, LockMode::Shared)
        .expect("lock")
        .expect("free");
    let _writer = file
        .try_lock(LockByte::collection_writer(3), LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    write_at(file.file(), b"payload", 4);
    assert_eq!(read_at(file.file(), 11, 0), b"fxdbpayload");
    assert_eq!(file.file().metadata().expect("stat").len(), 11);
}

#[test]
fn a_waiting_lock_times_out_while_the_byte_stays_held() {
    let (_dir, path) = database("brl-timeout");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let _held = file
        .try_lock(LockByte::ALLOCATION, LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    let started = Instant::now();
    let error = file
        .lock(
            LockByte::ALLOCATION,
            LockMode::Exclusive,
            Duration::from_millis(50),
        )
        .expect_err("held byte");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(50), "{waited:?}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
}

#[test]
fn a_waiting_lock_wakes_when_another_thread_releases() {
    let (_dir, path) = database("brl-wake");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let held = file
        .try_lock(LockByte::collection_writer(0), LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let other = ByteRangeLockFile::open(&path).expect("open in thread");
        sender.send(()).expect("signal");
        other
            .lock(
                LockByte::collection_writer(0),
                LockMode::Exclusive,
                Duration::from_secs(10),
            )
            .map(|guard| guard.byte())
    });
    receiver.recv().expect("waiter started");
    thread::sleep(Duration::from_millis(20));
    drop(held);
    assert_eq!(
        waiter.join().expect("waiter").expect("acquired"),
        LockByte::collection_writer(0)
    );
}

#[test]
fn closing_a_second_handle_keeps_the_first_handles_locks() {
    let (_dir, path) = database("brl-close");
    let first = ByteRangeLockFile::open(&path).expect("open");
    let held = first
        .try_lock(LockByte::LEASE, LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    let second = ByteRangeLockFile::open(&path).expect("open again");
    drop(second);
    assert!(
        first
            .try_lock(LockByte::LEASE, LockMode::Shared)
            .expect("try")
            .is_none()
    );
    drop(held);
    drop(first);
    // The last handle closed, so a fresh open owns the file again.
    let third = ByteRangeLockFile::open(&path).expect("reopen");
    assert!(
        third
            .try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("lock")
            .is_some()
    );
}

#[test]
fn a_guard_outlives_the_handle_that_took_it() {
    let (_dir, path) = database("brl-guard");
    let guard = ByteRangeLockFile::open(&path)
        .expect("open")
        .try_lock(LockByte::LEASE, LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    let other = ByteRangeLockFile::open(&path).expect("open again");
    assert!(
        other
            .try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("try")
            .is_none()
    );
    drop(guard);
    assert!(
        other
            .try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("lock")
            .is_some()
    );
}

#[cfg(unix)]
fn write_at(file: &std::fs::File, bytes: &[u8], offset: u64) {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset).expect("write");
}

#[cfg(unix)]
fn read_at(file: &std::fs::File, len: usize, offset: u64) -> Vec<u8> {
    let mut bytes = vec![0; len];
    std::os::unix::fs::FileExt::read_exact_at(file, &mut bytes, offset).expect("read");
    bytes
}

#[cfg(windows)]
fn write_at(file: &std::fs::File, bytes: &[u8], offset: u64) {
    let written = std::os::windows::fs::FileExt::seek_write(file, bytes, offset).expect("write");
    assert_eq!(written, bytes.len());
}

#[cfg(windows)]
fn read_at(file: &std::fs::File, len: usize, offset: u64) -> Vec<u8> {
    let mut bytes = vec![0; len];
    let read = std::os::windows::fs::FileExt::seek_read(file, &mut bytes, offset).expect("read");
    assert_eq!(read, len);
    bytes
}

#[test]
fn a_local_filesystem_is_named_and_takes_a_probe_lock() {
    let (_dir, path) = database("brl-probe");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let filesystem = FilesystemInfo::of(file.file()).expect("filesystem");
    assert!(!filesystem.name.is_empty());
    assert!(file.probe(), "{filesystem:?} refused a byte-range lock");
    // The probe leaves nothing held.
    assert!(
        ByteRangeLockFile::open(&path)
            .expect("open again")
            .try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("lock")
            .is_some()
    );
}

#[test]
fn process_local_bytes_still_exclude_threads_in_the_process() {
    let (_dir, path) = database("brl-local");
    let file = ByteRangeLockFile::open(&path).expect("open");
    file.set_process_local(true);
    let other = ByteRangeLockFile::open(&path).expect("open again");
    let held = file
        .try_lock(LockByte::collection_writer(0), LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    assert!(
        other
            .try_lock(LockByte::collection_writer(0), LockMode::Exclusive)
            .expect("try")
            .is_none()
    );
    drop(held);
    let shared = other
        .try_lock(LockByte::LEASE, LockMode::Shared)
        .expect("lock")
        .expect("free");
    assert!(
        file.try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("try")
            .is_none()
    );
    drop(shared);
    file.set_process_local(false);
    assert!(
        file.try_lock(LockByte::LEASE, LockMode::Exclusive)
            .expect("lock")
            .is_some()
    );
}
