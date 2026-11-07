use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_fylo-rust");

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("fylo-cli-identity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(BINARY)
            .current_dir(&self.0)
            .arg("identity")
            .args(arguments)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "fylo identity failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_owned()
}

fn failure(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "fylo identity unexpectedly succeeded"
    );
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn assert_identifier(text: &str) {
    let encoded = text.strip_prefix("fid1:p256:").unwrap();
    assert_eq!(encoded.len(), 52, "{text}");
    assert!(
        encoded
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte)),
        "{text}"
    );
}

#[test]
fn create_writes_a_key_file_and_show_prints_the_same_identifier() {
    let fixture = Fixture::new("file-round-trip");
    let created = stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
    assert_identifier(&created);
    assert_eq!(stdout(&fixture.run(&["show", "file:yon.pem"])), created);
}

#[test]
fn create_and_show_report_json() {
    let fixture = Fixture::new("file-json");
    let created: Value = serde_json::from_str(&stdout(&fixture.run(&[
        "create",
        "--store",
        "file:yon.pem",
        "--json",
    ])))
    .unwrap();
    let shown: Value =
        serde_json::from_str(&stdout(&fixture.run(&["show", "file:yon.pem", "--json"]))).unwrap();
    assert_eq!(created["reference"], "file:yon.pem");
    assert_eq!(shown, created);
}

#[test]
fn the_key_file_is_pkcs8_pem() {
    let fixture = Fixture::new("file-pem");
    stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
    let text = fs::read_to_string(fixture.path("yon.pem")).unwrap();
    assert!(text.starts_with("-----BEGIN PRIVATE KEY-----\n"), "{text}");
}

#[test]
fn two_keys_have_different_identifiers() {
    let fixture = Fixture::new("file-distinct");
    let first = stdout(&fixture.run(&["create", "--store", "file:first.pem"]));
    let second = stdout(&fixture.run(&["create", "--store", "file:second.pem"]));
    assert_ne!(first, second);
}

#[test]
fn create_never_replaces_a_key_file() {
    let fixture = Fixture::new("file-exists");
    let created = stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
    let error = failure(&fixture.run(&["create", "--store", "file:yon.pem"]));
    assert!(error.starts_with("EIDENTITY_EXISTS:"), "{error}");
    assert_eq!(stdout(&fixture.run(&["show", "file:yon.pem"])), created);
}

#[test]
fn show_reports_a_missing_key_file() {
    let fixture = Fixture::new("file-missing");
    let error = failure(&fixture.run(&["show", "file:absent.pem"]));
    assert!(error.starts_with("EIDENTITY_NOT_FOUND:"), "{error}");
}

#[test]
fn show_refuses_a_file_that_is_not_a_key() {
    let fixture = Fixture::new("file-not-key");
    fs::write(fixture.path("notes.pem"), "not a key\n").unwrap();
    set_mode(&fixture.path("notes.pem"), 0o600);
    let error = failure(&fixture.run(&["show", "file:notes.pem"]));
    assert!(error.starts_with("EIDENTITY_KEY:"), "{error}");
}

#[test]
fn show_refuses_a_directory() {
    let fixture = Fixture::new("file-directory");
    fs::create_dir(fixture.path("keys")).unwrap();
    let error = failure(&fixture.run(&["show", "file:keys"]));
    assert!(error.starts_with("EIDENTITY_KEY:"), "{error}");
}

#[test]
fn refuses_an_unknown_store() {
    let fixture = Fixture::new("unknown-store");
    let error = failure(&fixture.run(&["create", "--store", "vault:yon"]));
    assert!(error.starts_with("EIDENTITY_REFERENCE:"), "{error}");
}

#[test]
fn prints_usage_for_unknown_arguments() {
    let fixture = Fixture::new("usage");
    let error = failure(&fixture.run(&["rotate", "file:yon.pem"]));
    assert!(error.starts_with("Usage:"), "{error}");
}

