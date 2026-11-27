use byckr_storage_native::{
    CollectionKind, NativeRoot, NativeStorageErrorCode, NativeWriteRoot, ReadScope, RootLease,
    RootOpenOptions,
};
use std::path::{Path, PathBuf};

struct ScopeFixture {
    directory: PathBuf,
    lease: Option<RootLease>,
}

impl ScopeFixture {
    const ROOT_NAME: &str = "db";
    const OWNER_RECORD: &str = ".db.fylo-root-owner.lock.json";

    fn new(name: &str) -> Self {
        let directory = byckr_test_support::TestFolder::path().join(format!(
            "fylo-read-scope-{name}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        let lease = RootLease::acquire(directory.join(Self::ROOT_NAME)).unwrap();
        let fixture = Self {
            directory,
            lease: Some(lease),
        };
        fixture
            .writer()
            .create_collection("docs", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn root(&self) -> PathBuf {
        self.directory.join(Self::ROOT_NAME)
    }

    fn lease(&self) -> RootLease {
        self.lease.clone().unwrap()
    }

    fn reader(&self) -> NativeRoot {
        NativeRoot::open_profiled(RootOpenOptions::new(self.root()).with_lease(self.lease()))
            .unwrap()
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(RootOpenOptions::new(self.root()).with_lease(self.lease()))
            .unwrap()
    }

    fn lose_owner_record(&self) {
        std::fs::remove_file(self.directory.join(Self::OWNER_RECORD)).unwrap();
    }

    fn lease_error(&self) -> NativeStorageErrorCode {
        self.lease().assert_owned().unwrap_err().code()
    }

    fn reader_error(reader: &NativeRoot) -> NativeStorageErrorCode {
        reader.collection("docs").unwrap_err().code()
    }
}

impl Drop for ScopeFixture {
    fn drop(&mut self) {
        drop(self.lease.take());
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn a_read_scope_reads_the_owner_record_once() {
    let fixture = ScopeFixture::new("once");
    let scope = ReadScope::enter(true);
    fixture.lease().assert_owned().unwrap();
    fixture.lose_owner_record();
    fixture.lease().assert_owned().unwrap();
    drop(scope);
    assert_eq!(fixture.lease_error(), NativeStorageErrorCode::RootLeaseLost);
}

#[test]
fn a_new_read_scope_reads_the_owner_record_again() {
    let fixture = ScopeFixture::new("again");
    drop(ReadScope::enter(true));
    let first = ReadScope::enter(true);
    fixture.lease().assert_owned().unwrap();
    drop(first);
    fixture.lose_owner_record();
    let _second = ReadScope::enter(true);
    assert_eq!(fixture.lease_error(), NativeStorageErrorCode::RootLeaseLost);
}

#[test]
fn a_scope_for_a_write_reuses_nothing() {
    let fixture = ScopeFixture::new("inactive");
    let _scope = ReadScope::enter(false);
    fixture.lease().assert_owned().unwrap();
    fixture.lose_owner_record();
    assert_eq!(fixture.lease_error(), NativeStorageErrorCode::RootLeaseLost);
}

#[test]
fn a_reader_reuses_its_ownership_check_inside_a_read_scope() {
    let fixture = ScopeFixture::new("reader");
    let reader = fixture.reader();
    let scope = ReadScope::enter(true);
    reader.collection("docs").unwrap();
    fixture.lose_owner_record();
    reader.collection("docs").unwrap();
    drop(scope);
    assert_eq!(
        ScopeFixture::reader_error(&reader),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn a_writer_checks_ownership_even_inside_a_read_scope() {
    let fixture = ScopeFixture::new("writer");
    let reader = fixture.reader();
    let writer = fixture.writer();
    let _scope = ReadScope::enter(true);
    reader.collection("docs").unwrap();
    fixture.lease().assert_owned().unwrap();
    fixture.lose_owner_record();
    let refused = writer
        .create_collection("more", CollectionKind::Document, None)
        .unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLeaseLost);
}

#[test]
fn an_inner_scope_keeps_the_outer_scope_open() {
    let fixture = ScopeFixture::new("nested");
    let _outer = ReadScope::enter(true);
    drop(ReadScope::enter(true));
    fixture.lease().assert_owned().unwrap();
    fixture.lose_owner_record();
    drop(ReadScope::enter(true));
    fixture.lease().assert_owned().unwrap();
}

#[test]
fn another_thread_has_its_own_scope() {
    let fixture = ScopeFixture::new("thread");
    let _scope = ReadScope::enter(true);
    fixture.lease().assert_owned().unwrap();
    fixture.lose_owner_record();
    let lease = fixture.lease();
    let code = std::thread::spawn(move || lease.assert_owned().unwrap_err().code())
        .join()
        .unwrap();
    assert_eq!(code, NativeStorageErrorCode::RootLeaseLost);
}

#[test]
fn canonicalize_outside_a_scope_resolves_every_time() {
    let fixture = ScopeFixture::new("plain-path");
    let alias = fixture.directory.join("alias");
    point(&alias, &fixture.root());
    assert_eq!(
        ReadScope::canonicalize(&alias).unwrap(),
        canonical(&fixture.root())
    );
    let other = fixture.directory.join("other");
    std::fs::create_dir(&other).unwrap();
    point(&alias, &other);
    assert_eq!(ReadScope::canonicalize(&alias).unwrap(), canonical(&other));
}

#[test]
fn canonicalize_inside_a_scope_resolves_a_path_once() {
    let fixture = ScopeFixture::new("scoped-path");
    let alias = fixture.directory.join("alias");
    point(&alias, &fixture.root());
    let scope = ReadScope::enter(true);
    assert_eq!(
        ReadScope::canonicalize(&alias).unwrap(),
        canonical(&fixture.root())
    );
    let other = fixture.directory.join("other");
    std::fs::create_dir(&other).unwrap();
    point(&alias, &other);
    assert_eq!(
        ReadScope::canonicalize(&alias).unwrap(),
        canonical(&fixture.root())
    );
    drop(scope);
    assert_eq!(ReadScope::canonicalize(&alias).unwrap(), canonical(&other));
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[cfg(unix)]
fn point(alias: &Path, target: &Path) {
    let _ = std::fs::remove_file(alias);
    std::os::unix::fs::symlink(target, alias).unwrap();
}

#[cfg(windows)]
fn point(alias: &Path, target: &Path) {
    let _ = std::fs::remove_dir(alias);
    junction(alias, target);
}

#[cfg(windows)]
fn junction(alias: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(alias)
        .arg(target)
        .status()
        .unwrap();
    assert!(status.success());
}
