mod crash;
mod data;
mod fylo_store;
mod parquet_store;
mod sizes;
mod stats;
mod store;
mod workload;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::data::Shape;
use crate::fylo_store::FyloStore;
use crate::parquet_store::{Codec, ParquetStore};
use crate::sizes::Footprint;
use crate::stats::Recorder;
use crate::store::{Outcome, Store};
use crate::workload::Plan;

pub struct Arguments {
    values: HashMap<String, String>,
}

impl Arguments {
    fn parse(raw: &[String]) -> Self {
        let mut values = HashMap::new();
        let mut position = 0;
        while position < raw.len() {
            let key = raw[position].trim_start_matches("--").to_owned();
            let value = raw.get(position + 1).filter(|next| !next.starts_with("--"));
            match value {
                Some(value) => {
                    values.insert(key, value.clone());
                    position += 2;
                }
                None => {
                    values.insert(key, "true".into());
                    position += 1;
                }
            }
        }
        Self { values }
    }

    pub fn text(&self, key: &str) -> Outcome<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing --{key}"))
    }

    pub fn number(&self, key: &str, default: u64) -> Outcome<u64> {
        match self.values.get(key) {
            Some(value) => value
                .parse()
                .map_err(|_| format!("--{key} must be a number")),
            None => Ok(default),
        }
    }

    pub fn flag(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn path(&self, key: &str) -> Outcome<PathBuf> {
        Ok(PathBuf::from(self.text(key)?))
    }
}

pub enum Engine {
    Fylo,
    Parquet(Codec),
}

impl Engine {
    pub fn parse(text: &str) -> Outcome<Self> {
        match text {
            "fylo-files" => Ok(Self::Fylo),
            "parquet" => Ok(Self::Parquet(Codec::Snappy)),
            "parquet-zstd" => Ok(Self::Parquet(Codec::Zstd(3))),
            "parquet-none" => Ok(Self::Parquet(Codec::Uncompressed)),
            other => Err(format!("unknown engine {other}")),
        }
    }

    pub fn store_path(&self, directory: &Path) -> PathBuf {
        match self {
            Self::Fylo => FyloStore::path_in(directory),
            Self::Parquet(_) => directory.join("parquet"),
        }
    }

    pub fn create(&self, directory: &Path, shape: Shape) -> Outcome<Box<dyn Store>> {
        Ok(match self {
            Self::Fylo => Box::new(FyloStore::create(directory)?),
            Self::Parquet(codec) => Box::new(ParquetStore::create(
                &directory.join("parquet"),
                shape,
                *codec,
            )?),
        })
    }

    pub fn open(&self, directory: &Path, shape: Shape) -> Outcome<Box<dyn Store>> {
        Ok(match self {
            Self::Fylo => Box::new(FyloStore::open(directory)?),
            Self::Parquet(codec) => Box::new(ParquetStore::open(
                &directory.join("parquet"),
                shape,
                *codec,
            )?),
        })
    }
}

fn plan(arguments: &Arguments, engine: &Engine, directory: &Path) -> Outcome<Plan> {
    Ok(Plan {
        shape: Shape::parse(arguments.text("shape")?)?,
        documents: arguments.number("n", 10_000)?,
        batch: arguments.number("batch", 1000)?,
        reads: arguments.number("reads", 1000)? as usize,
        queries: arguments.number("queries", 30)? as usize,
        scans: arguments.number("scans", 5)? as usize,
        writes: arguments.number("writes", 200)? as usize,
        prebuilt: arguments.flag("prebuilt"),
        read_only: arguments.flag("read-only"),
        ids_path: match arguments.text("ids") {
            Ok(path) => PathBuf::from(path),
            Err(_) => directory.join("ids.txt"),
        },
        store_path: engine.store_path(directory),
    })
}

fn recorder(arguments: &Arguments, phase: &str) -> Outcome<Recorder> {
    let context = json!({
        "phase": phase,
        "engine": arguments.text("engine")?,
        "shape": arguments.text("shape").unwrap_or("-"),
        "n": arguments.number("n", 10_000)?,
        "location": arguments.text("location").unwrap_or("local"),
    });
    Ok(Recorder::new(arguments.path("results")?, context))
}

fn run(arguments: &Arguments) -> Outcome<()> {
    let engine = Engine::parse(arguments.text("engine")?)?;
    let directory = arguments.path("dir")?;
    let plan = plan(arguments, &engine, &directory)?;
    let recorder = recorder(arguments, "run")?;
    println!(
        "=== run {} {} n={} at {}",
        arguments.text("engine")?,
        plan.shape.name(),
        plan.documents,
        directory.display()
    );
    let mut store = if plan.prebuilt {
        engine.open(&directory, plan.shape)?
    } else {
        engine.create(&directory, plan.shape)?
    };
    workload::run(store.as_mut(), &plan, &recorder)?;
    drop(store);
    extras(&engine, &directory, plan.shape, arguments, &recorder)
}

fn extras(
    engine: &Engine,
    directory: &Path,
    shape: Shape,
    arguments: &Arguments,
    recorder: &Recorder,
) -> Outcome<()> {
    if arguments.flag("no-extras") {
        return Ok(());
    }
    match engine {
        Engine::Parquet(codec) => {
            let mut store = ParquetStore::open(&directory.join("parquet"), shape, *codec)?;
            for variant in [Codec::Uncompressed, Codec::Snappy, Codec::Zstd(3)] {
                let path = directory.join(format!("copy-{}.parquet", variant.name()));
                let milliseconds = store.write_copy(&path, variant)?;
                let measured = Footprint::measure(&path);
                recorder.note(
                    "size_by_codec",
                    json!({ "codec": variant.name(), "write_ms": milliseconds, "footprint": measured.to_json() }),
                );
                std::fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
            recorder.note(
                "directory_sync_failures",
                json!({ "count": store.directory_sync_failures() }),
            );
        }
        Engine::Fylo => {
            recorder.note(
                "compression",
                json!({ "note": "the per-document file layout stores canonical JSON uncompressed" }),
            );
        }
    }
    Ok(())
}

fn load_only(arguments: &Arguments) -> Outcome<()> {
    let engine = Engine::parse(arguments.text("engine")?)?;
    let directory = arguments.path("dir")?;
    let plan = plan(arguments, &engine, &directory)?;
    let recorder = recorder(arguments, "load")?;
    println!(
        "=== load {} {} n={}",
        arguments.text("engine")?,
        plan.shape.name(),
        plan.documents
    );
    let mut store = engine.create(&directory, plan.shape)?;
    workload::load(store.as_mut(), &plan, &recorder)?;
    let measured = Footprint::measure(&plan.store_path);
    recorder.note(
        "footprint_after_load",
        json!({ "footprint": measured.to_json() }),
    );
    Ok(())
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = raw.first().cloned() else {
        eprintln!("usage: fylo-parquet-bench run|load|crash|child|damage --engine ...");
        std::process::exit(2);
    };
    let arguments = Arguments::parse(&raw[1..]);
    let outcome = match command.as_str() {
        "run" => run(&arguments),
        "load" => load_only(&arguments),
        "crash" => crash::crash(&arguments),
        "child" => crash::child(&arguments),
        "damage" => crash::damage(&arguments),
        other => Err(format!("unknown command {other}")),
    };
    if let Err(error) = outcome {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
