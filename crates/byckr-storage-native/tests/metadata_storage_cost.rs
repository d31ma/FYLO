//! What one document put with metadata costs when the app folder keeps
//! metadata in extended attributes against sidecar files (BYCKR-L1-4b). The
//! probe gives plain ext4 sidecar files, so this measures what that choice
//! costs there. It prints the median of 20 puts per storage and asserts only
//! that every put worked; run it on the disk to measure, through
//! `scripts/ci/linux-lane.sh` for plain ext4.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use byckr_storage_native::{
    CollectionKind, FilesystemProfile, NativeWriteRoot, PutDocumentOptions, RootLease,
    RootOpenOptions,
};
use serde_json::json;

const PUTS: usize = 20;
const IDS: [&str; PUTS] = [
    "4VRNF52JPD0",
    "4VRNF52JPD1",
    "4VRNF52JPD2",
    "4VRNF52JPD3",
    "4VRNF52JPD4",
    "4VRNF52JPD5",
    "4VRNF52JPD6",
    "4VRNF52JPD7",
    "4VRNF52JPD8",
    "4VRNF52JPD9",
    "4VRNF52JPE0",
    "4VRNF52JPE1",
    "4VRNF52JPE2",
    "4VRNF52JPE3",
    "4VRNF52JPE4",
    "4VRNF52JPE5",
    "4VRNF52JPE6",
    "4VRNF52JPE7",
    "4VRNF52JPE8",
    "4VRNF52JPE9",
];

struct MeasuredRoot {
    folder: PathBuf,
    writer: NativeWriteRoot,
}

impl MeasuredRoot {
    fn new(profile: FilesystemProfile) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder = byckr_test_support::TestFolder::path()
            .canonicalize()
            .unwrap()
            .join(format!(
                "fylo-metadata-cost-{}-{}-{stamp}",
                profile.as_str(),
                std::process::id()
            ));
        let root = folder.join("root");
        fs::create_dir_all(&root).unwrap();
        let lease = RootLease::acquire_with_profile(&root, profile).unwrap();
        let options = RootOpenOptions::new(&root)
            .with_profile(profile)
            .with_lease(lease);
        let writer = NativeWriteRoot::open_profiled(options).unwrap();
        writer
            .create_collection("records", CollectionKind::Document, None)
            .unwrap();
        Self { folder, writer }
    }

    fn put_times(&self) -> Vec<Duration> {
        IDS.iter().map(|id| self.timed_put(id)).collect()
    }

    fn timed_put(&self, id: &str) -> Duration {
        let options = PutDocumentOptions {
            metadata: BTreeMap::from([("reviewer".into(), json!("measured"))]),
        };
        let started = Instant::now();
        self.writer
            .put_document("records", id, br#"{"name":"Ada","score":42}"#, options)
            .unwrap();
        started.elapsed()
    }
}

impl Drop for MeasuredRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
    }
}

fn median_millis(mut times: Vec<Duration>) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1000.0
}

#[test]
#[ignore = "measurement: run on the disk to measure, with YCKR_TEST_ROOT on it"]
fn one_put_with_metadata_costs_this_much_in_each_storage() {
    let attributes = MeasuredRoot::new(FilesystemProfile::Native).put_times();
    let sidecars = MeasuredRoot::new(FilesystemProfile::EfsSidecarV1).put_times();

    println!(
        "=== metadata cost, one put with metadata, median of {PUTS}: xattr {:.3} ms, sidecar {:.3} ms",
        median_millis(attributes.clone()),
        median_millis(sidecars.clone())
    );

    assert_eq!((attributes.len(), sidecars.len()), (PUTS, PUTS));
}
