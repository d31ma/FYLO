//! Provider-independent, exactly-one-frame invocation and checked lease contracts.
//! Source-only tests: no native, remote filesystem, or provider qualification implied.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{
    ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime, serve_application,
};
use fylo_storage_native::{
    FilesystemProfile, NativeRoot, NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions,
    RootConfig, RootLease, RootOpenOptions,
};
use serde_json::{Value, json};

const ID: &str = "4VRNF52JPCO";
const SECOND_ID: &str = "4VRNF52JPCP";
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
            "fylo-invocation-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        Self { parent, root }
    }

    fn options(&self, profile: FilesystemProfile) -> RootOpenOptions {
        RootOpenOptions::new(&self.root).with_profile(profile)
    }

    fn runtime(&self, profile: FilesystemProfile) -> InvocationRuntime {
        runtime(
            self.options(profile),
            ApplicationConfig::default(),
            FrameLimits::default(),
        )
    }

    fn metadata_path(&self) -> PathBuf {
        fs::canonicalize(&self.root)
            .unwrap()
            .parent()
            .unwrap()
            .join(".root.fylo-root-owner.lock.json")
    }

    fn assert_released(&self, profile: FilesystemProfile) {
        assert!(
            !self.metadata_path().exists(),
            "checked completion left owner metadata"
        );
        let contender = RootLease::acquire_with_profile(&self.root, profile).unwrap();
        assert!(contender.kernel_enforced());
        contender.release_checked().unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn runtime(
    root: RootOpenOptions,
    application: ApplicationConfig,
    limits: FrameLimits,
) -> InvocationRuntime {
    let options = InvocationOptions::new(
        root,
        application,
        limits,
        (
            Instant::now() + Duration::from_secs(30),
            Duration::from_millis(100),
        ),
    )
    .unwrap();
    InvocationRuntime::new(options).unwrap()
}

fn frame(request: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(request).unwrap();
    bytes.push(b'\n');
    bytes
}

fn execute(runtime: &InvocationRuntime, request: &Value) -> Value {
    let bytes = runtime.execute(&frame(request)).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(bytes.split(|byte| *byte == b'\n').skip(1).count(), 1);
    serde_json::from_slice(&bytes).unwrap()
}

fn success(runtime: &InvocationRuntime, request: &Value) -> Value {
    let response = execute(runtime, request);
    assert_eq!(response["ok"], true, "{response}");
    response
}

fn validator_application(fixture: &Fixture) -> ApplicationConfig {
    let schema = fixture.parent.join("validator-schemas");
    fs::create_dir_all(&schema).unwrap();
    fs::write(schema.join("records.schema.json"), br#"{"type":"object"}"#).unwrap();
    let binary = fixture.parent.join("owned-validator.sh");
    fs::write(&binary, b"#!/bin/sh\nprintf started > \"${0}.started\"\nIFS= read -r request || exit 1\nprintf '%s\\n' '{\"ok\":true,\"result\":{\"validated\":true}}'\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    ApplicationConfig {
        schema: Some(schema),
        chex_binary: Some(binary),
        ..ApplicationConfig::default()
    }
}

#[test]
fn invocation_refuses_external_schema_validation_before_owner_or_helper_publication() {
    for profile in [FilesystemProfile::Native, FilesystemProfile::EfsSidecarV1] {
        let fixture = Fixture::new();
        let application = validator_application(&fixture);
        let invocation = runtime(
            fixture.options(profile),
            application,
            FrameLimits::default(),
        );
        let before = snapshot(&fixture.parent);
        let result = invocation.execute(&frame(&json!({
            "op":"schemaValidate","collection":"records","document":{"input":true}
        })));
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(snapshot(&fixture.parent), before);
        let materialized = success(
            &invocation,
            &json!({
                "op":"schemaMaterialize","collection":"records","document":{"input":true}
            }),
        );
        assert_eq!(materialized["result"]["document"], json!({"input":true}));
        assert!(!fixture.parent.join("owned-validator.sh.started").exists());
    }
}

#[test]
fn strict_schema_invocation_configuration_is_refused_before_acquiring_a_lease() {
    for profile in [FilesystemProfile::Native, FilesystemProfile::EfsSidecarV1] {
        let fixture = Fixture::new();
        let mut application = validator_application(&fixture);
        application.storage.strict_schema = true;
        let before = snapshot(&fixture.parent);
        let options = InvocationOptions::new(
            fixture.options(profile),
            application,
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_secs(30),
                Duration::from_millis(100),
            ),
        );
        assert!(matches!(options, Err(error) if error.kind() == std::io::ErrorKind::Unsupported));
        assert_eq!(snapshot(&fixture.parent), before);
    }
}

#[test]
fn native_streaming_retains_explicit_schema_validation() {
    let fixture = Fixture::new();
    let application = validator_application(&fixture);
    let mut input = Cursor::new(frame(&json!({
        "op":"schemaValidate","collection":"records","document":{"input":true}
    })));
    let mut output = Vec::new();
    serve_application(
        &mut input,
        &mut output,
        Some(fixture.root.clone()),
        FrameLimits::default(),
        application,
        false,
        true,
    )
    .unwrap();
    let response: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["document"], json!({"validated":true}));
    assert_eq!(
        fs::read(fixture.parent.join("owned-validator.sh.started")).unwrap(),
        b"started"
    );
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

fn create_record(runtime: &InvocationRuntime) {
    success(
        runtime,
        &json!({"op":"createCollection","collection":"records"}),
    );
    success(
        runtime,
        &json!({"op":"putData","collection":"records","id":ID,"data":{"name":"Ada"}}),
    );
}

fn committed_recovery_fixture(root: &Path) -> PathBuf {
    // A real on-disk committed journal whose generation must be recovered.
    let state = root.join(".fylo-transactions/.collections/records/state.json");
    let generation: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    let before = generation["generation"].as_u64().unwrap();
    let transaction = state.parent().unwrap().join("invocation-cold-recovery");
    fs::create_dir_all(transaction.join("captures")).unwrap();
    fs::create_dir_all(transaction.join("before")).unwrap();
    fs::write(
        transaction.join("transaction.json"),
        serde_json::to_vec(&json!({
            "format":"fylo.collection-transaction.v1", "id":"invocation-cold-recovery",
            "collection":"records", "operation":"put-document", "phase":"committed",
            "generationBefore":before, "eventOffset":0, "captures":[],
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        &state,
        serde_json::to_vec(&json!({
            "format":"fylo.collection-generation.v1", "generation":before + 1,
            "state":"writing", "transactionId":"invocation-cold-recovery",
        }))
        .unwrap(),
    )
    .unwrap();
    transaction
}

#[test]
fn invocation_options_refuse_external_lease_clones_for_every_profile() {
    for profile in [FilesystemProfile::Native, FilesystemProfile::EfsSidecarV1] {
        let fixture = Fixture::new();
        let lease = RootLease::acquire_with_profile(&fixture.root, profile).unwrap();
        let before = snapshot(&fixture.parent);
        let options = InvocationOptions::new(
            fixture.options(profile).with_lease(lease.clone()),
            ApplicationConfig::default(),
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_secs(30),
                Duration::from_secs(1),
            ),
        );
        assert!(options.is_err());
        assert_eq!(snapshot(&fixture.parent), before);
        lease.assert_owned().unwrap();
        lease.release_checked().unwrap();
    }
}

#[test]
fn invocation_budget_requires_a_future_deadline_and_nonzero_usable_cleanup_reserve() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.parent);
    for budget in [
        (
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("test setup requires a deadline one second in the past"),
            Duration::from_millis(1),
        ),
        (Instant::now() + Duration::from_secs(30), Duration::ZERO),
        (
            Instant::now() + Duration::from_secs(1),
            Duration::from_secs(2),
        ),
    ] {
        assert!(
            InvocationOptions::new(
                fixture.options(FilesystemProfile::Native),
                ApplicationConfig::default(),
                FrameLimits::default(),
                budget,
            )
            .is_err()
        );
    }
    assert_eq!(snapshot(&fixture.parent), before);
}

#[test]
fn expired_warm_runtime_refuses_mutation_before_acquiring_a_lease() {
    let fixture = Fixture::new();
    let options = InvocationOptions::new(
        fixture.options(FilesystemProfile::Native),
        ApplicationConfig::default(),
        FrameLimits::default(),
        (
            Instant::now() + Duration::from_millis(100),
            Duration::from_millis(1),
        ),
    )
    .unwrap();
    let runtime = InvocationRuntime::new(options).unwrap();
    let before = snapshot(&fixture.parent);
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        runtime
            .execute(&frame(
                &json!({"op":"createCollection","collection":"records"})
            ))
            .is_err()
    );
    assert_eq!(snapshot(&fixture.parent), before);
}

