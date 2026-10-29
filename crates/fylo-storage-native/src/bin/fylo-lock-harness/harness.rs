//! The harness itself; see `main.rs`.

use fylo_storage_native::{ByteRangeLockFile, LockByte, LockMode};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SLOT_BYTES: u64 = 64;
const SHARED_FLAGS: u64 = 16 * 1024;
const FILE_BYTES: u64 = 32 * 1024;
const MAX_HOLDERS: u64 = FILE_BYTES - SHARED_FLAGS;
const WAIT: Duration = Duration::from_secs(10);
const RELEASE_LIMIT: Duration = Duration::from_mins(1);

pub(crate) fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("qualify") => qualify(&Options::parse(&args[1..])),
        Some("worker") => {
            worker(&Options::parse(&args[1..]));
            Ok(true)
        }
        Some("hold") => hold(&Options::parse(&args[1..])).map(|()| true),
        _ => {
            eprintln!(
                "usage: fylo-lock-harness qualify --dir DIR [--procs N] [--threads N] \
                 [--seconds N] [--writers N] [--sync] [--label NAME] [--json FILE]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("fylo-lock-harness: {error}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::struct_excessive_bools, reason = "each is a command-line flag")]
#[derive(Clone, Debug, Default)]
struct Options {
    dir: Option<PathBuf>,
    file: Option<PathBuf>,
    procs: u64,
    threads: u64,
    seconds: u64,
    writers: u32,
    id: u64,
    sync: bool,
    label: Option<String>,
    json: Option<PathBuf>,
    bytes: Vec<LockByte>,
    mode: Option<LockMode>,
    duplicate_close: bool,
    foreign_close: bool,
    unlocked: bool,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut options = Self {
            procs: 8,
            threads: 2,
            seconds: 10,
            writers: 4,
            ..Self::default()
        };
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let mut value = || args.next().cloned().unwrap_or_default();
            match flag.as_str() {
                "--dir" => options.dir = Some(value().into()),
                "--file" => options.file = Some(value().into()),
                "--procs" => options.procs = number(&value()),
                "--threads" => options.threads = number(&value()),
                "--seconds" => options.seconds = number(&value()),
                "--writers" => {
                    options.writers = u32::try_from(number(&value())).unwrap_or(u32::MAX);
                }
                "--id" => options.id = number(&value()),
                "--sync" => options.sync = true,
                "--label" => options.label = Some(value()),
                "--json" => options.json = Some(value().into()),
                "--byte" => options.bytes.push(parse_byte(&value())),
                "--mode" => options.mode = Some(parse_mode(&value())),
                "--duplicate-close" => options.duplicate_close = true,
                "--foreign-close" => options.foreign_close = true,
                "--unlocked" => options.unlocked = true,
                other => panic!("unknown flag {other}"),
            }
        }
        options
    }

    fn file(&self) -> &Path {
        self.file.as_deref().expect("--file")
    }
}

fn number(value: &str) -> u64 {
    value
        .parse()
        .unwrap_or_else(|_| panic!("not a number: {value}"))
}

fn parse_byte(value: &str) -> LockByte {
    match value {
        "lease" => LockByte::LEASE,
        "allocation" => LockByte::ALLOCATION,
        writer => LockByte::collection_writer(
            writer
                .strip_prefix("writer:")
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("unknown byte {writer}")),
        ),
    }
}

fn byte_flag(byte: LockByte) -> String {
    match byte.to_string().split_once(' ') {
        Some(("writer", n)) => format!("writer:{n}"),
        _ => byte.to_string(),
    }
}

fn parse_mode(value: &str) -> LockMode {
    match value {
        "shared" => LockMode::Shared,
        "exclusive" => LockMode::Exclusive,
        other => panic!("unknown mode {other}"),
    }
}

fn mode_flag(mode: LockMode) -> &'static str {
    match mode {
        LockMode::Shared => "shared",
        LockMode::Exclusive => "exclusive",
    }
}

#[cfg(unix)]
fn read_u64(file: &File, offset: u64) -> io::Result<u64> {
    let mut bytes = [0; 8];
    std::os::unix::fs::FileExt::read_exact_at(file, &mut bytes, offset)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(unix)]
