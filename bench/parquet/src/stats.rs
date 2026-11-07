use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub struct Samples {
    durations: Vec<Duration>,
}

impl Samples {
    pub fn new() -> Self {
        Self {
            durations: Vec::new(),
        }
    }

    pub fn time<T>(&mut self, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = work();
        self.durations.push(started.elapsed());
        result
    }

    fn percentile(sorted: &[Duration], fraction: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let rank = ((fraction * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
        sorted[rank - 1].as_secs_f64() * 1e6
    }

    pub fn summary(&self) -> Value {
        let mut sorted = self.durations.clone();
        sorted.sort();
        let total: f64 = sorted.iter().map(Duration::as_secs_f64).sum();
        let count = sorted.len();
        let mean_us = if count == 0 {
            0.0
        } else {
            total * 1e6 / count as f64
        };
        let ops_per_s = if total > 0.0 {
            count as f64 / total
        } else {
            0.0
        };
        json!({
            "count": count,
            "p50_us": Self::percentile(&sorted, 0.50),
            "p95_us": Self::percentile(&sorted, 0.95),
            "max_us": Self::percentile(&sorted, 1.0),
            "mean_us": mean_us,
            "total_ms": total * 1e3,
            "ops_per_s": ops_per_s,
        })
    }
}

pub struct Recorder {
    path: PathBuf,
    context: Value,
}

impl Recorder {
    pub fn new(path: PathBuf, context: Value) -> Self {
        Self { path, context }
    }

    pub fn record(&self, workload: &str, samples: &Samples, extra: Value) {
        self.write(workload, samples.summary(), extra);
    }

    pub fn note(&self, workload: &str, extra: Value) {
        self.write(workload, json!({}), extra);
    }

    fn write(&self, workload: &str, summary: Value, extra: Value) {
        let mut line = self.context.clone();
        line["workload"] = json!(workload);
        line["stats"] = summary;
        line["extra"] = extra;
        println!(
            "  {:<28} {}",
            workload,
            Self::short(&line["stats"], &line["extra"])
        );
        let opened = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path);
        match opened {
            Ok(mut file) => {
                let _ = writeln!(file, "{line}");
            }
            Err(error) => eprintln!("cannot write results to {}: {error}", self.path.display()),
        }
    }

    fn short(stats: &Value, extra: &Value) -> String {
        let mut text = String::new();
        if stats.get("count").is_some() {
            text = format!(
                "n={} p50={:.1}us p95={:.1}us ops/s={:.1}",
                stats["count"],
                stats["p50_us"].as_f64().unwrap_or(0.0),
                stats["p95_us"].as_f64().unwrap_or(0.0),
                stats["ops_per_s"].as_f64().unwrap_or(0.0)
            );
        }
        if extra.as_object().is_some_and(|fields| !fields.is_empty()) {
            text.push(' ');
            text.push_str(&extra.to_string());
        }
        text
    }
}
