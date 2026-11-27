//! Open, read, commit and fencing-check latency of a leased root in a given
//! folder (ADR 0033 section 4 asks L1-3 to measure them on a network mount).
//!
//! Ignored in ordinary runs. Run it with
//! `FYLO_LEASE_MEASURE_DIR=<folder> cargo test -p fylo-storage-native --test lease_latency -- --ignored --nocapture`.
//! `FYLO_LEASE_MEASURE_PROFILE=efs` opens the root with the EFS profile, the
//! only profile admitted on a network mount before the layout release.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use byckr_storage_native::{
    CollectionKind, FilesystemProfile, NativeWriteRoot, PutDocumentOptions, RootLease,
    RootOpenOptions,
};

const MEASURE_DIR: &str = "FYLO_LEASE_MEASURE_DIR";
const MEASURE_PROFILE: &str = "FYLO_LEASE_MEASURE_PROFILE";
const ROUNDS: usize = 40;
const ITEMS: &str = "items";

fn profile() -> FilesystemProfile {
    match std::env::var(MEASURE_PROFILE).as_deref() {
        Ok("efs") => FilesystemProfile::EfsSidecarV1,
        _ => FilesystemProfile::Native,
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

fn open(root: &PathBuf) -> (RootLease, NativeWriteRoot) {
    let lease = RootLease::acquire_with_profile(root, profile()).unwrap();
    let options = RootOpenOptions::new(root)
        .with_profile(profile())
        .with_lease(lease.clone());
    (lease, NativeWriteRoot::open_profiled(options).unwrap())
}

fn summary(name: &str, mut samples: Vec<Duration>) -> String {
    samples.sort();
    let at = |percent: usize| samples[(samples.len() - 1) * percent / 100];
    format!(
        "{name}: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms ({} samples)",
        at(50).as_secs_f64() * 1000.0,
        at(95).as_secs_f64() * 1000.0,
        at(100).as_secs_f64() * 1000.0,
        samples.len()
    )
}

fn timed(action: impl FnOnce()) -> Duration {
    let started = Instant::now();
    action();
    started.elapsed()
}

#[test]
#[ignore = "measures a given folder; set FYLO_LEASE_MEASURE_DIR"]
fn measure_open_read_commit_and_fencing_latency() {
    let base = PathBuf::from(std::env::var(MEASURE_DIR).expect("set FYLO_LEASE_MEASURE_DIR"));
    let folder = base.join(format!("fylo-lease-latency-{}", std::process::id()));
    let root = folder.join("db");
    std::fs::create_dir_all(&root).unwrap();
    {
        let (_lease, writer) = open(&root);
        writer
            .create_collection(ITEMS, CollectionKind::Document, None)
            .unwrap();
    }
    let mut opens = Vec::new();
    for _ in 0..ROUNDS {
        opens.push(timed(|| drop(open(&root))));
    }
    let (lease, writer) = open(&root);
    let mut commits = Vec::new();
    for number in 0..ROUNDS {
        let body = format!("{{\"n\":{number}}}");
        commits.push(timed(|| {
            writer
                .put_document(
                    ITEMS,
                    &identifier(number),
                    body.as_bytes(),
                    PutDocumentOptions::default(),
                )
                .unwrap();
        }));
    }
    let collection = byckr_storage_native::NativeRoot::open_profiled(
        RootOpenOptions::new(&root)
            .with_profile(profile())
            .with_lease(lease.clone()),
    )
    .unwrap()
    .collection(ITEMS)
    .unwrap();
    let reads = (0..ROUNDS)
        .map(|number| timed(|| drop(collection.read_document(&identifier(number)).unwrap())))
        .collect();
    let fences = (0..ROUNDS)
        .map(|_| timed(|| lease.assert_owned().unwrap()))
        .collect();
    println!("folder: {}", folder.display());
    println!("profile: {}", profile().as_str());
    println!("{}", summary("open (lease + writer)", opens));
    println!("{}", summary("commit (put_document)", commits));
    println!("{}", summary("read (read_document)", reads));
    println!("{}", summary("fencing check", fences));
    drop(collection);
    drop(writer);
    drop(lease);
    std::fs::remove_dir_all(&folder).unwrap();
}
