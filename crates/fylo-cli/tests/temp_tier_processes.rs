//! The temp-disk read tier across processes (ADR 0030 slice S3): a long
//! `exec --loop` session keeps answering what the root holds while other
//! `fylo` processes change it, and a process stopped while it writes an
//! entry leaves nothing a later read trusts.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

const NOTES: &str = "notes";
const FIRST: &str = "4VRNF52JPCO";
const SECOND: &str = "4VRNF52JPCP";

/// A root and a temp cache base folder for one test, removed when it ends,
/// with the options that pick the root's filesystem profile.
struct Place(PathBuf, &'static [&'static str]);

impl Place {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");
    const FAILPOINT: &'static str = "FYLO_RUST_TEMP_CACHE_FAILPOINT";

    #[cfg(unix)]
    const EFS: &'static [&'static str] = &["--filesystem-profile", "efs-sidecar-v1"];

    fn new() -> Self {
        Self::with_profile(&[])
    }

    #[cfg(unix)]
    fn efs() -> Self {
        Self::with_profile(Self::EFS)
    }

    fn with_profile(profile: &'static [&'static str]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = fylo_test_support::TestFolder::path().join(format!(
            "fylo-temp-tier-processes-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("root")).unwrap();
        fs::create_dir_all(path.join("base")).unwrap();
        Self(path, profile)
    }

    fn root(&self) -> PathBuf {
        self.0.join("root")
    }

    fn base(&self) -> PathBuf {
        self.0.join("base")
    }

    fn command(&self, mode: &[&str]) -> Command {
        let mut command = Command::new(Self::BINARY);
        command
            .arg("exec")
            .args(mode)
            .arg("--root")
            .arg(self.root())
            .args(self.1)
            .args(["--temp-cache", "on", "--temp-cache-dir"])
            .arg(self.base())
            .current_dir(&self.0)
            .env_remove("FYLO_ROOT")
            .env_remove(Self::FAILPOINT);
        command
    }

    /// One request in its own process.
    fn request(&self, frame: &Value) -> Output {
        self.command(&["--request", &frame.to_string()])
            .output()
            .unwrap()
    }

    /// One request in its own process, which must succeed.
    fn succeed(&self, frame: &Value) -> Value {
        let output = self.request(frame);
        assert!(output.status.success(), "{output:?}");
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["ok"], true, "{frame} failed: {reply}");
        reply["result"].clone()
    }

    /// One request in a process stopped at `failpoint`.
    fn stopped_at(&self, failpoint: &str, frame: &Value) -> Output {
        self.command(&["--request", &frame.to_string()])
            .env(Self::FAILPOINT, failpoint)
            .output()
            .unwrap()
    }

    fn session(&self) -> Session {
        let mut child = self
            .command(&["--loop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Session {
            child,
            input,
            output,
        }
    }

    fn files_named(&self, part: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut folders = vec![self.base()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else if name_of(&path).contains(part) {
                    found.push(path);
                }
            }
        }
        found
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A long `exec --loop` process.
struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Session {
    fn ask(&mut self, frame: &Value) -> Value {
        writeln!(self.input, "{frame}").unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["ok"], true, "{frame} failed: {reply}");
        reply["result"].clone()
    }

    fn temp_cache(&mut self) -> Value {
        self.ask(&json!({"op":"handshake"}))["capabilities"]["tempCache"].clone()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn get(id: &str) -> Value {
    json!({"op":"getDoc","collection":NOTES,"id":id})
}

fn seed(place: &Place) {
    place.succeed(&json!({"op":"createCollection","collection":NOTES}));
    place.succeed(&json!({"op":"putData","collection":NOTES,"id":FIRST,"data":{"title":"one"}}));
}

#[test]
fn each_process_answers_what_the_processes_before_it_wrote() {
    check_each_process_answers_what_the_processes_before_it_wrote(&Place::new());
}

#[cfg(unix)]
#[test]
fn each_process_answers_what_the_processes_before_it_wrote_on_efs() {
    check_each_process_answers_what_the_processes_before_it_wrote(&Place::efs());
}

fn check_each_process_answers_what_the_processes_before_it_wrote(place: &Place) {
    seed(place);
    let mut first = place.session();
    assert_eq!(first.ask(&get(FIRST))[FIRST]["title"], "one");
    assert_eq!(first.ask(&get(FIRST))[FIRST]["title"], "one");
    assert_eq!(first.temp_cache()["hits"], 1);
    drop(first);

    place.succeed(
        &json!({"op":"patchDoc","collection":NOTES,"id":FIRST,"newDoc":{"title":"changed"}}),
    );
    place.succeed(&json!({"op":"putData","collection":NOTES,"id":SECOND,"data":{"title":"two"}}));
    let mut second = place.session();
    assert_eq!(second.ask(&get(FIRST))[FIRST]["title"], "changed");
    let found = second.ask(&json!({"op":"findDocs","collection":NOTES,"query":{}}));
    assert_eq!(found.as_object().unwrap().len(), 2, "{found}");
    assert_eq!(second.temp_cache()["rejected"], 1);
    drop(second);

    place.succeed(&json!({"op":"dropCollection","collection":NOTES}));
    place.succeed(&json!({"op":"createCollection","collection":NOTES}));
    let mut third = place.session();
    assert_eq!(third.ask(&get(FIRST)), json!({}));
    assert_eq!(third.ask(&get(SECOND)), json!({}));
    assert_eq!(third.temp_cache()["hits"], 0);
}

fn assert_cut_write_is_ignored(failpoint: &str) {
    let place = Place::new();
    seed(&place);
    let stopped = place.stopped_at(failpoint, &get(FIRST));
    assert!(!stopped.status.success(), "{stopped:?}");
    assert_eq!(place.files_named(".partial-").len(), 1);
    assert!(
        place
            .files_named(".fyc")
            .iter()
            .all(|path| name_of(path).contains(".partial-"))
    );

    let mut session = place.session();
    assert_eq!(session.ask(&get(FIRST))[FIRST]["title"], "one");
    let counters = session.temp_cache();
    assert_eq!(counters["hits"], 0, "{counters}");
    assert_eq!(counters["writes"], 1, "{counters}");
    assert_eq!(session.ask(&get(FIRST))[FIRST]["title"], "one");
    assert_eq!(session.temp_cache()["hits"], 1);
}

#[test]
fn a_process_stopped_in_the_middle_of_an_entry_leaves_only_a_partial_file() {
    assert_cut_write_is_ignored("temp-entry-torn-partial");
}

#[test]
fn a_process_stopped_before_renaming_an_entry_leaves_only_a_partial_file() {
    assert_cut_write_is_ignored("temp-entry-before-rename");
}

/// One root directly below a data disk's mount point (ADR 0033 section
/// 5.1), with its configuration and the temp cache in the OS temp folder,
/// which is on the system disk on purpose (ADR 0030 section 4.2). The drive
/// rule judges only the primary root, so the tier must work.
struct DataDiskRoot {
    folder: PathBuf,
    root: PathBuf,
}

impl DataDiskRoot {
    const LIST: &'static str = "FYLO_TEST_DATA_DISK_ROOTS";

    fn below(parent: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "fylo-temp-tier-data-disk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let folder = std::env::temp_dir().join(&name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("application.json"), br#"{"fylo":{}}"#).unwrap();
        Self {
            folder,
            root: parent.join(name),
        }
    }

    fn command(&self, mode: &[&str]) -> Command {
        let mut command = Command::new(Place::BINARY);
        command
            .arg("exec")
            .args(mode)
            .arg("--root")
            .arg(&self.root)
            .arg("--config")
            .arg(self.folder.join("application.json"))
            .args(["--temp-cache", "on"])
            .current_dir(&self.folder)
            .env_remove("FYLO_ROOT")
            .env_remove("FYLO_APP_ID")
            .env_remove("FYLO_ALLOW_SYSTEM_DISK")
            .env_remove(Place::FAILPOINT);
        command
    }

    fn succeed(&self, frame: &Value) {
        let output = self
            .command(&["--request", &frame.to_string()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["ok"], true, "{frame} failed: {reply}");
    }

    /// The answers to `frames` in one `exec --loop` process, and its stderr.
    fn session(&self, frames: &[Value]) -> (Vec<Value>, String) {
        let mut input = String::new();
        for frame in frames {
            input.push_str(&frame.to_string());
            input.push('\n');
        }
        let mut child = self
            .command(&["--loop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let replies = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (
            replies,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

impl Drop for DataDiskRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_tier_reads_for_a_data_disk_root(parent: &Path) {
    let place = DataDiskRoot::below(parent);
    place.succeed(&json!({"op":"createCollection","collection":NOTES}));
    place.succeed(&json!({"op":"putData","collection":NOTES,"id":FIRST,"data":{"title":"one"}}));
    let frames = [get(FIRST), json!({"op":"handshake"})];
    let (first, first_stderr) = place.session(&frames);
    assert_eq!(first[0]["result"][FIRST]["title"], "one", "{first:?}");
    assert!(
        !first_stderr.contains("system-disk check disabled by"),
        "{first_stderr}"
    );
    let (second, stderr) = place.session(&frames);
    assert_eq!(second[0]["result"][FIRST]["title"], "one", "{second:?}");
    let temp_cache = &second[1]["result"]["capabilities"]["tempCache"];
    let lock = &second[1]["result"]["storage"]["rootLock"];
    assert_eq!(lock["placement"], "dataDisk", "{lock}");
    assert_eq!(lock["systemDisk"], false, "{lock}");
    assert_eq!(temp_cache["active"], true, "{temp_cache}");
    assert_eq!(temp_cache["hits"], 1, "{temp_cache}");
    let cache_folder = PathBuf::from(temp_cache["dir"].as_str().unwrap());
    assert!(
        cache_folder.starts_with(fs::canonicalize(std::env::temp_dir()).unwrap())
            || cache_folder.starts_with(std::env::temp_dir()),
        "{temp_cache}"
    );
    assert!(
        !stderr.contains("system-disk check disabled by"),
        "{stderr}"
    );
}

#[test]
fn the_tier_works_in_the_os_temp_folder_for_a_data_disk_root() {
    let roots = std::env::var_os(DataDiskRoot::LIST).unwrap_or_default();
    for parent in std::env::split_paths(&roots).filter(|path| !path.as_os_str().is_empty()) {
        assert_tier_reads_for_a_data_disk_root(&parent);
    }
}

#[test]
fn invocations_fill_and_answer_from_the_tier() {
    let place = Place::new();
    seed(&place);
    let invocation = [
        "--invocation",
        "--deadline-ms",
        "30000",
        "--cleanup-reserve-ms",
        "2000",
    ];
    let invoke = |frame: &Value| {
        let mut arguments = invocation.to_vec();
        let frame = frame.to_string();
        arguments.extend(["--request", frame.as_str()]);
        let output = place.command(&arguments).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    assert_eq!(invoke(&get(FIRST))["result"][FIRST]["title"], "one");
    assert_eq!(place.files_named(".fyc").len(), 1);
    assert_eq!(invoke(&get(FIRST))["result"][FIRST]["title"], "one");
    let handshake = invoke(&json!({"op":"handshake"}));
    let temp_cache = &handshake["result"]["capabilities"]["tempCache"];
    assert_eq!(temp_cache["active"], true, "{temp_cache}");
}

/// An EFS-profile root on a real network mount, with the default `auto`
/// mode and the OS temp folder: the tier turns itself on, and a second
/// process answers from it.
#[cfg(unix)]
struct NetworkRoot(PathBuf);

#[cfg(unix)]
impl NetworkRoot {
    const LIST: &'static str = "FYLO_TEST_NETWORK_ROOTS";

    fn below(parent: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let folder = parent.join(format!(
            "fylo-temp-tier-network-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(folder.join("root")).unwrap();
        Self(folder)
    }

    fn command(&self, mode: &[&str]) -> Command {
        let mut command = Command::new(Place::BINARY);
        command
            .arg("exec")
            .args(mode)
            .arg("--root")
            .arg(self.0.join("root"))
            .args(Place::EFS)
            .current_dir(&self.0)
            .env_remove("FYLO_ROOT")
            .env_remove(Place::FAILPOINT);
        command
    }

    fn succeed(&self, frame: &Value) {
        let output = self
            .command(&["--request", &frame.to_string()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["ok"], true, "{frame} failed: {reply}");
    }

    fn get_and_handshake(&self) -> Vec<Value> {
        let input = format!("{}\n{}\n", get(FIRST), json!({"op":"handshake"}));
        let mut child = self
            .command(&["--loop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[cfg(unix)]
impl Drop for NetworkRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn assert_auto_reads_through_the_tier(parent: &Path) {
    let place = NetworkRoot::below(parent);
    place.succeed(&json!({"op":"createCollection","collection":NOTES}));
    place.succeed(&json!({"op":"putData","collection":NOTES,"id":FIRST,"data":{"title":"one"}}));
    let first = place.get_and_handshake();
    assert_eq!(first[0]["result"][FIRST]["title"], "one", "{first:?}");
    let second = place.get_and_handshake();
    assert_eq!(second[0]["result"][FIRST]["title"], "one", "{second:?}");
    let temp_cache = &second[1]["result"]["capabilities"]["tempCache"];
    assert_eq!(temp_cache["mode"], "auto", "{temp_cache}");
    assert_eq!(temp_cache["active"], true, "{temp_cache}");
    assert_eq!(temp_cache["hits"], 1, "{temp_cache}");
}

/// Run against real network mounts:
/// `FYLO_TEST_NETWORK_ROOTS=/mnt/nfs cargo nextest run -p fylo-cli --test
/// temp_tier_processes --run-ignored only`. Each folder gets a throwaway
/// subfolder, removed afterwards.
#[cfg(unix)]
#[test]
#[ignore = "needs FYLO_TEST_NETWORK_ROOTS naming folders on network mounts"]
fn auto_reads_an_efs_root_on_a_network_mount_through_the_tier() {
    let roots = std::env::var_os(NetworkRoot::LIST).unwrap_or_default();
    for parent in std::env::split_paths(&roots).filter(|path| !path.as_os_str().is_empty()) {
        assert_auto_reads_through_the_tier(&parent);
    }
}
