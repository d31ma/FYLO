//! The hot set in an embedded session that holds the root's exclusive
//! lease: clients of an application server that embeds FYLO never see an
//! incoherent or stale settled row, warm reads come from memory, and the
//! resident bytes stay within the budget under load.

mod support;

use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, serve, serve_exclusive};
use serde_json::{Value, json};
use support::app_server::AppServer;

const COLLECTION: &str = "records";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-embedded-hot-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = parent.join("root");
        std::fs::create_dir_all(&root).unwrap();
        Self { parent, root }
    }

    fn start(&self, hot_set_bytes: usize) -> Running {
        let application = ApplicationConfig {
            hot_set_bytes: Some(hot_set_bytes),
            ..ApplicationConfig::default()
        };
        Running(AppServer::start(&self.root, application))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

struct Running(AppServer);

impl Running {
    fn address(&self) -> SocketAddr {
        self.0.address()
    }

    fn client(&self) -> Client {
        Client::connect(self.address())
    }

    fn stop(self) {
        drop(self.0);
    }
}

struct Client {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let writer = TcpStream::connect(address).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_mins(1)))
            .unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        Self { writer, reader }
    }

    fn call(&mut self, frame: &Value) -> Value {
        writeln!(self.writer, "{frame}").unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn result(&mut self, frame: &Value) -> Value {
        let reply = self.call(frame);
        assert_eq!(reply["ok"], json!(true), "{frame} failed: {reply}");
        reply["result"].clone()
    }

    fn hot_set(&mut self) -> Value {
        self.result(&json!({ "op": "handshake" }))["capabilities"]["hotSet"].clone()
    }

    fn get(&mut self, id: &str) -> Value {
        let found = self.result(&json!({ "op": "getDoc", "collection": COLLECTION, "id": id }));
        found[id].clone()
    }

    fn find(&mut self, query: &Value) -> serde_json::Map<String, Value> {
        let found =
            self.result(&json!({ "op": "findDocs", "collection": COLLECTION, "query": query }));
        found.as_object().cloned().unwrap_or_default()
    }

    fn patch(&mut self, id: &str, sequence: u64) {
        self.result(&json!({
            "op": "patchDoc", "collection": COLLECTION, "id": id,
            "newDoc": { "sequence": sequence, "score": sequence, "mirror": sequence }
        }));
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 20_000 + number)
}

fn seed(client: &mut Client, count: usize) {
    client.result(&json!({ "op": "createCollection", "collection": COLLECTION }));
    let documents: serde_json::Map<String, Value> = (0..count)
        .map(|number| {
            let fields = json!({
                "sequence": 0, "score": 0, "mirror": 0, "group": "all", "number": number,
                "body": "payload ".repeat(16)
            });
            (id(number), fields)
        })
        .collect();
    for (identifier, fields) in documents {
        client.result(&json!({
            "op": "putData", "collection": COLLECTION, "id": identifier, "data": fields
        }));
    }
}

/// Stops the readers when a writer panics, so a failing writer cannot leave
/// them running forever.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

/// A row is coherent when one write produced all three fields.
fn coherence_failure(identifier: &str, row: &Value) -> Option<String> {
    let coherent = row["sequence"] == row["score"] && row["score"] == row["mirror"];
    (!coherent).then(|| format!("incoherent row {identifier}: {row}"))
}

/// Readers check every row they see for coherence and for going backwards.
fn reader(address: SocketAddr, count: usize, stop: &AtomicBool) -> (u64, Vec<String>) {
    let mut client = Client::connect(address);
    let mut seen = vec![0_u64; count];
    let mut failures = Vec::new();
    let mut reads = 0;
    let everything = json!({ "$ops": [{ "group": { "$eq": "all" } }] });
    while !stop.load(Ordering::SeqCst) {
        for (identifier, row) in client.find(&everything) {
            failures.extend(coherence_failure(&identifier, &row));
            let number = usize::try_from(row["number"].as_u64().unwrap()).unwrap();
            let sequence = row["sequence"].as_u64().unwrap();
            if sequence < seen[number] {
                failures.push(format!(
                    "{identifier} went back from {} to {sequence}",
                    seen[number]
                ));
            }
            seen[number] = sequence;
        }
        let number = usize::try_from(reads % count as u64).unwrap();
        let row = client.get(&id(number));
        failures.extend(coherence_failure(&id(number), &row));
        reads += 2;
    }
    (reads, failures)
}

/// A writer owns some documents. After each acknowledged write, a read on
/// its second connection must already see it: the settled check.
fn writer(address: SocketAddr, owned: &[usize], steps: u64) -> (u64, Vec<String>) {
    let mut writes = Client::connect(address);
    let mut reads = Client::connect(address);
    let mut failures = Vec::new();
    let mut settled = 0;
    for sequence in 1..=steps {
        for &number in owned {
            writes.patch(&id(number), sequence);
            let row = reads.get(&id(number));
            if row["sequence"] != json!(sequence) {
                failures.push(format!("settled get of {} saw {row}", id(number)));
            }
            let by_score = reads.find(&json!({ "$ops": [{ "score": { "$eq": sequence } }] }));
            if !by_score.contains_key(&id(number)) {
                failures.push(format!("settled find missed {} at {sequence}", id(number)));
            }
            settled += 2;
        }
    }
    (settled, failures)
}

