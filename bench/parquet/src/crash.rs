use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::data::{Doc, Generator, Rng, Shape};
use crate::fylo_store::FyloStore;
use crate::stats::Recorder;
use crate::store::{Outcome, Store};
use crate::{Arguments, Engine};

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn seed(engine: &Engine, directory: &Path, shape: Shape, documents: u64) -> Outcome<Vec<String>> {
    let mut store = engine.create(directory, shape)?;
    let mut generator = Generator::new(shape, 7);
    let mut ids = Vec::new();
    let mut next = 0;
    while next < documents {
        let end = (next + 1000).min(documents);
        let docs: Vec<Doc> = (next..end).map(|index| generator.doc(index)).collect();
        ids.extend(store.put_batch(docs)?);
        next = end;
    }
    store.compact()?;
    Ok(ids)
}

fn count_temporary(directory: &Path) -> usize {
    let mut found = 0;
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found += count_temporary(&path);
        } else if path.to_string_lossy().ends_with(".tmp") {
            found += 1;
        }
    }
    found
}

/// Kill a writer with SIGKILL at a random moment, then reopen and check that
/// every write it acknowledged is readable.
pub fn crash(arguments: &Arguments) -> Outcome<()> {
    let engine_name = arguments.text("engine")?;
    let engine = Engine::parse(engine_name)?;
    let shape = Shape::parse(arguments.text("shape")?)?;
    let directory = arguments.path("dir")?;
    let mode = arguments.text("mode").unwrap_or("put").to_owned();
    let rounds = arguments.number("rounds", 10)?;
    let recorder = Recorder::new(
        arguments.path("results")?,
        json!({ "phase": "crash", "engine": engine_name, "shape": shape.name(), "mode": mode, "location": "local" }),
    );
    println!("=== crash {engine_name} {} mode={mode}", shape.name());
    seed(&engine, &directory, shape, arguments.number("n", 5000)?)?;
    let executable = std::env::current_exe().map_err(text)?;
    let mut rng = Rng::new(99);
    let mut total_missing = 0;
    for round in 0..rounds {
        let mut child = Command::new(&executable)
            .args([
                "child",
                "--engine",
                engine_name,
                "--shape",
                shape.name(),
                "--mode",
                &mode,
            ])
            .arg("--dir")
            .arg(&directory)
            .arg("--round")
            .arg(round.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(text)?;
        let stdout = child.stdout.take().ok_or("no child stdout")?;
        let acknowledged = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&acknowledged);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(id) = line.strip_prefix("ack ") {
                    sink.lock().map(|mut ids| ids.push(id.to_owned())).ok();
                }
            }
        });
        std::thread::sleep(Duration::from_millis(400 + rng.below(1200)));
        child.kill().map_err(text)?;
        child.wait().map_err(text)?;
        reader.join().ok();
        let acknowledged = acknowledged.lock().map_err(text)?.clone();
        let leftovers = count_temporary(&engine.store_path(&directory));
        let started = Instant::now();
        let reopened = engine.open(&directory, shape);
        let mut store = match reopened {
            Ok(store) => store,
            Err(error) => {
                recorder.note(
                    "crash_round",
                    json!({ "round": round, "open_error": error }),
                );
                return Err(error);
            }
        };
        let documents = store.count()?;
        let recovery_ms = started.elapsed().as_secs_f64() * 1e3;
        let mut missing = 0;
        for id in &acknowledged {
            if store.get(id)?.is_none() {
                missing += 1;
            }
        }
        total_missing += missing;
        recorder.note(
            "crash_round",
            json!({
                "round": round,
                "acknowledged_in_round": acknowledged.len(),
                "acknowledged_missing": missing,
                "open_and_count_ms": recovery_ms,
                "documents_after": documents,
                "temporary_files_left": leftovers,
            }),
        );
    }
    recorder.note(
        "crash_total",
        json!({ "rounds": rounds, "acknowledged_missing": total_missing }),
    );
    Ok(())
}

pub fn child(arguments: &Arguments) -> Outcome<()> {
    let engine = Engine::parse(arguments.text("engine")?)?;
    let shape = Shape::parse(arguments.text("shape")?)?;
    let directory = arguments.path("dir")?;
    let compacting = arguments.text("mode").unwrap_or("put") == "compact";
    let round = arguments.number("round", 0)?;
    let mut store = engine.open(&directory, shape)?;
    let mut generator = Generator::new(shape, 1000 + round);
    let mut output = std::io::stdout().lock();
    let mut index = 0_u64;
    loop {
        let id = store.put(generator.doc(1_000_000 + round * 100_000 + index))?;
        writeln!(output, "ack {id}").map_err(text)?;
        output.flush().map_err(text)?;
        index += 1;
        if compacting && index % 50 == 0 {
            store.compact()?;
        }
    }
}

fn largest(path: &Path, suffix: &str, best: &mut Option<(u64, PathBuf)>) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            largest(&entry.path(), suffix, best);
        }
        return;
    }
    let name = path.to_string_lossy();
    let matches = name.ends_with(suffix) && (suffix != ".json" || name.contains("/docs/"));
    let bigger = best.as_ref().is_none_or(|(size, _)| metadata.len() > *size);
    if matches && bigger {
        *best = Some((metadata.len(), path.to_path_buf()));
    }
}

