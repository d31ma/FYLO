use super::RootLease;
use super::lease_timing::LeaseTiming;
use super::machine_identity::MachineIdentity;
use crate::{FilesystemProfile, NativeStorageErrorCode};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct TakeoverFixture {
    directory: PathBuf,
    root: PathBuf,
}

impl TakeoverFixture {
    const HEARTBEAT: Duration = Duration::from_millis(20);
    // On Windows a taker reads the owner's start time through PowerShell,
    // which can take seconds on a busy VM, so "at once" needs more room.
    #[cfg(not(windows))]
    const TAKEOVER: Duration = Duration::from_secs(2);
    #[cfg(windows)]
    const TAKEOVER: Duration = Duration::from_secs(15);
    const LOST_WAIT: Duration = Duration::from_secs(10);
    const OTHER_MACHINE: &str = "ffffffffffffffffffffffffffffffff";
    const OTHER_OWNER: &str = "0123456789abcdef0123456789abcdef";
    const RECORD_NAME: &str = ".db.fylo-root-owner.lock.json";

    fn new() -> Self {
        let directory = fylo_test_support::TestFolder::path()
            .join(crate::write::unique_name("fylo-lease-takeover"));
        let root = directory.join("db");
        std::fs::create_dir_all(&root).unwrap();
        Self { directory, root }
    }

    fn timing() -> LeaseTiming {
        LeaseTiming::new(Self::HEARTBEAT, Self::TAKEOVER).unwrap()
    }

    fn acquire(&self) -> Result<RootLease, crate::NativeStorageError> {
        RootLease::acquire_with_timing(&self.root, FilesystemProfile::Native, Self::timing())
    }

    fn record_path(&self) -> PathBuf {
        self.directory.join(Self::RECORD_NAME)
    }

    fn record(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.record_path()).unwrap()).unwrap()
    }

    /// The heartbeat counter now, or `None` while the record is being
    /// replaced (Windows can refuse a read during the rename).
    fn heartbeat_now(&self) -> Option<u64> {
        let bytes = std::fs::read(self.record_path()).ok()?;
        let record = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
        record["heartbeat"].as_u64()
    }

    fn generation(&self) -> u64 {
        self.record()["generation"].as_u64().unwrap()
    }

    fn write_record(&self, record: &serde_json::Value) {
        std::fs::write(self.record_path(), serde_json::to_vec(record).unwrap()).unwrap();
    }

    /// A held record of generation `generation`, as an owner on `machine`
    /// with process `pid` started at `start` would leave it.
    fn held_record(
        generation: u64,
        machine: Option<&str>,
        pid: u32,
        start: &str,
    ) -> serde_json::Value {
        let mut record = serde_json::json!({
            "format": "fylo.namespace-lease.v1",
            "namespace": "db",
            "ownerId": Self::OTHER_OWNER,
            "generation": generation,
            "heartbeat": 7,
            "state": "held",
            "heartbeatMillis": 20,
            "takeoverMillis": 2000,
            "processStart": start,
            "host": "elsewhere",
            "pid": pid,
        });
        if let Some(machine) = machine {
            record["machine"] = serde_json::json!(machine);
        }
        record
    }

    fn timed<T>(action: impl FnOnce() -> T) -> (T, Duration) {
        let started = Instant::now();
        let value = action();
        (value, started.elapsed())
    }

    fn ended_pid() -> u32 {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn claim_path(directory: &Path, generation: u64) -> PathBuf {
        directory.join(format!(".db.fylo-root-owner.claim.{generation}"))
    }
}