#[test]
fn invocation_rejects_empty_truncated_multiple_and_oversized_frames_before_mutation() {
    let fixture = Fixture::new();
    let limits = FrameLimits {
        max_request_bytes: 128,
        max_response_bytes: 4096,
    };
    let runtime = runtime(
        fixture.options(FilesystemProfile::Native),
        ApplicationConfig::default(),
        limits,
    );
    let valid = frame(&json!({"op":"createCollection","collection":"records"}));
    let mut multiple = valid.clone();
    multiple.extend_from_slice(&frame(&json!({"op":"handshake"})));
    let mut trailing_blank = valid.clone();
    trailing_blank.push(b'\n');
    let oversized =
        frame(&json!({"op":"createCollection","collection":"records","pad":"x".repeat(256)}));
    let before = snapshot(&fixture.parent);
    for bytes in [
        Vec::new(),
        b"\n".to_vec(),
        valid[..valid.len() - 1].to_vec(),
        multiple,
        trailing_blank,
        oversized,
    ] {
        assert!(runtime.execute(&bytes).is_err());
        assert_eq!(snapshot(&fixture.parent), before);
    }
}

#[test]
fn host_repository_must_be_the_same_actual_root_before_invocation_admission() {
    let fixture = Fixture::new();
    let other = fixture.parent.join("other-repository");
    fs::create_dir_all(&other).unwrap();
    let before = snapshot(&fixture.parent);
    let options = InvocationOptions::new(
        fixture
            .options(FilesystemProfile::Native)
            .with_repository(&other),
        ApplicationConfig::default(),
        FrameLimits::default(),
        (
            Instant::now() + Duration::from_secs(30),
            Duration::from_millis(100),
        ),
    );
    assert!(options.and_then(InvocationRuntime::new).is_err());
    assert_eq!(snapshot(&fixture.parent), before);
}

