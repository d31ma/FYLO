use super::{ContainerLease, ContainerLeaseError, ContainerLeaseTiming};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_DATABASE: &str = "FYLO_VDBX_LEASE_CHILD_DATABASE";
const CHILD_TEST: &str = "container_lease::tests::a_child_process";

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn database(name: &str) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("app.vdbx");
    (Database(dir), path)
}

const FAST: ContainerLeaseTiming = ContainerLeaseTiming {
    takeover_after: Duration::from_millis(300),
    poll: Duration::from_millis(10),
};

fn lease_files(database: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(database.parent().expect("parent"))
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("utf-8")
        })
        .filter(|name| name.starts_with("app.vdbx.lease."))
        .collect();
    names.sort();
    names
}

fn busy(result: Result<ContainerLease, ContainerLeaseError>) -> u64 {
    match result {
        Err(ContainerLeaseError::Busy { generation }) => generation,
        other => panic!("expected Busy, got {other:?}"),
    }
}

#[test]
fn one_writer_holds_the_lease_and_release_hands_it_on_at_once() {
    let (_guard, path) = database("vdbx-lease-basic");
    let first = ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire");
    assert_eq!(first.generation(), 1);
    assert_eq!(lease_files(&path), vec!["app.vdbx.lease.1"]);
    assert_eq!(
        busy(ContainerLease::acquire(
            &path,
            FAST,
            Duration::from_millis(50)
        )),
        1
    );
    first.release().expect("release");
    // A released lease is free: the next writer needs no takeover wait, and
    // the generation keeps growing.
    let started = Instant::now();
    let second = ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire");
    assert!(started.elapsed() < FAST.takeover_after);
    assert_eq!(second.generation(), 2);
    assert_eq!(lease_files(&path), vec!["app.vdbx.lease.2"]);
    drop(second);
    assert_eq!(
        ContainerLease::acquire(&path, FAST, Duration::ZERO)
            .expect("acquire after drop")
            .generation(),
        3
    );
}

#[test]
fn a_crashed_owner_is_taken_over_and_learns_it_lost() {
    let (_guard, path) = database("vdbx-lease-takeover");
    let mut stalled = ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire");
    // Not renewed: after the takeover time, another writer takes it.
    let winner = ContainerLease::acquire(&path, FAST, Duration::from_secs(5)).expect("take over");
    assert_eq!(winner.generation(), 2);
    assert_eq!(lease_files(&path), vec!["app.vdbx.lease.2"]);
    assert!(matches!(
        stalled.assert_owned(),
        Err(ContainerLeaseError::Lost)
    ));
    assert!(matches!(stalled.renew(), Err(ContainerLeaseError::Lost)));
    // Dropping the loser leaves the winner's lease alone.
    drop(stalled);
    winner.assert_owned().expect("winner still owns it");
}

#[test]
fn a_renewing_owner_is_never_taken_over() {
    let (_guard, path) = database("vdbx-lease-renew");
    let mut owner = ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire");
    let contender_path = path.clone();
    let contender = thread::spawn(move || {
        ContainerLease::acquire(&contender_path, FAST, FAST.takeover_after * 3)
    });
    let deadline = Instant::now() + FAST.takeover_after * 4;
    while Instant::now() < deadline {
        owner.renew().expect("renew");
        thread::sleep(FAST.takeover_after / 6);
    }
    assert_eq!(busy(contender.join().expect("join")), 1);
    owner.assert_owned().expect("still owned");
}

