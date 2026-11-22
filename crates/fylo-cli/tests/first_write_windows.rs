//! The first write of a `fylo` process takes its first collection lock,
//! which records the process's start time. On Windows that time came from
//! WMI through PowerShell, 1 to 10 seconds, so a host that stops `fylo` one
//! second after its last request could cut that first write off. The time
//! now comes from the process handle.
#![cfg(windows)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use fylo_test_support::TestFolder;
use serde_json::{Value, json};

struct LoopProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    root: PathBuf,
}

impl LoopProcess {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");

    fn start(name: &str) -> Self {
        let root =
            TestFolder::path().join(format!("fylo-first-write-{}-{name}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let mut child = Command::new(Self::BINARY)
            .args(["exec", "--loop", "--root"])
            .arg(&root)
            .env("FYLO_ALLOW_SYSTEM_DISK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
            root,
        }
    }

    fn ask(&mut self, request: &Value) -> Value {
        writeln!(self.input, "{request}").unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn timed(&mut self, request: &Value) -> Duration {
        let started = Instant::now();
        let answer = self.ask(request);
        let took = started.elapsed();
        assert_eq!(answer["ok"], json!(true), "{answer}");
        took
    }

    fn stop(mut self) {
        drop(self.input);
        self.child.wait().unwrap();
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn first_put(name: &str) -> Duration {
    let mut process = LoopProcess::start(name);
    process.timed(&json!({"op": "createCollection", "collection": "events"}));
    let took = process.timed(&json!({"op": "putData", "collection": "events", "data": {"n": 1}}));
    process.stop();
    took
}

#[test]
fn the_first_write_of_a_process_starts_no_powershell() {
    let quickest = ["a", "b", "c"].into_iter().map(first_put).min().unwrap();
    assert!(
        quickest < Duration::from_millis(300),
        "quickest first put took {quickest:?}"
    );
}