#[test]
fn request_root_and_schema_overrides_cannot_mutate_an_unadmitted_root_or_replace_host_configuration()
 {
    let fixture = Fixture::new();
    let other = fixture.parent.join("other-root");
    fs::create_dir_all(&other).unwrap();
    let runtime = fixture.runtime(FilesystemProfile::Native);
    let before = snapshot(&fixture.parent);
    for request in [
        json!({"op":"createCollection","collection":"records","root":other}),
        json!({"op":"createCollection","collection":"records","root":null}),
        json!({"op":"createCollection","collection":"records","root":17}),
        json!({"op":"createCollection","collection":"records","schemaDir":other}),
        json!({"op":"createCollection","collection":"records","schemaDir":null}),
    ] {
        assert!(runtime.execute(&frame(&request)).is_err());
        assert_eq!(snapshot(&fixture.parent), before);
    }
    success(
        &runtime,
        &json!({"op":"createCollection","collection":"records"}),
    );
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn canonical_host_and_request_aliases_admit_only_the_same_physical_root() {
    for profile in [FilesystemProfile::Native, FilesystemProfile::EfsSidecarV1] {
        let fixture = Fixture::new();
        let alias = fixture.parent.join("alias");
        symlink(&fixture.root, &alias).unwrap();
        let canonical = fs::canonicalize(&fixture.root).unwrap();
        let runtime = runtime(
            RootOpenOptions::new(&alias)
                .with_profile(profile)
                .with_repository(&canonical),
            ApplicationConfig::default(),
            FrameLimits::default(),
        );
        success(
            &runtime,
            &json!({"op":"createCollection","collection":"records","root":canonical}),
        );
        success(
            &runtime,
            &json!({"op":"putData","collection":"records","id":ID,"root":alias,"data":{"name":"Ada"}}),
        );
        let response = success(
            &runtime,
            &json!({"op":"getDoc","collection":"records","id":ID}),
        );
        assert_eq!(response["result"][ID]["name"], "Ada");
        fixture.assert_released(profile);
    }
}

#[test]
fn checked_invocation_completion_releases_the_root_before_success_or_domain_error_is_returned() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(FilesystemProfile::Native);
    let response = success(&runtime, &json!({"op":"handshake"}));
    assert_eq!(
        response["result"]["storage"]["rootLock"],
        json!({
            "owned":true, "kernelEnforced":true, "remoteQualified":false,
        })
    );
    fixture.assert_released(FilesystemProfile::Native);
    let response = execute(&runtime, &json!({"op":"unknown-invocation-operation"}));
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "EUNSUPPORTEDOP");
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn a_contending_owner_prevents_recovery_and_mutation_but_does_not_poison_the_runtime() {
    let fixture = Fixture::new();
    // A contended invocation waits for the root until its budget runs out, so
    // this one gets a short budget; the next provider call gets a fresh one.
    let contended = InvocationRuntime::new(
        InvocationOptions::new(
            fixture.options(FilesystemProfile::Native),
            ApplicationConfig::default(),
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_millis(400),
                Duration::from_millis(100),
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let lease = RootLease::acquire(&fixture.root).unwrap();
    let before = snapshot(&fixture.parent);
    let outcome = contended.execute(&frame(
        &json!({"op":"createCollection","collection":"records"}),
    ));
    if let Ok(response) = outcome {
        assert_eq!(
            serde_json::from_slice::<Value>(&response).unwrap()["ok"],
            false
        );
    }
    assert_eq!(snapshot(&fixture.parent), before);
    lease.assert_owned().unwrap();
    lease.release_checked().unwrap();
    success(
        &fixture.runtime(FilesystemProfile::Native),
        &json!({"op":"createCollection","collection":"records"}),
    );
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn cold_and_warm_invocations_revalidate_changed_index_inputs_and_keep_no_query_cursor() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(FilesystemProfile::Native);
    create_record(&runtime);
    let query = json!({"op":"findDocs","collection":"records","query":{}});
    let first = success(&runtime, &query);
    assert_eq!(first["result"].as_object().unwrap().len(), 1);
    // A separate admitted owner changes the same collection after its index was cached.
    let lease = RootLease::acquire(&fixture.root).unwrap();
    let writer = NativeWriteRoot::open_profiled(
        fixture
            .options(FilesystemProfile::Native)
            .with_lease(lease.clone()),
    )
    .unwrap();
    writer
        .put_document(
            "records",
            SECOND_ID,
            br#"{"name":"Grace"}"#,
            PutDocumentOptions::default(),
        )
        .unwrap();
    drop(writer);
    lease.release_checked().unwrap();
    let warm = success(&runtime, &query);
    let cold = success(&fixture.runtime(FilesystemProfile::Native), &query);
    assert_eq!(warm["result"], cold["result"]);
    assert_eq!(warm["result"].as_object().unwrap().len(), 2);
    assert_eq!(warm["result"][SECOND_ID]["name"], "Grace");
    for page in [
        json!({"limit":1}),
        json!({"limit":1,"cursor":"previous-invocation"}),
    ] {
        let response = execute(
            &runtime,
            &json!({"op":"findDocs","collection":"records","query":{},"page":page}),
        );
        assert_eq!(response["error"]["code"], "EQUERYLOOPREQUIRED");
        fixture.assert_released(FilesystemProfile::Native);
    }
}

#[test]
fn cold_invocation_recovers_the_touched_collection_before_a_point_read() {
    for profile in [FilesystemProfile::Native, FilesystemProfile::EfsSidecarV1] {
        let fixture = Fixture::new();
        let runtime = fixture.runtime(profile);
        create_record(&runtime);
        let transaction = committed_recovery_fixture(&fixture.root);
        let response = success(
            &fixture.runtime(profile),
            &json!({"op":"getDoc","collection":"records","id":ID}),
        );
        assert_eq!(response["result"][ID]["name"], "Ada");
        assert!(!transaction.exists());
        let generation: Value = serde_json::from_slice(
            &fs::read(
                fixture
                    .root
                    .join(".fylo-transactions/.collections/records/state.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(generation["state"], "stable");
        fixture.assert_released(profile);
    }
}

#[test]
fn corrupt_collection_recovery_refuses_mutation_and_releases_the_internal_lease() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(FilesystemProfile::Native);
    create_record(&runtime);
    let transaction = committed_recovery_fixture(&fixture.root);
    fs::write(transaction.join("transaction.json"), b"{}").unwrap();
    let before = snapshot(&fixture.root);
    let outcome = runtime.execute(&frame(&json!({
        "op":"putData","collection":"records","id":SECOND_ID,"data":{"name":"must not replay"},
    })));
    if let Ok(response) = outcome {
        assert_eq!(
            serde_json::from_slice::<Value>(&response).unwrap()["ok"],
            false
        );
    }
    assert_eq!(snapshot(&fixture.root), before);
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn repository_recovery_failure_occurs_before_dispatch_and_releases_the_internal_lease() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(FilesystemProfile::Native);
    let target = fixture.parent.join("untrusted-staging");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(fixture.root.join(".fylo-vcs")).unwrap();
    symlink(&target, fixture.root.join(".fylo-vcs/staging")).unwrap();
    let before = snapshot(&fixture.root);
    let outcome = runtime.execute(&frame(
        &json!({"op":"createCollection","collection":"records"}),
    ));
    if let Ok(response) = outcome {
        assert_eq!(
            serde_json::from_slice::<Value>(&response).unwrap()["ok"],
            false
        );
    }
    assert_eq!(snapshot(&fixture.root), before);
    assert!(fs::read_dir(target).unwrap().next().is_none());
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn invocation_retains_host_storage_schema_credentials_and_published_frame_limits() {
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
        },
        schema: Some(schema.clone()),
        credentials: Some((FIXTURE_KEY.into(), "fixture-salt".into())),
        chex_binary: None,
    };
    let limits = FrameLimits {
        max_request_bytes: 8192,
        max_response_bytes: 16384,
    };
    let runtime = runtime(
        fixture.options(FilesystemProfile::Native),
        application,
        limits,
    );
    let handshake = success(&runtime, &json!({"op":"handshake"}));
    assert_eq!(handshake["result"]["machine"]["maxRequestBytes"], 8192);
    assert_eq!(handshake["result"]["machine"]["maxResponseBytes"], 16384);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":"records"}),
    );
    success(
        &runtime,
        &json!({"op":"putData","collection":"records","id":ID,"data":{"secret":"fixture-plaintext","name":"Ada"}}),
    );
    let response = success(
        &runtime,
        &json!({"op":"getDoc","collection":"records","id":ID}),
    );
    assert_eq!(response["result"][ID]["secret"], "fixture-plaintext");
    let inspect = success(
        &runtime,
        &json!({"op":"schemaInspect","collection":"records"}),
    );
    assert_eq!(inspect["result"]["schemaDir"], json!(schema));
    let collection = NativeRoot::open(&fixture.root)
        .unwrap()
        .collection("records")
        .unwrap();
    let stored = collection.read_document(ID).unwrap();
    assert_eq!(collection.shard_width(), 2);
    assert_eq!(stored.path.parent().unwrap().file_name().unwrap(), "CO");
    let body: Value = serde_json::from_slice(&stored.bytes).unwrap();
    assert_ne!(body["secret"], "fixture-plaintext");
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn response_and_error_frames_stay_bounded_and_small_limits_never_expose_an_oversized_buffer() {
    let fixture = Fixture::new();
    let normal = fixture.runtime(FilesystemProfile::Native);
    success(
        &normal,
        &json!({"op":"createCollection","collection":"records"}),
    );
    success(
        &normal,
        &json!({"op":"putData","collection":"records","id":ID,"data":{"large":"x".repeat(4096)}}),
    );
    let bounded = runtime(
        fixture.options(FilesystemProfile::Native),
        ApplicationConfig::default(),
        FrameLimits {
            max_request_bytes: 8192,
            max_response_bytes: 512,
        },
    );
    let bytes = bounded
        .execute(&frame(
            &json!({"op":"getDoc","collection":"records","id":ID}),
        ))
        .unwrap();
    assert!(bytes.len() <= 513);
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["error"]["code"], "EFRAME_RESPONSE_TOO_LARGE");
    fixture.assert_released(FilesystemProfile::Native);
    let tiny = runtime(
        fixture.options(FilesystemProfile::Native),
        ApplicationConfig::default(),
        FrameLimits {
            max_request_bytes: 8192,
            max_response_bytes: 1,
        },
    );
    assert!(
        tiny.execute(&frame(
            &json!({"op":"unknown-operation","requestId":"x".repeat(2048)})
        ))
        .is_err()
    );
    fixture.assert_released(FilesystemProfile::Native);
}

#[test]
fn an_uncertain_response_after_mutation_does_not_automatically_replay_the_operation() {
    let fixture = Fixture::new();
    let normal = fixture.runtime(FilesystemProfile::Native);
    success(
        &normal,
        &json!({"op":"createCollection","collection":"records"}),
    );
    let tiny = runtime(
        fixture.options(FilesystemProfile::Native),
        ApplicationConfig::default(),
        FrameLimits {
            max_request_bytes: 8192,
            max_response_bytes: 1,
        },
    );
    // No caller identifier: a retry would create another document, so the
    // create is refused before anything is written.
    let refused = normal
        .execute(&frame(
            &json!({"op":"putData","collection":"records","data":{"name":"no id"}}),
        ))
        .unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
    // With an identifier, the uncertain first attempt committed and the
    // caller's retry is refused as existing instead of duplicated.
    let create =
        json!({"op":"putData","collection":"records","id":ID,"data":{"name":"one mutation"}});
    assert!(tiny.execute(&frame(&create)).is_err());
    fixture.assert_released(FilesystemProfile::Native);
    let retry = execute(&normal, &create);
    assert_eq!(retry["ok"], false);
    assert!(
        retry["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );
    let response = success(
        &normal,
        &json!({"op":"findDocs","collection":"records","query":{}}),
    );
    let records = response["result"].as_object().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records.values().next().unwrap()["name"], "one mutation");
}

#[test]
fn a_concurrent_invocation_waits_for_the_root_within_its_budget() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(FilesystemProfile::Native);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":"records"}),
    );
    let holder = RootLease::acquire(&fixture.root).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        holder.release_checked().unwrap();
    });
    success(
        &runtime,
        &json!({"op":"putData","collection":"records","id":ID,"data":{"name":"waited"}}),
    );
    release.join().unwrap();
    // A budget that ends while the root is still held returns the lock error.
    let holder = RootLease::acquire(&fixture.root).unwrap();
    let short = InvocationRuntime::new(
        InvocationOptions::new(
            fixture.options(FilesystemProfile::Native),
            ApplicationConfig::default(),
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_millis(400),
                Duration::from_millis(100),
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let error = short
        .execute(&frame(
            &json!({"op":"getDoc","collection":"records","id":ID}),
        ))
        .unwrap_err();
    assert!(error.to_string().contains("EROOTLOCKED"), "{error}");
    holder.release_checked().unwrap();
}

#[test]
fn existing_native_streaming_accepts_multiple_frames_and_preserves_legacy_defaults() {
    let fixture = Fixture::new();
    let requests = [
        json!({"op":"handshake"}),
        json!({"op":"createCollection","collection":"records"}),
    ];
    let mut input = Cursor::new(requests.iter().flat_map(frame).collect::<Vec<_>>());
    let mut output = Vec::new();
    serve_application(
        &mut input,
        &mut output,
        Some(fixture.root.clone()),
        FrameLimits::default(),
        ApplicationConfig::default(),
        false,
        true,
    )
    .unwrap();
    let frames: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["ok"], true);
    assert_eq!(frames[1]["ok"], true);
    assert_eq!(
        frames[0]["result"]["storage"]["filesystemProfile"],
        "native"
    );
    assert_eq!(frames[0]["result"]["storage"]["rootLock"]["owned"], false);
    assert_eq!(
        frames[0]["result"]["storage"]["rootLock"]["kernelEnforced"],
        false
    );
    assert!(
        !fixture
            .root
            .join(".fylo-catalog/filesystem-profile.json")
            .exists()
    );
}