fn write_bytes(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(windows)]
fn read_u64(file: &File, offset: u64) -> io::Result<u64> {
    let mut bytes = [0; 8];
    let mut read = 0;
    while read < bytes.len() {
        let n = std::os::windows::fs::FileExt::seek_read(
            file,
            &mut bytes[read..],
            offset + read as u64,
        )?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        read += n;
    }
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(windows)]
fn write_bytes(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        written += std::os::windows::fs::FileExt::seek_write(
            file,
            &bytes[written..],
            offset + written as u64,
        )?;
    }
    Ok(())
}

fn read_flag(file: &File, holder: u64) -> io::Result<u8> {
    let word = read_u64(file, SHARED_FLAGS + holder / 8 * 8)?;
    Ok(word.to_le_bytes()[usize::try_from(holder % 8).expect("small")])
}

/// A small fast generator; the schedule only needs to differ per thread.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let clock = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::from(d.subsec_nanos()) ^ d.as_secs());
        Self((seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ clock) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

// ---------------------------------------------------------------- qualify ---

struct Report {
    rows: Vec<Value>,
    passed: bool,
}

impl Report {
    fn check(&mut self, name: &str, passed: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!(
            "{:<4} {name:<44} {detail}",
            if passed { "ok" } else { "FAIL" }
        );
        self.passed &= passed;
        self.rows
            .push(json!({ "check": name, "passed": passed, "detail": detail }));
    }

    fn note(&mut self, name: &str, detail: impl Into<String>) {
        let detail = detail.into();
        println!("{:<4} {name:<44} {detail}", "info");
        self.rows
            .push(json!({ "check": name, "passed": Value::Null, "detail": detail }));
    }
}

fn qualify(options: &Options) -> io::Result<bool> {
    let dir = options.dir.as_deref().expect("--dir");
    let path = dir.join(format!("lock-harness-{}.fxdb", std::process::id()));
    std::fs::write(&path, vec![0; usize::try_from(FILE_BYTES).expect("small")])?;
    let mut report = Report {
        rows: Vec::new(),
        passed: true,
    };
    println!(
        "fylo-lock-harness on {} ({} {})",
        path.display(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let started = Instant::now();
    let outcome = run_checks(options, &path, &mut report);
    let _ = std::fs::remove_file(&path);
    if let Err(error) = outcome {
        report.check("harness ran to the end", false, error.to_string());
    }
    let summary = json!({
        "label": options.label,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "dir": dir.display().to_string(),
        "procs": options.procs,
        "threads": options.threads,
        "seconds": options.seconds,
        "writers": options.writers,
        "sync": options.sync,
        "elapsed_ms": started.elapsed().as_millis(),
        "passed": report.passed,
        "checks": report.rows,
    });
    if let Some(json_path) = &options.json {
        std::fs::write(json_path, serde_json::to_vec_pretty(&summary)?)?;
    }
    println!("{}", if report.passed { "PASSED" } else { "FAILED" });
    Ok(report.passed)
}

fn run_checks(options: &Options, path: &Path, report: &mut Report) -> io::Result<()> {
    let file = ByteRangeLockFile::open(path)?;
    beyond_the_end(&file, report)?;
    conflict_matrix(&file, path, report)?;
    release_on_exit(&file, path, report)?;
    close_safety(&file, path, report)?;
    load(options, path, report)
}

fn beyond_the_end(file: &ByteRangeLockFile, report: &mut Report) -> io::Result<()> {
    let guards = [
        file.try_lock(LockByte::LEASE, LockMode::Shared)?,
        file.try_lock(LockByte::ALLOCATION, LockMode::Exclusive)?,
        file.try_lock(LockByte::collection_writer(0), LockMode::Exclusive)?,
        file.try_lock(LockByte::collection_writer(u32::MAX), LockMode::Exclusive)?,
    ];
    let all = guards.iter().all(Option::is_some);
    write_bytes(file.file(), b"fxdb", 0)?;
    let length = file.file().metadata()?.len();
    drop(guards);
    write_bytes(file.file(), &[0; 4], 0)?;
    report.check(
        "locks at 2^62+ beyond EOF",
        all && length == FILE_BYTES,
        format!("all granted: {all}, length after: {length} (expected {FILE_BYTES})"),
    );
    Ok(())
}

/// A child process holding bytes until its stdin closes.
struct Holder {
    child: Child,
}

impl Holder {
    fn start(path: &Path, bytes: &[LockByte], mode: LockMode, extra: &[&str]) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("hold").arg("--file").arg(path);
        command.args(["--mode", mode_flag(mode)]);
        for &byte in bytes {
            command.args(["--byte", &byte_flag(byte)]);
        }
        command.args(extra);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().expect("piped")).read_line(&mut line)?;
        if line.trim() != "held" {
            let _ = child.kill();
            return Err(io::Error::other(format!("holder said {line:?}")));
        }
        Ok(Self { child })
    }

    /// Close stdin and wait for a normal exit.
    fn finish(mut self) -> io::Result<()> {
        drop(self.child.stdin.take());
        let status = self.child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("holder exited with {status}")))
        }
    }
}

