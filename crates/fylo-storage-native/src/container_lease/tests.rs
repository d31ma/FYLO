use super::{ContainerLease, ContainerLeaseError, ContainerLeaseTiming};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

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
