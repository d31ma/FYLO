use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::data::{Doc, Generator, Rng, Shape};
use crate::sizes::Footprint;
use crate::stats::{Recorder, Samples};
use crate::store::{Filter, Outcome, Store};

pub struct Plan {
    pub shape: Shape,
    pub documents: u64,
    pub batch: u64,
    pub reads: usize,
    pub queries: usize,
    pub scans: usize,
    pub writes: usize,
    pub prebuilt: bool,
    pub read_only: bool,
    pub ids_path: PathBuf,
    pub store_path: PathBuf,
}

const PROJECTED: [&str; 3] = ["name", "city", "score"];
const SEED: u64 = 20_261_107;

pub fn load(store: &mut dyn Store, plan: &Plan, recorder: &Recorder) -> Outcome<Vec<String>> {
    let mut generator = Generator::new(plan.shape, SEED);
    let mut batches = Samples::new();
    let mut ids = Vec::with_capacity(plan.documents as usize);
    let started = Instant::now();
    let mut next = 0_u64;
    let mut last_report = Instant::now();
    while next < plan.documents {
        let end = (next + plan.batch).min(plan.documents);
        let docs: Vec<Doc> = (next..end).map(|index| generator.doc(index)).collect();
        let written = batches
            .time(|| store.put_batch(docs))
            .map_err(|error| format!("{error} (after {next} documents loaded)"))?;
        ids.extend(written);
        next = end;
        if last_report.elapsed().as_secs() >= 30 {
            println!(
                "  ... loaded {next}/{} in {:.0} s",
                plan.documents,
                started.elapsed().as_secs_f64()
            );
            last_report = Instant::now();
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    recorder.record(
        &format!("batch_insert_{}", plan.batch),
        &batches,
        json!({ "docs_per_batch": plan.batch }),
    );
    recorder.note(
        "bulk_load",
        json!({ "documents": plan.documents, "seconds": seconds, "docs_per_s": plan.documents as f64 / seconds }),
    );
    std::fs::write(&plan.ids_path, ids.join("\n")).map_err(|error| error.to_string())?;
    Ok(ids)
}

pub fn read_ids(path: &Path) -> Outcome<Vec<String>> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn pick(ids: &[String], count: usize, rng: &mut Rng) -> Vec<String> {
    (0..count)
        .map(|_| ids[rng.below(ids.len() as u64) as usize].clone())
        .collect()
}

pub fn run(store: &mut dyn Store, plan: &Plan, recorder: &Recorder) -> Outcome<()> {
    let ids = if plan.prebuilt {
        read_ids(&plan.ids_path)?
    } else {
        load(store, plan, recorder)?
    };
    let files_after_load = store.file_count();
    if !plan.read_only {
        let mut compaction = Samples::new();
        let report = compaction.time(|| store.compact())?;
        recorder.record(
            "compact_after_load",
            &compaction,
            json!({ "report": report, "files_before": files_after_load }),
        );
    }
    footprint(store, plan, recorder, "footprint_after_load");
    open_and_first_read(store, &ids, recorder)?;
    let count = store.count()?;
    recorder.note("count_after_load", json!({ "documents": count }));
    reads(store, plan, &ids, recorder)?;
    queries(store, plan, recorder)?;
    if plan.read_only {
        return Ok(());
    }
    writes(store, plan, &ids, recorder)?;
    let count = store.count()?;
    recorder.note(
        "count_after_writes",
        json!({ "documents": count, "files": store.file_count() }),
    );
    let mut compaction = Samples::new();
    let report = compaction.time(|| store.compact())?;
    recorder.record(
        "compact_after_writes",
        &compaction,
        json!({ "report": report }),
    );
    footprint(store, plan, recorder, "footprint_final");
    Ok(())
}

fn footprint(store: &dyn Store, plan: &Plan, recorder: &Recorder, label: &str) {
    let measured = Footprint::measure(&plan.store_path);
    recorder.note(
        label,
        json!({ "footprint": measured.to_json(), "store_files": store.file_count() }),
    );
}

fn open_and_first_read(store: &mut dyn Store, ids: &[String], recorder: &Recorder) -> Outcome<()> {
    let mut open = Samples::new();
    open.time(|| store.reopen())?;
    let mut first = Samples::new();
    let found = first.time(|| store.get(&ids[ids.len() / 2]))?;
    recorder.record("open", &open, json!({}));
    recorder.record(
        "first_get_after_open",
        &first,
        json!({ "found": found.is_some() }),
    );
    Ok(())
}

fn reads(store: &mut dyn Store, plan: &Plan, ids: &[String], recorder: &Recorder) -> Outcome<()> {
    let mut rng = Rng::new(SEED + 1);
    let chosen = pick(ids, plan.reads, &mut rng);
    let mut gets = Samples::new();
    let mut missing = 0;
    for id in &chosen {
        if gets.time(|| store.get(id))?.is_none() {
            missing += 1;
        }
    }
    recorder.record("get_by_id", &gets, json!({ "missing": missing }));
    let chosen = pick(ids, plan.reads, &mut rng);
    let mut latest = Samples::new();
    for id in &chosen {
        latest.time(|| store.latest(id))?;
    }
    recorder.record("get_latest", &latest, json!({}));
    Ok(())
}

fn queries(store: &mut dyn Store, plan: &Plan, recorder: &Recorder) -> Outcome<()> {
    let mut rng = Rng::new(SEED + 2);
    let mut samples = Samples::new();
    let mut rows = 0_usize;
    for _ in 0..plan.queries {
        let bucket = rng.below(1000) as i64;
        rows += samples.time(|| store.find_eq("bucket", bucket))?.len();
    }
    recorder.record(
        "find_eq_indexed",
        &samples,
        json!({ "query": "bucket = k (0.1%)", "avg_rows": rows as f64 / plan.queries as f64 }),
    );
    let mut samples = Samples::new();
    let mut rows = 0_usize;
    for _ in 0..plan.queries {
        let low = rng.below(99) as f64;
        rows += samples
            .time(|| store.find_range("score", low, low + 1.0))?
            .len();
    }
    recorder.record(
        "find_range",
        &samples,
        json!({ "query": "k <= score < k+1 (1%)", "avg_rows": rows as f64 / plan.queries as f64 }),
    );
    let filter = Filter {
        field: "age",
        minimum: 90,
    };
    let mut samples = Samples::new();
    let mut rows = 0_usize;
    for _ in 0..plan.scans {
        rows += samples
            .time(|| store.scan_project(Some(&filter), &PROJECTED))?
            .len();
    }
    recorder.record(
        "filter_project_3",
        &samples,
        json!({ "query": "age >= 90 (10%), project name, city, score", "avg_rows": rows as f64 / plan.scans as f64 }),
    );
    let mut samples = Samples::new();
    let mut rows = 0_usize;
    for _ in 0..plan.scans {
        rows += samples.time(|| store.scan_project(None, &PROJECTED))?.len();
    }
    recorder.record(
        "full_scan_project_3",
        &samples,
        json!({ "query": "every document, project name, city, score", "avg_rows": rows as f64 / plan.scans as f64 }),
    );
    Ok(())
}

fn writes(store: &mut dyn Store, plan: &Plan, ids: &[String], recorder: &Recorder) -> Outcome<()> {
    let mut generator = Generator::new(plan.shape, SEED + 3);
    let mut samples = Samples::new();
    for index in 0..plan.writes {
        let doc = generator.doc(plan.documents + index as u64);
        samples.time(|| store.put(doc))?;
    }
    recorder.record("put_single", &samples, json!({}));
    let stride = (ids.len() / (2 * plan.writes + 1)).max(1);
    let mut samples = Samples::new();
    for index in 0..plan.writes {
        let id = &ids[(2 * index) * stride];
        let mut changes = Doc::new();
        changes.insert("score".into(), json!(index as f64 / 3.0));
        changes.insert("active".into(), Value::Bool(index % 2 == 0));
        samples.time(|| store.patch(id, &changes))?;
    }
    recorder.record("patch_single", &samples, json!({ "fields_changed": 2 }));
    let mut samples = Samples::new();
    for index in 0..plan.writes {
        let id = &ids[(2 * index + 1) * stride];
        samples.time(|| store.delete(id))?;
    }
    recorder.record("delete_single", &samples, json!({}));
    Ok(())
}
