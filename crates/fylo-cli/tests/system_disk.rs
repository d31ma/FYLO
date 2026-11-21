//! ADR 0032: a server root lives on a disk that is not the system disk.
//! ADR 0033 section 5 adds that the root sits directly below a mount point
//! (`ENOTVOLUMEROOT`); that rule ships switched off until slice L1-5
//! (`VolumeRootRule::ENFORCED`), so a root in the OS temp folder is refused
//! with `ESYSTEMDISK`. These tests start `fylo` with the opt-out cleared,
//! because the repository's `.cargo/config.toml` (and CI, and the nightly
//! run) turn it on for every test process.

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// A throwaway folder in the OS temp folder, which is on the system disk.
struct Place(PathBuf);

impl Place {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");
    const ALLOW: &'static str = "FYLO_ALLOW_SYSTEM_DISK";
    const APP_ID: &'static str = "FYLO_APP_ID";
    const SYSTEM_DISK: &'static str = "ESYSTEMDISK";
    const NOT_VOLUME_ROOT: &'static str = "ENOTVOLUMEROOT";
    const REMOTE_ROOT: &'static str = "EREMOTEROOT";
    const NETWORK_ROOTS: &'static str = "FYLO_TEST_NETWORK_ROOTS";
    const DATA_DISK_ROOTS: &'static str = "FYLO_TEST_DATA_DISK_ROOTS";
    const APP: &'static str = "com.fylo.systemdisktest";
    const WARNING: &'static str = "FYLO warning: system-disk check disabled by";

