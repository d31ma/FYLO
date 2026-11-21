//! The root lease with a heartbeat and fencing (ADR 0033 section 4), with a
//! second process as the other owner: a crash at every step of a claim, a
//! paused owner, an owner on "another machine" (a test machine id), and an
//! owner whose kernel lock was lost as a router reboot loses it.
//!
//! The roots live in TMPDIR. `FYLO_LEASE_TEST_DIR` puts them in another
//! folder, and `FYLO_LEASE_TEST_PROFILE=efs` takes them with the EFS profile,
//! which is how a root on an NFS mount is opened before the layout release.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use fylo_storage_native::{
    CollectionKind, FilesystemProfile, LeaseTiming, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, RootLease, RootOpenOptions,
};

const CHILD: &str = "FYLO_LEASE_TEST_CHILD";
const ROOT: &str = "FYLO_LEASE_TEST_ROOT";
const TEST_DIR: &str = "FYLO_LEASE_TEST_DIR";
const TEST_PROFILE: &str = "FYLO_LEASE_TEST_PROFILE";
const MACHINE: &str = "FYLO_TEST_LEASE_MACHINE";
const ITEMS: &str = "items";
const HEARTBEAT_SECONDS: &str = "0.05";
const TAKEOVER_SECONDS: &str = "1";
const HEARTBEAT: Duration = Duration::from_millis(50);
const TAKEOVER: Duration = Duration::from_secs(1);
const READY_WAIT: Duration = Duration::from_secs(60);