fn target(engine: &Engine, directory: &Path) -> Outcome<PathBuf> {
    let suffix = match engine {
        Engine::Parquet(_) => ".parquet",
        Engine::Fylo => ".json",
    };
    let mut best = None;
    largest(&engine.store_path(directory), suffix, &mut best);
    best.map(|(_, path)| path)
        .ok_or_else(|| "no data file to damage".into())
}

fn damage_file(path: &Path, kind: &str) -> Outcome<u64> {
    let length = std::fs::metadata(path).map_err(text)?.len();
    let mut file = OpenOptions::new().write(true).open(path).map_err(text)?;
    let span = 4096.min(length / 2);
    if kind == "tail" {
        file.set_len(length - span).map_err(text)?;
    } else {
        file.seek(SeekFrom::Start(length / 2)).map_err(text)?;
        file.write_all(&vec![0xA5; span as usize]).map_err(text)?;
    }
    file.sync_all().map_err(text)?;
    Ok(span)
}

/// Damage the largest data file (a torn tail or 4 KiB of garbage in the
/// middle), then count how many documents are still readable, and, for
/// FYLO, how long rebuilding the derived indexes from the documents takes.
pub fn damage(arguments: &Arguments) -> Outcome<()> {
    let engine_name = arguments.text("engine")?;
    let engine = Engine::parse(engine_name)?;
    let shape = Shape::parse(arguments.text("shape")?)?;
    let directory = arguments.path("dir")?;
    let kind = arguments.text("kind")?.to_owned();
    let recorder = Recorder::new(
        arguments.path("results")?,
        json!({ "phase": "damage", "engine": engine_name, "shape": shape.name(), "kind": kind, "location": "local" }),
    );
    println!("=== damage {engine_name} {} kind={kind}", shape.name());
    let ids = seed(&engine, &directory, shape, arguments.number("n", 10_000)?)?;
    if kind == "index" {
        return rebuild_index(&engine, &directory, &recorder);
    }
    let path = target(&engine, &directory)?;
    let file_bytes = std::fs::metadata(&path).map_err(text)?.len();
    let span = damage_file(&path, &kind)?;
    let opened = engine.open(&directory, shape);
    let mut store = match opened {
        Ok(store) => store,
        Err(error) => {
            recorder.note(
                "damage",
                json!({ "file": path, "file_bytes": file_bytes, "damaged_bytes": span, "documents": ids.len(), "readable": 0, "open_error": error }),
            );
            return Ok(());
        }
    };
    let mut readable = 0;
    let mut errors = 0;
    let mut first_error = String::new();
    for id in &ids {
        match store.get(id) {
            Ok(Some(_)) => readable += 1,
            Ok(None) => {}
            Err(error) => {
                errors += 1;
                if first_error.is_empty() {
                    first_error = error;
                }
            }
        }
    }
    recorder.note(
        "damage",
        json!({
            "file": path,
            "file_bytes": file_bytes,
            "damaged_bytes": span,
            "documents": ids.len(),
            "readable": readable,
            "read_errors": errors,
            "first_error": first_error,
        }),
    );
    Ok(())
}

fn rebuild_index(engine: &Engine, directory: &Path, recorder: &Recorder) -> Outcome<()> {
    let Engine::Fylo = engine else {
        recorder.note(
            "rebuild_index",
            json!({ "skipped": "Parquet keeps no derived index on disk" }),
        );
        return Ok(());
    };
    let mut store = FyloStore::open(directory)?;
    let index_bytes_before =
        crate::sizes::Footprint::measure(&store.root().join(".collections/items/index"))
            .logical_bytes;
    let timer = Instant::now();
    let before = store.find_eq("bucket", 7)?.len();
    let query_ms_before = timer.elapsed().as_secs_f64() * 1e3;
    let index = store
        .root()
        .join(".collections")
        .join("items")
        .join("index");
    let existed = index.exists();
    if existed {
        std::fs::remove_dir_all(&index).map_err(text)?;
    }
    let after_removal = store.find_eq("bucket", 7).map(|rows| rows.len());
    let started = Instant::now();
    store.rebuild_indexes()?;
    let rebuild_ms = started.elapsed().as_secs_f64() * 1e3;
    let timer = Instant::now();
    let after = store.find_eq("bucket", 7)?.len();
    let query_ms_after = timer.elapsed().as_secs_f64() * 1e3;
    let index_bytes_after =
        crate::sizes::Footprint::measure(&store.root().join(".collections/items/index"))
            .logical_bytes;
    recorder.note(
        "rebuild_index",
        json!({
            "index_directory_removed": existed,
            "rows_before": before,
            "rows_with_index_removed": format!("{after_removal:?}"),
            "rebuild_ms": rebuild_ms,
            "rows_after_rebuild": after,
            "index_bytes_before": index_bytes_before,
            "index_bytes_after": index_bytes_after,
            "eq_query_ms_before_rebuild": query_ms_before,
            "eq_query_ms_after_rebuild": query_ms_after,
        }),
    );
    store.close()
}
