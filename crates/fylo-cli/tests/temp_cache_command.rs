use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// A throwaway folder for one test: a root and a base folder for the temp
/// cache, removed when the test ends.
struct Place(PathBuf);

impl Place {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-temp-cache-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("base")).unwrap();
        Self(path)
    }

    fn root(&self) -> PathBuf {
        self.0.join("root")
    }

    fn base(&self) -> PathBuf {
        self.0.join("base")
    }

    fn base_entries(&self) -> usize {
        fs::read_dir(self.base()).unwrap().count()
    }

    fn fylo(&self, arguments: &[&str]) -> Output {
        Command::new(Self::BINARY)
            .args(arguments)
            .current_dir(&self.0)
            .env_remove("FYLO_ROOT")
            .output()
            .unwrap()
    }

    fn json(&self, arguments: &[&str]) -> Value {
        let output = self.fylo(arguments);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn temp_cache(&self, flags: &[&str]) -> Value {
        let root = self.root();
        let base = self.base();
        let mut arguments = vec![
            "exec",
            "--request",
            r#"{"op":"handshake"}"#,
            "--root",
            path_text(&root),
            "--temp-cache-dir",
            path_text(&base),
        ];
        arguments.extend_from_slice(flags);
        let frame = self.json(&arguments);
        assert_eq!(frame["ok"], true, "{frame}");
        frame["result"]["capabilities"]["tempCache"].clone()
    }

    fn cache(&self, action: &str) -> Value {
        let base = self.base();
        self.json(&[
            "cache",
            action,
            "--temp-cache-dir",
            path_text(&base),
            "--json",
        ])
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn off_reports_false_and_creates_nothing() {
    let place = Place::new();
    assert_eq!(place.temp_cache(&["--temp-cache", "off"]), json!(false));
    assert_eq!(place.base_entries(), 0);
}

#[test]
fn auto_is_the_default_and_reports_a_local_root() {
    let place = Place::new();
    let capability = place.temp_cache(&[]);
    assert_eq!(capability["mode"], "auto", "{capability}");
    assert_eq!(capability["active"], false, "{capability}");
    assert_eq!(capability["reason"], "root is local", "{capability}");
    assert_eq!(capability["budgetBytes"], 268_435_456, "{capability}");
    for counter in [
        "hits",
        "misses",
        "revalidations",
        "rejected",
        "writes",
        "evictions",
        "errors",
    ] {
        assert_eq!(capability[counter], 0, "{counter}: {capability}");
    }
    assert_eq!(place.base_entries(), 0);
}

#[test]
fn on_creates_the_folder_and_activates_the_tier_for_a_native_root() {
    let place = Place::new();
    let capability = place.temp_cache(&["--temp-cache", "on", "--temp-cache-bytes", "1048576"]);
    assert_eq!(capability["mode"], "on", "{capability}");
    assert_eq!(capability["active"], true, "{capability}");
    assert_eq!(capability["reason"], Value::Null, "{capability}");
    assert_eq!(capability["budgetBytes"], 1_048_576, "{capability}");
    let folder = PathBuf::from(capability["dir"].as_str().unwrap());
    assert_eq!(folder.parent(), Some(place.base().as_path()));
    assert!(folder.is_dir());
}

#[test]
fn status_and_clear_report_and_remove_the_folder() {
    let place = Place::new();
    let missing = place.cache("status");
    assert_eq!(missing["exists"], false, "{missing}");
    assert_eq!(place.cache("clear")["removed"], false);
    place.temp_cache(&["--temp-cache", "on"]);
    let folder = PathBuf::from(place.cache("status")["dir"].as_str().unwrap());
    fs::write(folder.join("entry.fyc"), [0_u8; 64]).unwrap();
    let status = place.cache("status");
    assert_eq!(status["exists"], true, "{status}");
    assert_eq!(status["entries"], 1, "{status}");
    assert_eq!(status["usedBytes"], 64, "{status}");
    assert!(status["lastSweepMs"].is_u64(), "{status}");
    assert_eq!(place.cache("clear")["removed"], true);
    assert!(!folder.exists());
}

#[test]
fn status_prints_plain_text_without_json() {
    let place = Place::new();
    let base = place.base();
    let output = place.fylo(&["cache", "status", "--temp-cache-dir", path_text(&base)]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("exists: false"), "{text}");
    assert!(text.contains("dir: "), "{text}");
}

#[test]
fn the_folder_comes_from_application_json() {
    let place = Place::new();
    fs::write(
        place.0.join("application.json"),
        r#"{"fylo":{"tempCache":{"mode":"on","dir":"base"}}}"#,
    )
    .unwrap();
    let status = place.json(&["cache", "status", "--json"]);
    let folder = PathBuf::from(status["dir"].as_str().unwrap());
    assert_eq!(
        fs::canonicalize(folder.parent().unwrap()).unwrap(),
        fs::canonicalize(place.base()).unwrap()
    );
}

#[test]
fn bad_temp_cache_options_are_configuration_errors() {
    let place = Place::new();
    for arguments in [
        &["exec", "--request", "{}", "--temp-cache", "always"][..],
        &["exec", "--request", "{}", "--temp-cache-bytes", "many"][..],
        &["cache", "status", "--root", "somewhere"][..],
        &["cache", "empty"][..],
    ] {
        let output = place.fylo(arguments);
        assert!(!output.status.success(), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        assert!(!output.stderr.is_empty(), "{arguments:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_folder_other_users_may_reach_is_refused_and_kept() {
    use std::os::unix::fs::PermissionsExt;

    let place = Place::new();
    place.temp_cache(&["--temp-cache", "on"]);
    let folder = PathBuf::from(place.cache("status")["dir"].as_str().unwrap());
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        place.temp_cache(&["--temp-cache", "on"])["reason"],
        "unsafe folder"
    );
    let base = place.base();
    let output = place.fylo(&["cache", "clear", "--temp-cache-dir", path_text(&base)]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("other users"));
    assert!(folder.is_dir());
}
