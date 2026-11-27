//! Commit latency of one transaction of 25 writes spread over 7 collections
//! against 25 writes in one collection (FYLO-TXN2), in a given folder.
//!
//! Ignored in ordinary runs. Run it with
//! `FYLO_TXN2_MEASURE_DIR=<folder> cargo test -p fylo-storage-native --test namespace_transaction_cost -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use byckr_storage_native::{
    CollectionKind, CollectionWrites, DocumentWrite, NativeWriteRoot, PlainDocumentCodec,
};
use serde_json::json;

const MEASURE_DIR: &str = "FYLO_TXN2_MEASURE_DIR";
const ROUNDS: usize = 20;
const WRITES: usize = 25;
const COLLECTIONS: usize = 7;

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

fn put(number: usize) -> DocumentWrite {
    DocumentWrite::Put {
        identifier: identifier(number),
        fields: json!({ "body": "x".repeat(200), "number": number })
            .as_object()
            .unwrap()
            .clone(),
    }
}

fn collection_name(index: usize) -> String {
    format!("c{index}")
}

fn batches(round: usize, collections: usize) -> Vec<CollectionWrites> {
    let mut batches: Vec<CollectionWrites> = (0..collections)
        .map(|index| CollectionWrites {
            collection: collection_name(index),
            writes: Vec::new(),
        })
        .collect();
    for write in 0..WRITES {
        batches[write % collections]
            .writes
            .push(put(round * WRITES + write));
    }
    batches
}

fn fresh_root(base: &Path, label: &str) -> PathBuf {
    let root = base.join(format!("txn2-cost-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let writer = NativeWriteRoot::open(&root).unwrap();
    for index in 0..COLLECTIONS {
        writer
            .create_collection(&collection_name(index), CollectionKind::Document, None)
            .unwrap();
    }
    root
}

fn measure(base: &Path, label: &str, collections: usize) -> Vec<Duration> {
    let root = fresh_root(base, label);
    let writer = NativeWriteRoot::open(&root).unwrap();
    let mut times = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let batches = batches(round, collections);
        let started = Instant::now();
        writer
            .commit_collection_writes_with(&batches, &PlainDocumentCodec)
            .unwrap();
        times.push(started.elapsed());
    }
    NativeWriteRoot::close_logs_under(&root).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    times.sort();
    times
}

/// The same writes as one commit per collection, as a caller without
/// transactions over several collections would make them.
fn measure_separately(base: &Path, label: &str) -> Vec<Duration> {
    let root = fresh_root(base, label);
    let writer = NativeWriteRoot::open(&root).unwrap();
    let mut times = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let batches = batches(round, COLLECTIONS);
        let started = Instant::now();
        for batch in &batches {
            writer
                .commit_document_writes_with(&batch.collection, &batch.writes, &PlainDocumentCodec)
                .unwrap();
        }
        times.push(started.elapsed());
    }
    NativeWriteRoot::close_logs_under(&root).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    times.sort();
    times
}

fn report(label: &str, times: &[Duration]) {
    let at = |percent: usize| times[(times.len() - 1) * percent / 100];
    println!(
        "{label}: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms ({} rounds)",
        at(50).as_secs_f64() * 1000.0,
        at(95).as_secs_f64() * 1000.0,
        at(100).as_secs_f64() * 1000.0,
        times.len()
    );
}

#[test]
#[ignore = "measures commit latency in FYLO_TXN2_MEASURE_DIR"]
fn twenty_five_writes_over_seven_collections_against_one() {
    let base = PathBuf::from(std::env::var(MEASURE_DIR).expect(MEASURE_DIR));
    let one = measure(&base, "one", 1);
    let seven = measure(&base, "seven", COLLECTIONS);
    let separate = measure_separately(&base, "separate");
    report("25 writes, 1 collection", &one);
    report("25 writes, 7 collections", &seven);
    report("25 writes, 7 separate commits", &separate);
}
