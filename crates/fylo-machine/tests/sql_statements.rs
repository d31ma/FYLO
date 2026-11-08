//! SQL statements and missing collections over the machine protocol:
//! `CREATE TABLE` and `DROP TABLE` manage document collections, `EXPLAIN`
//! describes a statement without executing it, `EXPLAIN ANALYZE` executes it,
//! and a collection that does not exist is reported as absent or not found.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::{FrameLimits, serve};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fylo-machine-sql-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(fs::canonicalize(directory).unwrap())
    }

    fn session(&self, requests: &[Value]) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut reader = Cursor::new(input.into_bytes());
        let mut output = Vec::new();
        serve(
            &mut reader,
            &mut output,
            Some(self.0.clone()),
            FrameLimits::default(),
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn with_one_post(&self) {
        let frames = self.session(&[
            sql("CREATE TABLE posts"),
            sql("INSERT INTO posts (title) VALUES ('Hello')"),
        ]);
        assert_eq!(frames[0]["ok"], json!(true), "{}", frames[0]);
        assert_eq!(frames[1]["ok"], json!(true), "{}", frames[1]);
    }

    fn post_count(&self) -> usize {
        let frames = self.session(&[json!({"op":"findDocs","collection":"posts","query":{}})]);
        assert_eq!(frames[0]["ok"], json!(true), "{}", frames[0]);
        frames[0]["result"].as_object().unwrap().len()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sql(statement: &str) -> Value {
    json!({"op":"executeSQL","sql":statement})
}

fn inspect(collection: &str) -> Value {
    json!({"op":"inspectCollection","collection":collection})
}

#[test]
fn inspect_collection_reports_a_missing_collection_as_absent() {
    let fixture = Fixture::new();
    let frames = fixture.session(&[inspect("nope")]);
    assert_eq!(frames[0]["ok"], json!(true), "{}", frames[0]);
    assert_eq!(
        frames[0]["result"],
        json!({
            "collection": "nope",
            "exists": false,
            "worm": false,
            "docsStored": 0,
            "deletedDocs": 0,
            "indexedDocs": 0,
        })
    );
}

#[test]
fn reading_a_missing_collection_reports_not_found() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[
        json!({"op":"getDoc","collection":"nope","id":"4WNMX8R4TRG"}),
        json!({"op":"findDocs","collection":"nope","query":{}}),
    ]);
    assert_eq!(frames[0]["error"]["code"], json!("ENATIVE_NOT_FOUND"));
    assert_eq!(
        frames[0]["error"]["message"],
        json!("EENGINE_STORAGE: ENATIVE_NOT_FOUND: collection \"nope\" does not exist")
    );
    assert_eq!(frames[1]["error"]["code"], json!("ENATIVE_NOT_FOUND"));
}

#[test]
fn create_table_creates_a_document_collection() {
    let fixture = Fixture::new();
    let frames = fixture.session(&[sql("CREATE TABLE posts"), inspect("posts")]);
    assert_eq!(
        frames[0]["result"],
        json!({"collection":"posts","kind":"document"})
    );
    assert_eq!(frames[1]["result"]["exists"], json!(true));
    assert_eq!(frames[1]["result"]["kind"], json!("document"));
}

#[test]
fn drop_table_drops_the_collection() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[sql("DROP TABLE posts"), inspect("posts")]);
    assert_eq!(frames[0]["result"], json!({"collection":"posts"}));
    assert_eq!(frames[1]["result"]["exists"], json!(false));
}

#[test]
fn explain_describes_a_select_without_executing_it() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[sql("EXPLAIN SELECT * FROM posts WHERE title = 'Hello'")]);
    assert_eq!(
        frames[0]["result"],
        json!({
            "operation": "SELECT",
            "collection": "posts",
            "access": [{"kind":"prefix-index","field":"title","operators":["$eq"]}],
            "executed": false,
        })
    );
}

#[test]
fn explain_leaves_the_data_of_a_delete_unchanged() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[sql("EXPLAIN DELETE FROM posts WHERE title = 'Hello'")]);
    assert_eq!(frames[0]["result"]["operation"], json!("DELETE"));
    assert_eq!(frames[0]["result"]["executed"], json!(false));
    assert_eq!(fixture.post_count(), 1);
}

#[test]
fn explain_leaves_the_data_of_an_insert_and_an_update_unchanged() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[
        sql("EXPLAIN INSERT INTO posts (title) VALUES ('Other')"),
        sql("EXPLAIN UPDATE posts SET title = 'Changed' WHERE title = 'Hello'"),
        sql("SELECT * FROM posts WHERE title = 'Hello'"),
    ]);
    assert_eq!(frames[0]["result"]["executed"], json!(false));
    assert_eq!(frames[1]["result"]["executed"], json!(false));
    assert_eq!(frames[2]["result"].as_object().unwrap().len(), 1);
    assert_eq!(fixture.post_count(), 1);
}

#[test]
fn explain_does_not_create_or_drop_a_collection() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[
        sql("EXPLAIN CREATE TABLE drafts"),
        sql("EXPLAIN DROP TABLE posts"),
        inspect("drafts"),
        inspect("posts"),
    ]);
    assert_eq!(frames[0]["result"]["executed"], json!(false));
    assert_eq!(frames[1]["result"]["executed"], json!(false));
    assert_eq!(frames[2]["result"]["exists"], json!(false));
    assert_eq!(frames[3]["result"]["exists"], json!(true));
}

#[test]
fn explain_analyze_executes_the_statement_and_reports_the_result() {
    let fixture = Fixture::new();
    fixture.with_one_post();
    let frames = fixture.session(&[sql(
        "EXPLAIN ANALYZE DELETE FROM posts WHERE title = 'Hello'",
    )]);
    let result = &frames[0]["result"];
    assert_eq!(result["operation"], json!("DELETE"));
    assert_eq!(result["executed"], json!(true));
    assert_eq!(result["result"], json!(1));
    assert!(result["elapsedMs"].is_u64());
    assert_eq!(fixture.post_count(), 0);
}
