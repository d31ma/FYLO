use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    const BINARY: &'static str = env!("CARGO_BIN_EXE_fylo-rust");
    const APPLICATION_FILE: &'static str = "application.json";
    const SECRETS_FILE: &'static str = ".env";
    const DEFAULT_ROOT: &'static str = ".fylo-data";
    const EXECUTE: &'static str = "exec";
    const CONFIG_OPTION: &'static str = "--config";
    const REQUEST_OPTION: &'static str = "--request";
    const ROOT_OPTION: &'static str = "--root";
    const SHARD_OPTION: &'static str = "--shard-width";
    const SHARD_WIDTH: &'static str = "1";
    const IGNORED_ROOT: &'static str = "ignored";
    const UNUSED_ROOT: &'static str = "unused";
    const SELECTED_ROOT: &'static str = "selected";
    const INJECTED_SECRET: &'static str = "injected-secret";
    const FILE_SECRET: &'static str = "file-secret";
    const SENSITIVE_TEXT: &'static str = "TOP-SECRET";
    const NESTED_CONFIG: &'static str = "settings/application.json";
    const HANDSHAKE: &'static str = r#"{"op":"handshake"}"#;
    const CREATE_COLLECTION: &'static str = r#"{"op":"createCollection","collection":"users"}"#;
    const DOCUMENT_FILE: &'static str = "selected/.collections/users/docs/O/4VRNF52JPCO.json";

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, source: &str) {
        fs::write(self.0.join(name), source).unwrap();
    }
    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(Self::BINARY)
            .current_dir(&self.0)
            .args(arguments)
            .env("FYLO_ROOT", self.0.join(Fixture::IGNORED_ROOT))
            .env("FYLO_SHARD_WIDTH", "invalid")
            .env("FYLO_CONFIG_TEST_SECRET", Fixture::INJECTED_SECRET)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn unknown_commands_report_usage_without_loading_configuration() {
    let fixture = Fixture::new();
    fixture.write(Fixture::APPLICATION_FILE, "invalid configuration");
    let output = fixture.run(&["put"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn application_root_replaces_environment_and_is_relative_to_config() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("settings")).unwrap();
    fixture.write(
        Fixture::NESTED_CONFIG,
        r#"{"other":{"port":1234},"fylo":{"root":"../data","shard":{"width":2}}}"#,
    );
    let output = fixture.run(&[
        Fixture::EXECUTE,
        Fixture::CONFIG_OPTION,
        Fixture::NESTED_CONFIG,
        Fixture::REQUEST_OPTION,
        Fixture::CREATE_COLLECTION,
    ]);
    Fixture::require_success(&output);
    assert!(fixture.0.join("data").is_dir());
    assert!(!fixture.0.join(Fixture::IGNORED_ROOT).exists());
}

#[test]
fn flat_configuration_key_fails_without_creating_data() {
    let fixture = Fixture::new();
    let output = fixture.invalid_config(r#"{"fylo":{"shard_width":2}}"#);
    assert!(!output.status.success());
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn oversized_shard_width_fails_without_creating_data() {
    let fixture = Fixture::new();
    let output = fixture.invalid_config(r#"{"fylo":{"shard":{"width":9}}}"#);
    assert!(!output.status.success());
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn zero_shard_width_fails_without_creating_data() {
    let fixture = Fixture::new();
    let output = fixture.invalid_config(r#"{"fylo":{"shard":{"width":0}}}"#);
    assert!(!output.status.success());
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn string_strict_flag_fails_without_creating_data() {
    let fixture = Fixture::new();
    let output = fixture.invalid_config(r#"{"fylo":{"strict":"false"}}"#);
    assert!(!output.status.success());
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn duplicate_root_fails_without_creating_data() {
    let fixture = Fixture::new();
    let output = fixture.invalid_config(r#"{"fylo":{"root":"a","root":"b"}}"#);
    assert!(!output.status.success());
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn null_configuration_fails_without_creating_data() {
    let fixture = Fixture::new();
    let output = fixture.invalid_config(r#"{"fylo":null}"#);
    assert!(!output.status.success());
    assert!(!fixture.0.join(Fixture::DEFAULT_ROOT).exists());
}

#[test]
fn explicit_missing_file_fails_and_absent_default_is_optional() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .run(&[
                Fixture::EXECUTE,
                Fixture::CONFIG_OPTION,
                "missing.json",
                Fixture::REQUEST_OPTION,
                Fixture::HANDSHAKE
            ])
            .status
            .success()
    );
    assert!(
        fixture
            .run(&[
                Fixture::EXECUTE,
                Fixture::REQUEST_OPTION,
                Fixture::HANDSHAKE
            ])
            .status
            .success()
    );
}

#[test]
fn secret_references_are_explicit_fail_closed_and_do_not_leak() {
    let fixture = Fixture::new();
    fixture.write(
        Fixture::APPLICATION_FILE,
        r#"{"fylo":{"root":"${FYLO_CONFIG_TEST_SECRET}"}}"#,
    );
    fixture.write(
        Fixture::SECRETS_FILE,
        "FYLO_CONFIG_TEST_SECRET=file-secret\nFYLO_ROOT=not-settings\n",
    );
    assert!(
        fixture
            .run(&[
                Fixture::EXECUTE,
                Fixture::REQUEST_OPTION,
                Fixture::CREATE_COLLECTION
            ])
            .status
            .success()
    );
    assert!(fixture.0.join(Fixture::INJECTED_SECRET).is_dir());
    assert!(!fixture.0.join(Fixture::FILE_SECRET).exists());
    fixture.write(
        Fixture::APPLICATION_FILE,
        r#"{"fylo":{"root":"${FYLO_MISSING_CONFIG_TEST_SECRET}"}}"#,
    );
    let output = fixture.run(&[
        Fixture::EXECUTE,
        Fixture::REQUEST_OPTION,
        Fixture::HANDSHAKE,
    ]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(Fixture::FILE_SECRET));
}

#[test]
fn adjacent_dotenv_is_not_a_global_environment_loader() {
    let fixture = Fixture::new();
    fixture.write(
        Fixture::APPLICATION_FILE,
        r#"{"fylo":{"root":"${FYLO_CONFIG_FILE_ONLY}"}}"#,
    );
    fixture.write(
        Fixture::SECRETS_FILE,
        "# secret reference\nexport FYLO_CONFIG_FILE_ONLY='from-file'\nFYLO_SHARD_WIDTH=invalid\n",
    );
    assert!(
        fixture
            .run(&[
                Fixture::EXECUTE,
                Fixture::REQUEST_OPTION,
                Fixture::CREATE_COLLECTION
            ])
            .status
            .success()
    );
    assert!(fixture.0.join("from-file").is_dir());
}

#[test]
fn exclusive_loop_applies_config_and_explicit_overrides() {
    let fixture = Fixture::new();
    let output = fixture.run_override(true);
    Fixture::require_success(&output);
    assert!(fixture.0.join(Fixture::DOCUMENT_FILE).is_file());
    assert!(!fixture.0.join(Fixture::UNUSED_ROOT).exists());
}

#[test]
fn one_shot_applies_config_and_explicit_overrides() {
    let fixture = Fixture::new();
    let output = fixture.run_override(false);
    Fixture::require_success(&output);
    assert!(fixture.0.join(Fixture::DOCUMENT_FILE).is_file());
    assert!(!fixture.0.join(Fixture::UNUSED_ROOT).exists());
}

impl Fixture {
    fn require_success(output: &Output) {
        assert!(output.status.success(), "{}", Self::output_text(output));
    }

    fn output_text(output: &Output) -> String {
        format!(
            "{} {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        )
    }

    fn invalid_config(&self, config: &str) -> Output {
        self.write(Fixture::APPLICATION_FILE, config);
        self.run(&[
            Fixture::EXECUTE,
            Fixture::REQUEST_OPTION,
            Fixture::HANDSHAKE,
        ])
    }

    fn run_override(&self, persistent: bool) -> Output {
        self.prepare_override(persistent);
        let mut command = self.override_command();
        Self::run_command(&mut command, persistent)
    }

    fn prepare_override(&self, persistent: bool) {
        self.write(
            Fixture::APPLICATION_FILE,
            r#"{"fylo":{"root":"unused","shard":{"width":2}}}"#,
        );
        let root = self.0.join(Fixture::SELECTED_ROOT);
        let create = Fixture::CREATE_COLLECTION;
        if !persistent {
            assert!(
                self.run(&[
                    Fixture::EXECUTE,
                    Fixture::ROOT_OPTION,
                    root.to_str().unwrap(),
                    Fixture::SHARD_OPTION,
                    Fixture::SHARD_WIDTH,
                    Fixture::REQUEST_OPTION,
                    create
                ])
                .status
                .success()
            );
        }
    }

    fn override_command(&self) -> Command {
        let root = self.0.join(Fixture::SELECTED_ROOT);
        let mut command = Command::new(Self::BINARY);
        command
            .current_dir(&self.0)
            .args([
                Fixture::CONFIG_OPTION,
                Fixture::APPLICATION_FILE,
                Fixture::EXECUTE,
                Fixture::ROOT_OPTION,
            ])
            .arg(&root)
            .args([Fixture::SHARD_OPTION, Fixture::SHARD_WIDTH])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run_command(command: &mut Command, persistent: bool) -> Output {
        let create = Fixture::CREATE_COLLECTION;
        let request =
            r#"{"op":"putData","collection":"users","id":"4VRNF52JPCO","data":{"name":"Ada"}}"#;
        if persistent {
            let mut child = command
                .args(["--loop", "--exclusive-root"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            writeln!(child.stdin.take().unwrap(), "{create}\n{request}").unwrap();
            return child.wait_with_output().unwrap();
        }
        command
            .args([Fixture::REQUEST_OPTION, request])
            .output()
            .unwrap()
    }

    fn invalid_secret(&self, source: &str) -> Output {
        self.write(
            Fixture::APPLICATION_FILE,
            r#"{"fylo":{"encryption":{"key":"${CONFIG_KEY}"},"cipher":{"salt":"${CONFIG_SALT}"}}}"#,
        );
        self.write(Fixture::SECRETS_FILE, source);
        self.run(&[
            Fixture::EXECUTE,
            Fixture::REQUEST_OPTION,
            Fixture::HANDSHAKE,
        ])
    }
}

#[test]
fn short_secret_values_are_never_echoed() {
    let fixture = Fixture::new();
    let output = fixture.invalid_secret("CONFIG_KEY='TOP-SECRET'\nCONFIG_SALT=salt");
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(Fixture::SENSITIVE_TEXT));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(Fixture::SENSITIVE_TEXT));
}

#[test]
fn unfinished_secret_values_are_never_echoed() {
    let fixture = Fixture::new();
    let output = fixture.invalid_secret("CONFIG_KEY='TOP-SECRET\nCONFIG_SALT=salt");
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(Fixture::SENSITIVE_TEXT));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(Fixture::SENSITIVE_TEXT));
}