#[test]
fn contenders_for_a_stale_lease_produce_exactly_one_winner() {
    let (_guard, path) = database("vdbx-lease-race");
    std::mem::forget(ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire"));
    let contenders = 8;
    let barrier = Arc::new(Barrier::new(contenders));
    let handles: Vec<_> = (0..contenders)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                // Long enough to see the stale lease go, too short to see
                // the winner's go stale as well.
                ContainerLease::acquire(&path, FAST, FAST.takeover_after * 3 / 2)
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect();
    let winners: Vec<_> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert_eq!(winners[0].generation(), 2);
    assert!(
        results
            .iter()
            .filter(|result| result.is_err())
            .all(|result| matches!(result, Err(ContainerLeaseError::Busy { generation: 2 })))
    );
}

#[test]
fn an_empty_lease_left_by_a_crash_during_creation_is_taken_over() {
    let (_guard, path) = database("vdbx-lease-empty");
    std::fs::write(path.with_file_name("app.vdbx.lease.4"), b"").expect("plant");
    std::fs::write(path.with_file_name("app.vdbx.lease.x"), b"").expect("ignored");
    let lease = ContainerLease::acquire(&path, FAST, Duration::from_secs(5)).expect("take over");
    assert_eq!(lease.generation(), 5);
}

/// In a child process the parent spawned: take the lease with the default
/// timing, say so, and hold it, never renewing, until the parent kills it.
#[test]
fn a_child_process() {
    let Ok(database) = std::env::var(CHILD_DATABASE) else {
        return;
    };
    let database = PathBuf::from(database);
    let lease = ContainerLease::acquire(&database, ContainerLeaseTiming::default(), Duration::ZERO)
        .expect("child acquire");
    std::fs::write(database.with_extension("ready"), b"").expect("signal");
    thread::sleep(Duration::from_mins(2));
    drop(lease);
}

/// Spawn a child holding the lease on `database`, with `temporary` as its
/// temporary directory when given, and wait until it holds it.
fn holder(database: &Path, temporary: Option<&Path>) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_DATABASE, database)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(temporary) = temporary {
        command
            .env("TMPDIR", temporary)
            .env("TMP", temporary)
            .env("TEMP", temporary);
    }
    let mut child = command.spawn().expect("spawn holder");
    let deadline = Instant::now() + Duration::from_mins(1);
    while !database.with_extension("ready").exists() {
        assert!(
            child.try_wait().expect("poll holder").is_none(),
            "the holder stopped before it took the lease"
        );
        assert!(Instant::now() < deadline, "the holder never took the lease");
        thread::sleep(Duration::from_millis(10));
    }
    child
}

fn kill(mut child: Child) {
    child.kill().expect("kill holder");
    child.wait().expect("reap holder");
}

fn markers_named(owner: &str, directory: &Path) -> bool {
    directory
        .join(format!("{}{owner}", super::MARKER_PREFIX))
        .exists()
}

fn owner_of(database: &Path, generation: u64) -> String {
    let bytes = std::fs::read(database.with_file_name(format!("app.vdbx.lease.{generation}")))
        .expect("read lease");
    super::owner_token(&bytes).expect("owner token").to_owned()
}

#[test]
fn a_held_lease_keeps_a_locked_marker_until_it_is_released() {
    let (_guard, path) = database("vdbx-lease-marker");
    let first = ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire");
    let owner = owner_of(&path, 1);
    assert!(markers_named(&owner, &std::env::temp_dir()));
    // This process's own live owner is not taken for a stopped one.
    assert!(!super::owner_stopped(
        &std::fs::read(path.with_file_name("app.vdbx.lease.1")).expect("read")
    ));
    first.release().expect("release");
    assert!(!markers_named(&owner, &std::env::temp_dir()));
    let second = ContainerLease::acquire(&path, FAST, Duration::ZERO).expect("acquire");
    let owner = owner_of(&path, 2);
    drop(second);
    assert!(!markers_named(&owner, &std::env::temp_dir()));
}

#[test]
fn a_killed_owner_on_this_host_is_taken_over_at_once_and_a_live_one_is_not() {
    let (_guard, path) = database("vdbx-lease-killed");
    let child = holder(&path, None);
    let owner = owner_of(&path, 1);
    assert!(markers_named(&owner, &std::env::temp_dir()));
    // Alive, it keeps the lease for the whole takeover time.
    assert_eq!(
        busy(ContainerLease::acquire(
            &path,
            ContainerLeaseTiming::default(),
            Duration::from_secs(1)
        )),
        1
    );
    kill(child);
    let started = Instant::now();
    let lease = ContainerLease::acquire(
        &path,
        ContainerLeaseTiming::default(),
        Duration::from_secs(1),
    )
    .expect("taken over at once");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(lease.generation(), 2);
    assert_eq!(lease_files(&path), vec!["app.vdbx.lease.2"]);
    assert!(!markers_named(&owner, &std::env::temp_dir()));
}

#[test]
fn a_killed_owner_whose_marker_this_host_cannot_see_is_waited_out() {
    let (guard, path) = database("vdbx-lease-elsewhere");
    // Another temporary directory stands in for another host: the marker
    // is not where this process looks.
    let elsewhere = guard.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("create directory");
    let child = holder(&path, Some(&elsewhere));
    let owner = owner_of(&path, 1);
    assert!(markers_named(&owner, &elsewhere));
    kill(child);
    assert_eq!(
        busy(ContainerLease::acquire(
            &path,
            ContainerLeaseTiming::default(),
            Duration::from_secs(1)
        )),
        1
    );
    // The takeover time still applies.
    let lease = ContainerLease::acquire(&path, FAST, Duration::from_secs(5)).expect("take over");
    assert_eq!(lease.generation(), 2);
}
