//! Processes waiting for one byte take turns (ADR 0016 section 7). The
//! tests spawn this test binary again, running [`a_child_process`] in the
//! role an environment variable names.

use super::super::{ByteLockGuard, ByteRangeLockFile, LockByte, LockMode, TURN_LEVELS};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROLE: &str = "YCKR_BYTE_LOCK_CHILD_ROLE";
const DATABASE: &str = "YCKR_BYTE_LOCK_CHILD_DATABASE";
const NAME: &str = "YCKR_BYTE_LOCK_CHILD_NAME";
const CHILD_TEST: &str = "byte_range_lock::tests::turns::a_child_process";
const WAIT: Duration = Duration::from_mins(1);
const WAITER_FIRST: &str = "waiter first";
const HOLDER_FIRST: &str = "holder first";
/// How often each looping child takes the byte.
const ROUNDS: u64 = 12;
/// How long a looping child holds the byte each time.
const HOLD: Duration = Duration::from_millis(5);
/// The most times one wait of two processes may be passed by a later
/// request. Two processes take turns strictly whatever the timing; a child
/// can still be passed while it is paused between noting its request and
/// making it, as on a loaded Windows VM. Measured on 2026-11-14: at most 0
/// on macOS, 3 on Windows while 192 other tests ran beside it, and 11 with
/// turns switched off. The bound is twice the worst with turns and about
/// half the count without them.
const MOST_PASSES_OF_TWO: usize = 6;
/// The most times one wait of more processes than turn levels may be passed
/// by later requests. Their order is strict only while waiters notice a
/// freed turn byte faster than a holder keeps the byte: Windows rounds
/// every short sleep up to its 15.6 ms timer tick, so there two of them
/// can race for the last turn level (ADR 0016 section 7). Measured on
/// 2026-11-14 with four processes: at most 3 on macOS, 8 on Windows, and
/// 23 to 34 with turns switched off. Three times the worst with turns (24)
/// would no longer catch the bug, so the bound is twice the worst with
/// turns and about half the count without them.
const MOST_PASSES_OF_MANY: usize = 16;

/// The child processes of a test. Those still running when the test ends
/// without waiting for them are stopped and reaped, so a failed test leaves
/// no child behind.
struct ChildProcesses(Vec<Option<Child>>);

impl ChildProcesses {
    /// Take the child at `index` out to wait for it.
    fn take(&mut self, index: usize) -> Child {
        self.0[index].take().expect("child already taken")
    }

    /// Whether every child not yet taken is still running.
    fn all_running(&mut self) -> bool {
        self.0
            .iter_mut()
            .flatten()
            .all(|child| child.try_wait().expect("child state").is_none())
    }

    /// Wait for every child and require that each passed.
    fn all_passed(&mut self) {
        for index in 0..self.0.len() {
            passed(self.take(index));
        }
    }
}

