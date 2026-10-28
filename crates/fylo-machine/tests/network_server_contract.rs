//! `fylo serve` contract: one held root lease, authenticated TCP sessions that
//! speak the machine protocol, serialized execution, bounded frames and
//! connections, and graceful shutdown that releases the root.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fylo_machine::{
    ApplicationConfig, FrameLimits, MachineServer, ServeOptions, ServeToken, ServerHandle,
};
use fylo_storage_native::{NativeStorageErrorCode, RootLease};
use serde_json::{Value, json};

const TOKEN: &str = "network-server-contract-token-0123456789";
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
            "fylo-serve-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = parent.join("root");
        std::fs::create_dir_all(&root).unwrap();
        Self { parent, root }
    }

    fn options() -> ServeOptions {
        ServeOptions::new(
            "127.0.0.1:0".parse().unwrap(),
            ServeToken::new(TOKEN).unwrap(),
        )
    }

    fn start(&self, options: ServeOptions) -> Running {
        let server = MachineServer::bind(&self.root, ApplicationConfig::default(), options)
            .expect("server binds");
        let handle = server.handle();
        let thread = std::thread::spawn(move || server.run());
        Running { handle, thread }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

struct Running {
    handle: ServerHandle,
    thread: JoinHandle<std::io::Result<()>>,
}

impl Running {
    fn address(&self) -> SocketAddr {
        self.handle.local_addr()
    }

    fn client(&self) -> Client {
        Client::authenticated(self.address(), TOKEN)
    }

    fn stop(self) {
        self.handle.shutdown();
        self.thread
            .join()
            .expect("server thread")
            .expect("clean release");
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
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        Self { writer, reader }
    }

    fn authenticated(address: SocketAddr, token: &str) -> Self {
        let mut client = Self::connect(address);
        let reply = client.call(&json!({ "fyloServe": 1, "token": token }));
        assert_eq!(reply["ok"], json!(true), "hello refused: {reply}");
        client
    }

    fn call(&mut self, frame: &Value) -> Value {
        writeln!(self.writer, "{frame}").unwrap();
        self.read()
    }

    /// `Null` when the server closed the connection; Windows may report
    /// that as a reset rather than end of stream.
    fn read(&mut self) -> Value {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => Value::Null,
            Ok(_) => serde_json::from_str(&line).unwrap(),
        }
    }

    fn result(&mut self, frame: &Value) -> Value {
        let reply = self.call(frame);
        assert_eq!(reply["ok"], json!(true), "{frame} failed: {reply}");
        reply["result"].clone()
    }

    fn error_code(&mut self, frame: &Value) -> Value {
        let reply = self.call(frame);
        assert_eq!(reply["ok"], json!(false), "{frame} unexpectedly succeeded");
        reply["error"]["code"].clone()
    }
}

#[test]
fn a_remote_client_puts_patches_reads_and_deletes_a_document() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options());
    let mut client = running.client();
    let handshake = client.result(&json!({ "op": "handshake" }));
    assert_eq!(
        handshake["capabilities"]["networkServer"]["version"],
        json!(1)
    );
    assert_eq!(
        handshake["capabilities"]["documentBuckets"]["putInputs"],
        json!([])
    );
    client.result(&json!({ "op": "createCollection", "collection": "users" }));
    let identifier = client.result(&json!({
        "op": "putData", "collection": "users", "data": { "name": "Ada", "role": "admin" }
    }));
    let identifier = identifier.as_str().expect("put returns an id").to_owned();
    client.result(&json!({
        "op": "patchDoc", "collection": "users", "id": identifier, "newDoc": { "role": "owner" }
    }));
    let latest = client.result(&json!({
        "op": "getLatest", "collection": "users", "id": identifier
    }));
    assert!(
        latest.to_string().contains("owner"),
        "patched body: {latest}"
    );
    let found = client.result(&json!({
        "op": "findDocs", "collection": "users", "query": { "$ops": [{ "role": { "$eq": "owner" } }] }
    }));
    assert!(found.to_string().contains("Ada"), "query result: {found}");
    client.result(&json!({ "op": "delDoc", "collection": "users", "id": identifier }));
    let after = client.result(&json!({
        "op": "findDocs", "collection": "users", "query": { "$ops": [{ "role": { "$eq": "owner" } }] }
    }));
    assert!(
        !after.to_string().contains("Ada"),
        "deleted doc still found: {after}"
    );
    drop(client);
    running.stop();
}

#[test]
fn the_server_holds_the_root_until_it_stops() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options());
    let contender = RootLease::acquire(&fixture.root).unwrap_err();
    assert_eq!(contender.code(), NativeStorageErrorCode::RootLocked);
    let second = MachineServer::bind(
        &fixture.root,
        ApplicationConfig::default(),
        Fixture::options(),
    );
    assert!(
        second.is_err(),
        "a second server must not own the same root"
    );
    running.stop();
    let lease = RootLease::acquire(&fixture.root).expect("released after shutdown");
    lease.release_checked().unwrap();
}