fn conflicts(file: &ByteRangeLockFile, byte: LockByte, mode: LockMode) -> io::Result<bool> {
    Ok(file.try_lock(byte, mode)?.is_none())
}

fn conflict_matrix(file: &ByteRangeLockFile, path: &Path, report: &mut Report) -> io::Result<()> {
    use LockMode::{Exclusive, Shared};
    let holder = Holder::start(path, &[LockByte::LEASE], Shared, &[])?;
    let shared = !conflicts(file, LockByte::LEASE, Shared)?;
    let exclusive = conflicts(file, LockByte::LEASE, Exclusive)?;
    holder.finish()?;
    report.check(
        "other process holds lease shared",
        shared && exclusive,
        format!("shared granted: {shared}, exclusive refused: {exclusive}"),
    );

    let holder = Holder::start(path, &[LockByte::LEASE], Exclusive, &[])?;
    let shared = conflicts(file, LockByte::LEASE, Shared)?;
    let exclusive = conflicts(file, LockByte::LEASE, Exclusive)?;
    holder.finish()?;
    report.check(
        "other process holds lease exclusive",
        shared && exclusive,
        format!("shared refused: {shared}, exclusive refused: {exclusive}"),
    );

    let holder = Holder::start(
        path,
        &[LockByte::ALLOCATION, LockByte::collection_writer(5)],
        Exclusive,
        &[],
    )?;
    let allocation = conflicts(file, LockByte::ALLOCATION, Exclusive)?;
    let writer = conflicts(file, LockByte::collection_writer(5), Exclusive)?;
    let neighbour = !conflicts(file, LockByte::collection_writer(6), Exclusive)?;
    let lease = !conflicts(file, LockByte::LEASE, Exclusive)?;
    holder.finish()?;
    report.check(
        "other process holds allocation + writer 5",
        allocation && writer && neighbour && lease,
        format!(
            "allocation refused: {allocation}, writer 5 refused: {writer}, \
             writer 6 granted: {neighbour}, lease granted: {lease}"
        ),
    );

    // A waiting request is granted once the other process releases.
    let holder = Holder::start(path, &[LockByte::collection_writer(1)], Exclusive, &[])?;
    let started = Instant::now();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        holder.finish()
    });
    let waited = file.lock(LockByte::collection_writer(1), Exclusive, WAIT);
    let elapsed = started.elapsed();
    releaser.join().expect("releaser")?;
    report.check(
        "waiting lock granted after release",
        waited.is_ok() && elapsed >= Duration::from_millis(100),
        match waited {
            Ok(guard) => format!("{} granted after {elapsed:.1?}", guard.byte()),
            Err(error) => format!("{error} after {elapsed:.1?}"),
        },
    );
    Ok(())
}