impl Drop for ChildProcesses {
    fn drop(&mut self) {
        for child in self.0.iter_mut().flatten() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One acquisition of the byte, as a looping child logged it.
struct Acquisition {
    child: String,
    asked_at: u128,
}

impl Acquisition {
    fn parse(line: &str) -> Self {
        let (child, asked_at) = line.split_once(' ').expect("log line");
        Self {
            child: child.to_owned(),
            asked_at: asked_at.parse().expect("asked at"),
        }
    }
}

fn byte() -> LockByte {
    LockByte::collection_writer(3)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `hold`: take the byte, say so, let it go when the parent says `go` and
///   ask for it again at once, then write down whether the parent's waiting
///   thread had it first.
/// - `loop`: once the parent says `go`, take the byte [`ROUNDS`] times as a
///   writer putting documents one after another would.
/// - `legacy`: the same, waiting as BYCKR did before turns: trying the byte
///   alone until it is free.
/// - `queue`: wait for the byte, which the parent holds, until stopped.
#[test]
fn a_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let database = PathBuf::from(std::env::var(DATABASE).expect("database"));
    let file = ByteRangeLockFile::open(&database).expect("open");
    match role.as_str() {
        "hold" => hold_and_ask_again(&file, &database),
        "loop" => take_in_rounds(&database, || {
            file.lock(byte(), LockMode::Exclusive, WAIT).expect("lock")
        }),
        "legacy" => take_in_rounds(&database, || wait_without_turns(&file)),
        "queue" => drop(file.lock(byte(), LockMode::Exclusive, WAIT)),
        other => panic!("unknown child role {other}"),
    }
}

fn hold_and_ask_again(file: &ByteRangeLockFile, database: &Path) {
    let directory = database.parent().expect("directory");
    let held = file.lock(byte(), LockMode::Exclusive, WAIT).expect("hold");
    fs::write(directory.join("held"), b"").expect("signal held");
    wait_for(&directory.join("go"));
    drop(held);
    let again = file.lock(byte(), LockMode::Exclusive, WAIT).expect("again");
    let order = order(directory);
    fs::write(directory.join("order"), order).expect("write order");
    drop(again);
}

fn order(directory: &Path) -> &'static str {
    if directory.join("waiter-has").exists() {
        WAITER_FIRST
    } else {
        HOLDER_FIRST
    }
}

/// Take the byte [`ROUNDS`] times with `take`, each time adding one to the
/// counter beside the database while holding it, slowly enough that a
/// process breaking the lock would lose an update, and logging when it
/// asked.
fn take_in_rounds(database: &Path, take: impl Fn() -> ByteLockGuard) {
    let directory = database.parent().expect("directory");
    let name = std::env::var(NAME).expect("name");
    fs::write(directory.join(format!("{name}.ready")), b"").expect("ready");
    wait_for(&directory.join("go"));
    for _ in 0..ROUNDS {
        let asked_at = now_nanos();
        let guard = take();
        add_one(directory);
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("log"))
            .expect("log");
        writeln!(log, "{name} {asked_at}").expect("log line");
        drop(guard);
    }
}

/// Add one to the counter beside the database, reading it, pausing, then
/// writing it.
fn add_one(directory: &Path) {
    let counter = directory.join("counter");
    let count: u64 = fs::read_to_string(&counter)
        .unwrap_or_default()
        .parse()
        .unwrap_or(0);
    thread::sleep(HOLD);
    fs::write(&counter, (count + 1).to_string()).expect("count");
}

/// Wait for the byte as `ByteRangeLockFile::lock` did before turns: try
/// it alone, sleeping up to a millisecond between tries.
fn wait_without_turns(file: &ByteRangeLockFile) -> ByteLockGuard {
    let deadline = Instant::now() + WAIT;
    let mut interval = Duration::from_micros(20);
    loop {
        if let Some(guard) = file
            .try_lock(byte(), LockMode::Exclusive)
            .expect("try lock")
        {
            return guard;
        }
        assert!(Instant::now() < deadline, "the legacy wait timed out");
        thread::sleep(interval);
        interval = (interval * 2).min(Duration::from_millis(1));
    }
}

#[test]
fn a_holder_that_lets_a_byte_go_and_asks_again_waits_behind_a_waiting_process() {
    let (_dir, path) = super::database("brl-turns");
    let directory = path.parent().expect("directory").to_owned();
    let mut holder = ChildProcesses(vec![Some(spawn(&path, "hold", "holder"))]);
    wait_until(&directory.join("held"), &mut holder);
    let file = ByteRangeLockFile::open(&path).expect("open");
    let waiter = {
        let (file, directory) = (file.clone(), directory.clone());
        thread::spawn(move || {
            let guard = file.lock(byte(), LockMode::Exclusive, WAIT).expect("wait");
            fs::write(directory.join("waiter-has"), b"").expect("signal waiter");
            thread::sleep(Duration::from_millis(100));
            drop(guard);
        })
    };
    wait_for_turn(&file, Duration::from_secs(1));
    fs::write(directory.join("go"), b"").expect("signal go");
    waiter.join().expect("waiter");
    holder.all_passed();
    assert_eq!(
        fs::read_to_string(directory.join("order")).expect("order"),
        WAITER_FIRST
    );
}

