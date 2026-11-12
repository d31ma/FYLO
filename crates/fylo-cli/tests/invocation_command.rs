use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Fixture(PathBuf, bool);

impl Fixture {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");
    const COLLECTION: &'static str = "records";
    const ID: &'static str = "4VRNF52JPCO";
    const ID_PREFIX: &'static str = "4VRNF52JP";
    const LONG_DEADLINE: &'static str = "10000";
    const RESERVE: &'static str = "200";
    const DIGITS: &'static [u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-invocation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path, true)
    }

    /// The same root for another thread; only the original removes it.
    fn share(&self) -> Self {
        Self(self.0.clone(), false)
    }

    fn root(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    fn invoke_with(&self, request: &Value, deadline: &str, reserve: &str) -> Output {
        Command::new(Self::BINARY)
            .args(["exec", "--invocation", "--root", &self.root()])
            .args(["--deadline-ms", deadline, "--cleanup-reserve-ms", reserve])
            .args(["--request", &request.to_string()])
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap()
    }

    fn invoke(&self, request: &Value) -> Output {
        self.invoke_with(request, Self::LONG_DEADLINE, Self::RESERVE)
    }

    fn one_shot(&self, request: &Value) -> Output {
        Command::new(Self::BINARY)
            .args([
                "exec",
                "--root",
                &self.root(),
                "--request",
                &request.to_string(),
            ])
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap()
    }

    fn create_collection(&self) {
        let output = self.invoke(&json!({"op":"createCollection","collection":Self::COLLECTION}));
        Self::frame(&output, true);
    }

    fn documents(&self) -> serde_json::Map<String, Value> {
        let output =
            self.invoke(&json!({"op":"findDocs","collection":Self::COLLECTION,"query":{}}));
        let frame = Self::frame(&output, true);
        frame["result"].as_object().unwrap().clone()
    }

    fn put(id: &str, name: &str) -> Value {
        json!({"op":"putData","collection":Self::COLLECTION,"id":id,"data":{"name":name}})
    }

    fn id(number: usize) -> String {
        let high = Self::DIGITS[number / Self::DIGITS.len()] as char;
        let low = Self::DIGITS[number % Self::DIGITS.len()] as char;
        format!("{}{high}{low}", Self::ID_PREFIX)
    }

    /// One answer frame on stdout, nothing on stderr, and an exit code that
    /// matches the frame's `ok`.
    fn frame(output: &Output, ok: bool) -> Value {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), ok, "{stdout}{stderr}");
        assert!(stderr.is_empty(), "{stderr}");
        assert_eq!(stdout.lines().count(), 1, "{stdout}");
        let frame: Value = serde_json::from_str(stdout.trim_end()).unwrap();
        assert_eq!(frame["ok"], json!(ok), "{frame}");
        frame
    }

    fn failure_code(output: &Output) -> String {
        Self::frame(output, false)["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// An `exec --loop --exclusive-root` that owns the root once it answered
    /// its handshake. Dropping its stdin ends it and releases the root.
    fn hold_root(&self) -> (Child, BufReader<ChildStdout>) {
        let mut child = Command::new(Self::BINARY)
            .args(["exec", "--loop", "--exclusive-root", "--root", &self.root()])
            .env_remove("FYLO_ROOT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"{\"op\":\"handshake\"}\n").unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["ok"], json!(true), "{ready}");
        child.stdin = Some(stdin);
        (child, reader)
    }

    fn release(mut holder: Child) {
        drop(holder.stdin.take());
        assert!(holder.wait().unwrap().success());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
fn two_invocation_processes_share_one_root_without_root_locked() {
    let fixture = Fixture::new();
    fixture.create_collection();
    let per_worker = 15;
    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let fixture = fixture.share();
            thread::spawn(move || {
                for index in 0..per_worker {
                    let id = Fixture::id(worker * per_worker + index);
                    Fixture::frame(&fixture.invoke(&Fixture::put(&id, "parallel")), true);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(fixture.documents().len(), 2 * per_worker);
}

#[test]
fn a_held_root_is_waited_for_until_the_reserve_then_root_locked_changes_nothing() {
    let fixture = Fixture::new();
    fixture.create_collection();
    let (holder, _reader) = fixture.hold_root();
    let started = Instant::now();
    let output = fixture.invoke_with(&Fixture::put(Fixture::ID, "blocked"), "800", "300");
    let waited = started.elapsed();
    assert_eq!(Fixture::failure_code(&output), "EROOTLOCKED");
    assert!(waited >= Duration::from_millis(450), "{waited:?}");
    let one_shot = fixture.one_shot(&Fixture::put(Fixture::ID, "blocked"));
    let one_shot = Fixture::frame(&one_shot, false);
    let invocation = Fixture::frame(&output, false);
    let keys = |frame: &Value| {
        frame
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&invocation), keys(&one_shot));
    assert_eq!(keys(&invocation["error"]), keys(&one_shot["error"]));
    assert_eq!(invocation["error"]["code"], one_shot["error"]["code"]);
    Fixture::release(holder);
    assert!(fixture.documents().is_empty());
}

#[test]
fn a_root_released_inside_the_deadline_lets_the_waiting_invocation_succeed() {
    let fixture = Fixture::new();
    fixture.create_collection();
    let (holder, _reader) = fixture.hold_root();
    let shared = fixture.share();
    let waiting = thread::spawn(move || {
        shared.invoke_with(&Fixture::put(Fixture::ID, "waited"), "5000", "500")
    });
    thread::sleep(Duration::from_millis(400));
    Fixture::release(holder);
    let output = waiting.join().unwrap();
    Fixture::frame(&output, true);
    assert_eq!(fixture.documents()[Fixture::ID]["name"], "waited");
}

#[test]
fn a_deadline_at_or_below_the_reserve_fails_before_any_change() {
    let fixture = Fixture::new();
    let create = json!({"op":"createCollection","collection":Fixture::COLLECTION});
    let equal = fixture.invoke_with(&create, "300", "300");
    assert_eq!(Fixture::failure_code(&equal), "EDEADLINE");
    let below = fixture.invoke_with(&create, "100", "300");
    assert_eq!(Fixture::failure_code(&below), "EDEADLINE");
    let no_reserve = fixture.invoke_with(&create, "300", "0");
    assert_eq!(Fixture::failure_code(&no_reserve), "EBADREQUEST");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn put_data_needs_a_caller_id_and_a_repeated_insert_is_stored_once() {
    let fixture = Fixture::new();
    fixture.create_collection();
    let no_id = json!({"op":"putData","collection":Fixture::COLLECTION,"data":{"name":"no id"}});
    assert_eq!(
        Fixture::failure_code(&fixture.invoke(&no_id)),
        "EBADREQUEST"
    );
    let create = Fixture::put(Fixture::ID, "once");
    Fixture::frame(&fixture.invoke(&create), true);
    let repeated = Fixture::frame(&fixture.invoke(&create), false);
    assert_eq!(repeated["error"]["code"], "EDOCEXISTS");
    assert!(
        repeated["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already exists"),
        "{repeated}"
    );
    let documents = fixture.documents();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[Fixture::ID]["name"], "once");
}

#[test]
fn cursors_and_repository_operations_are_refused() {
    let fixture = Fixture::new();
    fixture.create_collection();
    let page =
        json!({"op":"findDocs","collection":Fixture::COLLECTION,"query":{},"page":{"limit":1}});
    assert_eq!(
        Fixture::failure_code(&fixture.invoke(&page)),
        "EQUERYLOOPREQUIRED"
    );
    let log = json!({"op":"log"});
    assert_eq!(
        Fixture::failure_code(&fixture.invoke(&log)),
        "EUNSUPPORTEDOP"
    );
    let commit = json!({"op":"commit","message":"refused"});
    assert_eq!(
        Fixture::failure_code(&fixture.invoke(&commit)),
        "EUNSUPPORTEDOP"
    );
}

#[test]
fn an_oversized_request_is_refused_with_the_frame_code() {
    let fixture = Fixture::new();
    fixture.create_collection();
    let large = json!({"op":"putData","collection":Fixture::COLLECTION,"id":Fixture::ID,
        "data":{"pad":"x".repeat(2 * 1024 * 1024)}});
    let output = Command::new(Fixture::BINARY)
        .args(["exec", "--invocation", "--root", &fixture.root()])
        .args(["--deadline-ms", Fixture::LONG_DEADLINE])
        .args(["--cleanup-reserve-ms", Fixture::RESERVE, "--request", "-"])
        .env_remove("FYLO_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(large.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert_eq!(Fixture::failure_code(&output), "EFRAME_REQUEST_TOO_LARGE");
    assert!(fixture.documents().is_empty());
}

#[test]
fn host_configuration_errors_go_to_stderr() {
    let fixture = Fixture::new();
    let handshake = json!({"op":"handshake"}).to_string();
    for arguments in [
        vec![
            "--request",
            handshake.as_str(),
            "--cleanup-reserve-ms",
            "10",
        ],
        vec![
            "--request",
            handshake.as_str(),
            "--deadline-ms",
            "soon",
            "--cleanup-reserve-ms",
            "10",
        ],
        vec![
            "--request",
            "[1]",
            "--deadline-ms",
            "1000",
            "--cleanup-reserve-ms",
            "10",
        ],
        vec![
            "--request",
            handshake.as_str(),
            "--deadline-ms",
            "1000",
            "--cleanup-reserve-ms",
            "10",
            "--loop",
        ],
    ] {
        let output = Command::new(Fixture::BINARY)
            .args(["exec", "--invocation", "--root", &fixture.root()])
            .args(arguments)
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn a_successful_invocation_exits_zero_with_one_frame() {
    let fixture = Fixture::new();
    let frame = Fixture::frame(
        &fixture.invoke(&json!({"op":"handshake","requestId":"r1"})),
        true,
    );
    assert_eq!(frame["requestId"], "r1");
    assert_eq!(frame["protocolVersion"], 1);
}

/// `cargo nextest run -p fylo-cli --run-ignored only -E 'test(invocation_cost)' --no-capture`
#[test]
#[ignore = "benchmark: prints the cost of one invocation process"]
fn invocation_cost() {
    let fixture = Fixture::new();
    fixture.create_collection();
    Fixture::frame(&fixture.invoke(&Fixture::put(Fixture::ID, "bench")), true);
    let read = json!({"op":"getDoc","collection":Fixture::COLLECTION,"id":Fixture::ID});
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for number in 0..200 {
        let started = Instant::now();
        Fixture::frame(&fixture.invoke(&read), true);
        reads.push(started.elapsed());
        let started = Instant::now();
        let id = Fixture::id(number + Fixture::DIGITS.len());
        Fixture::frame(&fixture.invoke(&Fixture::put(&id, "bench")), true);
        writes.push(started.elapsed());
    }
    for (name, mut times) in [("getDoc", reads), ("putData", writes)] {
        times.sort();
        let at = |share: usize| times[times.len() * share / 100];
        println!("invocation {name}: p50 {:?} p95 {:?}", at(50), at(95));
    }
}

#[test]
fn a_repeated_id_in_a_segment_collection_answers_document_exists() {
    let fixture = Fixture::new();
    let create =
        json!({"op":"createCollection","collection":Fixture::COLLECTION,"layout":"segments"});
    Fixture::frame(&fixture.invoke(&create), true);
    let put = Fixture::put(Fixture::ID, "once");
    Fixture::frame(&fixture.invoke(&put), true);
    assert_eq!(Fixture::failure_code(&fixture.invoke(&put)), "EDOCEXISTS");
}

#[test]
fn unknown_and_valueless_exec_options_are_configuration_errors() {
    let fixture = Fixture::new();
    let handshake = json!({"op":"handshake"}).to_string();
    let cases: [(&[&str], &str); 4] = [
        (
            &["--bogus-flag", "--request", handshake.as_str()],
            "unknown option --bogus-flag",
        ),
        (
            &[
                "--invocation",
                "--deadline-ms",
                "1000",
                "--cleanup-reserve-ms",
                "10",
                "--request",
            ],
            "missing value for --request",
        ),
        (
            &[
                "--request",
                handshake.as_str(),
                "--max-request-bytes",
                "--loop",
            ],
            "missing value for --max-request-bytes",
        ),
        (
            &["--request", handshake.as_str(), "--json"],
            "unknown option --json",
        ),
    ];
    for (arguments, message) in cases {
        let output = Command::new(Fixture::BINARY)
            .args(["exec", "--root", &fixture.root()])
            .args(arguments)
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert!(stderr.contains(message), "{arguments:?}: {stderr}");
    }
}

#[test]
fn other_subcommands_refuse_unknown_options_too() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["get", "records", Fixture::ID, "--bogus-flag"],
        vec!["version", "--output"],
        vec!["pack", "--out", "copy.vdbx", "--bogus-flag"],
        vec!["SELECT * FROM records", "--bogus-flag"],
    ] {
        let output = Command::new(Fixture::BINARY)
            .args(&arguments)
            .args(["--root", &fixture.root()])
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert!(!output.stderr.is_empty(), "{arguments:?}");
    }
}

#[test]
fn the_handshake_reports_the_invocation_capability() {
    let output = Command::new(Fixture::BINARY)
        .args(["version", "--output", "json"])
        .env_remove("FYLO_ROOT")
        .output()
        .unwrap();
    assert!(output.status.success());
    let identity: Value = serde_json::from_slice(&output.stdout).unwrap();
    let invocation = &identity["capabilities"]["invocation"];
    assert_eq!(invocation["version"], 1);
    assert_eq!(invocation["command"], "exec --invocation");
    assert_eq!(invocation["createRequiresId"], true);
    assert!(
        invocation["excludedOperations"]
            .as_array()
            .unwrap()
            .contains(&json!("commit"))
    );
}