#[test]
fn unique_checked_release_removes_its_owner_metadata_and_admits_an_alias_contender() {
    let fixture = Fixture::new();
    let alias = fixture.parent.join("alias");
    symlink(&fixture.root, &alias).unwrap();
    let lease = RootLease::acquire(&fixture.root).unwrap();
    assert!(lease.kernel_enforced());
    let lock_path = fixture.metadata_path().with_extension("");
    lease.release_checked().unwrap();
    assert!(!fixture.metadata_path().exists());
    assert!(
        lock_path.is_file(),
        "checked release removed the shared lock inode"
    );
    let contender = RootLease::acquire(&alias).unwrap();
    assert_eq!(contender.root(), fs::canonicalize(&fixture.root).unwrap());
    contender.release_checked().unwrap();
}

#[test]
fn escaped_clone_refuses_checked_release_and_keeps_the_actual_root_locked() {
    let fixture = Fixture::new();
    let lease = RootLease::acquire(&fixture.root).unwrap();
    let escaped = lease.clone();
    let before = fs::read(fixture.metadata_path()).unwrap();
    assert_eq!(
        lease.release_checked().unwrap_err().code(),
        NativeStorageErrorCode::RootLeaseLost
    );
    assert_eq!(fs::read(fixture.metadata_path()).unwrap(), before);
    escaped.assert_owned().unwrap();
    assert_eq!(
        RootLease::acquire(&fixture.root).unwrap_err().code(),
        NativeStorageErrorCode::RootLocked
    );
    drop(escaped);
    RootLease::acquire(&fixture.root)
        .unwrap()
        .release_checked()
        .unwrap();
}