#[test]
fn two_processes_taking_a_busy_byte_in_rounds_take_turns() {
    let (_dir, path) = super::database("brl-turn-two");
    let acquisitions = run_rounds(&path, &[("loop", 0), ("loop", 1)]);
    let expected = usize::try_from(ROUNDS * 2).expect("small");
    assert_eq!(acquisitions.len(), expected);
    assert_eq!(count(&path), expected);
    let passes = most_passes(&acquisitions);
    eprintln!("two processes: one wait was passed at most {passes} times");
    assert!(
        passes <= MOST_PASSES_OF_TWO,
        "a request was passed {passes} times"
    );
}

#[test]
fn more_processes_than_turn_levels_taking_a_busy_byte_all_finish_and_none_starves() {
    let (_dir, path) = super::database("brl-turn-many");
    let children: Vec<(&str, u64)> = (0..=TURN_LEVELS).map(|number| ("loop", number)).collect();
    let acquisitions = run_rounds(&path, &children);
    let expected = usize::try_from(ROUNDS * (TURN_LEVELS + 1)).expect("small");
    assert_eq!(acquisitions.len(), expected);
    assert_eq!(count(&path), expected);
    let passes = most_passes(&acquisitions);
    eprintln!(
        "{} processes: one wait was passed at most {passes} times",
        TURN_LEVELS + 1
    );
    assert!(
        passes <= MOST_PASSES_OF_MANY,
        "a request was passed {passes} times"
    );
}

#[test]
fn a_process_waiting_without_turns_and_processes_taking_turns_exclude_each_other() {
    let (_dir, path) = super::database("brl-turn-legacy");
    let acquisitions = run_rounds(&path, &[("legacy", 0), ("loop", 1), ("loop", 2)]);
    let expected = usize::try_from(ROUNDS * 3).expect("small");
    assert_eq!(acquisitions.len(), expected);
    assert_eq!(count(&path), expected);
}

#[test]
fn a_process_that_dies_while_it_holds_a_turn_lets_the_next_waiter_in() {
    let (_dir, path) = super::database("brl-turn-crash");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let held = file
        .try_lock(byte(), LockMode::Exclusive)
        .expect("lock")
        .expect("free");
    let mut queued = ChildProcesses(vec![Some(spawn(&path, "queue", "queued"))]);
    wait_until_turn_held(&file, &mut queued);
    let mut child = queued.take(0);
    child.kill().expect("kill the waiter");
    child.wait().expect("reap the waiter");
    wait_until_turns_free(&file);
    drop(held);
    let taken = file.lock(byte(), LockMode::Exclusive, Duration::from_secs(1));
    assert_eq!(taken.expect("taken after the crash").byte(), byte());
}

#[test]
fn a_read_only_handle_waits_for_the_lease_without_taking_a_turn() {
    let (_dir, path) = super::database("brl-read-turn");
    let reader = ByteRangeLockFile::open_read_only(&path).expect("open reader");
    let lease = reader
        .lock(LockByte::LEASE, LockMode::Shared, Duration::from_secs(1))
        .expect("lease shared");
    assert_eq!(lease.mode(), LockMode::Shared);
}

/// What a wait costs a writer alone: the three turn bytes it walks, against
/// taking the byte alone. A measurement, run on purpose.
#[test]
#[ignore = "a measurement: run with --run-ignored"]
fn measure_a_wait_nobody_competes_with() {
    let (_dir, path) = super::database("brl-turn-cost");
    let file = ByteRangeLockFile::open(&path).expect("open");
    let with_turns = average(|| file.lock(byte(), LockMode::Exclusive, WAIT).expect("lock"));
    let byte_alone = average(|| {
        file.try_lock(byte(), LockMode::Exclusive)
            .expect("try lock")
            .expect("free")
    });
    eprintln!(
        "a wait nobody competes with: {with_turns:?} with turns, {byte_alone:?} for the byte alone"
    );
    assert!(with_turns < Duration::from_millis(1), "{with_turns:?}");
}