/// Poll until every byte is free, returning how long it took.
fn time_release(file: &ByteRangeLockFile, bytes: &[LockByte]) -> io::Result<Option<Duration>> {
    let started = Instant::now();
    while started.elapsed() < RELEASE_LIMIT {
        let mut free = true;
        for &byte in bytes {
            free &= file.try_lock(byte, LockMode::Exclusive)?.is_some();
        }
        if free {
            return Ok(Some(started.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(None)
}

fn shown(duration: Option<Duration>) -> String {
    duration.map_or_else(
        || format!("never (gave up after {RELEASE_LIMIT:?})"),
        |duration| format!("{duration:.1?}"),
    )
}

fn release_on_exit(file: &ByteRangeLockFile, path: &Path, report: &mut Report) -> io::Result<()> {
    let bytes = [
        LockByte::LEASE,
        LockByte::ALLOCATION,
        LockByte::collection_writer(0),
    ];
    let holder = Holder::start(path, &bytes, LockMode::Exclusive, &[])?;
    let held = conflicts(file, LockByte::LEASE, LockMode::Exclusive)?;
    holder.finish()?;
    let released = time_release(file, &bytes)?;
    report.check(
        "holder exits: locks released",
        held && released.is_some(),
        format!("held before: {held}, free {} after exit", shown(released)),
    );

    let mut holder = Holder::start(path, &bytes, LockMode::Exclusive, &[])?;
    let held = conflicts(file, LockByte::LEASE, LockMode::Exclusive)?;
    let killed = Instant::now();
    holder.child.kill()?;
    let released = time_release(file, &bytes)?;
    let after_kill = killed.elapsed();
    holder.child.wait()?;
    report.check(
        "holder killed: locks released",
        held && released.is_some(),
        format!(
            "held before: {held}, free {} after the kill",
            shown(released.map(|_| after_kill))
        ),
    );
    Ok(())
}

fn close_safety(file: &ByteRangeLockFile, path: &Path, report: &mut Report) -> io::Result<()> {
    let holder = Holder::start(
        path,
        &[LockByte::LEASE],
        LockMode::Exclusive,
        &["--duplicate-close"],
    )?;
    let held = conflicts(file, LockByte::LEASE, LockMode::Exclusive)?;
    holder.finish()?;
    report.check(
        "holder closes a second lock handle",
        held,
        format!("lease still held: {held}"),
    );

    let holder = Holder::start(
        path,
        &[LockByte::LEASE],
        LockMode::Exclusive,
        &["--foreign-close"],
    )?;
    let held = conflicts(file, LockByte::LEASE, LockMode::Exclusive)?;
    holder.finish()?;
    report.note(
        "holder opens+closes the path with std::fs",
        format!(
            "lease still held: {held} (false where fcntl locks belong to the process, as on macOS; \
             do all I/O through the lock handle)"
        ),
    );
    Ok(())
}

fn hold(options: &Options) -> io::Result<()> {
    let file = ByteRangeLockFile::open(options.file())?;
    let mode = options.mode.unwrap_or(LockMode::Exclusive);
    let mut guards = Vec::new();
    for &byte in &options.bytes {
        guards.push(file.lock(byte, mode, WAIT)?);
    }
    if options.duplicate_close {
        drop(ByteRangeLockFile::open(options.file())?);
    }
    if options.foreign_close {
        drop(File::open(options.file())?);
    }
    println!("held");
    io::stdout().flush()?;
    // Hold until the harness closes stdin, or kills this process.
    let _ = io::stdin().read_to_end(&mut Vec::new());
    drop(guards);
    Ok(())
}

// ------------------------------------------------------------------- load ---

/// The witness slots in the file's data: lease (exclusive), allocation, and
/// one per writer byte.
fn slots(writers: u32) -> Vec<LockByte> {
    let mut slots = vec![LockByte::LEASE, LockByte::ALLOCATION];
    slots.extend((0..writers).map(LockByte::collection_writer));
    slots
}

fn load(options: &Options, path: &Path, report: &mut Report) -> io::Result<()> {
    let holders = options.procs * options.threads;
    if holders == 0 || holders > MAX_HOLDERS {
        return Err(io::Error::other(format!(
            "procs × threads must be 1..={MAX_HOLDERS}"
        )));
    }
    // A control run with no locks shows the witness catches an overlap here.
    let control = run_load(options, path, CONTROL_SECONDS, true)?;
    report.check(
        "control: unlocked holders are caught",
        control.overlaps() > 0,
        format!(
            "{CONTROL_SECONDS}s without locks: {} overlaps, {} lost updates",
            control.totals.violations,
            control.lost.len()
        ),
    );
    write_bytes(
        ByteRangeLockFile::open(path)?.file(),
        &vec![0; usize::try_from(FILE_BYTES).expect("small")],
        0,
    )?;

    let run = run_load(options, path, options.seconds, false)?;
    let totals = &run.totals;
    let exclusive: u64 = totals.acquired.iter().sum();
    let seconds = options.seconds.max(1);
    report.check(
        "load: mutual exclusion",
        run.failed_workers == 0 && totals.violations == 0 && totals.errors.is_empty(),
        format!(
            "{} processes × {} threads for {}s, overlaps seen: {}, failed workers: {}, errors: {:?}",
            options.procs,
            options.threads,
            options.seconds,
            totals.violations,
            run.failed_workers,
            totals.errors
        ),
    );
    report.check(
        "load: no lost updates",
        run.lost.is_empty(),
        if run.lost.is_empty() {
            format!("{exclusive} exclusive entries all counted in the file")
        } else {
            run.lost.join("; ")
        },
    );
    report.note(
        "load: throughput",
        format!(
            "{} exclusive/s, {} shared lease/s, {} refused try-locks, {} timeouts, longest wait {} ms",
            exclusive / seconds,
            totals.shared / seconds,
            totals.contended,
            totals.timeouts,
            totals.max_wait_us / 1000
        ),
    );
    Ok(())
}

const CONTROL_SECONDS: u64 = 2;

struct LoadRun {
    totals: Totals,
    lost: Vec<String>,
    failed_workers: u32,
}

impl LoadRun {
    fn overlaps(&self) -> u64 {
        self.totals.violations + self.lost.len() as u64
    }
}

fn run_load(options: &Options, path: &Path, seconds: u64, unlocked: bool) -> io::Result<LoadRun> {
    let mut children = Vec::new();
    for id in 0..options.procs {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("worker").arg("--file").arg(path);
        for (flag, value) in [
            ("--id", id),
            ("--procs", options.procs),
            ("--threads", options.threads),
            ("--seconds", seconds),
            ("--writers", u64::from(options.writers)),
        ] {
            command.args([flag, &value.to_string()]);
        }
        if options.sync {
            command.arg("--sync");
        }
        if unlocked {
            command.arg("--unlocked");
        }
        children.push(command.stdout(Stdio::piped()).spawn()?);
    }
    let mut totals = Totals::new(options.writers);
    let mut failed_workers = 0;
    for mut child in children {
        let stdout: ChildStdout = child.stdout.take().expect("piped");
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line)?;
        let status = child.wait()?;
        match serde_json::from_str::<Value>(&line) {
            Ok(value) if status.success() => totals.add(&value),
            _ => failed_workers += 1,
        }
    }
    let file = ByteRangeLockFile::open(path)?;
    let mut lost = Vec::new();
    for (index, byte) in slots(options.writers).iter().enumerate() {
        let counted = read_u64(file.file(), SLOT_BYTES * index as u64 + 8)?;
        if counted != totals.acquired[index] {
            lost.push(format!(
                "{byte}: file counted {counted}, holders {}",
                totals.acquired[index]
            ));
        }
    }
    Ok(LoadRun {
        totals,
        lost,
        failed_workers,
    })
}

struct Totals {
    acquired: Vec<u64>,
    shared: u64,
    violations: u64,
    contended: u64,
    timeouts: u64,
    max_wait_us: u64,
    errors: Vec<String>,
}

impl Totals {
    fn new(writers: u32) -> Self {
        Self {
            acquired: vec![0; slots(writers).len()],
            shared: 0,
            violations: 0,
            contended: 0,
            timeouts: 0,
            max_wait_us: 0,
            errors: Vec::new(),
        }
    }

    fn add(&mut self, value: &Value) {
        let field = |name: &str| value[name].as_u64().unwrap_or(0);
        if let Some(acquired) = value["acquired"].as_array() {
            for (total, count) in self.acquired.iter_mut().zip(acquired) {
                *total += count.as_u64().unwrap_or(0);
            }
        }
        self.shared += field("shared");
        self.violations += field("violations");
        self.contended += field("contended");
        self.timeouts += field("timeouts");
        self.max_wait_us = self.max_wait_us.max(field("max_wait_us"));
        if let Some(errors) = value["errors"].as_array() {
            self.errors
                .extend(errors.iter().filter_map(Value::as_str).map(String::from));
        }
    }
}

#[derive(Default)]
struct Counters {
    acquired: Vec<AtomicU64>,
    shared: AtomicU64,
    violations: AtomicU64,
    contended: AtomicU64,
    timeouts: AtomicU64,
    max_wait_us: AtomicU64,
}

fn worker(options: &Options) {
    let counters = Counters {
        acquired: (0..slots(options.writers).len())
            .map(|_| AtomicU64::new(0))
            .collect(),
        ..Counters::default()
    };
    let deadline = Instant::now() + Duration::from_secs(options.seconds);
    let errors: Vec<String> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..options.threads)
            .map(|thread| {
                let holder = options.id * options.threads + thread;
                let counters = &counters;
                scope.spawn(move || {
                    worker_thread(options, holder, deadline, counters).map_err(|e| e.to_string())
                })
            })
            .collect();
        threads
            .into_iter()
            .filter_map(|thread| thread.join().expect("worker thread").err())
            .collect()
    });
    let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    let line = json!({
        "acquired": counters.acquired.iter().map(load).collect::<Vec<_>>(),
        "shared": load(&counters.shared),
        "violations": load(&counters.violations),
        "contended": load(&counters.contended),
        "timeouts": load(&counters.timeouts),
        "max_wait_us": load(&counters.max_wait_us),
        "errors": errors,
    });
    println!("{line}");
}

