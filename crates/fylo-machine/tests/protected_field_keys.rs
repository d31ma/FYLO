//! `$encrypted` fields of a protected `.vdbx` database (ADR 0021 section 7):
//! each collection's values are encrypted with a key derived from that
//! collection's key, so only identities granted the collection read them,
//! and `fylo.encryption.key` is never needed or used to write.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_identity::{Identity, KeyReference};
use fylo_machine::{ApplicationConfig, FrameLimits, serve_application};
use fylo_storage_native::{
    AccessAction, AccessGrantRequest, AccessRule, AccessTrust, DatabaseIdentity, NativeRoot,
    NativeWriteRoot, RootOpenOptions,
};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

const PII: &str = "pii";
const CY: &str = "4VRNF52JPCO";
const BO: &str = "4VRNF52JPCP";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";

struct Fixture {
    parent: PathBuf,
}

impl Fixture {
    /// A folder holding the identities `admin` and `reader`.
    fn new() -> Self {
        let parent = std::env::temp_dir().join(format!(
            "fylo-protected-field-keys-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent).unwrap();
        let fixture = Self {
            parent: fs::canonicalize(&parent).unwrap(),
        };
        Identity::create(&fixture.key("admin")).unwrap();
        Identity::create(&fixture.key("reader")).unwrap();
        fixture
    }

    fn key(&self, name: &str) -> KeyReference {
        KeyReference::File(self.parent.join(format!("{name}.pem")))
    }

    fn identity(&self, name: &str) -> DatabaseIdentity {
        DatabaseIdentity::new(
            Identity::open(&self.key(name)).unwrap(),
            AccessTrust::FirstUse,
        )
    }

    fn database(&self) -> PathBuf {
        self.parent.join("app.vdbx")
    }

    /// Answer `requests` in one session as `who`, with the shared field
    /// key or without it, then close the database so the next session may
    /// open it as another identity.
    fn session(&self, who: &str, keyed: bool, requests: &[Value]) -> Vec<Value> {
        let application = ApplicationConfig {
            schema: None,
            credentials: keyed.then(|| (KEY.into(), "fixture-salt".into())),
            identity: Some(self.identity(who)),
            hot_set_bytes: None,
            ..ApplicationConfig::default()
        };
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(self.database());
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        serve_application(
            &mut Cursor::new(input.into_bytes()),
            &mut output,
            None,
            FrameLimits::default(),
            application,
            false,
            false,
        )
        .unwrap();
        NativeRoot::close_database(self.database()).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn succeed(&self, who: &str, keyed: bool, requests: &[Value]) -> Vec<Value> {
        self.session(who, keyed, requests)
            .into_iter()
            .zip(requests)
            .map(|(reply, request)| {
                assert_eq!(reply["ok"], json!(true), "{request} failed: {reply}");
                reply["result"].clone()
            })
            .collect()
    }

    fn open(&self, who: &str) -> NativeRoot {
        NativeRoot::open_profiled(
            RootOpenOptions::new(self.database()).with_identity(self.identity(who)),
        )
        .unwrap()
    }

    /// Create `pii` as the admin, with `ssn` in its encrypted-fields record
    /// (a `.vdbx` database takes no `fylo.schema` folder).
    fn create(&self) {
        self.succeed(
            "admin",
            false,
            &[json!({"op":"createCollection","collection":PII})],
        );
        self.writer("admin")
            .record_encrypted_fields(PII, &["ssn".to_owned()])
            .unwrap();
        NativeRoot::close_database(self.database()).unwrap();
    }

    fn writer(&self, who: &str) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(
            RootOpenOptions::new(self.database()).with_identity(self.identity(who)),
        )
        .unwrap()
    }

    /// The stored `ssn` of `id`, as the admin reads it from storage.
    fn stored_ssn(&self, id: &str) -> Value {
        let stored = self
            .open("admin")
            .collection(PII)
            .unwrap()
            .read_document(id)
            .unwrap();
        NativeRoot::close_database(self.database()).unwrap();
        serde_json::from_slice::<Value>(&stored.bytes).unwrap()["ssn"].clone()
    }

    /// Grant `reader` the action `action` on `pii`, as rule `rule`.
    fn grant(&self, rule: &str, action: AccessAction) {
        let reader = Identity::open(&self.key("reader")).unwrap();
        self.writer("admin")
            .grant_access(&AccessGrantRequest {
                keys: vec![reader.public()],
                label: None,
                rule: AccessRule {
                    id: rule.into(),
                    identity: "reader".into(),
                    actions: vec![action],
                    resources: vec![format!("collection:{PII}")],
                },
            })
            .unwrap();
        NativeRoot::close_database(self.database()).unwrap();
    }

    fn revoke(&self, rule: &str) {
        self.writer("admin").revoke_access(rule).unwrap();
        NativeRoot::close_database(self.database()).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.database());
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn put(id: &str, name: &str, ssn: &str) -> Value {
    json!({"op":"putData","collection":PII,"id":id,"data":{"name":name,"ssn":ssn}})
}

fn get(id: &str) -> Value {
    json!({"op":"getDoc","collection":PII,"id":id})
}

fn is_ciphertext(value: &Value) -> bool {
    value.as_str().is_some_and(|text| text.starts_with("v2."))
}

#[test]
fn an_admin_without_the_shared_key_encrypts_and_reads_its_fields() {
    let fixture = Fixture::new();
    fixture.create();
    let replies = fixture.succeed("admin", false, &[put(CY, "Cy", "4444"), get(CY)]);
    assert_eq!(replies[1][CY]["ssn"], "4444");
    assert!(is_ciphertext(&fixture.stored_ssn(CY)));
}

#[test]
fn values_are_not_under_the_shared_key_even_when_it_is_configured() {
    let fixture = Fixture::new();
    fixture.create();
    fixture.succeed("admin", true, &[put(CY, "Cy", "4444")]);
    fixture.grant("readers", AccessAction::Read);
    // The reader has no shared key, so it reads the value with the field
    // key of the collection it was granted.
    let replies = fixture.succeed("reader", false, &[get(CY)]);
    assert_eq!(replies[0][CY]["ssn"], "4444");
}

#[test]
fn a_revocation_starts_a_new_field_key_and_keeps_earlier_values_readable() {
    let fixture = Fixture::new();
    fixture.create();
    fixture.succeed("admin", false, &[put(CY, "Cy", "4444")]);
    fixture.grant("writers", AccessAction::Write);
    fixture.revoke("writers");
    let keys = fixture.open("admin").field_keys(PII).unwrap().unwrap();
    NativeRoot::close_database(fixture.database()).unwrap();
    assert_eq!(keys.all().len(), 2);
    assert_eq!(keys.newest().unwrap().epoch(), 2);
    let replies = fixture.succeed("admin", false, &[put(BO, "Bo", "9999"), get(CY), get(BO)]);
    assert_eq!(replies[1][CY]["ssn"], "4444");
    assert_eq!(replies[2][BO]["ssn"], "9999");
}

#[test]
fn a_collection_the_identity_holds_no_key_of_is_refused_its_field_keys() {
    let fixture = Fixture::new();
    fixture.create();
    fixture.grant("database", AccessAction::Read);
    let reader = fixture.open("reader");
    let refused = reader.field_keys("ledger").unwrap_err();
    assert_eq!(refused.code().as_str(), "EACCES");
}

#[test]
fn a_database_that_is_not_protected_has_no_field_keys() {
    let fixture = Fixture::new();
    let root = fixture.parent.join("root");
    fs::create_dir_all(&root).unwrap();
    assert!(
        NativeRoot::open(&root)
            .unwrap()
            .field_keys(PII)
            .unwrap()
            .is_none()
    );
}
