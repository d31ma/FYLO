//! A batch of document writes to a `.vdbx` collection is one segment plan:
//! it commits whole or not at all, as on the files layout (ADR 0013).

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::identifier;
use crate::{
    CollectionKind, DocumentWrite, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PlainDocumentCodec, PutDocumentOptions,
};

struct Database(PathBuf);

impl Database {
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self(std::fs::canonicalize(directory).unwrap());
        NativeRoot::create_database(database.path()).unwrap();
        database
            .writer()
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        database
    }

    fn path(&self) -> PathBuf {
        self.0.join("db.vdbx")
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(self.path()).unwrap()
    }

    fn name_of(&self, number: usize) -> Option<Value> {
        let items = NativeRoot::open(self.path())
            .unwrap()
            .collection("items")
            .unwrap();
        let stored = items.read_document(&identifier(number)).ok()?;
        let document: Value = serde_json::from_slice(&stored.bytes).unwrap();
        Some(document["name"].clone())
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn named(name: &str) -> Map<String, Value> {
    json!({ "name": name }).as_object().unwrap().clone()
}

fn put_one(writer: &NativeWriteRoot, number: usize, name: &str) {
    let body = json!({ "name": name }).to_string();
    writer
        .put_document(
            "items",
            &identifier(number),
            body.as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

#[test]
fn a_batch_puts_replaces_and_deletes_together() {
    let database = Database::create("vdbx-batch-commit");
    let writer = database.writer();
    put_one(&writer, 1, "one");
    put_one(&writer, 2, "two");
    let batch = [
        DocumentWrite::Replace {
            identifier: identifier(1),
            fields: named("one again"),
        },
        DocumentWrite::Delete {
            identifier: identifier(2),
        },
        DocumentWrite::Put {
            identifier: identifier(3),
            fields: named("three"),
        },
    ];

    writer
        .commit_document_writes_with("items", &batch, &PlainDocumentCodec)
        .unwrap();
    drop(writer);

    assert_eq!(database.name_of(1), Some(json!("one again")));
    assert_eq!(database.name_of(2), None);
    assert_eq!(database.name_of(3), Some(json!("three")));
}

#[test]
fn a_batch_with_one_refused_write_writes_nothing() {
    let database = Database::create("vdbx-batch-refused");
    let writer = database.writer();
    put_one(&writer, 1, "one");
    let batch = [
        DocumentWrite::Put {
            identifier: identifier(2),
            fields: named("two"),
        },
        DocumentWrite::Put {
            identifier: identifier(1),
            fields: named("one, twice"),
        },
    ];

    let refused = writer
        .commit_document_writes_with("items", &batch, &PlainDocumentCodec)
        .unwrap_err();
    drop(writer);

    assert_eq!(refused.code(), NativeStorageErrorCode::DocumentExists);
    assert_eq!(database.name_of(1), Some(json!("one")));
    assert_eq!(database.name_of(2), None);
}
