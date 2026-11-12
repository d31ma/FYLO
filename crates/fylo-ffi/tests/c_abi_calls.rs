//! The C ABI, called the way a C, Swift or Kotlin host calls it: pointers,
//! lengths, handle numbers and buffers given back (ADR 0026).

#![allow(unsafe_code, reason = "these tests call the C ABI as a C host does")]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_ffi::{CAbi, FyloBytes, FyloStatus};
use serde_json::{Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "fylo-ffi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self(fs::canonicalize(directory).unwrap())
    }

    fn database(&self) -> String {
        self.0.join("app.vdbx").to_string_lossy().into_owned()
    }

    fn key_file(&self, name: &str) -> String {
        format!("file:{}", self.0.join(format!("{name}.pem")).display())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One call's status and the JSON it handed out, its buffer given back.
struct Answer {
    status: FyloStatus,
    json: Value,
}

fn answer(status: FyloStatus, bytes: FyloBytes) -> Answer {
    let json = if bytes.len == 0 {
        Value::Null
    } else {
        let text = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        serde_json::from_slice(text).unwrap()
    };
    unsafe { CAbi::fylo_bytes_free(bytes) };
    Answer { status, json }
}

fn open(path: &str, options: &str) -> (Answer, u64) {
    let mut root = 0_u64;
    let mut error = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_open(
            path.as_ptr(),
            path.len(),
            options.as_ptr(),
            options.len(),
            std::ptr::null(),
            0,
            &raw mut root,
            &raw mut error,
        )
    };
    (answer(status, error), root)
}

fn open_plain(fixture: &Fixture) -> u64 {
    let (opened, root) = open(&fixture.database(), r#"{"create": {"mode": "plain"}}"#);
    assert_eq!(opened.status, FyloStatus::Ok, "{}", opened.json);
    root
}

fn close(root: u64) -> Answer {
    let mut out = FyloBytes::empty();
    let status = unsafe { CAbi::fylo_close(root, &raw mut out) };
    answer(status, out)
}

fn create_collection(root: u64, name: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status =
        unsafe { CAbi::fylo_create_collection(root, name.as_ptr(), name.len(), &raw mut out) };
    answer(status, out)
}

fn put(root: u64, collection: &str, document: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_put(
            root,
            collection.as_ptr(),
            collection.len(),
            document.as_ptr(),
            document.len(),
            &raw mut out,
        )
    };
    answer(status, out)
}

fn get(root: u64, collection: &str, id: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_get(
            root,
            collection.as_ptr(),
            collection.len(),
            id.as_ptr(),
            id.len(),
            &raw mut out,
        )
    };
    answer(status, out)
}

fn delete(root: u64, collection: &str, id: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_delete(
            root,
            collection.as_ptr(),
            collection.len(),
            id.as_ptr(),
            id.len(),
            &raw mut out,
        )
    };
    answer(status, out)
}

fn find(root: u64, collection: &str, query: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_find(
            root,
            collection.as_ptr(),
            collection.len(),
            query.as_ptr(),
            query.len(),
            &raw mut out,
        )
    };
    answer(status, out)
}

fn find_page(root: u64, collection: &str, query: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_find_page(
            root,
            collection.as_ptr(),
            collection.len(),
            query.as_ptr(),
            query.len(),
            &raw mut out,
        )
    };
    answer(status, out)
}

fn identity_create(reference: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status =
        unsafe { CAbi::fylo_identity_create(reference.as_ptr(), reference.len(), &raw mut out) };
    answer(status, out)
}

fn identity_show(reference: &str) -> Answer {
    let mut out = FyloBytes::empty();
    let status =
        unsafe { CAbi::fylo_identity_show(reference.as_ptr(), reference.len(), &raw mut out) };
    answer(status, out)
}

fn stored_id(answer: &Answer) -> String {
    answer.json.as_str().unwrap().to_owned()
}

#[test]
fn the_library_reports_abi_version_1() {
    assert_eq!(CAbi::fylo_abi_version(), 1);
}