fn run_round(hot_set_bytes: usize) -> (u64, u64) {
    const DOCUMENTS: usize = 8;
    const STEPS: u64 = 12;
    let fixture = Fixture::new();
    let running = fixture.start(hot_set_bytes);
    seed(&mut running.client(), DOCUMENTS);
    let address = running.address();
    let stop = AtomicBool::new(false);
    let (reads, settled, failures) = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..3)
            .map(|_| scope.spawn(|| reader(address, DOCUMENTS, &stop)))
            .collect();
        let writers: Vec<_> = [vec![0, 2, 4, 6], vec![1, 3, 5, 7]]
            .into_iter()
            .map(|owned| {
                let stop = &stop;
                scope.spawn(move || {
                    let _stop = StopOnDrop(stop);
                    writer(address, &owned, STEPS)
                })
            })
            .collect();
        let mut failures = Vec::new();
        let mut settled = 0;
        for writer in writers {
            let (checks, found) = writer.join().unwrap();
            settled += checks;
            failures.extend(found);
        }
        stop.store(true, Ordering::SeqCst);
        let mut reads = 0;
        for reader in readers {
            let (count, found) = reader.join().unwrap();
            reads += count;
            failures.extend(found);
        }
        (reads, settled, failures)
    });
    assert!(failures.is_empty(), "failures: {failures:#?}");
    let mut client = running.client();
    for number in 0..DOCUMENTS {
        assert_eq!(client.get(&id(number))["sequence"], json!(STEPS));
    }
    let statistics = client.hot_set();
    assert!(statistics["hits"].as_u64().unwrap() > 0, "{statistics}");
    drop(client);
    running.stop();
    (reads, settled)
}

#[test]
fn repeated_concurrent_reads_and_writes_never_see_a_stale_settled_row() {
    let mut reads = 0;
    let mut settled = 0;
    for _ in 0..4 {
        let (round_reads, round_settled) = run_round(1024 * 1024);
        reads += round_reads;
        settled += round_settled;
    }
    assert!(reads > 0 && settled == 4 * 2 * 8 * 12);
}

#[test]
fn a_tight_budget_still_serves_correct_rows() {
    run_round(4 * 1024);
}

#[test]
fn warm_reads_come_from_memory() {
    let fixture = Fixture::new();
    let running = fixture.start(1024 * 1024);
    let mut client = running.client();
    seed(&mut client, 20);
    let query = json!({ "$ops": [{ "number": { "$eq": 7 } }] });
    assert_eq!(client.find(&query).len(), 1);
    client.get(&id(3));
    let warm = client.hot_set();
    for _ in 0..5 {
        assert_eq!(client.find(&query).len(), 1);
        assert_eq!(client.get(&id(3))["number"], json!(3));
    }
    let after = client.hot_set();
    assert_eq!(after["misses"], warm["misses"], "warm reads read storage");
    assert_eq!(
        after["hits"].as_u64().unwrap(),
        warm["hits"].as_u64().unwrap() + 10
    );
    drop(client);
    running.stop();
}

#[test]
fn the_budget_holds_under_load() {
    const DOCUMENTS: usize = 150;
    let budget = 32 * 1024;
    let fixture = Fixture::new();
    let running = fixture.start(budget);
    seed(&mut running.client(), DOCUMENTS);
    let address = running.address();
    let stop = AtomicBool::new(false);
    let peak = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..3)
            .map(|_| scope.spawn(|| reader(address, DOCUMENTS, &stop)))
            .collect();
        let stop_ref = &stop;
        let writer = scope.spawn(move || {
            let _stop = StopOnDrop(stop_ref);
            writer(address, &[0, 50, 100], 5)
        });
        let monitor = scope.spawn(|| {
            let mut client = Client::connect(address);
            let mut peak = 0;
            while !stop.load(Ordering::SeqCst) {
                let statistics = client.hot_set();
                peak = peak.max(statistics["residentBytes"].as_u64().unwrap());
            }
            peak
        });
        let (_, failures) = writer.join().unwrap();
        assert!(failures.is_empty(), "{failures:#?}");
        std::thread::sleep(Duration::from_millis(200));
        stop.store(true, Ordering::SeqCst);
        for reader in readers {
            let (_, failures) = reader.join().unwrap();
            assert!(failures.is_empty(), "{failures:#?}");
        }
        monitor.join().unwrap()
    });
    let statistics = running.client().hot_set();
    assert_eq!(statistics["budgetBytes"], json!(budget));
    assert!(
        peak > 0 && peak <= budget as u64,
        "peak {peak} over {budget}"
    );
    assert!(
        statistics["evictions"].as_u64().unwrap() > 0,
        "{statistics}"
    );
    running.stop();
}

#[test]
fn a_zero_budget_turns_the_hot_set_off() {
    let fixture = Fixture::new();
    let running = fixture.start(0);
    let mut client = running.client();
    assert_eq!(client.hot_set(), json!(false));
    seed(&mut client, 2);
    assert_eq!(client.get(&id(1))["number"], json!(1));
    drop(client);
    running.stop();
}

fn handshake_hot_set(exclusive: bool, root: PathBuf) -> Value {
    let mut input = Cursor::new(b"{\"op\":\"handshake\"}\n".to_vec());
    let mut output = Vec::new();
    let session = if exclusive { serve_exclusive } else { serve };
    session(&mut input, &mut output, Some(root), FrameLimits::default()).unwrap();
    let frame: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(frame["ok"], json!(true), "{frame}");
    frame["result"]["capabilities"]["hotSet"].clone()
}

/// Without the exclusive lease another process may write the root, so a
/// session has no hot set; `exec --loop --exclusive-root` has one by default.
#[test]
fn only_a_session_holding_the_exclusive_lease_keeps_a_hot_set() {
    let fixture = Fixture::new();
    assert_eq!(handshake_hot_set(false, fixture.root.clone()), json!(false));
    let statistics = handshake_hot_set(true, fixture.root.clone());
    assert_eq!(
        statistics["budgetBytes"],
        json!(fylo_engine::HotSet::DEFAULT_BUDGET_BYTES)
    );
}
