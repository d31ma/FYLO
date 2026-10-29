//! The browser's validator against the native one (ADR 0017 section 4).
//!
//! A WebAssembly build validates with the `compiled` bytes a native push
//! stored in the schema registry; a native build recompiles `chex`. These
//! tests push the CHEX corpus through a real registry, load each head back
//! from its stored file both ways, and require identical answers and
//! messages on every document. They also hold the fail-closed rule: whatever
//! a build without a compiler cannot load refuses with `ESCHEMAUNAVAILABLE`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_storage_native::CollectionKind;
use serde_json::{Map, Value, json};

use crate::{EngineErrorCode, WriteEngine};

const COLLECTION: &str = "corpus";
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A root and a `fylo.schema` folder, removed on drop.
struct Fixture {
    parent: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-stored-validation-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(parent.join("root")).unwrap();
        fs::create_dir_all(parent.join("schemas")).unwrap();
        Self { parent }
    }

    fn root(&self) -> PathBuf {
        self.parent.join("root")
    }

    fn schemas(&self) -> PathBuf {
        self.parent.join("schemas")
    }

    /// An engine with the folder attached; each call reopens the root.
    fn engine(&self) -> WriteEngine {
        WriteEngine::open(self.root())
            .unwrap()
            .with_schema_configuration(self.schemas(), None)
            .unwrap()
    }

    /// Author `chex` as `v1` of the collection in the folder.
    fn author(&self, chex: &Value) {
        let folder = self.schemas().join(COLLECTION);
        fs::create_dir_all(folder.join("history")).unwrap();
        fs::write(
            folder.join("manifest.json"),
            serde_json::to_vec(&json!({ "current": "v1", "versions": [{ "v": "v1" }] })).unwrap(),
        )
        .unwrap();
        fs::write(
            folder.join("history").join("v1.schema.json"),
            serde_json::to_vec(chex).unwrap(),
        )
        .unwrap();
    }

    /// Author `chex`, create the collection and push it into the registry.
    fn pushed(&self, chex: &Value) -> WriteEngine {
        self.author(chex);
        let engine = self.engine();
        create_collection(&engine);
        engine.schema_push(COLLECTION, None, None).unwrap();
        engine
    }

    fn record_path(&self) -> PathBuf {
        self.root()
            .join(".fylo-schemas")
            .join(COLLECTION)
            .join("1.json")
    }

    /// Rewrite the committed record, as damage or a hostile file would.
    fn tamper(&self, change: impl FnOnce(&mut Map<String, Value>)) {
        let mut record: Value =
            serde_json::from_slice(&fs::read(self.record_path()).unwrap()).unwrap();
        change(record.as_object_mut().unwrap());
        fs::write(
            self.record_path(),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn create_collection(engine: &WriteEngine) {
    engine
        .writer
        .create_collection(COLLECTION, CollectionKind::Document, None)
        .unwrap();
}

fn corpus_fixture(name: &str) -> Value {
    let path = format!(
        "{}/../fylo-chex/tests/fixtures/valid/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
}

/// Every way to perturb one property of `data`, as the CHEX parity suite
/// does, and the same one level down.
fn mutations(data: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let replacements = [
        Value::Null,
        json!("zz"),
        json!("Jane Doe"),
        json!("héllo"),
        json!("HELLO"),
        json!(7),
        json!(1.5),
        json!(-3),
        json!(true),
        json!([]),
        json!(["a", 1]),
        json!(["typescript", "Ünïcode"]),
        json!({}),
        json!({ "city": "Paris" }),
        json!({ "Bad Key": "x" }),
    ];
    let mut out = vec![data.clone()];
    for key in data.keys() {
        let mut removed = data.clone();
        removed.shift_remove(key);
        out.push(removed);
        for replacement in &replacements {
            let mut changed = data.clone();
            changed.insert(key.clone(), replacement.clone());
            out.push(changed);
        }
    }
    let mut extra = data.clone();
    extra.insert("unexpected".into(), json!("x"));
    out.push(extra);
    for (key, value) in data {
        if let Value::Object(nested) = value {
            for changed in mutations(nested) {
                let mut document = data.clone();
                document.insert(key.clone(), Value::Object(changed));
                out.push(document);
            }
        }
    }
    out
}

/// The CHEX corpus whose schemas a push accepts (`order` declares an array
/// of objects, which a push refuses with `EARRAYOFOBJECTS`), and a schema
/// leaning on Unicode classes, case folding and `$encrypted`.
fn corpus() -> Vec<(&'static str, Value, Value)> {
    let mut cases: Vec<(&'static str, Value, Value)> = ["person", "measure", "status"]
        .into_iter()
        .map(|name| {
            (
                name,
                corpus_fixture(&format!("{name}.schema.json")),
                corpus_fixture(&format!("{name}.data.json")),
            )
        })
        .collect();
    cases.push((
        "unicode",
        json!({
            "$encrypted": ["email"],
            "email": "^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$",
            "name?": "^\\w+$",
            "greeting": "(?i)^hello",
            "city": "^\\w[\\w ]*$",
            "when": "^\\d{4}-\\d{2}-\\d{2}$",
            "labels": { "^[a-z]+$": "^\\S+$" },
            "tags": ["^[a-zà-ÿ]+$"]
        }),
        json!({
            "email": "a@b.example",
            "name": "héllo",
            "greeting": "HELLO there",
            "city": "São Paulo",
            "when": "2026-10-29",
            "labels": { "en": "Hello", "fr": "Bonjour" },
            "tags": ["café", "thé"]
        }),
    ));
    cases
}

#[test]
fn stored_validators_answer_exactly_as_native_compilation_does() {
    let mut compared = 0;
    let mut rejected = 0;
    for (name, chex, data) in corpus() {
        let fixture = Fixture::new();
        drop(fixture.pushed(&chex));
        // A fresh engine reads the head back from the registry file.
        let engine = fixture.engine();
        let head = engine.registry_head(COLLECTION).unwrap().unwrap();
        assert_eq!(head.record.portable, Some(true), "{name}");
        engine.require_stored_validator(COLLECTION).unwrap();
        let stored = head.stored_validator(COLLECTION).unwrap();
        let Value::Object(data) = data else {
            unreachable!("corpus data is an object")
        };
        for document in mutations(&data) {
            let native = head.validate(COLLECTION, &document);
            let browser = stored.validate(&document).map_err(|error| error.message);
            assert_eq!(browser, native, "{name}: {document:?}");
            compared += 1;
            rejected += usize::from(native.is_err());
        }
    }
    assert!(compared > 300, "only {compared} documents compared");
    assert!(
        rejected > 100 && rejected < compared,
        "{rejected} of {compared} rejected"
    );
}

fn unavailable(engine: &WriteEngine) -> String {
    let error = engine.require_stored_validator(COLLECTION).unwrap_err();
    assert_eq!(error.code(), EngineErrorCode::SchemaUnavailable, "{error}");
    assert_eq!(error.code().as_str(), "ESCHEMAUNAVAILABLE");
    error.to_string()
}

#[test]
fn a_non_portable_head_is_unavailable() {
    let fixture = Fixture::new();
    drop(fixture.pushed(&json!({ "word": "\\bword\\b" })));
    let engine = fixture.engine();
    // Native still validates it with `regex`.
    let head = engine.registry_head(COLLECTION).unwrap().unwrap();
    assert_eq!(head.record.portable, Some(false));
    head.validate(COLLECTION, json!({ "word": "a word" }).as_object().unwrap())
        .unwrap();
    assert!(unavailable(&engine).contains("is not portable"));
}

#[test]
fn a_head_without_stored_bytes_is_unavailable() {
    let fixture = Fixture::new();
    drop(fixture.pushed(&json!({ "name": "^[a-z]+$" })));
    fixture.tamper(|record| {
        record.remove("compiled");
    });
    assert!(unavailable(&fixture.engine()).contains("has no compiled validator"));

    // A record that never said it was portable is not trusted to be.
    fixture.tamper(|record| {
        record.remove("portable");
    });
    assert!(unavailable(&fixture.engine()).contains("is not portable"));
}

#[test]
fn stored_bytes_the_loader_refuses_are_unavailable() {
    let fixture = Fixture::new();
    drop(fixture.pushed(&json!({ "name": "^[a-z]+$" })));
    let original: Value =
        serde_json::from_slice(&fs::read(fixture.record_path()).unwrap()).unwrap();
    let compiled = original["compiled"].as_str().unwrap().to_owned();

    fixture.tamper(|record| {
        record.insert("compiled".into(), json!("not base64url!"));
    });
    assert!(unavailable(&fixture.engine()).contains("not base64url"));

    // One flipped byte: the envelope's checksum refuses it.
    let mut bytes = crate::encryption::decode_base64_url(&compiled).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x40;
    fixture.tamper(|record| {
        record.insert(
            "compiled".into(),
            json!(crate::encryption::encode_base64_url(&bytes)),
        );
    });
    assert!(unavailable(&fixture.engine()).contains("this build refuses"));
}

#[test]
fn stored_bytes_compiled_from_another_schema_are_unavailable() {
    let fixture = Fixture::new();
    drop(fixture.pushed(&json!({ "name": "^[a-z]+$" })));
    // A well-formed envelope, but of a schema that accepts anything.
    let lenient =
        fylo_chex::CompiledSchema::compile(&json!({ "name": ".*" }), "corpus@v1").unwrap();
    fixture.tamper(|record| {
        record.insert(
            "compiled".into(),
            json!(crate::encryption::encode_base64_url(&lenient.to_bytes())),
        );
    });
    assert!(unavailable(&fixture.engine()).contains("was not compiled from its chex"));

    // The right schema under another collection's label is refused too.
    let relabelled =
        fylo_chex::CompiledSchema::compile(&json!({ "name": "^[a-z]+$" }), "other@v1").unwrap();
    fixture.tamper(|record| {
        record.insert(
            "compiled".into(),
            json!(crate::encryption::encode_base64_url(&relabelled.to_bytes())),
        );
    });
    assert!(unavailable(&fixture.engine()).contains("was not compiled from its chex"));
}

#[test]
fn a_schema_only_in_the_folder_is_unavailable() {
    let fixture = Fixture::new();
    fixture.author(&json!({ "name": "^[a-z]+$" }));
    let engine = fixture.engine();
    create_collection(&engine);
    assert!(unavailable(&engine).contains("no schema registry"));

    // A flat `<collection>.schema.json` is a schema too.
    fs::remove_dir_all(fixture.schemas().join(COLLECTION)).unwrap();
    engine.require_stored_validator(COLLECTION).unwrap();
    fs::write(
        fixture.schemas().join(format!("{COLLECTION}.schema.json")),
        b"{\"name\":\"^[a-z]+$\"}",
    )
    .unwrap();
    assert!(unavailable(&engine).contains("no schema registry"));
}

#[test]
fn a_collection_without_a_schema_needs_no_validator() {
    let fixture = Fixture::new();
    let engine = fixture.engine();
    create_collection(&engine);
    engine.require_stored_validator(COLLECTION).unwrap();
    drop(engine);
    // Nor does one opened without any folder.
    WriteEngine::open(fixture.root())
        .unwrap()
        .require_stored_validator(COLLECTION)
        .unwrap();
}

#[test]
fn a_recorded_validation_mode_without_a_registry_is_unavailable() {
    use fylo_storage_native::ValidationMode;

    let fixture = Fixture::new();
    let engine = WriteEngine::open(fixture.root()).unwrap();
    create_collection(&engine);
    // A browser has no `fylo.schema` folder to look in, but a collection
    // whose descriptor asks for validation is meant to have a schema.
    for mode in [ValidationMode::Strict, ValidationMode::Warn] {
        engine.set_validation_mode(COLLECTION, mode).unwrap();
        assert!(unavailable(&engine).contains("no schema registry"));
    }
    engine
        .set_validation_mode(COLLECTION, ValidationMode::Off)
        .unwrap();
    engine.require_stored_validator(COLLECTION).unwrap();
}