#[test]
fn a_plain_database_is_created_written_read_found_and_deleted() {
    let fixture = Fixture::new();
    let root = open_plain(&fixture);
    let created = create_collection(root, "messages");
    let stored = put(root, "messages", r#"{"subject": "Hello", "size": 3}"#);
    let id = stored_id(&stored);
    let read = get(root, "messages", &id);
    let found = find(
        root,
        "messages",
        r#"{"$ops": [{"subject": {"$eq": "Hello"}}]}"#,
    );
    let deleted = delete(root, "messages", &id);
    let after = get(root, "messages", &id);
    let closed = close(root);

    assert_eq!(created.status, FyloStatus::Ok);
    assert_eq!(
        created.json,
        json!({"collection": "messages", "kind": "document"})
    );
    assert_eq!(stored.status, FyloStatus::Ok);
    assert_eq!(read.status, FyloStatus::Ok);
    assert_eq!(
        read.json,
        json!({ id.clone(): {"subject": "Hello", "size": 3} })
    );
    assert_eq!(found.status, FyloStatus::Ok);
    assert_eq!(
        found.json,
        json!({ id.clone(): {"subject": "Hello", "size": 3} })
    );
    assert_eq!(deleted.json, json!({"deleted": true}));
    assert_eq!(after.status, FyloStatus::NotFound);
    assert_eq!(after.json, Value::Null);
    assert_eq!(closed.status, FyloStatus::Ok);
}

#[test]
fn a_missing_database_without_create_is_not_made() {
    let fixture = Fixture::new();
    let (opened, root) = open(&fixture.database(), "");

    assert_eq!(opened.status, FyloStatus::Error);
    assert_eq!(root, 0);
    assert!(opened.json["code"].is_string());
    assert!(!PathBuf::from(fixture.database()).exists());
}

#[test]
fn a_closed_handle_answers_closed() {
    let fixture = Fixture::new();
    let root = open_plain(&fixture);
    close(root);
    let again = close(root);
    let read = get(root, "messages", "x");

    assert_eq!(again.status, FyloStatus::Closed);
    assert_eq!(again.json["code"], "EROOTCLOSED");
    assert_eq!(read.status, FyloStatus::Closed);
}

#[test]
fn a_handle_that_never_existed_answers_closed() {
    let answered = get(u64::MAX, "messages", "x");

    assert_eq!(answered.status, FyloStatus::Closed);
}

#[test]
fn a_null_pointer_with_a_length_is_an_invalid_argument() {
    let mut out = FyloBytes::empty();
    let status = unsafe { CAbi::fylo_identity_show(std::ptr::null(), 4, &raw mut out) };
    let answered = answer(status, out);

    assert_eq!(answered.status, FyloStatus::InvalidArgument);
    assert_eq!(answered.json["code"], "EBADREQUEST");
}

#[test]
fn text_that_is_not_utf8_is_an_invalid_argument() {
    let fixture = Fixture::new();
    let root = open_plain(&fixture);
    let collection = [0xFF_u8, 0xFE];
    let mut out = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_create_collection(root, collection.as_ptr(), collection.len(), &raw mut out)
    };
    let answered = answer(status, out);
    close(root);

    assert_eq!(answered.status, FyloStatus::InvalidArgument);
}

#[test]
fn options_with_an_unknown_field_are_an_invalid_argument() {
    let fixture = Fixture::new();
    let (opened, _) = open(
        &fixture.database(),
        r#"{"create": {"mode": "plain"}, "cache": 1}"#,
    );

    assert_eq!(opened.status, FyloStatus::InvalidArgument);
    assert!(!PathBuf::from(fixture.database()).exists());
}

#[test]
fn a_document_that_is_not_an_object_is_an_invalid_argument() {
    let fixture = Fixture::new();
    let root = open_plain(&fixture);
    create_collection(root, "messages");
    let stored = put(root, "messages", "[1, 2]");
    close(root);

    assert_eq!(stored.status, FyloStatus::InvalidArgument);
}