/// The average time `take` and the release of what it took cost.
fn average(take: impl Fn() -> ByteLockGuard) -> Duration {
    let rounds = 10_000;
    let started = Instant::now();
    for _ in 0..rounds {
        drop(take());
    }
    started.elapsed() / rounds
}

/// Start a child per `(role, number)`, let them take the byte in rounds
/// together, require that each passed, and return the acquisitions in the
/// order they happened.
fn run_rounds(database: &Path, children: &[(&str, u64)]) -> Vec<Acquisition> {
    let directory = database.parent().expect("directory");
    let names: Vec<String> = children
        .iter()
        .map(|(role, number)| format!("{role}{number}"))
        .collect();
    let mut running = ChildProcesses(
        children
            .iter()
            .zip(&names)
            .map(|((role, _), name)| Some(spawn(database, role, name)))
            .collect(),
    );
    for name in &names {
        wait_until(&directory.join(format!("{name}.ready")), &mut running);
    }
    fs::write(directory.join("go"), b"").expect("signal go");
    running.all_passed();
    fs::read_to_string(directory.join("log"))
        .expect("log")
        .lines()
        .map(Acquisition::parse)
        .collect()
}

fn count(database: &Path) -> usize {
    let counter = database.parent().expect("directory").join("counter");
    fs::read_to_string(counter)
        .expect("counter")
        .parse()
        .expect("count")
}

/// The most times one request was passed: taken after an acquisition by
/// another child that asked later.
fn most_passes(acquisitions: &[Acquisition]) -> usize {
    acquisitions
        .iter()
        .enumerate()
        .map(|(index, acquisition)| passes(&acquisitions[..index], acquisition))
        .max()
        .unwrap_or(0)
}

fn passes(earlier: &[Acquisition], acquisition: &Acquisition) -> usize {
    earlier
        .iter()
        .filter(|other| other.child != acquisition.child && other.asked_at > acquisition.asked_at)
        .count()
}

fn spawn(database: &Path, role: &str, name: &str) -> Child {
    Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DATABASE, database)
        .env(NAME, name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child")
}

/// Wait for a child and require that it passed, showing what it printed
/// when it did not.
fn passed(child: Child) {
    let output = child.wait_with_output().expect("child output");
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Wait for `path` to exist while `children`, one of which creates it, run.
fn wait_until(path: &Path, children: &mut ChildProcesses) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            children.all_running(),
            "a child stopped before {}",
            path.display()
        );
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Whether another holder, in this process or another, has `turn`.
fn turn_held(file: &ByteRangeLockFile, turn: LockByte) -> bool {
    file.try_lock(turn, LockMode::Exclusive)
        .expect("turn")
        .is_none()
}

/// Wait, up to `limit`, until a waiting thread holds the byte's first
/// turn. Without turns it never does, and the test goes on after `limit`.
fn wait_for_turn(file: &ByteRangeLockFile, limit: Duration) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline && !turn_held(file, byte().turn(1)) {
        thread::sleep(Duration::from_millis(1));
    }
}

/// Wait until the queued child holds the byte's first turn.
fn wait_until_turn_held(file: &ByteRangeLockFile, children: &mut ChildProcesses) {
    let deadline = Instant::now() + WAIT;
    while !turn_held(file, byte().turn(1)) {
        assert!(children.all_running(), "the queued child stopped");
        assert!(Instant::now() < deadline, "the queued child took no turn");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until the OS has let every turn byte of a dead holder go. Windows
/// may take a moment after the process ends.
fn wait_until_turns_free(file: &ByteRangeLockFile) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while (1..=TURN_LEVELS).any(|level| turn_held(file, byte().turn(level))) {
        assert!(Instant::now() < deadline, "a dead waiter kept its turn");
        thread::sleep(Duration::from_millis(5));
    }
}
