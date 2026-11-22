//! Read cost in a segment collection whose commits hold many documents
//! (FYLO-CR2). Run by hand, in a release build:
//! `cargo test --release -p fylo-engine --test segment_read_cost -- --ignored --nocapture`

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, CollectionLayout, DocumentWrite, NativeWriteRoot};
use serde_json::{Map, Value, json};

struct Bench(PathBuf);

impl Bench {
    const COLLECTION: &str = "people";
    const COMMITS: usize = 10;
    const PER_COMMIT: usize = 1_000;
    const NOTE_BYTES: usize = 2_400;

    fn new() -> Self {
        let root = fylo_test_support::TestFolder::path()
            .join(format!("fylo-segment-read-cost-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        NativeWriteRoot::open(&root)
            .unwrap()
            .create_collection_with_layout(
                Self::COLLECTION,
                CollectionKind::Document,
                None,
                Some(CollectionLayout::Segments),
            )
            .unwrap();
        Self(root)
    }

    fn id(number: usize) -> String {
        format!("4VRNF5{number:05}")
    }

    fn fields(number: usize) -> Map<String, Value> {
        let note = "n".repeat(Self::NOTE_BYTES);
        json!({ "number": number, "team": number % 7, "note": note })
            .as_object()
            .unwrap()
            .clone()
    }

    fn seed(&self) {
        let writer = WriteEngine::open(&self.0).unwrap();
        for commit in 0..Self::COMMITS {
            let first = commit * Self::PER_COMMIT;
            let writes: Vec<DocumentWrite> = (first..first + Self::PER_COMMIT)
                .map(|number| DocumentWrite::Put {
                    identifier: Self::id(number),
                    fields: Self::fields(number),
                })
                .collect();
            writer
                .commit_document_writes(Self::COLLECTION, &writes)
                .unwrap();
        }
    }

    fn overwrite_one_per_commit(&self) {
        let writer = WriteEngine::open(&self.0).unwrap();
        for commit in 0..Self::COMMITS {
            let number = commit * Self::PER_COMMIT;
            writer
                .put_document(Self::COLLECTION, &Self::id(number), Self::fields(number))
                .unwrap();
        }
    }

    fn reader(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn find_all(&self) -> usize {
        let query = StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap();
        self.reader().find(Self::COLLECTION, &query).unwrap().len()
    }

    fn point_reads(&self, count: usize) -> usize {
        let reader = self.reader();
        (0..count)
            .map(|number| reader.get(Self::COLLECTION, &Self::id(number)).unwrap())
            .count()
    }

    fn full_check(&self) -> usize {
        self.reader()
            .inspect_verified(Self::COLLECTION)
            .unwrap()
            .indexed_documents
    }

    fn compact(&self) -> String {
        let report = NativeWriteRoot::open(&self.0)
            .unwrap()
            .compact_collection(Self::COLLECTION)
            .unwrap();
        format!("{report:?}")
    }

    fn timed<T: std::fmt::Debug>(name: &str, step: impl FnOnce() -> T) {
        let started = Instant::now();
        let result = step();
        println!(
            "=== {name}: {:.3} s ({result:?})",
            started.elapsed().as_secs_f64()
        );
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "a timing bench, run by hand in a release build"]
fn segment_read_cost() {
    let bench = Bench::new();
    Bench::timed("seed 10 commits x 1000 documents", || bench.seed());
    Bench::timed("find all (batch reads)", || bench.find_all());
    Bench::timed("1000 point reads (get)", || bench.point_reads(Bench::PER_COMMIT));
    Bench::timed("full index check (inspect verify)", || bench.full_check());
    Bench::timed("overwrite one document per commit", || {
        bench.overwrite_one_per_commit();
    });
    Bench::timed("compact", || bench.compact());
    Bench::timed("find all after compaction", || bench.find_all());
}