#[test]
fn an_engine_refusal_is_an_error_with_its_code() {
    let fixture = Fixture::new();
    let root = open_plain(&fixture);
    let stored = put(root, "missing", r#"{"a": 1}"#);
    close(root);

    assert_eq!(stored.status, FyloStatus::Error);
    assert!(stored.json["code"].as_str().unwrap().starts_with('E'));
}

#[test]
fn an_encryption_key_is_refused_until_keys_are_supported() {
    let fixture = Fixture::new();
    let path = fixture.database();
    let options = r#"{"create": {"mode": "plain"}}"#;
    let key = [7_u8; 32];
    let mut root = 0_u64;
    let mut error = FyloBytes::empty();
    let status = unsafe {
        CAbi::fylo_open(
            path.as_ptr(),
            path.len(),
            options.as_ptr(),
            options.len(),
            key.as_ptr(),
            key.len(),
            &raw mut root,
            &raw mut error,
        )
    };
    let answered = answer(status, error);

    assert_eq!(answered.status, FyloStatus::InvalidArgument);
}

#[test]
fn an_identity_is_created_shown_and_opens_the_database_it_protects() {
    let fixture = Fixture::new();
    let reference = fixture.key_file("device");
    let created = identity_create(&reference);
    let shown = identity_show(&reference);
    let options = json!({"create": {"mode": "protected", "admin": reference}}).to_string();
    let (opened, root) = open(&fixture.database(), &options);
    create_collection(root, "messages");
    let id = stored_id(&put(root, "messages", r#"{"body": "sealed"}"#));
    let read = get(root, "messages", &id);
    close(root);
    let pinned =
        json!({"identity": {"reference": reference, "trust": {"pinned": created.json["id"]}}});
    let (reopened, again) = open(&fixture.database(), &pinned.to_string());
    let reread = get(again, "messages", &id);
    close(again);

    assert_eq!(created.status, FyloStatus::Ok);
    assert!(
        created.json["id"]
            .as_str()
            .unwrap()
            .starts_with("fid1:p256:")
    );
    assert!(
        created.json["publicKey"]
            .as_str()
            .unwrap()
            .starts_with("fpk1:p256:")
    );
    assert_eq!(shown.json, created.json);
    assert_eq!(opened.status, FyloStatus::Ok, "{}", opened.json);
    assert_eq!(read.json, json!({ id.clone(): {"body": "sealed"} }));
    assert_eq!(reopened.status, FyloStatus::Ok, "{}", reopened.json);
    assert_eq!(reread.json, json!({ id: {"body": "sealed"} }));
}

#[test]
fn a_protected_database_opened_without_an_identity_refuses_reads() {
    let fixture = Fixture::new();
    let reference = fixture.key_file("device");
    identity_create(&reference);
    let options = json!({"create": {"mode": "protected", "admin": reference}}).to_string();
    let (_, root) = open(&fixture.database(), &options);
    create_collection(root, "messages");
    close(root);
    let (_, locked) = open(&fixture.database(), "");
    let read = get(locked, "messages", "x");
    close(locked);

    assert_eq!(read.status, FyloStatus::Error);
}

#[test]
fn calls_from_many_threads_share_one_handle() {
    let fixture = Fixture::new();
    let root = open_plain(&fixture);
    create_collection(root, "messages");
    let workers: Vec<_> = (0..4)
        .map(|worker| {
            std::thread::spawn(move || {
                (0..5)
                    .map(|index| {
                        put(
                            root,
                            "messages",
                            &json!({"worker": worker, "index": index}).to_string(),
                        )
                        .status
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let statuses: Vec<FyloStatus> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();
    let found = find(root, "messages", r#"{"$ops": []}"#);
    close(root);

    assert!(statuses.iter().all(|status| *status == FyloStatus::Ok));
    assert_eq!(found.json.as_object().unwrap().len(), 20);
}

#[test]
fn sorted_pages_follow_the_cursor_to_the_last_page() {
    let fixture = Fixture::new();
    let (_, root) = open(&fixture.database(), r#"{"create": {"mode": "plain"}}"#);
    create_collection(root, "messages");
    for (subject, sent) in [("first", 1), ("second", 2), ("third", 3)] {
        put(
            root,
            "messages",
            &json!({ "subject": subject, "sentAt": sent }).to_string(),
        );
    }

    let first = find_page(
        root,
        "messages",
        r#"{"$sort": {"sentAt": "desc"}, "$limit": 2}"#,
    );
    let cursor = first.json["nextCursor"].as_str().unwrap().to_owned();
    let second = find_page(
        root,
        "messages",
        &json!({ "$sort": { "sentAt": "desc" }, "$limit": 2, "$after": cursor }).to_string(),
    );
    let ids_only = find_page(
        root,
        "messages",
        r#"{"$sort": {"sentAt": "asc"}, "$limit": 1, "$onlyIds": true}"#,
    );

    assert_eq!(first.status, FyloStatus::Ok);
    let subjects: Vec<&str> = first.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["doc"]["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, ["third", "second"]);
    assert_eq!(second.json["items"][0]["doc"]["subject"], json!("first"));
    assert_eq!(second.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(second.json["nextCursor"], Value::Null);
    assert!(ids_only.json["items"][0].is_string(), "{}", ids_only.json);
    close(root);
}

#[test]
fn a_page_without_sort_is_an_engine_error() {
    let fixture = Fixture::new();
    let (_, root) = open(&fixture.database(), r#"{"create": {"mode": "plain"}}"#);
    create_collection(root, "messages");

    let page = find_page(root, "messages", r#"{"$limit": 2}"#);

    assert_eq!(page.status, FyloStatus::Error);
    assert!(
        page.json["code"].as_str().unwrap().starts_with('E'),
        "{}",
        page.json
    );
    close(root);
}