struct Scratch {
    folder: PathBuf,
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::var(TEST_DIR).map_or_else(|_| std::env::temp_dir(), PathBuf::from);
        let folder = base.join(format!(
            "fylo-lease-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = folder.join("db");
        fs::create_dir_all(&root).unwrap();
        Self {
            folder: fs::canonicalize(&folder).unwrap(),
            root: fs::canonicalize(&root).unwrap(),
        }
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.folder.join(name)
    }

    fn record(&self) -> serde_json::Value {
        let bytes = fs::read(self.folder.join(".db.fylo-root-owner.lock.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn generation(&self) -> u64 {
        self.record()["generation"].as_u64().unwrap()
    }

    fn claim_exists(&self, generation: u64) -> bool {
        self.folder
            .join(format!(".db.fylo-root-owner.claim.{generation}"))
            .exists()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
    }
}

fn profile() -> FilesystemProfile {
    match std::env::var(TEST_PROFILE).as_deref() {
        Ok("efs") => FilesystemProfile::EfsSidecarV1,
        _ => FilesystemProfile::Native,
    }
}

fn timing() -> LeaseTiming {
    LeaseTiming::new(HEARTBEAT, TAKEOVER).unwrap()
}

fn acquire(root: &Path) -> Result<RootLease, fylo_storage_native::NativeStorageError> {
    RootLease::acquire_with_timing(root, profile(), timing())
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

fn writer(root: &Path, lease: RootLease) -> NativeWriteRoot {
    let options = RootOpenOptions::new(root)
        .with_profile(profile())
        .with_lease(lease);
    NativeWriteRoot::open_profiled(options).unwrap()
}

fn put(writer: &NativeWriteRoot, number: usize) -> Result<(), fylo_storage_native::NativeStorageError> {
    let body = format!("{{\"n\":{number}}}");
    writer.put_document(ITEMS, &identifier(number), body.as_bytes(), PutDocumentOptions::default())
}

fn create_items(root: &Path) {
    let lease = acquire(root).unwrap();
    let writer = writer(root, lease);
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
}

/// Start a child that holds the root as `mode` says, with `machine` as its
/// machine id when one is given, and `extra` environment variables.
fn start_child(scratch: &Scratch, mode: &str, machine: Option<&str>, extra: &[(&str, &str)]) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "lease_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, mode)
        .env(ROOT, &scratch.root)
        .env("FYLO_LEASE_HEARTBEAT_SECONDS", HEARTBEAT_SECONDS)
        .env("FYLO_LEASE_TAKEOVER_SECONDS", TAKEOVER_SECONDS)
        .stdout(std::process::Stdio::null());
    if let Some(machine) = machine {
        command.env(MACHINE, machine);
    }
    for (name, value) in extra {
        command.env(name, value);
    }
    command.spawn().unwrap()
}

fn wait_for(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(started.elapsed() < READY_WAIT, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .args([&format!("-{name}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

fn timed<T>(action: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = action();
    (value, started.elapsed())
}

/// The other half of the tests: does nothing in an ordinary run.
///
/// `hold` takes the root, writes `ready`, then waits for `go`, then commits
/// one document and writes the outcome to `result`. `lose-kernel-lock`
/// first gives up its kernel lock, as a router reboot does. `crash` only
/// takes the root: a failpoint set by the parent aborts it on the way.
#[test]
fn lease_child() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT).unwrap());
    let folder = root.parent().unwrap().to_path_buf();
    let lease = RootLease::acquire_with_profile(&root, profile()).unwrap();
    if mode == "crash" {
        drop(lease);
        return;
    }
    if mode == "lose-kernel-lock" {
        lease.drop_kernel_lock_for_test();
    }
    let writer = writer(&root, lease);
    fs::write(folder.join("ready"), b"").unwrap();
    wait_for(&folder.join("go"));
    let outcome = put(&writer, 1).map_or_else(|error| error.code().as_str().to_owned(), |()| "ok".to_owned());
    fs::write(folder.join("result"), outcome).unwrap();
}

#[test]
fn a_killed_owner_on_this_machine_is_taken_over_at_once() {
    let scratch = Scratch::new("killed-here");
    create_items(&scratch.root);
    let mut child = start_child(&scratch, "hold", None, &[]);
    wait_for(&scratch.marker("ready"));
    let generation = scratch.generation();
    child.kill().unwrap();
    child.wait().unwrap();
    let (lease, waited) = timed(|| acquire(&scratch.root).unwrap());
    assert!(waited < TAKEOVER, "waited {waited:?}");
    assert_eq!(scratch.generation(), generation + 1);
    lease.assert_owned().unwrap();
}

#[test]
fn a_killed_owner_on_another_machine_is_taken_over_after_the_takeover_time() {
    let scratch = Scratch::new("killed-elsewhere");
    create_items(&scratch.root);
    let mut child = start_child(&scratch, "hold", Some("machine-b"), &[]);
    wait_for(&scratch.marker("ready"));
    let generation = scratch.generation();
    child.kill().unwrap();
    child.wait().unwrap();
    let (lease, waited) = timed(|| acquire(&scratch.root).unwrap());
    assert!(waited >= TAKEOVER, "waited only {waited:?}");
    assert_eq!(scratch.generation(), generation + 1);
    lease.assert_owned().unwrap();
}

#[test]
fn a_live_owner_on_another_machine_whose_kernel_lock_was_lost_keeps_the_root() {
    let scratch = Scratch::new("lost-kernel-lock");
    create_items(&scratch.root);
    let mut child = start_child(&scratch, "lose-kernel-lock", Some("machine-b"), &[]);
    wait_for(&scratch.marker("ready"));
    let generation = scratch.generation();
    let refused = acquire(&scratch.root).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    assert_eq!(scratch.generation(), generation);
    fs::write(scratch.marker("go"), b"").unwrap();
    wait_for(&scratch.marker("result"));
    child.wait().unwrap();
    assert_eq!(fs::read_to_string(scratch.marker("result")).unwrap(), "ok");
}

#[cfg(unix)]
#[test]
fn a_paused_owner_is_taken_over_and_stops_with_lease_lost_before_it_writes() {
    let scratch = Scratch::new("paused");
    create_items(&scratch.root);
    let mut child = start_child(&scratch, "hold", Some("machine-b"), &[]);
    wait_for(&scratch.marker("ready"));
    let generation = scratch.generation();
    signal(&child, "STOP");
    let (successor, waited) = timed(|| acquire(&scratch.root).unwrap());
    assert!(waited >= TAKEOVER, "waited only {waited:?}");
    assert_eq!(scratch.generation(), generation + 1);
    fs::write(scratch.marker("go"), b"").unwrap();
    signal(&child, "CONT");
    wait_for(&scratch.marker("result"));
    child.wait().unwrap();
    assert_eq!(fs::read_to_string(scratch.marker("result")).unwrap(), "EROOTLEASELOST");
    let successor_writer = writer(&scratch.root, successor);
    put(&successor_writer, 1).unwrap();
    assert_eq!(scratch.generation(), generation + 1);
}

/// Crash at each step of a claim and of a clean stop. Every case ends with
/// a successor that owns the root and a generation that only grew.
fn crash_then_take(failpoint: &str, machine: Option<&str>) -> (Duration, Scratch, u64) {
    let scratch = Scratch::new(failpoint);
    drop(acquire(&scratch.root).unwrap());
    let before = scratch.generation();
    let mut child = start_child(
        &scratch,
        "crash",
        machine,
        &[("FYLO_RUST_FAILPOINT", failpoint), ("FYLO_RUST_FAILPOINT_ACTION", "abort")],
    );
    assert!(!child.wait().unwrap().success(), "the child did not stop at {failpoint}");
    let (lease, waited) = timed(|| acquire(&scratch.root).unwrap());
    lease.assert_owned().unwrap();
    assert!(scratch.generation() > before);
    drop(lease);
    (waited, scratch, before)
}

#[test]
fn a_crash_after_the_claim_file_waits_then_skips_the_stale_claim() {
    let (waited, scratch, before) = crash_then_take("lease-after-claim", None);
    assert!(waited >= TAKEOVER, "waited only {waited:?}");
    assert_eq!(scratch.generation(), before + 2);
    assert!(!scratch.claim_exists(before + 1));
}

#[test]
fn a_crash_after_the_record_on_this_machine_is_taken_at_once() {
    let (waited, scratch, before) = crash_then_take("lease-after-record", None);
    assert!(waited < TAKEOVER, "waited {waited:?}");
    assert_eq!(scratch.generation(), before + 2);
}

#[test]
fn a_crash_after_the_record_on_another_machine_waits_the_takeover_time() {
    let (waited, scratch, before) = crash_then_take("lease-after-record", Some("machine-b"));
    assert!(waited >= TAKEOVER, "waited only {waited:?}");
    assert_eq!(scratch.generation(), before + 2);
}

#[test]
fn a_crash_after_a_clean_stop_record_is_taken_at_once_from_any_machine() {
    let (waited, scratch, before) = crash_then_take("lease-after-release", Some("machine-b"));
    assert!(waited < TAKEOVER, "waited {waited:?}");
    assert_eq!(scratch.generation(), before + 2);
}
