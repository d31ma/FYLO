//! The machine Store operations (`api/machine/v2`, `api/store/v1`, ADR 0035
//! and its Amendment 1): entities with the id under `idField`, save, patch,
//! find, count, exists, scroll and delete, and the handshake capability.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_machine::{
    ApplicationConfig, FrameLimits, FrameSession, NamespaceSeparation, serve_configured,
    serve_exclusive,
};
use fylo_storage_native::RootConfig;
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    folder: PathBuf,
}

impl Fixture {
    const COLLECTION: &str = "tickets";
    const KNOWN_ID: &str = "4VRNF52J000";

    fn new() -> Self {
        let root = fylo_test_support::TestFolder::path().join(format!(
            "fylo-machine-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            folder: root.clone(),
            root,
        };
        let created =
            fixture.session(&[json!({ "op": "createCollection", "collection": Self::COLLECTION })]);
        assert_eq!(created[0]["ok"], json!(true), "{created:?}");
        fixture
    }

    fn database() -> Self {
        let directory = fylo_test_support::TestFolder::path().join(format!(
            "fylo-machine-store-db-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        let root = directory.join("app.vdbx");
        fylo_storage_native::NativeRoot::create_database(&root).unwrap();
        let fixture = Self {
            root,
            folder: directory,
        };
        let created =
            fixture.session(&[json!({ "op": "createCollection", "collection": Self::COLLECTION })]);
        assert_eq!(created[0]["ok"], json!(true), "{created:?}");
        fixture
    }

    fn session(&self, requests: &[Value]) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(self.root);
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        serve_configured(
            &mut Cursor::new(input.into_bytes()),
            &mut output,
            None,
            FrameLimits::default(),
            RootConfig::default(),
        )
        .unwrap();
        let _ = fylo_storage_native::NativeRoot::close_database(&self.root);
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn call(&self, request: Value) -> Value {
        let mut frames = self.session(&[request]);
        frames.remove(0)
    }

    fn result(&self, request: Value) -> Value {
        let frame = self.call(request);
        assert_eq!(frame["ok"], json!(true), "{frame}");
        frame["result"].clone()
    }

    fn error_code(&self, request: Value) -> Value {
        let frame = self.call(request);
        assert_eq!(frame["ok"], json!(false), "{frame}");
        frame["error"]["code"].clone()
    }

    fn save(&self, entity: Value) -> Value {
        let mut request = json!({ "op": "storeSave", "collection": Self::COLLECTION });
        request["entity"] = entity;
        self.result(request)
    }

    fn seed(&self) -> Vec<String> {
        let rows = [
            json!({ "status": "open", "priority": 3, "title": "alpha" }),
            json!({ "status": "open", "priority": 1, "title": "bravo" }),
            json!({ "status": "closed", "priority": 2, "title": "charlie" }),
            json!({ "priority": 5, "title": "delta" }),
        ];
        rows.into_iter()
            .map(|row| self.save(row)["id"].as_str().unwrap().to_owned())
            .collect()
    }

    fn titles(entities: &Value) -> Vec<&str> {
        entities
            .as_array()
            .unwrap()
            .iter()
            .map(|entity| entity["title"].as_str().unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fylo_storage_native::NativeRoot::close_database(&self.root);
        let _ = fs::remove_dir_all(&self.folder);
    }
}

#[test]
fn the_handshake_reports_the_store_capability() {
    let fixture = Fixture::new();
    let handshake = fixture.result(json!({ "op": "handshake" }));
    assert_eq!(handshake["capabilities"]["store"], json!(1));
}

#[test]
fn save_without_an_id_creates_and_answers_the_entity_with_its_id() {
    let fixture = Fixture::new();
    let saved = fixture.save(json!({ "title": "alpha" }));
    let id = saved["id"].as_str().unwrap();
    assert_eq!(saved, json!({ "id": id, "title": "alpha" }));
    let found = fixture
        .result(json!({ "op": "storeFindById", "collection": Fixture::COLLECTION, "id": id }));
    assert_eq!(found, saved);
}

#[test]
fn save_with_an_existing_id_replaces_the_whole_document() {
    let fixture = Fixture::new();
    let id = fixture.save(json!({ "title": "alpha", "status": "open" }))["id"].clone();
    let replaced = fixture.save(json!({ "id": id, "title": "beta" }));
    assert_eq!(replaced, json!({ "id": id, "title": "beta" }));
}

#[test]
fn save_with_an_unknown_valid_id_creates_it() {
    let fixture = Fixture::new();
    let saved = fixture.save(json!({ "id": Fixture::KNOWN_ID, "title": "alpha" }));
    assert_eq!(saved, json!({ "id": Fixture::KNOWN_ID, "title": "alpha" }));
}

#[test]
fn require_existing_refuses_an_unknown_id_and_writes_nothing() {
    let fixture = Fixture::new();
    let request = json!({
        "op": "storeSave",
        "collection": Fixture::COLLECTION,
        "entity": { "id": Fixture::KNOWN_ID, "title": "alpha" },
        "requireExisting": true
    });
    let frame = fixture.call(request);
    assert_eq!(frame["error"]["code"], json!("ENATIVE_NOT_FOUND"));
    let message = frame["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(Fixture::COLLECTION) && message.contains(Fixture::KNOWN_ID),
        "{message}"
    );
    let count = fixture.result(json!({ "op": "storeCount", "collection": Fixture::COLLECTION }));
    assert_eq!(count, json!(0));
}

#[test]
fn require_existing_replaces_an_existing_document() {
    let fixture = Fixture::new();
    let id = fixture.save(json!({ "title": "alpha" }))["id"].clone();
    let saved = fixture.result(json!({
        "op": "storeSave",
        "collection": Fixture::COLLECTION,
        "entity": { "id": id, "title": "beta" },
        "requireExisting": true
    }));
    assert_eq!(saved, json!({ "id": id, "title": "beta" }));
}

#[test]
fn require_existing_without_an_id_is_not_found() {
    let fixture = Fixture::new();
    let code = fixture.error_code(json!({
        "op": "storeSave",
        "collection": Fixture::COLLECTION,
        "entity": { "title": "alpha" },
        "requireExisting": true
    }));
    assert_eq!(code, json!("ENATIVE_NOT_FOUND"));
}

#[test]
fn find_by_id_answers_null_for_an_unknown_id_and_not_found_for_an_unknown_collection() {
    let fixture = Fixture::new();
    let missing = fixture.result(json!({ "op": "storeFindById", "collection": Fixture::COLLECTION, "id": Fixture::KNOWN_ID }));
    assert_eq!(missing, Value::Null);
    let code = fixture.error_code(
        json!({ "op": "storeFindById", "collection": "absent", "id": Fixture::KNOWN_ID }),
    );
    assert_eq!(code, json!("ENATIVE_NOT_FOUND"));
}

#[test]
fn patch_merges_top_level_fields_and_answers_null_for_an_unknown_id() {
    let fixture = Fixture::new();
    let id = fixture.save(json!({ "title": "alpha", "status": "open" }))["id"].clone();
    let patched = fixture.result(json!({
        "op": "storePatch", "collection": Fixture::COLLECTION, "id": id, "fields": { "status": null, "priority": 2 }
    }));
    assert_eq!(
        patched,
        json!({ "id": id, "title": "alpha", "status": null, "priority": 2 })
    );
    let missing = fixture.result(json!({
        "op": "storePatch", "collection": Fixture::COLLECTION, "id": Fixture::KNOWN_ID, "fields": { "a": 1 }
    }));
    assert_eq!(missing, Value::Null);
}

#[test]
fn find_count_and_exists_follow_the_query() {
    let fixture = Fixture::new();
    fixture.seed();
    let query = json!({
        "where": [{ "field": "status", "operator": "ne", "value": "closed" }],
        "orderBy": [{ "field": "priority", "direction": "desc" }]
    });
    let found = fixture
        .result(json!({ "op": "storeFind", "collection": Fixture::COLLECTION, "query": query }));
    assert_eq!(Fixture::titles(&found), ["alpha", "bravo"]);
    let count = fixture
        .result(json!({ "op": "storeCount", "collection": Fixture::COLLECTION, "query": query }));
    assert_eq!(count, json!(2));
    let exists = fixture
        .result(json!({ "op": "storeExists", "collection": Fixture::COLLECTION, "query": query }));
    assert_eq!(exists, json!(true));
    let absent = fixture.result(json!({ "op": "storeExists", "collection": "absent" }));
    assert_eq!(absent, json!(false));
}

#[test]
fn find_all_pages_in_id_order() {
    let fixture = Fixture::new();
    let ids = fixture.seed();
    let page = fixture.result(json!({
        "op": "storeFindAll", "collection": Fixture::COLLECTION, "page": { "size": 2, "offset": 1 }
    }));
    let found: Vec<&str> = page
        .as_array()
        .unwrap()
        .iter()
        .map(|entity| entity["id"].as_str().unwrap())
        .collect();
    assert_eq!(found, [ids[1].as_str(), ids[2].as_str()]);
}

#[test]
fn scroll_answers_content_and_a_cursor_until_the_end() {
    let fixture = Fixture::new();
    fixture.seed();
    let query = json!({ "orderBy": [{ "field": "title" }] });
    let first = fixture.result(json!({
        "op": "storeScroll", "collection": Fixture::COLLECTION, "query": query, "page": { "size": 3 }
    }));
    assert_eq!(
        Fixture::titles(&first["content"]),
        ["alpha", "bravo", "charlie"]
    );
    let second = fixture.result(json!({
        "op": "storeScroll", "collection": Fixture::COLLECTION, "query": query,
        "page": { "size": 3, "cursor": first["nextCursor"] }
    }));
    assert_eq!(Fixture::titles(&second["content"]), ["delta"]);
    assert_eq!(second["nextCursor"], Value::Null);
}

#[test]
fn a_scroll_cursor_works_only_in_the_namespace_that_made_it() {
    NamespaceSeparation::enable_for_tests();
    let fixture = Fixture::new();
    fixture.seed();
    let query = json!({ "orderBy": [{ "field": "title" }] });
    let first = fixture.result(json!({
        "op": "storeScroll", "collection": Fixture::COLLECTION, "query": query, "page": { "size": 2 }
    }));
    let page = json!({ "size": 2, "cursor": first["nextCursor"] });
    let same = fixture.result(json!({
        "op": "storeScroll", "collection": Fixture::COLLECTION, "namespace": "public",
        "query": query, "page": page
    }));
    assert_eq!(Fixture::titles(&same["content"]), ["charlie", "delta"]);
    let other = fixture.error_code(json!({
        "op": "storeScroll", "collection": Fixture::COLLECTION, "namespace": "slice-12",
        "query": query, "page": page
    }));
    assert_eq!(other, json!("ECURSOR"));
}

#[test]
fn delete_by_query_and_by_id_answer_how_many_were_deleted() {
    let fixture = Fixture::new();
    let ids = fixture.seed();
    let query = json!({ "where": [{ "field": "status", "operator": "eq", "value": "open" }] });
    let deleted = fixture
        .result(json!({ "op": "storeDelete", "collection": Fixture::COLLECTION, "query": query }));
    assert_eq!(deleted, json!(2));
    let by_id = fixture.result(
        json!({ "op": "storeDeleteById", "collection": Fixture::COLLECTION, "id": ids[2] }),
    );
    assert_eq!(by_id, json!(1));
    let again = fixture.result(
        json!({ "op": "storeDeleteById", "collection": Fixture::COLLECTION, "id": ids[2] }),
    );
    assert_eq!(again, json!(0));
    let left = fixture.result(json!({ "op": "storeCount", "collection": Fixture::COLLECTION }));
    assert_eq!(left, json!(1));
}

#[test]
fn a_custom_id_field_names_the_id_in_entities_and_queries() {
    let fixture = Fixture::new();
    let saved = fixture.result(json!({
        "op": "storeSave", "collection": Fixture::COLLECTION, "idField": "key", "entity": { "key": null, "title": "alpha" }
    }));
    let key = saved["key"].as_str().unwrap();
    let query = json!({ "where": [{ "field": "key", "operator": "eq", "value": key }] });
    let found = fixture.result(json!({
        "op": "storeFind", "collection": Fixture::COLLECTION, "idField": "key", "query": query
    }));
    assert_eq!(found, json!([{ "key": key, "title": "alpha" }]));
}

#[test]
fn malformed_pages_and_queries_answer_their_codes() {
    let fixture = Fixture::new();
    let bad_page =
        json!({ "op": "storeFind", "collection": Fixture::COLLECTION, "page": { "size": "x" } });
    assert_eq!(fixture.error_code(bad_page), json!("EBADREQUEST"));
    let big_page =
        json!({ "op": "storeFind", "collection": Fixture::COLLECTION, "page": { "size": 101 } });
    assert_eq!(fixture.error_code(big_page), json!("EPAGESIZE"));
    let predicates = vec![json!({ "field": "a", "operator": "isNull" }); 9];
    let wide = json!({ "op": "storeCount", "collection": Fixture::COLLECTION, "query": { "where": predicates } });
    assert_eq!(fixture.error_code(wide), json!("EQUERYLIMIT"));
    let unknown = json!({ "op": "storeCount", "collection": Fixture::COLLECTION, "query": { "where": [{ "field": "a", "operator": "between", "value": 1 }] } });
    assert_eq!(fixture.error_code(unknown), json!("EQUERY_INVALID"));
}

fn op(name: &str, fields: Value) -> Value {
    let mut request = fields;
    request["op"] = json!(name);
    request
}

fn in_tickets(name: &str, fields: Value) -> Value {
    let mut request = op(name, fields);
    request["collection"] = json!(Fixture::COLLECTION);
    request
}

const FIRST: &str = "4VRNF52J001";
const SECOND: &str = "4VRNF52J002";

/// One machine session that lives across frames, so a test can read a
/// transaction handle and use it in the next frame.
struct Live<'a> {
    fixture: &'a Fixture,
    session: FrameSession,
}

impl<'a> Live<'a> {
    fn new(fixture: &'a Fixture) -> Self {
        let session = FrameSession::embedded(ApplicationConfig::default(), FrameLimits::default());
        Self { fixture, session }
    }

    fn call(&mut self, request: Value) -> Value {
        let mut request = request;
        request["root"] = json!(self.fixture.root);
        let mut input = Cursor::new(format!("{request}\n").into_bytes());
        let mut output = Vec::new();
        self.session.serve(&mut input, &mut output).unwrap();
        serde_json::from_slice(&output).unwrap()
    }

    fn result(&mut self, request: Value) -> Value {
        let frame = self.call(request);
        assert_eq!(frame["ok"], json!(true), "{frame}");
        frame["result"].clone()
    }

    fn code(&mut self, request: Value) -> Value {
        let frame = self.call(request);
        assert_eq!(frame["ok"], json!(false), "{frame}");
        frame["error"]["code"].clone()
    }

    fn begin(&mut self) -> Value {
        self.result(in_tickets("transactionBegin", json!({})))["transaction"].clone()
    }
}

#[test]
fn a_batch_commits_every_write_and_answers_each_result_in_order() {
    let fixture = Fixture::new();
    fixture.save(json!({ "id": FIRST, "title": "alpha", "status": "open" }));
    let results = fixture.result(in_tickets(
        "storeTransaction",
        json!({ "writes": [
        { "save": { "id": SECOND, "title": "bravo" } },
        { "patch": { "id": FIRST, "fields": { "status": "closed" } } },
        { "deleteById": SECOND },
        { "save": { "id": SECOND, "title": "bravo again" } }
    ] }),
    ));
    assert_eq!(
        results,
        json!([
            { "id": SECOND, "title": "bravo" },
            { "id": FIRST, "title": "alpha", "status": "closed" },
            1,
            { "id": SECOND, "title": "bravo again" }
        ])
    );
    let all = fixture.result(in_tickets("storeFindAll", json!({})));
    assert_eq!(
        all,
        json!([
            { "id": FIRST, "title": "alpha", "status": "closed" },
            { "id": SECOND, "title": "bravo again" }
        ])
    );
}

#[test]
fn a_failing_batch_write_names_its_index_and_writes_nothing() {
    let fixture = Fixture::new();
    let frame = fixture.call(in_tickets(
        "storeTransaction",
        json!({ "writes": [
        { "save": { "id": FIRST, "title": "alpha" } },
        { "save": { "id": 7, "title": "bad id" } }
    ] }),
    ));
    assert_eq!(frame["error"]["code"], json!("EBADREQUEST"));
    assert_eq!(frame["error"]["details"]["index"], json!(1));
    assert_eq!(
        fixture.result(in_tickets("storeCount", json!({}))),
        json!(0)
    );
}

#[test]
fn a_batch_above_one_thousand_writes_is_refused() {
    let fixture = Fixture::new();
    let writes = vec![json!({ "deleteById": FIRST }); 1001];
    let code = fixture.error_code(in_tickets("storeTransaction", json!({ "writes": writes })));
    assert_eq!(code, json!("ETXNLIMIT"));
}

#[test]
fn reads_inside_see_the_transaction_and_outside_only_after_commit() {
    let fixture = Fixture::new();
    fixture.save(json!({ "id": FIRST, "title": "alpha" }));
    let mut live = Live::new(&fixture);
    let handle = live.begin();
    live.result(in_tickets(
        "storeSave",
        json!({ "transaction": handle, "entity": { "id": SECOND, "title": "bravo" } }),
    ));
    live.result(in_tickets(
        "storePatch",
        json!({ "transaction": handle, "id": FIRST, "fields": { "title": "alpha 2" } }),
    ));
    let inside = live.result(in_tickets(
        "storeFind",
        json!({ "transaction": handle, "query": { "orderBy": [{ "field": "title" }] } }),
    ));
    assert_eq!(Fixture::titles(&inside), ["alpha 2", "bravo"]);
    let outside = live.result(in_tickets("storeFind", json!({})));
    assert_eq!(Fixture::titles(&outside), ["alpha"]);
    let committed = live.result(op("transactionCommit", json!({ "transaction": handle })));
    assert_eq!(committed, json!({ "committed": true, "writes": 2 }));
    let after = live.result(in_tickets("storeFind", json!({})));
    assert_eq!(Fixture::titles(&after), ["alpha 2", "bravo"]);
}

#[test]
fn a_rollback_changes_nothing_and_ends_the_handle() {
    let fixture = Fixture::new();
    let mut live = Live::new(&fixture);
    let handle = live.begin();
    live.result(in_tickets(
        "storeSave",
        json!({ "transaction": handle, "entity": { "id": FIRST, "title": "alpha" } }),
    ));
    let rolled = live.result(op("transactionRollback", json!({ "transaction": handle })));
    assert_eq!(rolled, json!({ "rolledBack": true }));
    assert_eq!(
        live.code(op("transactionCommit", json!({ "transaction": handle }))),
        json!("ETXNUNKNOWN")
    );
    assert_eq!(live.result(in_tickets("storeCount", json!({}))), json!(0));
}

#[test]
fn another_collection_or_a_queue_operation_is_out_of_scope_and_rolls_back() {
    let fixture = Fixture::new();
    let mut live = Live::new(&fixture);
    let handle = live.begin();
    live.result(in_tickets(
        "storeSave",
        json!({ "transaction": handle, "entity": { "id": FIRST, "title": "alpha" } }),
    ));
    let other = json!({ "op": "storeSave", "collection": "others", "transaction": handle, "entity": { "title": "x" } });
    assert_eq!(live.code(other), json!("ETXNSCOPE"));
    assert_eq!(
        live.code(op("transactionCommit", json!({ "transaction": handle }))),
        json!("ETXNUNKNOWN")
    );
    let second = live.begin();
    let queue = json!({ "op": "queueStats", "transaction": second, "topic": "jobs" });
    assert_eq!(live.code(queue), json!("ETXNSCOPE"));
    assert_eq!(
        live.code(op("transactionRollback", json!({ "transaction": second }))),
        json!("ETXNUNKNOWN")
    );
    assert_eq!(live.result(in_tickets("storeCount", json!({}))), json!(0));
}

#[test]
fn a_write_outside_an_open_transaction_on_its_collection_is_busy() {
    let fixture = Fixture::new();
    let mut live = Live::new(&fixture);
    let handle = live.begin();
    let outside = in_tickets("storeSave", json!({ "entity": { "title": "outside" } }));
    assert_eq!(live.code(outside.clone()), json!("ETXNBUSY"));
    assert_eq!(
        live.code(in_tickets("transactionBegin", json!({}))),
        json!("ETXNBUSY")
    );
    live.result(op("transactionCommit", json!({ "transaction": handle })));
    live.result(outside);
}

#[test]
fn an_unknown_handle_is_refused() {
    let fixture = Fixture::new();
    let mut live = Live::new(&fixture);
    let request = in_tickets("storeCount", json!({ "transaction": "fytx1.0.0.0" }));
    assert_eq!(live.code(request), json!("ETXNUNKNOWN"));
}

#[test]
fn a_transaction_past_its_time_limit_is_expired() {
    let fixture = Fixture::new();
    let mut live = Live::new(&fixture);
    let handle =
        live.result(in_tickets("transactionBegin", json!({ "timeoutMs": 1 })))["transaction"]
            .clone();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let request = in_tickets(
        "storeSave",
        json!({ "transaction": handle, "entity": { "title": "late" } }),
    );
    assert_eq!(live.code(request), json!("ETXNEXPIRED"));
    assert_eq!(live.result(in_tickets("storeCount", json!({}))), json!(0));
}

#[test]
fn a_bad_timeout_is_a_bad_request() {
    let fixture = Fixture::new();
    let code = fixture.error_code(in_tickets(
        "transactionBegin",
        json!({ "timeoutMs": 300_001 }),
    ));
    assert_eq!(code, json!("EBADREQUEST"));
}

#[test]
fn a_session_that_ends_before_commit_wrote_nothing() {
    let fixture = Fixture::new();
    let mut live = Live::new(&fixture);
    let handle = live.begin();
    live.result(in_tickets(
        "storeSave",
        json!({ "transaction": handle, "entity": { "id": FIRST, "title": "alpha" } }),
    ));
    drop(live);
    assert_eq!(
        fixture.result(in_tickets("storeCount", json!({}))),
        json!(0)
    );
}

#[test]
fn a_batch_commits_in_a_vdbx_database() {
    let fixture = Fixture::database();
    let results = fixture.result(in_tickets(
        "storeTransaction",
        json!({ "writes": [
            { "save": { "id": FIRST, "title": "alpha" } },
            { "save": { "id": SECOND, "title": "bravo" } }
        ] }),
    ));
    assert_eq!(results.as_array().map(Vec::len), Some(2));
    assert_eq!(
        fixture.result(in_tickets("storeCount", json!({}))),
        json!(2)
    );
}

/// A session that holds the root's lease, so its hot set caches reads;
/// frames name no root and use the session's own.
fn exclusive_session(fixture: &Fixture, requests: &[Value]) -> Vec<Value> {
    let mut input = String::new();
    for request in requests {
        input.push_str(&request.to_string());
        input.push('\n');
    }
    let mut output = Vec::new();
    serve_exclusive(
        &mut Cursor::new(input.into_bytes()),
        &mut output,
        Some(fixture.root.clone()),
        FrameLimits::default(),
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn writes_answer_the_new_document_when_the_hot_set_has_the_old_one() {
    let fixture = Fixture::new();
    let frames = exclusive_session(
        &fixture,
        &[
            in_tickets(
                "storeSave",
                json!({ "entity": { "id": FIRST, "title": "alpha" } }),
            ),
            in_tickets("storeFindById", json!({ "id": FIRST })),
            in_tickets(
                "storeSave",
                json!({ "entity": { "id": FIRST, "title": "bravo" } }),
            ),
            in_tickets(
                "storePatch",
                json!({ "id": FIRST, "fields": { "status": "open" } }),
            ),
            in_tickets("storeFindById", json!({ "id": FIRST })),
        ],
    );
    assert_eq!(
        frames[2]["result"],
        json!({ "id": FIRST, "title": "bravo" }),
        "{frames:?}"
    );
    let patched = json!({ "id": FIRST, "title": "bravo", "status": "open" });
    assert_eq!(frames[3]["result"], patched);
    assert_eq!(frames[4]["result"], patched);
}

#[test]
fn a_raw_file_collection_cannot_hold_a_transaction() {
    let fixture = Fixture::new();
    let created =
        fixture.result(json!({ "op": "createCollection", "collection": "files", "kind": "file" }));
    assert_eq!(created, json!({ "collection": "files", "kind": "file" }));
    let begin = json!({ "op": "transactionBegin", "collection": "files" });
    assert_eq!(fixture.error_code(begin), json!("ETXNLAYOUT"));
    let batch = json!({ "op": "storeTransaction", "collection": "files", "writes": [{ "deleteById": FIRST }] });
    assert_eq!(fixture.error_code(batch), json!("ETXNLAYOUT"));
}

#[cfg(unix)]
#[test]
fn a_sidecar_store_root_holds_transactions_on_unix() {
    let folder = fylo_test_support::TestFolder::path().join(format!(
        "fylo-machine-store-efs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let root = folder.join("root");
    fs::create_dir_all(&root).unwrap();
    let options = fylo_storage_native::RootOpenOptions::new(&root)
        .with_profile(fylo_storage_native::FilesystemProfile::EfsSidecarV1);
    let requests = [
        json!({ "op": "createCollection", "collection": "tickets" }),
        json!({ "op": "storeTransaction", "collection": "tickets", "writes": [
            { "save": { "id": FIRST, "title": "alpha" } },
        ] }),
        json!({ "op": "transactionBegin", "collection": "tickets" }),
    ];
    let input = requests
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    fylo_machine::serve_profiled(
        (&mut Cursor::new(input.into_bytes()), &mut output),
        options,
        ApplicationConfig::default(),
        (FrameLimits::default(), false, true),
    )
    .unwrap();
    let frames: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let _ = fs::remove_dir_all(&folder);
    assert_eq!(frames[1]["ok"], json!(true), "{frames:?}");
    assert_eq!(
        frames[1]["result"],
        json!([{ "id": FIRST, "title": "alpha" }])
    );
    assert_eq!(frames[2]["ok"], json!(true), "{frames:?}");
    assert!(frames[2]["result"]["transaction"].is_string(), "{frames:?}");
}
