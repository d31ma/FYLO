use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_storage_native::{NativeRoot, RootFilesystem};
use serde_json::{Value, json};

/// A throwaway folder for one test, removed when the test ends.
struct Place(PathBuf);

impl Place {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");
    const NETWORK_ROOTS: &'static str = "FYLO_TEST_NETWORK_ROOTS";
    const REMOTE_ROOT: &'static str = "EREMOTEROOT";

    fn under(base: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = base.join(format!(
            "fylo-remote-root-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn local() -> Self {
        Self::under(&std::env::temp_dir())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn entries(&self) -> Vec<OsString> {
        let mut names: Vec<OsString> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    /// The frames `exec --loop` answers to `requests`, one per request.
    fn exec_loop(root: &Path, flags: &[&str], requests: &[Value]) -> Vec<Value> {
        let mut child = Command::new(Self::BINARY)
            .args(["exec", "--loop", "--root"])
            .arg(root)
            .args(flags)
            .env_remove("FYLO_ROOT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
        drop(stdin);
        let frames = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect();
        child.wait().unwrap();
        frames
    }

    /// The one frame an `exec --invocation` answers.
    fn invoke(root: &Path, request: &Value) -> Value {
        let output = Command::new(Self::BINARY)
            .args(["exec", "--invocation", "--root"])
            .arg(root)
            .args(["--deadline-ms", "10000", "--cleanup-reserve-ms", "200"])
            .args(["--request", &request.to_string()])
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap();
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim_end()).unwrap()
    }

    fn error_code(frame: &Value) -> &str {
        frame["error"]["code"].as_str().unwrap_or("")
    }

    fn handshake() -> Value {
        json!({"op":"handshake"})
    }

    fn create_collection() -> Value {
        json!({"op":"createCollection","collection":"records"})
    }

    /// The folders named in `FYLO_TEST_NETWORK_ROOTS`, each on a network
    /// filesystem.
    fn network_bases() -> Vec<PathBuf> {
        let Some(list) = std::env::var_os(Self::NETWORK_ROOTS) else {
            return Vec::new();
        };
        std::env::split_paths(&list).collect()
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_local_root_reports_its_filesystem_in_the_handshake() {
    let place = Place::local();
    let root = place.path("root");
    let expected = RootFilesystem::of(&place.0).unwrap();
    let frames = Place::exec_loop(&root, &["--exclusive-root"], &[Place::handshake()]);
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0]["ok"], true, "{}", frames[0]);
    assert_eq!(
        frames[0]["result"]["storage"]["rootLock"],
        json!({
            "owned": true,
            "kernelEnforced": true,
            "remoteQualified": false,
            "filesystem": expected.name(),
            "remote": false,
        })
    );
}

#[test]
fn a_local_root_reports_its_filesystem_before_the_lease_is_taken() {
    let place = Place::local();
    let root = place.path("root");
    let frames = Place::exec_loop(&root, &[], &[Place::handshake()]);
    let lock = &frames[0]["result"]["storage"]["rootLock"];
    assert_eq!(lock["owned"], false, "{lock}");
    assert_eq!(lock["remote"], false, "{lock}");
    assert_eq!(
        lock["filesystem"],
        RootFilesystem::of(&place.0).unwrap().name()
    );
}

#[test]
fn a_local_invocation_root_reports_its_filesystem() {
    let place = Place::local();
    let root = place.path("root");
    fs::create_dir(&root).unwrap();
    let frame = Place::invoke(&root, &Place::handshake());
    let lock = &frame["result"]["storage"]["rootLock"];
    assert_eq!(lock["remote"], false, "{frame}");
    assert_eq!(lock["kernelEnforced"], true, "{frame}");
    assert!(lock["filesystem"].is_string(), "{frame}");
}

/// Run against real network mounts:
/// `FYLO_TEST_NETWORK_ROOTS=/mnt/nfs:/mnt/smb cargo nextest run -p fylo-cli
/// --test remote_root --run-ignored only`. Each folder gets a throwaway
/// subfolder, removed afterwards.
#[test]
#[ignore = "needs FYLO_TEST_NETWORK_ROOTS naming folders on network mounts"]
fn network_roots_are_refused_before_any_write() {
    let bases = Place::network_bases();
    assert!(
        !bases.is_empty(),
        "set {} to folders on network mounts",
        Place::NETWORK_ROOTS
    );
    for base in bases {
        let filesystem = RootFilesystem::of(&base).unwrap();
        assert!(
            filesystem.remote(),
            "{} is not remote: {filesystem:?}",
            base.display()
        );
        eprintln!("{}: {}", base.display(), filesystem.name());
        let place = Place::under(&base);
        refuses_an_owned_loop_root(&place);
        refuses_an_invocation_root(&place);
        refuses_the_first_request_of_a_plain_loop(&place, filesystem.name());
        refuses_an_existing_root(&place);
        refuses_a_vdbx_database(&place);
        #[cfg(unix)]
        admits_the_efs_profile(&place);
    }
}

fn refuses_an_owned_loop_root(place: &Place) {
    let frames = Place::exec_loop(
        &place.path("owned"),
        &["--exclusive-root"],
        &[Place::handshake()],
    );
    eprintln!("exec --loop --exclusive-root: {}", frames[0]);
    assert_eq!(Place::error_code(&frames[0]), Place::REMOTE_ROOT);
    assert!(place.entries().is_empty(), "{:?}", place.entries());
}

fn refuses_an_invocation_root(place: &Place) {
    let root = place.path("invoked");
    fs::create_dir(&root).unwrap();
    let frame = Place::invoke(&root, &Place::create_collection());
    eprintln!("exec --invocation: {frame}");
    assert_eq!(Place::error_code(&frame), Place::REMOTE_ROOT);
    assert_eq!(place.entries(), vec![OsString::from("invoked")]);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir(&root).unwrap();
}

fn refuses_the_first_request_of_a_plain_loop(place: &Place, name: &str) {
    let frames = Place::exec_loop(
        &place.path("plain"),
        &[],
        &[Place::handshake(), Place::create_collection()],
    );
    eprintln!("exec --loop: {} then {}", frames[0], frames[1]);
    let lock = &frames[0]["result"]["storage"]["rootLock"];
    assert_eq!(lock["remote"], true, "{lock}");
    assert_eq!(lock["kernelEnforced"], false, "{lock}");
    assert_eq!(lock["filesystem"], name, "{lock}");
    assert_eq!(Place::error_code(&frames[1]), Place::REMOTE_ROOT);
    assert!(place.entries().is_empty(), "{:?}", place.entries());
}

fn refuses_an_existing_root(place: &Place) {
    let root = place.path("existing");
    fs::create_dir(&root).unwrap();
    let error = NativeRoot::open(&root).unwrap_err();
    assert_eq!(error.code().as_str(), Place::REMOTE_ROOT);
    let output = Command::new(Place::BINARY)
        .args(["exec", "--root"])
        .arg(&root)
        .args(["--request", &Place::create_collection().to_string()])
        .env_remove("FYLO_ROOT")
        .output()
        .unwrap();
    let frame: Value = serde_json::from_slice(&output.stdout).unwrap();
    eprintln!("exec --request: {frame}");
    assert_eq!(Place::error_code(&frame), Place::REMOTE_ROOT);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir(&root).unwrap();
}

fn refuses_a_vdbx_database(place: &Place) {
    let created = place.path("created.vdbx");
    let error = NativeRoot::create_database(&created).unwrap_err();
    eprintln!("create .vdbx: {error}");
    assert_eq!(error.code().as_str(), Place::REMOTE_ROOT);
    assert!(!created.exists());
    let local = Place::local();
    let source = local.path("copied.vdbx");
    NativeRoot::create_database(&source).unwrap();
    let copied = place.path("copied.vdbx");
    fs::copy(&source, &copied).unwrap();
    let error = NativeRoot::open(&copied).unwrap_err();
    eprintln!("open .vdbx: {error}");
    assert_eq!(error.code().as_str(), Place::REMOTE_ROOT);
    fs::remove_file(&copied).unwrap();
}

/// The EFS profile exists on Unix only.
#[cfg(unix)]
fn admits_the_efs_profile(place: &Place) {
    let root = place.path("efs");
    fs::create_dir(&root).unwrap();
    let frames = Place::exec_loop(
        &root,
        &["--exclusive-root", "--filesystem-profile", "efs-sidecar-v1"],
        &[Place::handshake()],
    );
    eprintln!("efs-sidecar-v1: {}", frames[0]);
    assert_eq!(frames[0]["ok"], true, "{}", frames[0]);
    let lock = &frames[0]["result"]["storage"]["rootLock"];
    assert_eq!(lock["owned"], true, "{lock}");
    assert_eq!(lock["remote"], true, "{lock}");
    assert_eq!(lock["kernelEnforced"], false, "{lock}");
}
