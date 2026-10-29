//! Additive four-argument machine profile API, retaining existing configuration.
//! This source-only contract is not a native, NFS, EFS, or Lambda qualification.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, serve_application, serve_profiled};
use fylo_storage_native::{
    FilesystemProfile, NativeRoot, NativeWriteRoot, RootConfig, RootLease, RootOpenOptions,
};
use serde_json::{Value, json};

const ID: &str = "4VRNF52JPCO";
const FIXTURE_KEY: &str = "fixture-key-at-least-thirty-two-bytes";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-efs-machine-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        Self { parent, root }
    }
    fn options(&self) -> RootOpenOptions {
        RootOpenOptions::new(&self.root).with_profile(FilesystemProfile::EfsSidecarV1)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn run(
    options: RootOpenOptions,
    application: ApplicationConfig,
    limits: FrameLimits,
    exclusive: bool,
    persistent: bool,
    requests: &[Value],
) -> Vec<Value> {
    let input = requests
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut input = Cursor::new(input.into_bytes());
    let mut output = Vec::new();
    serve_profiled(
        (&mut input, &mut output),
        options,
        application,
        (limits, exclusive, persistent),
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_storage_report(frame: &Value, profile: &str, owned: bool, kernel_enforced: bool) {
    assert_eq!(frame["ok"], true, "{frame}");
    assert_eq!(
        frame["result"]["capabilities"]["filesystemMetadata"],
        json!({
            "version": 1,
            "profiles": ["native", "efs-sidecar-v1"],
            "efsQualified": false,
        })
    );
    assert_eq!(
        frame["result"]["storage"],
        json!({
            "filesystemProfile": profile,
            "rootLock": {
                "owned": owned,
                "kernelEnforced": kernel_enforced,
                "remoteQualified": false,
            },
        })
    );
    if profile == "efs-sidecar-v1" {
        assert_eq!(frame["result"]["capabilities"]["serverlessQueue"], false);
    }
}

fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(base, &path, files);
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(path, path, &mut files);
    files
}

#[test]
fn efs_admission_acquires_the_actual_root_even_without_exclusive_root_option() {
    let fixture = Fixture::new();
    assert!(RootLease::platform_enforces_exclusivity());
    let lease =
        RootLease::acquire_with_profile(&fixture.root, FilesystemProfile::EfsSidecarV1).unwrap();
    assert!(lease.kernel_enforced());
    NativeWriteRoot::open_profiled(fixture.options().with_lease(lease.clone())).unwrap();
    let before = snapshot(&fixture.root);
    let denied = run(
        fixture.options(),
        ApplicationConfig::default(),
        FrameLimits::default(),
        false,
        true,
        &[json!({"op":"handshake"})],
    );
    assert_eq!(denied[0]["ok"], false);
    assert_eq!(denied[0]["error"]["code"], "EROOTLOCKED");
    assert_eq!(snapshot(&fixture.root), before);
    let allowed = run(
        fixture.options().with_lease(lease.clone()),
        ApplicationConfig::default(),
        FrameLimits::default(),
        false,
        true,
        &[json!({"op":"handshake"})],
    );
    assert_eq!(allowed[0]["ok"], true);
    assert_eq!(allowed[0]["result"]["capabilities"]["exclusiveRoot"], true);
    assert_storage_report(
        &allowed[0],
        "efs-sidecar-v1",
        lease.assert_owned().is_ok(),
        lease.kernel_enforced(),
    );
    lease.assert_owned().unwrap();
}

#[test]
fn efs_startup_owns_and_initializes_an_empty_root_before_the_first_frame() {
    let fixture = Fixture::new();
    let frames = run(
        fixture.options(),
        ApplicationConfig::default(),
        FrameLimits::default(),
        false,
        true,
        &[
            json!({"op":"handshake"}),
            json!({"op":"createCollection","collection":"records"}),
            json!({"op":"putData","collection":"records","id":ID,"data":{"name":"Ada"},"meta":{"reviewed":true}}),
            json!({"op":"getDoc","collection":"records","id":ID}),
            json!({"op":"queuePublish","topic":"jobs","payload":{"work":"blocked"}}),
        ],
    );
    assert_eq!(frames.len(), 5);
    for frame in &frames[..4] {
        assert_eq!(frame["ok"], true, "{frame}");
    }
    assert_eq!(frames[4]["ok"], false);
    assert_eq!(frames[4]["error"]["code"], "ENATIVE_UNSUPPORTED");
    // EFS admission requires an owned, enforced local lease; remote qualification stays false.
    assert_storage_report(&frames[0], "efs-sidecar-v1", true, true);
    assert!(
        fixture
            .root
            .join(".fylo-catalog/filesystem-profile.json")
            .is_file()
    );
    assert_eq!(frames[3]["result"][ID]["name"], "Ada");
    assert!(NativeRoot::open(&fixture.root).is_err());
    // Session teardown released the admitted root lease.
    assert!(
        RootLease::acquire_with_profile(&fixture.root, FilesystemProfile::EfsSidecarV1).is_ok()
    );
}

#[test]
fn profile_mismatch_is_rejected_before_repository_or_collection_recovery() {
    let fixture = Fixture::new();
    let lease =
        RootLease::acquire_with_profile(&fixture.root, FilesystemProfile::EfsSidecarV1).unwrap();
    let writer =
        NativeWriteRoot::open_profiled(fixture.options().with_lease(lease.clone())).unwrap();
    writer
        .create_collection(
            "records",
            fylo_storage_native::CollectionKind::Document,
            None,
        )
        .unwrap();
    let state = fixture
        .root
        .join(".fylo-transactions/.collections/records/state.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, br#"{"format":"fylo.collection-generation.v1","generation":1,"state":"writing","transactionId":"profile-check-must-precede-recovery"}"#).unwrap();
    let before = snapshot(&fixture.root);
    let frames = run(
        RootOpenOptions::new(&fixture.root).with_lease(lease.clone()),
        ApplicationConfig::default(),
        FrameLimits::default(),
        true,
        true,
        &[json!({"op":"handshake"})],
    );
    assert_eq!(frames[0]["ok"], false);
    assert_ne!(
        frames[0]["error"]["code"], "EROOTLOCKED",
        "the supplied owning lease must reach profile admission"
    );
    assert_eq!(
        snapshot(&fixture.root),
        before,
        "profile admission mutated a mismatched root"
    );
    lease.assert_owned().unwrap();
}

#[test]
fn efs_session_refuses_unknown_or_mixed_request_roots_without_mutating_them() {
    let fixture = Fixture::new();
    let other = fixture.parent.join("unadmitted");
    fs::create_dir_all(&other).unwrap();
    let before = snapshot(&other);
    let frames = run(
        fixture.options(),
        ApplicationConfig::default(),
        FrameLimits::default(),
        false,
        true,
        &[
            json!({"op":"createCollection","collection":"records","root":other}),
            json!({"op":"createCollection","collection":"records"}),
        ],
    );
    assert_eq!(frames[0]["ok"], false);
    assert_eq!(snapshot(&other), before);
    assert_eq!(
        frames[1]["ok"], true,
        "a rejected override must not poison the admitted root"
    );
}

#[test]
fn efs_default_admission_accepts_only_aliases_of_its_canonical_repository() {
    let fixture = Fixture::new();
    let alias = fixture.parent.join("root-alias");
    symlink(&fixture.root, &alias).unwrap();
    let canonical = fs::canonicalize(&fixture.root).unwrap();
    let current = std::env::current_dir().unwrap();
    let mut relative_alias = PathBuf::new();
    for component in current.components() {
        if matches!(component, std::path::Component::Normal(_)) {
            relative_alias.push("..");
        }
    }
    relative_alias.push(alias.strip_prefix(Path::new("/")).unwrap());
    assert!(!relative_alias.is_absolute());
    assert_eq!(fs::canonicalize(&relative_alias).unwrap(), canonical);
    for (sequence, default) in [alias.clone(), relative_alias].into_iter().enumerate() {
        let collection = format!("records{sequence}");
        let frames = run(
            RootOpenOptions::new(default).with_profile(FilesystemProfile::EfsSidecarV1),
            ApplicationConfig::default(),
            FrameLimits::default(),
            false,
            true,
            &[
                json!({"op":"handshake"}),
                json!({"op":"createCollection","collection":collection,"root":canonical}),
                json!({"op":"putData","collection":collection,"id":ID,"root":alias,"data":{"name":"Ada"}}),
                json!({"op":"getDoc","collection":collection,"id":ID,"root":canonical}),
                json!({"op":"handshake"}),
            ],
        );
        for frame in &frames {
            assert_eq!(frame["ok"], true, "{frame}");
        }
        assert_storage_report(&frames[0], "efs-sidecar-v1", true, true);
        assert_storage_report(&frames[4], "efs-sidecar-v1", true, true);
        assert_eq!(frames[3]["result"][ID]["name"], "Ada");
    }
    assert!(RootLease::acquire_with_profile(&canonical, FilesystemProfile::EfsSidecarV1).is_ok());
}

#[test]
fn profiled_machine_retains_frame_limits_root_config_schema_and_credentials() {
    let fixture = Fixture::new();
    let schema = fixture.parent.join("schemas");
    fs::create_dir_all(schema.join("records/history")).unwrap();
    fs::write(
        schema.join("records/manifest.json"),
        br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
    )
    .unwrap();
    fs::write(
        schema.join("records/history/v1.schema.json"),
        br#"{"$encrypted":["secret"]}"#,
    )
    .unwrap();
    let application = ApplicationConfig {
        root: Some(fixture.root.clone()),
        storage: RootConfig {
            strict_schema: false,
            shard_width: Some(2),
            ..Default::default()
        },
        schema: Some(schema.clone()),
        credentials: Some((FIXTURE_KEY.into(), "fixture-salt".into())),
        hot_set_bytes: None,
        ..ApplicationConfig::default()
    };
    let limits = FrameLimits {
        max_request_bytes: 8192,
        max_response_bytes: 16384,
    };
    let frames = run(
        fixture.options(),
        application,
        limits,
        false,
        true,
        &[
            json!({"op":"handshake"}),
            json!({"op":"createCollection","collection":"records"}),
            json!({"op":"putData","collection":"records","id":ID,"data":{"secret":"fixture-plaintext","name":"Ada"}}),
            json!({"op":"getDoc","collection":"records","id":ID}),
            json!({"op":"schemaInspect","collection":"records"}),
        ],
    );
    for frame in &frames {
        assert_eq!(frame["ok"], true, "{frame}");
    }
    assert_storage_report(&frames[0], "efs-sidecar-v1", true, true);
    assert_eq!(frames[0]["result"]["machine"]["maxRequestBytes"], 8192);
    assert_eq!(frames[0]["result"]["machine"]["maxResponseBytes"], 16384);
    assert_eq!(frames[3]["result"][ID]["secret"], "fixture-plaintext");
    assert_eq!(frames[4]["result"]["schemaDir"], json!(schema));
    let lease =
        RootLease::acquire_with_profile(&fixture.root, FilesystemProfile::EfsSidecarV1).unwrap();
    let native = NativeRoot::open_profiled(fixture.options().with_lease(lease)).unwrap();
    let collection = native.collection("records").unwrap();
    let stored = collection.read_document(ID).unwrap();
    assert_eq!(collection.shard_width(), 2);
    assert_eq!(stored.path.parent().unwrap().file_name().unwrap(), "CO");
    let body: Value = serde_json::from_slice(&stored.bytes).unwrap();
    assert_ne!(body["secret"], "fixture-plaintext");
    assert_eq!(body["name"], "Ada");
}

#[test]
fn a_supplied_native_lease_for_another_repository_cannot_admit_or_recover_this_root() {
    let fixture = Fixture::new();
    let other = fixture.parent.join("other-root");
    fs::create_dir_all(&other).unwrap();
    let lease = RootLease::acquire(&other).unwrap();
    let writer = NativeWriteRoot::open(&fixture.root).unwrap();
    writer
        .create_collection(
            "records",
            fylo_storage_native::CollectionKind::Document,
            None,
        )
        .unwrap();
    let state = fixture
        .root
        .join(".fylo-transactions/.collections/records/state.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, br#"{"format":"fylo.collection-generation.v1","generation":1,"state":"writing","transactionId":"foreign-lease-must-not-recover"}"#).unwrap();
    let before = snapshot(&fixture.root);
    let frames = run(
        RootOpenOptions::new(&fixture.root).with_lease(lease.clone()),
        ApplicationConfig::default(),
        FrameLimits::default(),
        false,
        true,
        &[json!({"op":"handshake"})],
    );
    assert_eq!(frames[0]["ok"], false);
    assert_eq!(frames[0]["error"]["code"], "EROOTLEASELOST");
    assert_eq!(snapshot(&fixture.root), before);
    lease.assert_owned().unwrap();
}

#[test]
fn additive_native_session_preserves_the_existing_handshake_and_frame_contract() {
    let fixture = Fixture::new();
    let limits = FrameLimits {
        max_request_bytes: 4096,
        max_response_bytes: 8192,
    };
    let requests = [json!({"op":"handshake"})];
    let profiled = run(
        RootOpenOptions::new(&fixture.root),
        ApplicationConfig::default(),
        limits,
        false,
        true,
        &requests,
    );
    let mut input = Cursor::new(b"{\"op\":\"handshake\"}\n".to_vec());
    let mut output = Vec::new();
    serve_application(
        &mut input,
        &mut output,
        Some(fixture.root.clone()),
        limits,
        ApplicationConfig::default(),
        false,
        true,
    )
    .unwrap();
    let legacy: Value = serde_json::from_slice(&output).unwrap();
    assert_storage_report(&profiled[0], "native", false, false);
    assert_storage_report(&legacy, "native", false, false);
    assert_eq!(profiled[0]["ok"], legacy["ok"]);
    for key in ["protocolVersion", "runtimeVersion", "machine", "storage"] {
        assert_eq!(profiled[0]["result"][key], legacy["result"][key]);
    }
    for key in [
        "exclusiveRoot",
        "machineAccess",
        "documentBuckets",
        "queryPagination",
        "serverlessQueue",
        "filesystemMetadata",
    ] {
        assert_eq!(
            profiled[0]["result"]["capabilities"][key],
            legacy["result"]["capabilities"][key]
        );
    }
    let lease = RootLease::acquire(&fixture.root).unwrap();
    let owned = run(
        RootOpenOptions::new(&fixture.root).with_lease(lease.clone()),
        ApplicationConfig::default(),
        limits,
        false,
        true,
        &requests,
    );
    assert_storage_report(
        &owned[0],
        "native",
        lease.assert_owned().is_ok(),
        lease.kernel_enforced(),
    );
    assert!(
        !fixture
            .root
            .join(".fylo-catalog/filesystem-profile.json")
            .exists()
    );
}
