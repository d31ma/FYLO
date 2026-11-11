//! `$match` on a word field reads its candidates from the word keys, then
//! only those documents, and returns exactly what a scan of every document
//! returns. A write by a release that does not know word fields is detected
//! and never makes a `$match` incomplete (ADR 0023).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, CollectionLayout, NativeRoot, NativeWriteRoot};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    const COLLECTION: &str = "messages";
    const BODIES: [&str; 8] = [
        "Meeting with Tomás on Monday",
        "Lunch menu: café crème and croissants",
        "The quarterly MEETING notes are attached",
        "Re: Re: meet-up at 2026-11-11?",
        "Straße closed; use the north road",
        "Tom says hello",
        "",
        "Résumé attached for the opening",
    ];

    fn new(layout: CollectionLayout) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-words-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        NativeWriteRoot::open(&root)
            .unwrap()
            .create_collection_with_layout(
                Self::COLLECTION,
                CollectionKind::Document,
                None,
                Some(layout),
            )
            .unwrap();
        Self(root)
    }

    fn written(layout: CollectionLayout) -> Self {
        let fixture = Self::new(layout);
        for (number, body) in Self::BODIES.iter().enumerate() {
            fixture.put(number, &Self::fields(number, body));
        }
        fixture.put(20, &json!({ "conversation": "c-1", "body": 42 }));
        fixture.put(
            21,
            &json!({ "conversation": "c-2", "body": ["tomorrow's agenda", "meeting room 4"] }),
        );
        fixture.put(22, &json!({ "conversation": "c-1" }));
        fixture
    }

    fn id(number: usize) -> String {
        format!("4VRNF52J{number:03}")
    }

    fn fields(number: usize, body: &str) -> Value {
        let conversation = if number.is_multiple_of(2) {
            "c-1"
        } else {
            "c-2"
        };
        json!({ "conversation": conversation, "body": body, "meta": { "subject": body } })
    }

    fn put(&self, number: usize, fields: &Value) {
        WriteEngine::open(&self.0)
            .unwrap()
            .put_document(
                Self::COLLECTION,
                &Self::id(number),
                fields.as_object().unwrap().clone(),
            )
            .unwrap();
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0).unwrap()
    }

    fn set_words(&self, fields: &[&str]) -> Vec<String> {
        let fields: Vec<String> = fields.iter().map(|field| (*field).to_owned()).collect();
        self.writer()
            .set_word_fields(Self::COLLECTION, &fields, &[])
            .unwrap()
    }

    fn complete_words(&self) -> Vec<String> {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection(Self::COLLECTION)
            .unwrap()
            .complete_word_fields()
            .unwrap()
    }

    fn engine(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn query(query: &Value) -> StructuredQuery {
        StructuredQuery::from_value(query, QueryLimits::default()).unwrap()
    }

    fn find(&self, query: &Value) -> Vec<String> {
        self.engine()
            .find(Self::COLLECTION, &Self::query(query))
            .unwrap()
            .into_iter()
            .map(|row| row.metadata.id)
            .collect()
    }

    /// What reading every document and testing the predicate returns.
    fn scanned(&self, query: &Value) -> Vec<String> {
        let parsed = Self::query(query);
        self.engine()
            .find(Self::COLLECTION, &Self::query(&json!({})))
            .unwrap()
            .into_iter()
            .filter(|row| {
                parsed.matches(
                    row.document.fields(),
                    row.metadata.created_at,
                    row.metadata.mtime,
                )
            })
            .map(|row| row.metadata.id)
            .collect()
    }

    fn assert_parity(&self, query: &Value) {
        assert_eq!(self.find(query), self.scanned(query), "{query}");
    }

    /// Make document `number` unreadable, so any query that reads it fails.
    fn damage(&self, number: usize) {
        let name = format!("{}.json", Self::id(number));
        let path = Self::find_file(&self.0.join(".collections"), &name).unwrap();
        fs::write(path, b"not a document").unwrap();
    }

    fn find_file(directory: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(directory).ok()? {
            let path = entry.ok()?.path();
            if path.file_name().is_some_and(|file| file == name) {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = Self::find_file(&path, name)
            {
                return Some(found);
            }
        }
        None
    }

    fn descriptor_path(&self) -> PathBuf {
        self.0
            .join(".fylo-catalog/collections")
            .join(format!("{}.json", Self::COLLECTION))
    }

    /// Run `write` as a release before ADR 0023 would: with a descriptor
    /// that has no `words` field.
    fn write_as_older_release(&self, write: impl FnOnce(&Self)) {
        let path = self.descriptor_path();
        let original = fs::read(&path).unwrap();
        let mut descriptor: Map<String, Value> = serde_json::from_slice(&original).unwrap();
        assert!(descriptor.remove("words").is_some());
        fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
        write(self);
        fs::write(&path, original).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn match_queries() -> Vec<Value> {
    vec![
        json!({ "$ops": [{ "body": { "$match": "meet" } }] }),
        json!({ "$ops": [{ "body": { "$match": "MEETING notes" } }] }),
        json!({ "$ops": [{ "body": { "$match": "meet tom" } }] }),
        json!({ "$ops": [{ "body": { "$match": "cafe creme" } }] }),
        json!({ "$ops": [{ "body": { "$match": "café" } }] }),
        json!({ "$ops": [{ "body": { "$match": "strasse" } }] }),
        json!({ "$ops": [{ "body": { "$match": "resume" } }] }),
        json!({ "$ops": [{ "body": { "$match": "2026 11" } }] }),
        json!({ "$ops": [{ "body": { "$match": "42" } }] }),
        json!({ "$ops": [{ "body": { "$match": "agenda room" } }] }),
        json!({ "$ops": [{ "body": { "$match": "nothing-here" } }] }),
        json!({ "$ops": [{ "body": { "$match": "meet" }, "conversation": { "$eq": "c-1" } }] }),
        json!({ "$ops": [{ "body": { "$match": "attached" } }, { "conversation": { "$eq": "c-2" } }] }),
        json!({ "$ops": [{ "meta.subject": { "$match": "lunch" } }] }),
        json!({ "$ops": [{ "meta/subject": { "$match": "north road" } }] }),
        json!({ "$ops": [{ "body": { "$match": "meet" } }], "$limit": 2 }),
    ]
}

#[test]
fn match_queries_match_a_full_scan_with_and_without_word_fields() {
    for layout in [CollectionLayout::Files, CollectionLayout::Segments] {
        let fixture = Fixture::written(layout);
        for query in match_queries() {
            fixture.assert_parity(&query);
        }
        assert_eq!(
            fixture.set_words(&["body", "meta.subject"]),
            ["body", "meta/subject"]
        );
        assert_eq!(fixture.complete_words(), ["body", "meta/subject"]);
        for query in match_queries() {
            fixture.assert_parity(&query);
        }
        assert_eq!(
            fixture.find(&json!({ "$ops": [{ "body": { "$match": "meet tom" } }] })),
            [Fixture::id(0)]
        );
    }
}

#[test]
fn a_match_on_a_word_field_reads_only_its_candidates() {
    for layout in [CollectionLayout::Files, CollectionLayout::Segments] {
        let fixture = Fixture::written(layout);
        fixture.set_words(&["body"]);
        if layout == CollectionLayout::Files {
            fixture.damage(1);
            assert!(
                fixture
                    .engine()
                    .find("messages", &Fixture::query(&json!({})))
                    .is_err()
            );
        }
        assert_eq!(
            fixture.find(&json!({ "$ops": [{ "body": { "$match": "meeting" } }] })),
            [Fixture::id(0), Fixture::id(2), Fixture::id(21)]
        );
        assert_eq!(
            fixture.find(&json!({
                "$ops": [{ "body": { "$match": "meeting" }, "conversation": { "$eq": "c-1" } }]
            })),
            [Fixture::id(0), Fixture::id(2)]
        );
    }
}

#[test]
fn a_match_on_a_field_without_word_keys_reads_every_document() {
    let fixture = Fixture::written(CollectionLayout::Files);
    fixture.damage(1);
    let query = Fixture::query(&json!({ "$ops": [{ "body": { "$match": "meeting" } }] }));
    assert!(fixture.engine().find("messages", &query).is_err());
}

#[test]
fn word_keys_follow_puts_patches_deletes_and_restores() {
    for layout in [CollectionLayout::Files, CollectionLayout::Segments] {
        let fixture = Fixture::written(layout);
        fixture.set_words(&["body"]);
        let writer = fixture.writer();
        writer
            .patch_document("messages", &Fixture::id(1), br#"{"body":"meeting moved"}"#)
            .unwrap();
        writer
            .patch_document("messages", &Fixture::id(0), br#"{"body":"cancelled"}"#)
            .unwrap();
        writer.delete_document("messages", &Fixture::id(2)).unwrap();
        fixture.put(
            30,
            &json!({ "conversation": "c-3", "body": "Meetings galore" }),
        );
        assert_eq!(fixture.complete_words(), ["body"]);
        for query in match_queries() {
            fixture.assert_parity(&query);
        }
        assert_eq!(
            fixture.find(&json!({ "$ops": [{ "body": { "$match": "meeting" } }] })),
            [Fixture::id(1), Fixture::id(21), Fixture::id(30)]
        );
        writer
            .restore_document("messages", &Fixture::id(2))
            .unwrap();
        fixture.assert_parity(&json!({ "$ops": [{ "body": { "$match": "meeting" } }] }));
        assert_eq!(
            fixture
                .find(&json!({ "$ops": [{ "body": { "$match": "meeting" } }] }))
                .len(),
            4
        );
    }
}

#[test]
fn clearing_word_fields_removes_their_keys() {
    let fixture = Fixture::written(CollectionLayout::Files);
    fixture.set_words(&["body"]);
    let snapshot = fixture.0.join(".collections/messages/index/keys.snapshot");
    assert!(
        fs::read_to_string(&snapshot)
            .unwrap()
            .contains("body/w/meeting/")
    );
    assert!(fixture.set_words(&[]).is_empty());
    let keys = fs::read_to_string(&snapshot).unwrap();
    assert!(!keys.contains("/w/"));
    assert!(!keys.contains("/wv/"));
    fixture.assert_parity(&json!({ "$ops": [{ "body": { "$match": "meeting" } }] }));
}

#[test]
fn a_put_by_an_older_release_is_detected_and_still_found() {
    let fixture = Fixture::written(CollectionLayout::Files);
    fixture.set_words(&["body"]);
    fixture.write_as_older_release(|fixture| {
        fixture.put(
            40,
            &json!({ "conversation": "c-9", "body": "meeting from an old binary" }),
        );
    });
    assert!(fixture.complete_words().is_empty());
    let query = json!({ "$ops": [{ "body": { "$match": "meeting" } }] });
    let expected = [
        Fixture::id(0),
        Fixture::id(2),
        Fixture::id(21),
        Fixture::id(40),
    ];
    assert_eq!(fixture.find(&query), expected);
    let repaired = fixture
        .writer()
        .repair_word_fields("messages", &["body".to_owned()])
        .unwrap();
    assert!(repaired);
    assert_eq!(fixture.complete_words(), ["body"]);
    fixture.damage(1);
    assert_eq!(fixture.find(&query), expected);
    assert!(
        !fixture
            .writer()
            .repair_word_fields("messages", &["body".to_owned()])
            .unwrap()
    );
}

#[test]
fn a_patch_or_delete_by_an_older_release_is_detected() {
    let fixture = Fixture::written(CollectionLayout::Files);
    fixture.set_words(&["body"]);
    fixture.write_as_older_release(|fixture| {
        fixture
            .writer()
            .patch_document(
                "messages",
                &Fixture::id(5),
                br#"{"body":"meeting after all"}"#,
            )
            .unwrap();
    });
    assert!(fixture.complete_words().is_empty());
    let query = json!({ "$ops": [{ "body": { "$match": "meeting" } }] });
    assert!(fixture.find(&query).contains(&Fixture::id(5)));
    fixture.assert_parity(&query);

    let fixture = Fixture::written(CollectionLayout::Files);
    fixture.set_words(&["body"]);
    fixture.write_as_older_release(|fixture| {
        fixture
            .writer()
            .delete_document("messages", &Fixture::id(0))
            .unwrap();
    });
    assert!(fixture.complete_words().is_empty());
    fixture.assert_parity(&query);
}

#[test]
fn an_older_write_to_another_field_keeps_the_word_keys_complete() {
    let fixture = Fixture::written(CollectionLayout::Files);
    fixture.set_words(&["body"]);
    fixture.write_as_older_release(|fixture| {
        fixture
            .writer()
            .patch_document("messages", &Fixture::id(0), br#"{"conversation":"c-7"}"#)
            .unwrap();
    });
    assert_eq!(fixture.complete_words(), ["body"]);
}

#[test]
fn word_fields_are_refused_when_they_cannot_be_kept() {
    let fixture = Fixture::written(CollectionLayout::Files);
    let writer = fixture.writer();
    let refuse = |fields: Vec<String>, encrypted: &[String]| {
        let error = writer
            .set_word_fields("messages", &fields, encrypted)
            .unwrap_err();
        assert_eq!(error.code().as_str(), "EQUERY_INVALID", "{fields:?}");
    };
    refuse(
        (0..17).map(|number| format!("field{number}")).collect(),
        &[],
    );
    refuse(vec!["body".into(), "body".into()], &[]);
    refuse(vec!["meta.subject".into(), "meta/subject".into()], &[]);
    refuse(vec![String::new()], &[]);
    refuse(vec!["x".repeat(257)], &[]);
    refuse(vec!["body".into()], &["body".into()]);
    writer
        .record_encrypted_fields("messages", &["meta/subject".into()])
        .unwrap();
    refuse(vec!["meta.subject".into()], &[]);
    writer
        .create_collection("files", CollectionKind::File, None)
        .unwrap();
    let error = writer
        .set_word_fields("files", &["name".into()], &[])
        .unwrap_err();
    assert_eq!(error.code().as_str(), "EQUERY_INVALID");
    assert!(fixture.complete_words().is_empty());
}

#[test]
fn a_match_operand_without_words_is_refused() {
    for operand in [
        json!(""),
        json!(" -- "),
        json!(42),
        json!("a b c d e f g h i j k l m n o p q"),
    ] {
        let query = json!({ "$ops": [{ "body": { "$match": operand } }] });
        let error = StructuredQuery::from_value(&query, QueryLimits::default()).unwrap_err();
        assert_eq!(error.code().as_str(), "EQUERY_INVALID", "{operand}");
    }
}