    fn under(base: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = base.join(format!(
            "fylo-system-disk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn temp() -> Self {
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

    /// An `application.json` with no placement settings, so a stray file in
    /// the working folder cannot turn the opt-out on.
    fn config(&self, settings: &Value) -> PathBuf {
        let path = self.path("application.json");
        fs::write(&path, json!({ "fylo": settings }).to_string()).unwrap();
        path
    }

    /// `fylo` with the opt-out and app mode cleared from its environment.
    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(Self::BINARY);
        command
            .args(arguments)
            .arg("--config")
            .arg(self.config(&json!({})))
            .env_remove(Self::ALLOW)
            .env_remove(Self::APP_ID)
            .env_remove("FYLO_ROOT");
        command
    }

    /// The frames and stderr of `exec --loop` answering `requests`.
    fn exec_loop(mut command: Command, requests: &[Value]) -> (Vec<Value>, String) {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let output = child.wait_with_output().unwrap();
        (frames, String::from_utf8_lossy(&output.stderr).into_owned())
    }

    fn loop_command(&self, root: &Path, flags: &[&str]) -> Command {
        let mut command = self.command(&["exec", "--loop"]);
        command.arg("--root").arg(root).args(flags);
        command
    }

    fn request(&self, root: &Path, request: &Value) -> Output {
        self.command(&["exec", "--request", &request.to_string()])
            .arg("--root")
            .arg(root)
            .output()
            .unwrap()
    }

    fn invoke(&self, root: &Path, request: &Value) -> Output {
        self.command(&["exec", "--invocation", "--deadline-ms", "10000"])
            .args([
                "--cleanup-reserve-ms",
                "200",
                "--request",
                &request.to_string(),
            ])
            .arg("--root")
            .arg(root)
            .output()
            .unwrap()
    }

    fn frame(output: &Output) -> Value {
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim_end()).unwrap()
    }

    fn error_code(frame: &Value) -> &str {
        frame["error"]["code"].as_str().unwrap_or("")
    }

    fn lock(frame: &Value) -> &Value {
        &frame["result"]["storage"]["rootLock"]
    }

    fn handshake() -> Value {
        json!({"op":"handshake"})
    }

    fn create_collection() -> Value {
        json!({"op":"createCollection","collection":"records"})
    }

    /// `path` as the handshake's `storage.volume` names it: without the
    /// Windows `\\?\` prefix that `fs::canonicalize` adds.
    fn volume_name(path: &Path) -> String {
        let text = path.to_string_lossy();
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }

    fn listed(variable: &str) -> Vec<PathBuf> {
        std::env::var_os(variable)
            .map(|list| std::env::split_paths(&list).collect())
            .unwrap_or_default()
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_test_process_itself_has_the_opt_out_on() {
    assert_eq!(
        std::env::var(Place::ALLOW).as_deref(),
        Ok("1"),
        "the repository's .cargo/config.toml turns the opt-out on for tests"
    );
}

#[test]
fn a_plain_loop_answers_the_handshake_and_refuses_the_first_request() {
    let place = Place::temp();
    let root = place.path("root");
    let (frames, _) = Place::exec_loop(
        place.loop_command(&root, &[]),
        &[Place::handshake(), Place::create_collection()],
    );
    let lock = Place::lock(&frames[0]);
    assert_eq!(frames[0]["ok"], true, "{}", frames[0]);
    assert_eq!(lock["placement"], Value::Null, "{lock}");
    assert!(
        frames[0]["result"]["storage"]["volume"].is_string(),
        "{}",
        frames[0]
    );
    assert_eq!(
        Place::error_code(&frames[1]),
        Place::SYSTEM_DISK,
        "{}",
        frames[1]
    );
    let message = frames[1]["error"]["message"].as_str().unwrap();
    assert!(message.contains("--allow-system-disk"), "{message}");
    assert!(message.contains("FYLO_ALLOW_SYSTEM_DISK=1"), "{message}");
    assert!(message.contains("fylo.allowSystemDisk"), "{message}");
    assert_eq!(place.entries(), vec![OsString::from("application.json")]);
}

#[test]
fn an_owned_loop_refuses_in_its_first_frame() {
    let place = Place::temp();
    let (frames, _) = Place::exec_loop(
        place.loop_command(&place.path("owned"), &["--exclusive-root"]),
        &[Place::handshake()],
    );
    assert_eq!(
        Place::error_code(&frames[0]),
        Place::SYSTEM_DISK,
        "{}",
        frames[0]
    );
    assert_eq!(place.entries(), vec![OsString::from("application.json")]);
}

#[test]
fn a_one_shot_handshake_answers_and_a_one_shot_write_is_refused() {
    let place = Place::temp();
    let root = place.path("root");
    let handshake = Place::frame(&place.request(&root, &Place::handshake()));
    assert_eq!(handshake["ok"], true, "{handshake}");
    assert_eq!(Place::lock(&handshake)["placement"], Value::Null);
    let write = Place::frame(&place.request(&root, &Place::create_collection()));
    assert_eq!(Place::error_code(&write), Place::SYSTEM_DISK, "{write}");
    assert_eq!(place.entries(), vec![OsString::from("application.json")]);
}

#[test]
fn an_invocation_is_refused_in_its_frame() {
    let place = Place::temp();
    let root = place.path("invoked");
    fs::create_dir(&root).unwrap();
    let output = place.invoke(&root, &Place::create_collection());
    let frame = Place::frame(&output);
    assert_eq!(Place::error_code(&frame), Place::SYSTEM_DISK, "{frame}");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn the_flag_admits_the_root_and_warns_on_stderr() {
    let place = Place::temp();
    let root = place.path("root");
    let (frames, stderr) = Place::exec_loop(
        place.loop_command(&root, &["--allow-system-disk"]),
        &[Place::handshake(), Place::create_collection()],
    );
    assert_eq!(Place::lock(&frames[0])["placement"], "systemDiskAllowed");
    assert_eq!(Place::lock(&frames[0])["systemDisk"], Value::Null);
    assert_eq!(frames[1]["ok"], true, "{}", frames[1]);
    assert!(
        stderr
            .contains("FYLO warning: system-disk check disabled by --allow-system-disk (ADR 0032)"),
        "{stderr}"
    );
    assert_eq!(stderr.matches(Place::WARNING).count(), 1, "{stderr}");
}

#[test]
fn the_environment_variable_admits_the_root_and_warns_on_stderr() {
    let place = Place::temp();
    let root = place.path("root");
    let mut command = place.loop_command(&root, &[]);
    command.env(Place::ALLOW, "1");
    let (frames, stderr) = Place::exec_loop(command, &[Place::create_collection()]);
    assert_eq!(frames[0]["ok"], true, "{}", frames[0]);
    assert!(
        stderr.contains("disabled by FYLO_ALLOW_SYSTEM_DISK=1"),
        "{stderr}"
    );
}

#[test]
fn the_config_key_admits_the_root_and_warns_on_stderr() {
    let place = Place::temp();
    let root = place.path("root");
    let config = place.config(&json!({"allowSystemDisk": true}));
    let mut command = Command::new(Place::BINARY);
    command
        .args(["exec", "--loop", "--root"])
        .arg(&root)
        .arg("--config")
        .arg(config)
        .env_remove(Place::ALLOW)
        .env_remove(Place::APP_ID);
    let (frames, stderr) = Place::exec_loop(command, &[Place::create_collection()]);
    assert_eq!(frames[0]["ok"], true, "{}", frames[0]);
    assert!(
        stderr.contains("disabled by fylo.allowSystemDisk"),
        "{stderr}"
    );
}

#[test]
fn the_warning_is_never_written_to_standard_output() {
    let place = Place::temp();
    let mut command =
        place.command(&["exec", "--request", &Place::create_collection().to_string()]);
    let output = command
        .arg("--root")
        .arg(place.path("root"))
        .arg("--allow-system-disk")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(Place::WARNING));
    assert!(String::from_utf8_lossy(&output.stderr).contains(Place::WARNING));
}

#[test]
fn the_refusal_holds_when_the_parent_has_the_opt_out_on() {
    assert_eq!(std::env::var(Place::ALLOW).as_deref(), Ok("1"));
    let place = Place::temp();
    let output = place.request(&place.path("root"), &Place::create_collection());
    let frame = Place::frame(&output);
    assert_eq!(Place::error_code(&frame), Place::SYSTEM_DISK, "{frame}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(Place::WARNING));
}

#[test]
fn an_invalid_opt_out_value_stops_the_command() {
    let place = Place::temp();
    let mut command = place.command(&["exec", "--request", &Place::handshake().to_string()]);
    let output = command.env(Place::ALLOW, "yes").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FYLO_ALLOW_SYSTEM_DISK must be 1, true, 0 or false"),
        "{stderr}"
    );
}

#[test]
fn an_app_id_that_escapes_its_folder_stops_the_command() {
    let place = Place::temp();
    let output = place
        .command(&[
            "exec",
            "--request",
            &Place::handshake().to_string(),
            "--app-id",
            "../x",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("app id \"../x\""), "{stderr}");
}

/// The app's data folder on this OS, below `home` where the OS allows a
/// test to choose it, and the variables that point the child there.
#[cfg(target_os = "macos")]
fn app_folder(home: &Path) -> (PathBuf, Vec<(&'static str, PathBuf)>) {
    let folder = home.join("Library/Application Support").join(Place::APP);
    (folder, vec![("HOME", home.to_path_buf())])
}

#[cfg(all(unix, not(target_os = "macos")))]
fn app_folder(home: &Path) -> (PathBuf, Vec<(&'static str, PathBuf)>) {
    let data = home.join("xdg");
    (data.join(Place::APP), vec![("XDG_DATA_HOME", data)])
}

#[cfg(windows)]
fn app_folder(_home: &Path) -> (PathBuf, Vec<(&'static str, PathBuf)>) {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap());
    (
        local.join(format!("{}-{}", Place::APP, std::process::id())),
        Vec::new(),
    )
}

#[test]
fn app_mode_admits_a_root_in_the_app_folder_only() {
    let place = Place::temp();
    let (folder, variables) = app_folder(&place.0);
    let app = folder.file_name().unwrap().to_string_lossy().into_owned();
    fs::create_dir_all(&folder).unwrap();
    let inside = folder.join("root");
    let mut command = place.loop_command(&inside, &["--app-id", &app]);
    command.envs(variables.iter().map(|(name, value)| (*name, value)));
    let (frames, _) = Place::exec_loop(command, &[Place::handshake(), Place::create_collection()]);
    assert_eq!(
        Place::lock(&frames[0])["placement"],
        "appOwned",
        "{}",
        frames[0]
    );
    assert_eq!(frames[1]["ok"], true, "{}", frames[1]);
    let beside = place.path("beside");
    let mut command = place.loop_command(&beside, &["--app-id", &app]);
    command.envs(variables.iter().map(|(name, value)| (*name, value)));
    let (frames, _) = Place::exec_loop(command, &[Place::create_collection()]);
    assert_eq!(
        Place::error_code(&frames[0]),
        Place::SYSTEM_DISK,
        "{}",
        frames[0]
    );
    let message = frames[0]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("is outside the folder of app {app}")),
        "{message}"
    );
    let _ = fs::remove_dir_all(&folder);
}

/// Opt-in, like ADR 0029's network tests: every folder in
/// `FYLO_TEST_NETWORK_ROOTS` is on a network share. Neither the opt-out
/// nor app mode admits a root there.
#[test]
fn network_roots_are_never_admitted_by_the_opt_out_or_app_mode() {
    for base in Place::listed(Place::NETWORK_ROOTS) {
        let place = Place::under(&base);
        let root = place.path("root");
        let (frames, _) = Place::exec_loop(
            place.loop_command(&root, &["--allow-system-disk"]),
            &[Place::create_collection()],
        );
        assert_eq!(
            Place::error_code(&frames[0]),
            Place::REMOTE_ROOT,
            "{}",
            frames[0]
        );
        let (folder, variables) = app_folder(&place.0);
        fs::create_dir_all(&folder).unwrap();
        let app = folder.file_name().unwrap().to_string_lossy().into_owned();
        let mut command = place.loop_command(&folder.join("root"), &["--app-id", &app]);
        command.envs(variables.iter().map(|(name, value)| (*name, value)));
        let (frames, _) = Place::exec_loop(command, &[Place::create_collection()]);
        assert_eq!(
            Place::error_code(&frames[0]),
            Place::REMOTE_ROOT,
            "{}",
            frames[0]
        );
    }
}

/// Opt-in: every folder in `FYLO_TEST_DATA_DISK_ROOTS` is the mount point
/// of a local disk that is not the system disk (on this Mac: the FYLO test
/// image on HANGAR). A root directly below it is admitted as a data disk,
/// and the handshake names the mount point.
#[test]
fn data_disk_roots_are_admitted_and_report_their_volume() {
    for base in Place::listed(Place::DATA_DISK_ROOTS) {
        let mount = fs::canonicalize(&base).unwrap();
        let top = Place::under(&mount);
        let place = Place::temp();
        let (frames, stderr) = Place::exec_loop(
            place.loop_command(&top.0, &[]),
            &[Place::handshake(), Place::create_collection()],
        );
        let lock = Place::lock(&frames[0]);
        assert_eq!(lock["placement"], "dataDisk", "{} {lock}", base.display());
        assert_eq!(lock["systemDisk"], false, "{lock}");
        assert_eq!(
            frames[0]["result"]["storage"]["volume"],
            Place::volume_name(&mount),
            "{}",
            frames[0]
        );
        assert_eq!(frames[1]["ok"], true, "{} {}", base.display(), frames[1]);
        assert!(!stderr.contains(Place::WARNING), "{stderr}");
    }
}

/// The mount-point rule once `VolumeRootRule::ENFORCED` is on (slice L1-5
/// turns it on and runs this test): a root in the OS temp folder, and a
/// root one folder below a mount point in `FYLO_TEST_DATA_DISK_ROOTS`, are
/// refused with `ENOTVOLUMEROOT`, which names the nearest mount point.
#[test]
#[ignore = "the mount-point rule is off until slice L1-5 (VolumeRootRule::ENFORCED)"]
fn the_enforced_mount_point_rule_refuses_roots_below_a_folder() {
    let place = Place::temp();
    let output = place.request(&place.path("root"), &Place::create_collection());
    let frame = Place::frame(&output);
    assert_eq!(Place::error_code(&frame), Place::NOT_VOLUME_ROOT, "{frame}");
    let message = frame["error"]["message"].as_str().unwrap();
    assert!(message.contains("is not a mount point"), "{message}");
    for base in Place::listed(Place::DATA_DISK_ROOTS) {
        let mount = fs::canonicalize(&base).unwrap();
        let deeper = Place::under(&mount);
        let frame = Place::frame(&place.request(&deeper.path("root"), &Place::create_collection()));
        assert_eq!(Place::error_code(&frame), Place::NOT_VOLUME_ROOT, "{frame}");
        let message = frame["error"]["message"].as_str().unwrap();
        let nearest = format!("the nearest mount point above it is {}", mount.display());
        assert!(message.contains(&nearest), "{message}");
    }
}

/// A root directly below the mount point of a disk image whose file is in
/// the temp folder (the system disk) is refused with `ESYSTEMDISK`, and the
/// handshake names the mount point; a fake `hdiutil` first in `PATH`, which
/// would answer that nothing is attached, is ignored. A root one folder
/// deeper is refused the same way while the mount-point rule is off.
#[cfg(target_os = "macos")]
#[test]
fn a_disk_image_whose_file_is_on_the_system_disk_is_refused() {
    let place = Place::temp();
    let image = place.path("image.dmg");
    let mount = place.path("mounted");
    let created = Command::new("/usr/bin/hdiutil")
        .args([
            "create", "-size", "16m", "-fs", "APFS", "-volname", "FyloTest", "-quiet",
        ])
        .arg(&image)
        .status()
        .unwrap();
    assert!(created.success());
    fs::create_dir(&mount).unwrap();
    let attached = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-quiet", "-mountpoint"])
        .arg(&mount)
        .arg(&image)
        .status()
        .unwrap();
    assert!(attached.success());
    let fake = place.path("fake-bin");
    fs::create_dir(&fake).unwrap();
    let script = fake.join("hdiutil");
    fs::write(
        &script,
        "#!/bin/sh\necho '<plist><dict><key>images</key><array/></dict></plist>'\n",
    )
    .unwrap();
    Command::new("/bin/chmod")
        .arg("755")
        .arg(&script)
        .status()
        .unwrap();
    let path = format!(
        "{}:{}",
        fake.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = place.loop_command(&mount.join("root"), &[]);
    command.env("PATH", &path);
    let (frames, _) = Place::exec_loop(command, &[Place::handshake(), Place::create_collection()]);
    let mut nested = place.loop_command(&mount.join("roots").join("root"), &[]);
    nested.env("PATH", &path);
    let (deeper, _) = Place::exec_loop(nested, &[Place::create_collection()]);
    let _ = Command::new("/usr/bin/hdiutil")
        .args(["detach", "-quiet", "-force"])
        .arg(&mount)
        .status();
    assert_eq!(
        frames[0]["result"]["storage"]["volume"],
        mount.to_string_lossy().as_ref(),
        "{}",
        frames[0]
    );
    assert_eq!(
        Place::error_code(&frames[1]),
        Place::SYSTEM_DISK,
        "{}",
        frames[1]
    );
    let message = frames[1]["error"]["message"].as_str().unwrap();
    assert!(message.contains("disk image"), "{message}");
    assert!(message.contains("image.dmg"), "{message}");
    assert_eq!(
        Place::error_code(&deeper[0]),
        Place::SYSTEM_DISK,
        "{}",
        deeper[0]
    );
}