impl Drop for TakeoverFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn a_clean_stop_marks_the_record_released_and_the_next_owner_claims_at_once() {
    let fixture = TakeoverFixture::new();
    drop(fixture.acquire().unwrap());
    assert_eq!(fixture.record()["state"], "released");
    let (lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(waited < TakeoverFixture::TAKEOVER, "waited {waited:?}");
    assert_eq!(fixture.generation(), 2);
    lease.assert_owned().unwrap();
}

#[test]
fn the_heartbeat_moves_the_counter_while_the_owner_lives() {
    let fixture = TakeoverFixture::new();
    let lease = fixture.acquire().unwrap();
    let first = fixture.record()["heartbeat"].as_u64().unwrap();
    // The first beat can wait for the process start (PowerShell on
    // Windows), so this waits for the counter instead of a fixed time.
    let started = Instant::now();
    while fixture.heartbeat_now().is_none_or(|beat| beat <= first) {
        assert!(
            started.elapsed() < TakeoverFixture::LOST_WAIT,
            "heartbeat stayed at {first}"
        );
        std::thread::sleep(TakeoverFixture::HEARTBEAT);
    }
    lease.assert_owned().unwrap();
}

#[test]
fn a_record_of_an_owner_on_another_machine_is_taken_only_after_the_takeover_time() {
    let fixture = TakeoverFixture::new();
    let held = TakeoverFixture::held_record(4, Some(TakeoverFixture::OTHER_MACHINE), 1, "x");
    fixture.write_record(&held);
    let (lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(
        waited >= TakeoverFixture::TAKEOVER,
        "waited only {waited:?}"
    );
    assert_eq!(fixture.generation(), 5);
    lease.assert_owned().unwrap();
}

#[test]
fn a_record_whose_heartbeat_moves_is_never_taken() {
    let fixture = TakeoverFixture::new();
    let path = fixture.record_path();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let beating = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut record =
                TakeoverFixture::held_record(4, Some(TakeoverFixture::OTHER_MACHINE), 1, "x");
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                record["heartbeat"] = serde_json::json!(record["heartbeat"].as_u64().unwrap() + 1);
                let scratch = path.with_extension("beat");
                std::fs::write(&scratch, serde_json::to_vec(&record).unwrap()).unwrap();
                std::fs::rename(&scratch, &path).unwrap();
                std::thread::sleep(TakeoverFixture::HEARTBEAT);
            }
        })
    };
    std::thread::sleep(TakeoverFixture::HEARTBEAT * 2);
    let refused = fixture.acquire().unwrap_err();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    beating.join().unwrap();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    assert!(refused.to_string().contains("elsewhere"), "{refused}");
    assert_eq!(fixture.generation(), 4);
}

