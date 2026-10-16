use super::RootLease;
use crate::NativeStorageErrorCode;
use std::path::PathBuf;

struct LeaseFixture {
    directory: PathBuf,
    lease: Option<RootLease>,
    metadata_path: PathBuf,
}

impl LeaseFixture {
    const ROOT_NAME: &str = "db";
    const INVALID_VERSION: u32 = 2;
    const OVERSIZED_RECORD_BYTES: usize = 4097;

    fn new() -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name("fylo-lease-contract"));
        let lease = RootLease::acquire(directory.join(Self::ROOT_NAME)).unwrap();
        let metadata_path = directory.join(".db.fylo-root-owner.lock.json");
        Self {
            directory,
            lease: Some(lease),
            metadata_path,
        }
    }

    fn lease(&self) -> &RootLease {
        self.lease.as_ref().unwrap()
    }

    fn take_lease(&mut self) -> RootLease {
        self.lease.take().unwrap()
    }

    fn record(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&self.metadata_path).unwrap()).unwrap()
    }

    fn write_record(&self, record: &serde_json::Value) {
        std::fs::write(&self.metadata_path, serde_json::to_vec(record).unwrap()).unwrap();
    }

    fn ownership_error(&self) -> NativeStorageErrorCode {
        self.lease().assert_owned().unwrap_err().code()
    }
}

impl Drop for LeaseFixture {
    fn drop(&mut self) {
        drop(self.lease.take());
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn a_second_lease_on_one_root_is_refused_until_the_first_is_dropped() {
    let mut fixture = LeaseFixture::new();
    let root = fixture.lease().root().to_path_buf();
    fixture.lease().assert_owned().unwrap();
    let refused = RootLease::acquire(&root).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    drop(fixture.take_lease());
    let successor = RootLease::acquire(&root).unwrap();
    assert!(successor.assert_owned().is_ok());
}

#[test]
fn acquiring_a_lease_initializes_a_missing_root() {
    let fixture = LeaseFixture::new();
    let root = fixture
        .directory
        .join("nested")
        .join(LeaseFixture::ROOT_NAME);
    let held = RootLease::acquire(&root).unwrap();
    assert!(root.is_dir());
    assert_eq!(held.root(), fylo_vfs::canonicalize(&root).unwrap());
}

#[test]
fn cloned_lease_holds_the_kernel_lock_until_the_last_clone_is_dropped() {
    let mut fixture = LeaseFixture::new();
    let root = fixture.lease().root().to_path_buf();
    let clone = fixture.lease().clone();
    drop(fixture.take_lease());
    clone.assert_owned().unwrap();
    assert_eq!(
        RootLease::acquire(&root).unwrap_err().code(),
        NativeStorageErrorCode::RootLocked
    );
    drop(clone);
    let successor = RootLease::acquire(&root).unwrap();
    assert!(successor.assert_owned().is_ok());
}

#[test]
fn rejects_a_changed_owner_record() {
    let fixture = LeaseFixture::new();
    let mut record = fixture.record();
    record["owner"] = serde_json::json!("replacement-owner");
    fixture.write_record(&record);
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn rejects_an_unknown_owner_record_version() {
    let fixture = LeaseFixture::new();
    let mut record = fixture.record();
    record["version"] = serde_json::json!(LeaseFixture::INVALID_VERSION);
    fixture.write_record(&record);
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn rejects_a_malformed_owner_record() {
    let fixture = LeaseFixture::new();
    std::fs::write(&fixture.metadata_path, b"{").unwrap();
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn rejects_an_oversized_owner_record() {
    let fixture = LeaseFixture::new();
    let bytes = vec![b' '; LeaseFixture::OVERSIZED_RECORD_BYTES];
    std::fs::write(&fixture.metadata_path, bytes).unwrap();
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn rejects_a_missing_owner_record() {
    let fixture = LeaseFixture::new();
    std::fs::remove_file(&fixture.metadata_path).unwrap();
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn rejects_a_directory_owner_record() {
    let fixture = LeaseFixture::new();
    std::fs::remove_file(&fixture.metadata_path).unwrap();
    std::fs::create_dir(&fixture.metadata_path).unwrap();
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn accepts_a_restored_owner_record() {
    let fixture = LeaseFixture::new();
    let original = std::fs::read(&fixture.metadata_path).unwrap();
    std::fs::remove_file(&fixture.metadata_path).unwrap();
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
    std::fs::write(&fixture.metadata_path, original).unwrap();
    assert!(fixture.lease().assert_owned().is_ok());
}

#[cfg(unix)]
#[test]
fn canonical_aliases_share_a_lock() {
    let fixture = LeaseFixture::new();
    let alias = fixture.directory.join("alias");
    std::os::unix::fs::symlink(fixture.lease().root(), &alias).unwrap();
    assert_eq!(
        RootLease::acquire(&alias).unwrap_err().code(),
        NativeStorageErrorCode::RootLocked
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_owner_records() {
    let fixture = LeaseFixture::new();
    let replacement = fixture.directory.join("record.json");
    std::fs::rename(&fixture.metadata_path, &replacement).unwrap();
    std::os::unix::fs::symlink(&replacement, &fixture.metadata_path).unwrap();
    assert_eq!(
        fixture.ownership_error(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn retains_the_process_id_wire_name() {
    let fixture = LeaseFixture::new();
    let record = fixture.record();
    assert_eq!(record["pid"], serde_json::json!(crate::process_id()));
    assert!(record.get("processId").is_none());
}