#[test]
fn a_wrong_or_missing_token_is_refused_before_any_frame() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options());
    let mut wrong = Client::connect(running.address());
    let reply = wrong.call(&json!({ "fyloServe": 1, "token": "x".repeat(TOKEN.len()) }));
    assert_eq!(reply["error"]["code"], json!("EAUTH"));
    assert_eq!(wrong.read(), Value::Null, "connection must close");
    let mut skipped = Client::connect(running.address());
    let reply = skipped.call(&json!({ "op": "handshake" }));
    assert_eq!(reply["error"]["code"], json!("EBADHELLO"));
    assert_eq!(skipped.read(), Value::Null, "connection must close");
    running.stop();
}

#[test]
fn a_short_token_is_rejected_at_startup() {
    let error = ServeToken::new("too-short").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!format!("{:?}", ServeToken::new(TOKEN).unwrap()).contains(TOKEN));
}

#[test]
fn a_remote_client_cannot_reach_other_roots_or_host_files() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options());
    let mut client = running.client();
    client.result(&json!({ "op": "createCollection", "collection": "files", "kind": "file" }));
    let elsewhere = fixture.parent.join("elsewhere");
    let refusals = [
        (
            json!({ "op": "handshake", "root": elsewhere }),
            "EBADREQUEST",
        ),
        (
            json!({ "op": "schemaCurrent", "collection": "files", "schemaDir": elsewhere }),
            "EBADREQUEST",
        ),
        (
            json!({ "op": "putData", "collection": "files", "file": { "path": "/etc/hosts" } }),
            "EUNSUPPORTEDOP",
        ),
        (
            json!({ "op": "putData", "collection": "files", "file": { "url": "http://127.0.0.1:9/x" } }),
            "EUNSUPPORTEDOP",
        ),
        (
            json!({ "op": "getFileData", "collection": "files", "id": "4VRNF52JPCO", "path": elsewhere }),
            "EUNSUPPORTEDOP",
        ),
        (
            json!({ "op": "importBulkData", "collection": "files", "url": "http://127.0.0.1:9/x" }),
            "EUNSUPPORTEDOP",
        ),
    ];
    for (frame, code) in refusals {
        assert_eq!(client.error_code(&frame), json!(code), "{frame}");
    }
    assert!(!elsewhere.exists(), "no other root may be created");
    let served = client.result(&json!({ "op": "handshake", "root": running_root(&fixture) }));
    assert_eq!(served["protocolVersion"], json!(1));
    drop(client);
    running.stop();
}

fn running_root(fixture: &Fixture) -> PathBuf {
    std::fs::canonicalize(&fixture.root).unwrap()
}

#[test]
fn an_oversized_frame_is_refused_and_the_session_continues() {
    let fixture = Fixture::new();
    let limits = FrameLimits {
        max_request_bytes: 256,
        ..FrameLimits::default()
    };
    let running = fixture.start(Fixture::options().with_limits(limits));
    let mut client = running.client();
    let large = json!({ "op": "handshake", "padding": "x".repeat(1024) });
    assert_eq!(client.error_code(&large), json!("EFRAME_REQUEST_TOO_LARGE"));
    client.result(&json!({ "op": "handshake" }));
    drop(client);
    running.stop();
}

#[test]
fn connections_over_the_limit_are_refused() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options().with_max_connections(1));
    let mut first = running.client();
    first.result(&json!({ "op": "handshake" }));
    let mut second = Client::connect(running.address());
    assert_eq!(second.read()["error"]["code"], json!("ESERVERBUSY"));
    drop(first);
    drop(second);
    running.stop();
}

#[test]
fn concurrent_writers_on_one_collection_all_commit() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options());
    running
        .client()
        .result(&json!({ "op": "createCollection", "collection": "events" }));
    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let address = running.address();
            std::thread::spawn(move || {
                let mut client = Client::authenticated(address, TOKEN);
                for sequence in 0..10 {
                    client.result(&json!({
                        "op": "putData", "collection": "events",
                        "data": { "writer": writer, "sequence": sequence }
                    }));
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().expect("writer thread");
    }
    let all = running.client().result(&json!({
        "op": "findDocs", "collection": "events", "query": {}
    }));
    assert_eq!(count_documents(&all), 40, "every write commits: {all}");
    running.stop();
}

fn count_documents(result: &Value) -> usize {
    match result {
        Value::Object(map) => map.len(),
        Value::Array(items) => items.len(),
        _ => 0,
    }
}

#[test]
fn a_shutdown_hello_stops_the_server_and_ends_idle_sessions() {
    let fixture = Fixture::new();
    let running = fixture.start(Fixture::options());
    let mut idle = running.client();
    idle.result(&json!({ "op": "handshake" }));
    let mut stopper = Client::connect(running.address());
    let reply = stopper.call(&json!({ "fyloServe": 1, "token": TOKEN, "shutdown": true }));
    assert_eq!(reply["ok"], json!(true));
    running
        .thread
        .join()
        .expect("server thread")
        .expect("clean release");
    assert_eq!(idle.read(), Value::Null, "idle session sees end of stream");
    RootLease::acquire(&fixture.root)
        .expect("root released")
        .release_checked()
        .unwrap();
}

#[test]
fn an_idle_connection_is_closed_after_its_timeout() {
    let fixture = Fixture::new();
    let options = Fixture::options().with_idle_timeout(Duration::from_millis(200));
    let running = fixture.start(options);
    let mut client = running.client();
    client.result(&json!({ "op": "handshake" }));
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(client.read(), Value::Null, "idle connection closed");
    running.stop();
}