#[test]
fn a_record_of_an_ended_process_on_this_machine_is_taken_at_once() {
    let fixture = TakeoverFixture::new();
    let machine = MachineIdentity::current().expect("this platform has a machine id");
    let held = TakeoverFixture::held_record(2, Some(&machine), TakeoverFixture::ended_pid(), "x");
    fixture.write_record(&held);
    let (lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(waited < TakeoverFixture::TAKEOVER, "waited {waited:?}");
    assert_eq!(fixture.generation(), 3);
    lease.assert_owned().unwrap();
}

#[test]
fn a_record_of_a_live_process_on_this_machine_waits_the_takeover_time() {
    let fixture = TakeoverFixture::new();
    let machine = MachineIdentity::current().expect("this platform has a machine id");
    let start = crate::write::own_process_identity().unwrap();
    let held = TakeoverFixture::held_record(2, Some(&machine), crate::process_id(), &start);
    fixture.write_record(&held);
    let (lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(
        waited >= TakeoverFixture::TAKEOVER,
        "waited only {waited:?}"
    );
    assert_eq!(fixture.generation(), 3);
    lease.assert_owned().unwrap();
}

#[test]
fn a_record_without_a_process_start_waits_the_takeover_time() {
    let fixture = TakeoverFixture::new();
    let machine = MachineIdentity::current().expect("this platform has a machine id");
    let mut held =
        TakeoverFixture::held_record(2, Some(&machine), TakeoverFixture::ended_pid(), "x");
    held.as_object_mut().unwrap().remove("processStart");
    fixture.write_record(&held);
    let (_lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(
        waited >= TakeoverFixture::TAKEOVER,
        "waited only {waited:?}"
    );
}

#[test]
fn a_deadline_before_the_takeover_time_is_reported_as_root_locked() {
    let fixture = TakeoverFixture::new();
    let held = TakeoverFixture::held_record(4, Some(TakeoverFixture::OTHER_MACHINE), 1, "x");
    fixture.write_record(&held);
    let wait_until = Instant::now() + TakeoverFixture::TAKEOVER / 4;
    let refused = RootLease::acquire_waiting_with_timing(
        &fixture.root,
        FilesystemProfile::Native,
        TakeoverFixture::timing(),
        wait_until,
    )
    .unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    assert_eq!(fixture.generation(), 4);
}

#[test]
fn short_tries_add_up_to_the_takeover_time() {
    let fixture = TakeoverFixture::new();
    let held = TakeoverFixture::held_record(4, Some(TakeoverFixture::OTHER_MACHINE), 1, "x");
    fixture.write_record(&held);
    let started = Instant::now();
    let mut lease = None;
    while lease.is_none() && started.elapsed() < TakeoverFixture::TAKEOVER * 4 {
        let wait_until = Instant::now() + TakeoverFixture::TAKEOVER / 8;
        lease = RootLease::acquire_waiting_with_timing(
            &fixture.root,
            FilesystemProfile::Native,
            TakeoverFixture::timing(),
            wait_until,
        )
        .ok();
    }
    assert!(lease.is_some(), "short tries never took the root");
    assert_eq!(fixture.generation(), 5);
}

#[test]
fn a_legacy_record_is_taken_at_once() {
    let fixture = TakeoverFixture::new();
    fixture.write_record(&serde_json::json!({
        "version": 1, "root": "/x/db", "owner": "rust-root-owner-1", "pid": 1,
        "host": "old", "acquiredAt": 1,
    }));
    let (_lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(waited < TakeoverFixture::TAKEOVER, "waited {waited:?}");
    assert_eq!(fixture.generation(), 1);
}

#[test]
fn an_unreadable_record_waits_and_continues_after_the_newest_claim() {
    let fixture = TakeoverFixture::new();
    std::fs::write(fixture.record_path(), b"{\"format\": torn").unwrap();
    std::fs::write(TakeoverFixture::claim_path(&fixture.directory, 9), b"").unwrap();
    let (_lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(
        waited >= TakeoverFixture::TAKEOVER,
        "waited only {waited:?}"
    );
    assert_eq!(fixture.generation(), 10);
    assert!(!TakeoverFixture::claim_path(&fixture.directory, 9).exists());
}

#[test]
fn a_claim_that_never_became_the_record_is_skipped_after_the_takeover_time() {
    let fixture = TakeoverFixture::new();
    drop(fixture.acquire().unwrap());
    std::fs::write(TakeoverFixture::claim_path(&fixture.directory, 2), b"").unwrap();
    let (_lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(
        waited >= TakeoverFixture::TAKEOVER,
        "waited only {waited:?}"
    );
    assert_eq!(fixture.generation(), 3);
}

#[test]
fn a_taken_over_owner_stops_with_lease_lost_even_if_the_record_comes_back() {
    let fixture = TakeoverFixture::new();
    let lease = fixture.acquire().unwrap();
    let original = std::fs::read(fixture.record_path()).unwrap();
    let mut successor = fixture.record();
    successor["ownerId"] = serde_json::json!(TakeoverFixture::OTHER_OWNER);
    successor["generation"] = serde_json::json!(2);
    fixture.write_record(&successor);
    let started = Instant::now();
    while !lease.heartbeat_lost() {
        assert!(
            started.elapsed() < TakeoverFixture::LOST_WAIT,
            "the heartbeat never saw the successor"
        );
        std::thread::sleep(TakeoverFixture::HEARTBEAT);
    }
    assert_eq!(
        fixture.record()["ownerId"],
        TakeoverFixture::OTHER_OWNER,
        "the lost owner wrote again"
    );
    std::fs::write(fixture.record_path(), original).unwrap();
    assert_eq!(
        lease.assert_owned().unwrap_err().code(),
        NativeStorageErrorCode::RootLeaseLost
    );
}

#[test]
fn an_owner_that_lost_its_kernel_lock_keeps_its_lease() {
    let fixture = TakeoverFixture::new();
    let lease = fixture.acquire().unwrap();
    lease.drop_kernel_lock_for_test();
    let refused = fixture.acquire().unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    lease.assert_owned().unwrap();
    assert_eq!(fixture.generation(), 1);
}

#[test]
fn a_takeover_time_not_longer_than_the_heartbeat_is_refused() {
    let refused = LeaseTiming::new(Duration::from_secs(5), Duration::from_secs(5)).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::Unsupported);
}

#[test]
fn a_heartbeat_under_ten_milliseconds_is_refused() {
    let refused = LeaseTiming::new(Duration::from_millis(5), Duration::from_secs(1)).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::Unsupported);
}

#[test]
fn the_default_timing_is_five_and_twenty_seconds() {
    let timing = LeaseTiming::default();
    assert_eq!(timing.heartbeat(), Duration::from_secs(5));
    assert_eq!(timing.takeover(), Duration::from_secs(20));
}

#[test]
fn a_taker_waits_the_longer_takeover_time_the_owner_recorded() {
    let fixture = TakeoverFixture::new();
    let mut held = TakeoverFixture::held_record(4, Some(TakeoverFixture::OTHER_MACHINE), 1, "x");
    held["takeoverMillis"] = serde_json::json!(4000);
    fixture.write_record(&held);
    let (_lease, waited) = TakeoverFixture::timed(|| fixture.acquire().unwrap());
    assert!(waited >= Duration::from_secs(4), "waited only {waited:?}");
}

#[test]
fn a_claim_made_by_another_taker_during_the_wait_is_reported_as_root_locked() {
    let fixture = TakeoverFixture::new();
    let held = TakeoverFixture::held_record(4, Some(TakeoverFixture::OTHER_MACHINE), 1, "x");
    fixture.write_record(&held);
    let racer = {
        let claim = TakeoverFixture::claim_path(&fixture.directory, 5);
        std::thread::spawn(move || {
            std::thread::sleep(TakeoverFixture::TAKEOVER / 4);
            std::fs::write(claim, b"").unwrap();
        })
    };
    let refused = fixture.acquire().unwrap_err();
    racer.join().unwrap();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    assert_eq!(fixture.generation(), 4);
}