#[test]
fn webcrypto_keys_are_refused_outside_the_browser() {
    let fixture = Fixture::new("webcrypto");
    let error = failure(&fixture.run(&["create", "--store", "webcrypto:yon"]));
    assert!(error.starts_with("EIDENTITY_UNSUPPORTED:"), "{error}");
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    use sha2::{Digest, Sha256};

    use super::{Fixture, failure, set_mode, stdout};

    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    const UNCOMPRESSED_POINT_BYTES: usize = 65;

    fn base32(bytes: &[u8]) -> String {
        let bits: Vec<u8> = bytes
            .iter()
            .flat_map(|byte| (0..8).rev().map(move |shift| (byte >> shift) & 1))
            .collect();
        bits.chunks(5)
            .map(|chunk| {
                let value = chunk
                    .iter()
                    .chain(std::iter::repeat(&0))
                    .take(5)
                    .fold(0usize, |value, bit| (value << 1) | usize::from(*bit));
                char::from(ALPHABET[value])
            })
            .collect()
    }

    #[test]
    fn create_makes_an_owner_only_file() {
        let fixture = Fixture::new("file-mode");
        stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
        let mode = fs::metadata(fixture.path("yon.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn show_accepts_a_read_only_key_file() {
        let fixture = Fixture::new("file-0400");
        let created = stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
        set_mode(&fixture.path("yon.pem"), 0o400);
        assert_eq!(stdout(&fixture.run(&["show", "file:yon.pem"])), created);
    }

    #[test]
    fn show_refuses_a_key_file_others_can_read() {
        let fixture = Fixture::new("file-0644");
        stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
        set_mode(&fixture.path("yon.pem"), 0o644);
        let error = failure(&fixture.run(&["show", "file:yon.pem"]));
        assert!(error.starts_with("EIDENTITY_EXPOSED:"), "{error}");
    }

    #[test]
    fn show_refuses_a_key_file_the_group_can_read() {
        let fixture = Fixture::new("file-0640");
        stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
        set_mode(&fixture.path("yon.pem"), 0o640);
        let error = failure(&fixture.run(&["show", "file:yon.pem"]));
        assert!(error.starts_with("EIDENTITY_EXPOSED:"), "{error}");
    }

    #[test]
    fn show_refuses_a_key_file_others_can_write() {
        let fixture = Fixture::new("file-0602");
        stdout(&fixture.run(&["create", "--store", "file:yon.pem"]));
        set_mode(&fixture.path("yon.pem"), 0o602);
        let error = failure(&fixture.run(&["show", "file:yon.pem"]));
        assert!(error.starts_with("EIDENTITY_EXPOSED:"), "{error}");
    }

    #[test]
    fn the_identifier_is_the_fingerprint_openssl_computes() {
        let fixture = Fixture::new("file-openssl");
        let output = Command::new("openssl")
            .current_dir(&fixture.0)
            .args([
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
            ])
            .args(["-out", "openssl.pem"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        set_mode(&fixture.path("openssl.pem"), 0o600);
        let public = Command::new("openssl")
            .current_dir(&fixture.0)
            .args(["pkey", "-in", "openssl.pem", "-pubout", "-outform", "DER"])
            .output()
            .unwrap();
        assert!(
            public.status.success(),
            "{}",
            String::from_utf8_lossy(&public.stderr)
        );
        let point = &public.stdout[public.stdout.len() - UNCOMPRESSED_POINT_BYTES..];
        assert_eq!(point[0], 4);
        let expected = format!("fid1:p256:{}", base32(&Sha256::digest(point)));
        assert_eq!(
            stdout(&fixture.run(&["show", "file:openssl.pem"])),
            expected
        );
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;
    use std::process::{Command, Output};

    use super::{BINARY, Fixture, assert_identifier, failure, stdout};

    const SECURITY: &str = "/usr/bin/security";
    const ITEM_NOT_FOUND_EXIT: i32 = 44;

    /// A throwaway keychain file, kept out of the user's search list and
    /// deleted afterwards, so no test leaves an item in the login keychain.
    struct Keychain {
        _fixture: Fixture,
        path: PathBuf,
    }

    impl Keychain {
        fn new(name: &str) -> Self {
            let fixture = Fixture::new(name);
            let path = fixture.path("test.keychain-db");
            let password = format!("fylo-test-{}", std::process::id());
            security(&["create-keychain", "-p", &password, path.to_str().unwrap()]);
            security(&["set-keychain-settings", path.to_str().unwrap()]);
            security(&["unlock-keychain", "-p", &password, path.to_str().unwrap()]);
            Self {
                _fixture: fixture,
                path,
            }
        }

        fn run(&self, arguments: &[&str]) -> Output {
            Command::new(BINARY)
                .arg("identity")
                .args(arguments)
                .env("FYLO_KEYCHAIN", &self.path)
                .output()
                .unwrap()
        }
    }

    impl Drop for Keychain {
        fn drop(&mut self) {
            let _ = Command::new(SECURITY)
                .args(["delete-keychain", self.path.to_str().unwrap()])
                .output();
        }
    }

    fn label(name: &str) -> String {
        format!("com.delma.fylo-test.{name}.{}", std::process::id())
    }

    fn security(arguments: &[&str]) {
        let output = Command::new(SECURITY).args(arguments).output().unwrap();
        assert!(
            output.status.success(),
            "security {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn create_stores_a_key_in_the_keychain_and_show_reads_it() {
        let keychain = Keychain::new("keychain-round-trip");
        let reference = format!("keychain:{}", label("round-trip"));
        let created = stdout(&keychain.run(&["create", "--store", &reference]));
        assert_identifier(&created);
        assert_eq!(stdout(&keychain.run(&["show", &reference])), created);
    }

    #[test]
    fn the_item_stays_out_of_the_users_keychains() {
        let keychain = Keychain::new("keychain-isolated");
        let label = label("isolated");
        stdout(&keychain.run(&["create", "--store", &format!("keychain:{label}")]));
        let search = Command::new(SECURITY)
            .args(["find-generic-password", "-s", &label])
            .output()
            .unwrap();
        assert_eq!(search.status.code(), Some(ITEM_NOT_FOUND_EXIT));
        security(&[
            "find-generic-password",
            "-s",
            &label,
            "-a",
            "fylo-identity",
            keychain.path.to_str().unwrap(),
        ]);
    }

    #[test]
    fn create_never_replaces_a_keychain_key() {
        let keychain = Keychain::new("keychain-exists");
        let reference = format!("keychain:{}", label("exists"));
        let created = stdout(&keychain.run(&["create", "--store", &reference]));
        let error = failure(&keychain.run(&["create", "--store", &reference]));
        assert!(error.starts_with("EIDENTITY_EXISTS:"), "{error}");
        assert_eq!(stdout(&keychain.run(&["show", &reference])), created);
    }

    #[test]
    fn show_reports_a_missing_keychain_key() {
        let keychain = Keychain::new("keychain-missing");
        let reference = format!("keychain:{}", label("missing"));
        let error = failure(&keychain.run(&["show", &reference]));
        assert!(error.starts_with("EIDENTITY_NOT_FOUND:"), "{error}");
    }

    #[test]
    fn linux_and_windows_stores_are_refused_on_macos() {
        let fixture = Fixture::new("other-platform-stores");
        let creds = failure(&fixture.run(&["create", "--store", "creds:yon"]));
        let cng = failure(&fixture.run(&["create", "--store", "cng:yon"]));
        assert!(creds.starts_with("EIDENTITY_UNSUPPORTED:"), "{creds}");
        assert!(cng.starts_with("EIDENTITY_UNSUPPORTED:"), "{cng}");
    }
}
