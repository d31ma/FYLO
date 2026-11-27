//! Time of one document commit and of a 25-document send, in a given folder
//! (BYCKR-PERF1). On a network mount every flush, rename and lookup is a
//! round trip, so this is the number to cut.
//!
//! Ignored in ordinary runs. Run it with
//! `YCKR_COMMIT_COST_DIR=<folder> cargo test -p fylo-storage-native --test commit_cost -- --ignored --nocapture`.
//! `YCKR_COMMIT_COST_PROFILE=efs` opens the root with the EFS profile, the
//! profile a network share gets. A call-counting library loaded with
//! `DYLD_INSERT_LIBRARIES` can watch the `stat` of `/fylo-trace/begin` and
//! `/fylo-trace/end/<label>/<commits>` to print the calls per commit.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use byckr_storage_native::{
    CollectionKind, DocumentWrite, FilesystemProfile, NativeWriteRoot, PlainDocumentCodec,
    PutDocumentOptions, RootLease, RootOpenOptions,
};
use serde_json::{Map, Value, json};

const MEASURE_DIR: &str = "YCKR_COMMIT_COST_DIR";
const MEASURE_PROFILE: &str = "YCKR_COMMIT_COST_PROFILE";
const SINGLE_ROUNDS: usize = 40;
const SEND_ROUNDS: usize = 8;
const SEND_SIZE: usize = 25;
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

fn fields(number: usize) -> Map<String, Value> {
    let Value::Object(fields) = json!({"n": number, "name": format!("item {number}")}) else {
        unreachable!()
    };
    fields
}

fn open(root: &Path) -> (RootLease, NativeWriteRoot) {
    let lease = RootLease::acquire_with_profile(root, profile()).unwrap();
    let options = RootOpenOptions::new(root)
        .with_profile(profile())
        .with_lease(lease.clone());
    (lease, NativeWriteRoot::open_profiled(options).unwrap())
}

fn trace_begin() {
    let _ = std::fs::metadata("/fylo-trace/begin");
}

fn trace_end(label: &str, per: usize) {
    let _ = std::fs::metadata(format!("/fylo-trace/end/{label}/{per}"));
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

fn put_one(writer: &NativeWriteRoot, number: usize) {
    let body = serde_json::to_vec(&fields(number)).unwrap();
    writer
        .put_document(
            ITEMS,
            &identifier(number),
            &body,
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn send(writer: &NativeWriteRoot, first: usize) {
    let batch: Vec<DocumentWrite> = (first..first + SEND_SIZE)
        .map(|number| DocumentWrite::Put {
            identifier: identifier(number),
            fields: fields(number),
        })
        .collect();
    writer
        .commit_document_writes_with(ITEMS, &batch, &PlainDocumentCodec)
        .unwrap();
}

#[test]
#[ignore = "measures a given folder; set YCKR_COMMIT_COST_DIR"]
fn measure_single_put_and_a_send_of_25() {
    let base = PathBuf::from(std::env::var(MEASURE_DIR).expect("set YCKR_COMMIT_COST_DIR"));
    let folder = base.join(format!("fylo-commit-cost-{}", std::process::id()));
    let root = folder.join("db");
    std::fs::create_dir_all(&root).unwrap();
    let (lease, writer) = open(&root);
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    put_one(&writer, 0);
    trace_begin();
    let singles: Vec<Duration> = (1..=SINGLE_ROUNDS)
        .map(|number| timed(|| put_one(&writer, number)))
        .collect();
    trace_end("single put", SINGLE_ROUNDS);
    let first = 1_000;
    trace_begin();
    let sends: Vec<Duration> = (0..SEND_ROUNDS)
        .map(|round| timed(|| send(&writer, first + round * SEND_SIZE)))
        .collect();
    trace_end("send of 25", SEND_ROUNDS);
    println!("folder: {}", folder.display());
    println!("profile: {}", profile().as_str());
    println!("{}", summary("single put", singles));
    println!("{}", summary("send of 25", sends));
    drop(writer);
    drop(lease);
    let _ = std::fs::remove_dir_all(&folder);
}