fn worker_thread(
    options: &Options,
    holder: u64,
    deadline: Instant,
    counters: &Counters,
) -> io::Result<()> {
    // Each thread opens the file itself, as separate engine handles would.
    let lock_file = ByteRangeLockFile::open(options.file())?;
    let file = lock_file.file();
    let token = holder + 1;
    let slots = slots(options.writers);
    let mut rng = Rng::new(token);
    while Instant::now() < deadline {
        let roll = rng.below(100);
        let (slot, mode) = match roll {
            0..=29 => (0, LockMode::Shared),
            30..=34 => (0, LockMode::Exclusive),
            35..=49 => (1, LockMode::Exclusive),
            _ if options.writers == 0 => (1, LockMode::Exclusive),
            _ => (
                2 + usize::try_from(rng.below(u64::from(options.writers))).expect("small"),
                LockMode::Exclusive,
            ),
        };
        let byte = slots[slot];
        let asked = Instant::now();
        let guard = if options.unlocked {
            None
        } else if rng.below(4) == 0 {
            if let Some(guard) = lock_file.try_lock(byte, mode)? {
                Some(guard)
            } else {
                counters.contended.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        } else {
            match lock_file.lock(byte, mode, WAIT) {
                Ok(guard) => Some(guard),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    counters.timeouts.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let waited = u64::try_from(asked.elapsed().as_micros()).unwrap_or(u64::MAX);
        counters.max_wait_us.fetch_max(waited, Ordering::Relaxed);
        let hold_for = Duration::from_micros(rng.below(200));
        let overlaps = match mode {
            LockMode::Shared => shared_section(file, holder, hold_for)?,
            LockMode::Exclusive => {
                exclusive_section(file, options, slot, token, slot == 0, hold_for)?
            }
        };
        counters.violations.fetch_add(overlaps, Ordering::Relaxed);
        match mode {
            LockMode::Shared => counters.shared.fetch_add(1, Ordering::Relaxed),
            LockMode::Exclusive => counters.acquired[slot].fetch_add(1, Ordering::Relaxed),
        };
        if options.sync {
            file.sync_data()?;
        }
        if let Some(guard) = guard {
            guard.unlock()?;
        }
    }
    Ok(())
}

/// Holding the lease shared: no exclusive owner may appear meanwhile.
fn shared_section(file: &File, holder: u64, hold_for: Duration) -> io::Result<u64> {
    let mut overlaps = u64::from(read_u64(file, 0)? != 0);
    write_bytes(file, &[1], SHARED_FLAGS + holder)?;
    std::thread::sleep(hold_for);
    overlaps += u64::from(read_u64(file, 0)? != 0);
    write_bytes(file, &[0], SHARED_FLAGS + holder)?;
    Ok(overlaps)
}

/// Holding a byte exclusively: the owner word must be free on entry and still
/// ours on exit, and for the lease no shared holder may be inside.
fn exclusive_section(
    file: &File,
    options: &Options,
    slot: usize,
    token: u64,
    lease: bool,
    hold_for: Duration,
) -> io::Result<u64> {
    let base = SLOT_BYTES * slot as u64;
    let holders = options.procs * options.threads;
    let shared_inside = |file: &File| -> io::Result<u64> {
        let mut inside = 0;
        if lease {
            for holder in 0..holders {
                inside += u64::from(read_flag(file, holder)? != 0);
            }
        }
        Ok(inside)
    };
    let mut overlaps = u64::from(read_u64(file, base)? != 0) + shared_inside(file)?;
    write_bytes(file, &token.to_le_bytes(), base)?;
    std::thread::sleep(hold_for);
    overlaps += u64::from(read_u64(file, base)? != token) + shared_inside(file)?;
    let count = read_u64(file, base + 8)?;
    write_bytes(file, &(count + 1).to_le_bytes(), base + 8)?;
    write_bytes(file, &0_u64.to_le_bytes(), base)?;
    Ok(overlaps)
}