#[test]
fn checked_release_preserves_foreign_owner_metadata_on_ownership_loss() {
    let fixture = Fixture::new();
    let lease = RootLease::acquire(&fixture.root).unwrap();
    let mut owner: Value =
        serde_json::from_slice(&fs::read(fixture.metadata_path()).unwrap()).unwrap();
    owner["owner"] = json!("foreign-generation-must-remain");
    let foreign = serde_json::to_vec(&owner).unwrap();
    fs::write(fixture.metadata_path(), &foreign).unwrap();
    assert_eq!(
        lease.release_checked().unwrap_err().code(),
        NativeStorageErrorCode::RootLeaseLost
    );
    assert_eq!(fs::read(fixture.metadata_path()).unwrap(), foreign);
}

#[test]
fn checked_release_fails_closed_on_missing_corrupt_or_symlinked_owner_metadata() {
    for invalid in ["missing", "corrupt", "symlink"] {
        let fixture = Fixture::new();
        let lease = RootLease::acquire(&fixture.root).unwrap();
        let metadata = fixture.metadata_path();
        let outside = fixture.parent.join("outside-owner.json");
        let original = fs::read(&metadata).unwrap();
        match invalid {
            "missing" => fs::remove_file(&metadata).unwrap(),
            "corrupt" => fs::write(&metadata, b"not JSON").unwrap(),
            "symlink" => {
                fs::write(&outside, &original).unwrap();
                fs::remove_file(&metadata).unwrap();
                symlink(&outside, &metadata).unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            lease.release_checked().unwrap_err().code(),
            NativeStorageErrorCode::RootLeaseLost
        );
        match invalid {
            "missing" => assert!(!metadata.exists()),
            "corrupt" => assert_eq!(fs::read(&metadata).unwrap(), b"not JSON"),
            "symlink" => {
                assert!(
                    fs::symlink_metadata(&metadata)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
                assert_eq!(fs::read(&outside).unwrap(), original);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn legacy_drop_still_leaves_diagnostic_metadata_and_allows_the_next_owner() {
    let fixture = Fixture::new();
    let lease = RootLease::acquire(&fixture.root).unwrap();
    let before = fs::read(fixture.metadata_path()).unwrap();
    drop(lease);
    assert_eq!(fs::read(fixture.metadata_path()).unwrap(), before);
    RootLease::acquire(&fixture.root)
        .unwrap()
        .release_checked()
        .unwrap();
}
