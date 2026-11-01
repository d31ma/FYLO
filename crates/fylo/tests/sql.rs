//! `Root::sql`: FYLO collections as Turso SQL tables. Rows are documents,
//! pushdown never changes an answer, writes commit through FYLO, and a
//! transaction writes one collection.
#![cfg(feature = "sql")]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use fylo::{OpenOptions, Root, Sql, SqlOptions, SqlRows, SqlValue};
use fylo_engine::WriteEngine;
use fylo_storage_native::{CollectionKind, NativeWriteRoot};
use serde_json::{Map, Value, json};

struct Fixture {
    parent: PathBuf,
    root: Arc<Root>,
}

impl Fixture {
    /// A root with `products` (head schema `name`, `price?`, `group?`) and an
    /// unschematized `notes` collection.
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::temp_dir().join(format!(
            "fylo-sql-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&parent);
        let root = parent.join("root");
        let schema = parent.join("schemas").join("products");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(schema.join("history")).unwrap();
        fs::write(
            schema.join("history/v1.schema.json"),
            serde_json::to_vec(&json!({"name": "^.+$", "price?": "^-?[0-9.]+$", "group?": "^.+$"}))
                .unwrap(),
        )
        .unwrap();
        fs::write(
            schema.join("manifest.json"),
            serde_json::to_vec(&json!({"current": "v1", "versions": [{"v": "v1"}]})).unwrap(),
        )
        .unwrap();
        let storage = NativeWriteRoot::open(&root).unwrap();
        for name in ["products", "notes"] {
            storage
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        WriteEngine::open(&root)
            .unwrap()
            .schema_push("products", Some(&parent.join("schemas")), None)
            .unwrap();
        let root =
            Arc::new(Root::open(fs::canonicalize(&root).unwrap(), OpenOptions::default()).unwrap());
        Self { parent, root }
    }

    fn sql(&self) -> Sql {
        self.root.sql().unwrap()
    }

    fn scan(&self) -> Sql {
        self.root
            .sql_with(SqlOptions::default().with_pushdown(false))
            .unwrap()
    }

    fn seed(&self, count: i64) -> Vec<String> {
        let products = self.root.collection("products");
        (0..count)
            .map(|n| {
                products
                    .put(fields(&json!({
                        "name": format!("item-{n:04}"),
                        "price": n % 50,
                        "group": format!("g{}", n % 7),
                    })))
                    .unwrap()
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn text(value: &SqlValue) -> &str {
    match value {
        SqlValue::Text(text) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn sorted(mut rows: SqlRows) -> Vec<Vec<SqlValue>> {
    rows.rows
        .sort_by(|left, right| text(&left[0]).cmp(text(&right[0])));
    rows.rows
}

#[test]
fn collections_are_tables_and_rows_are_their_documents() {
    let fixture = Fixture::new();
    let ids = fixture.seed(3);
    let sql = fixture.sql();
    assert_eq!(sql.tables(), ["notes", "products"]);
    let rows = fixture
        .sql()
        .query(
            "SELECT id, name, price, \"group\", doc FROM products ORDER BY price",
            &[],
        )
        .unwrap();
    assert_eq!(rows.columns, ["id", "name", "price", "group", "doc"]);
    assert_eq!(rows.rows.len(), 3);
    assert_eq!(text(&rows.rows[0][0]), ids[0]);
    assert_eq!(rows.rows[0][1], SqlValue::from("item-0000"));
    assert_eq!(rows.rows[2][2], SqlValue::Integer(2));
    let doc: Value = serde_json::from_str(text(&rows.rows[1][4])).unwrap();
    assert_eq!(
        doc,
        json!({"name": "item-0001", "price": 1, "group": "g1", "_v": "v1"})
    );
    let hidden = sql
        .query(
            "SELECT created_at, updated_at, meta FROM products WHERE id = ?",
            &[SqlValue::from(ids[0].as_str())],
        )
        .unwrap();
    assert!(matches!(hidden.rows[0][0], SqlValue::Integer(at) if at > 1_577_836_800_000));
    let extracted = sql
        .query(
            "SELECT json_extract(doc, '$.group') FROM products WHERE price = 2",
            &[],
        )
        .unwrap();
    assert_eq!(extracted.rows, [[SqlValue::from("g2")]]);
}

#[test]
fn pushdown_returns_exactly_what_a_full_scan_returns() {
    let fixture = Fixture::new();
    let ids = fixture.seed(300);
    let pushed = fixture.sql();
    let scanned = fixture.scan();
    let cases: Vec<(&str, Vec<SqlValue>)> = vec![
        (
            "SELECT id, price FROM products WHERE price = ?",
            vec![SqlValue::Integer(7)],
        ),
        (
            "SELECT id, price FROM products WHERE price > ?",
            vec![SqlValue::Integer(45)],
        ),
        (
            "SELECT id, price FROM products WHERE price >= ? AND price < ?",
            vec![SqlValue::Integer(10), SqlValue::Integer(13)],
        ),
        (
            "SELECT id, price FROM products WHERE price <= ?",
            vec![SqlValue::Real(2.5)],
        ),
        (
            "SELECT id, name FROM products WHERE name = ?",
            vec![SqlValue::from("item-0123")],
        ),
        (
            "SELECT id, name FROM products WHERE name > ?",
            vec![SqlValue::from("item-0290")],
        ),
        (
            "SELECT id FROM products WHERE \"group\" = ? AND price > ?",
            vec![SqlValue::from("g3"), SqlValue::Integer(40)],
        ),
        (
            "SELECT id FROM products WHERE id = ?",
            vec![SqlValue::from(ids[42].as_str())],
        ),
        (
            "SELECT id FROM products WHERE id = ? AND price = ?",
            vec![SqlValue::from(ids[42].as_str()), SqlValue::Integer(42)],
        ),
        (
            "SELECT id FROM products WHERE price != ?",
            vec![SqlValue::Integer(3)],
        ),
        (
            "SELECT id FROM products WHERE price = ?",
            vec![SqlValue::from("7")],
        ),
        (
            "SELECT id FROM products WHERE price = ?",
            vec![SqlValue::Null],
        ),
        (
            "SELECT id FROM products WHERE price > 48 OR name = 'item-0001'",
            vec![],
        ),
        (
            "SELECT a.id FROM products a JOIN products b ON a.price = b.price WHERE b.name = ?",
            vec![SqlValue::from("item-0007")],
        ),
    ];
    for (statement, parameters) in cases {
        let with = sorted(pushed.query(statement, &parameters).unwrap());
        let without = sorted(scanned.query(statement, &parameters).unwrap());
        assert_eq!(with, without, "{statement} {parameters:?}");
    }
    let equal = pushed
        .query("SELECT count(*) FROM products WHERE price = 7", &[])
        .unwrap();
    assert_eq!(equal.rows, [[SqlValue::Integer(6)]]);
}

#[test]
fn inserts_updates_and_deletes_commit_through_fylo() {
    let fixture = Fixture::new();
    let sql = fixture.sql();
    let changed = sql
        .execute(
            "INSERT INTO products (doc, price) VALUES (?, ?)",
            &[
                SqlValue::from(r#"{"name":"Ada","group":"g1","price":1}"#),
                SqlValue::Integer(9),
            ],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let id = text(&sql.query("SELECT id FROM products", &[]).unwrap().rows[0][0]).to_owned();
    let products = fixture.root.collection("products");
    assert_eq!(
        products.get(&id).unwrap(),
        Some(fields(
            &json!({"name": "Ada", "group": "g1", "price": 9, "_v": "v1"})
        ))
    );
    sql.execute(
        "UPDATE products SET price = ? WHERE id = ?",
        &[SqlValue::Integer(12), SqlValue::from(id.as_str())],
    )
    .unwrap();
    assert_eq!(products.get(&id).unwrap().unwrap()["price"], json!(12));
    sql.execute(
        "UPDATE products SET doc = json_set(doc, '$.price', 15, '$.city', 'London') WHERE id = ?",
        &[SqlValue::from(id.as_str())],
    )
    .unwrap();
    assert_eq!(
        products.get(&id).unwrap(),
        Some(fields(
            &json!({"name": "Ada", "group": "g1", "price": 15, "city": "London", "_v": "v1"})
        ))
    );
    let note = sql
        .execute(
            "INSERT INTO notes (doc) VALUES ('{\"text\":\"hello\"}')",
            &[],
        )
        .unwrap();
    assert_eq!(note, 1);
    let removed = sql
        .execute("DELETE FROM products WHERE price = 15", &[])
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(products.get(&id).unwrap(), None);
    assert!(
        sql.query("SELECT id FROM products", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn fylo_errors_keep_their_codes_and_a_statement_commits_whole_or_not_at_all() {
    let fixture = Fixture::new();
    let sql = fixture.sql();
    let refused = sql
        .execute(
            "INSERT INTO notes (doc) VALUES ('{\"ok\":1}'), ('{\"tags\":[{\"a\":1}]}')",
            &[],
        )
        .unwrap_err();
    assert_eq!(refused.code(), "EARRAYOFOBJECTS", "{refused}");
    assert!(
        sql.query("SELECT id FROM notes", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    sql.execute("INSERT INTO notes (doc) VALUES ('{\"after\":1}')", &[])
        .unwrap();
    let rows = sql.query("SELECT doc FROM notes", &[]).unwrap();
    assert_eq!(rows.rows, [[SqlValue::from(r#"{"after":1}"#)]]);
    let id = sql.query("SELECT id FROM notes", &[]).unwrap().rows[0][0].clone();
    let renamed = sql
        .execute("UPDATE notes SET id = 'X' WHERE id = ?", &[id])
        .unwrap_err();
    assert_eq!(renamed.code(), "EFYLO_SQL_ID", "{renamed}");
    let syntax = sql.execute("SELEKT 1", &[]).unwrap_err();
    assert_eq!(syntax.code(), "ESQL");
}

#[test]
fn a_transaction_buffers_its_writes_reads_them_and_commits_or_rolls_back() {
    let fixture = Fixture::new();
    let sql = fixture.sql();
    let notes = fixture.root.collection("notes");
    let all = fylo::Query::from_json(&json!({})).unwrap();
    sql.execute("BEGIN", &[]).unwrap();
    sql.execute(
        "INSERT INTO notes (doc) VALUES ('{\"n\":1}'), ('{\"n\":2}')",
        &[],
    )
    .unwrap();
    sql.execute(
        "UPDATE notes SET doc = json_set(doc, '$.seen', 1) WHERE json_extract(doc, '$.n') = 2",
        &[],
    )
    .unwrap();
    let inside = sql
        .query(
            "SELECT doc FROM notes ORDER BY json_extract(doc, '$.n')",
            &[],
        )
        .unwrap();
    assert_eq!(inside.rows.len(), 2, "the transaction reads its own writes");
    assert!(
        notes.find(&all).unwrap().is_empty(),
        "nothing is written before COMMIT"
    );
    sql.execute("COMMIT", &[]).unwrap();
    let committed = notes.find(&all).unwrap();
    assert_eq!(committed.len(), 2);
    assert!(
        committed
            .iter()
            .any(|(_, doc)| doc.get("seen") == Some(&json!(1)))
    );
    sql.execute("BEGIN", &[]).unwrap();
    sql.execute("DELETE FROM notes", &[]).unwrap();
    assert!(
        sql.query("SELECT id FROM notes", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    sql.execute("ROLLBACK", &[]).unwrap();
    assert_eq!(notes.find(&all).unwrap().len(), 2);
    assert_eq!(
        sql.query("SELECT id FROM notes", &[]).unwrap().rows.len(),
        2
    );
}

#[test]
fn a_transaction_that_writes_a_second_collection_is_refused() {
    let fixture = Fixture::new();
    let sql = fixture.sql();
    sql.execute("BEGIN", &[]).unwrap();
    sql.execute("INSERT INTO notes (doc) VALUES ('{\"n\":1}')", &[])
        .unwrap();
    let refused = sql
        .execute(
            "INSERT INTO products (doc) VALUES ('{\"name\":\"x\"}')",
            &[],
        )
        .unwrap_err();
    assert_eq!(refused.code(), "EFYLO_TXN_SCOPE", "{refused}");
    let _ = sql.execute("ROLLBACK", &[]);
    let all = fylo::Query::from_json(&json!({})).unwrap();
    assert!(
        fixture
            .root
            .collection("notes")
            .find(&all)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .root
            .collection("products")
            .find(&all)
            .unwrap()
            .is_empty()
    );
    sql.execute(
        "INSERT INTO products (doc) VALUES ('{\"name\":\"y\"}')",
        &[],
    )
    .unwrap();
    assert_eq!(
        fixture
            .root
            .collection("products")
            .find(&all)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_queue_and_version_are_tables() {
    let fixture = Fixture::new();
    let sql = fixture.sql();
    sql.execute_named(
        "INSERT INTO fylo_queue_publish (topic, payload) VALUES (:topic, :payload)",
        &[
            ("topic", SqlValue::from("jobs")),
            (":payload", SqlValue::from(r#"{"n":1}"#)),
        ],
    )
    .unwrap();
    let published = sql.query("SELECT id FROM fylo_queue_publish", &[]).unwrap();
    assert_eq!(published.rows.len(), 1);
    let claimed = sql
        .query(
            "SELECT id, receipt, attempt, payload FROM fylo_queue_claim WHERE topic = ? AND group_name = ?",
            &[SqlValue::from("jobs"), SqlValue::from("workers")],
        )
        .unwrap();
    assert_eq!(claimed.rows.len(), 1);
    assert_eq!(claimed.rows[0][0], published.rows[0][0]);
    assert_eq!(claimed.rows[0][2], SqlValue::Integer(1));
    assert_eq!(claimed.rows[0][3], SqlValue::from(r#"{"n":1}"#));
    sql.execute(
        "INSERT INTO fylo_queue_ack (topic, group_name, id, receipt) VALUES ('jobs', 'workers', ?, ?)",
        &[claimed.rows[0][0].clone(), claimed.rows[0][1].clone()],
    )
    .unwrap();
    let again = sql
        .query(
            "SELECT id FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'",
            &[],
        )
        .unwrap();
    assert!(again.rows.is_empty());
    let missing = sql
        .query("SELECT id FROM fylo_queue_claim", &[])
        .unwrap_err();
    assert_eq!(missing.code(), "EBADREQUEST", "{missing}");
    let version = sql
        .query("SELECT value FROM fylo_info WHERE name = 'turso_core'", &[])
        .unwrap();
    assert_eq!(version.rows, [[SqlValue::from(fylo::TURSO_PIN)]]);
}

#[test]
fn a_root_with_an_open_sql_connection_closes_after_it() {
    let fixture = Fixture::new();
    let sql = fixture.sql();
    assert!(Arc::strong_count(&fixture.root) > 1);
    drop(sql);
    assert_eq!(
        Arc::strong_count(&fixture.root),
        1,
        "the connection released the root"
    );
}

/// Pushdown against a full scan on 5,000 documents. Run with
/// `--ignored --nocapture`, ideally in release.
#[test]
#[ignore = "timing, not a check"]
fn pushdown_timings() {
    let fixture = Fixture::new();
    let ids = fixture.seed(5_000);
    let pushed = fixture.sql();
    let scanned = fixture.scan();
    let cases: Vec<(&str, &str, Vec<SqlValue>)> = vec![
        (
            "id =",
            "SELECT id FROM products WHERE id = ?",
            vec![SqlValue::from(ids[2_500].as_str())],
        ),
        (
            "name =",
            "SELECT id FROM products WHERE name = ?",
            vec![SqlValue::from("item-2500")],
        ),
        (
            "price =",
            "SELECT id FROM products WHERE price = ?",
            vec![SqlValue::Integer(7)],
        ),
        (
            "price range",
            "SELECT id FROM products WHERE price >= ? AND price < ?",
            vec![SqlValue::Integer(10), SqlValue::Integer(12)],
        ),
        (
            "group = AND price >",
            "SELECT id FROM products WHERE \"group\" = ? AND price > ?",
            vec![SqlValue::from("g3"), SqlValue::Integer(45)],
        ),
        ("full scan", "SELECT count(*) FROM products", vec![]),
    ];
    println!("| query | rows | pushdown ms | full scan ms |");
    println!("|---|---|---|---|");
    for (label, statement, parameters) in cases {
        let time = |sql: &Sql| {
            let mut best = f64::MAX;
            let mut rows = 0;
            for _ in 0..5 {
                let started = Instant::now();
                rows = sql.query(statement, &parameters).unwrap().rows.len();
                best = best.min(started.elapsed().as_secs_f64() * 1e3);
            }
            (rows, best)
        };
        let (rows, with) = time(&pushed);
        let (_, without) = time(&scanned);
        println!("| {label} | {rows} | {with:.2} | {without:.2} |");
    }
}
