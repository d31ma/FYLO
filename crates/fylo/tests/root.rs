//! The `fylo` crate opens a root in-process, holds its lease, and answers
//! documents, collections and the queue with the machine protocol's codes.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo::{ClaimOptions, OpenOptions, PublishOptions, Query, Root};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-crate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }

    fn open(&self) -> Root {
        Root::open(&self.0, OpenOptions::default()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn a_root_is_owned_by_one_handle_until_it_is_closed() {
    let fixture = Fixture::new();
    let root = fixture.open();
    let second = Root::open(&fixture.0, OpenOptions::default()).unwrap_err();
    assert!(second.code().starts_with('E'), "{second}");
    root.check_lease().unwrap();
    root.close().unwrap();
    fixture.open().close().unwrap();
}

/// The snapshot's length stands in for 240 MiB of index keys: a write only
/// measures the index files before it commits.
#[test]
fn a_full_collection_refuses_a_put_with_index_full() {
    let fixture = Fixture::new();
    let root = fixture.open();
    root.create_collection("people").unwrap();
    let people = root.collection("people");
    let ada = people.put(fields(&json!({"name": "Ada"}))).unwrap();
    fs::File::create(fixture.0.join(".collections/people/index/keys.snapshot"))
        .unwrap()
        .set_len(240 * 1024 * 1024)
        .unwrap();

    let refused = people.put(fields(&json!({"name": "Grace"}))).unwrap_err();

    assert_eq!(refused.code(), "EINDEX_FULL");
    assert!(refused.message().contains("collection people is full"));
    people.delete(&ada).unwrap();
}

#[test]
fn documents_round_trip_with_fylo_assigned_ttids() {
    let fixture = Fixture::new();
    let root = fixture.open();
    root.create_collection("people").unwrap();
    let people = root.collection("people");
    let id = people
        .put(fields(&json!({"name": "Ada", "age": 36})))
        .unwrap();
    assert!(id.bytes().all(|byte| byte.is_ascii_alphanumeric()), "{id}");
    assert_eq!(
        people.get(&id).unwrap(),
        Some(fields(&json!({"name": "Ada", "age": 36})))
    );
    people
        .patch(&id, &fields(&json!({"age": 37, "city": "London"})))
        .unwrap();
    assert_eq!(
        people.latest(&id).unwrap(),
        Some(fields(&json!({"name": "Ada", "age": 37, "city": "London"})))
    );
    let other = people.put(fields(&json!({"name": "Grace"}))).unwrap();
    let query = Query::from_json(&json!({"$ops": [{"name": {"$eq": "Grace"}}]})).unwrap();
    let found = people.find(&query).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, other);
    let everyone = people.find(&Query::from_json(&json!({})).unwrap()).unwrap();
    assert_eq!(everyone.len(), 2);
    people.delete(&id).unwrap();
    assert_eq!(people.get(&id).unwrap(), None);
    assert_eq!(people.latest(&id).unwrap(), None);
    root.close().unwrap();
}

#[test]
fn missing_documents_and_collections_keep_the_machine_protocol_codes() {
    let fixture = Fixture::new();
    let root = fixture.open();
    root.create_collection("people").unwrap();
    let people = root.collection("people");
    let id = people.put(fields(&json!({"name": "Ada"}))).unwrap();
    people.delete(&id).unwrap();
    let deleted = people.delete(&id).unwrap_err();
    assert!(deleted.is_not_found(), "{deleted}");
    let patched = people.patch(&id, &fields(&json!({"x": 1}))).unwrap_err();
    assert!(patched.is_not_found(), "{patched}");
    let missing = root.inspect_collection("nowhere").unwrap_err();
    assert!(missing.code().starts_with('E'), "{missing}");
    let invalid = Query::from_json(&json!({"$ops": "nope"})).unwrap_err();
    assert_eq!(invalid.code(), "EQUERY_INVALID");
    root.close().unwrap();
}

#[test]
fn arrays_of_objects_are_refused_before_anything_is_written() {
    let fixture = Fixture::new();
    let root = fixture.open();
    root.create_collection("people").unwrap();
    let refused = root
        .collection("people")
        .put(fields(&json!({"tags": [{"a": 1}]})))
        .unwrap_err();
    assert_eq!(refused.code(), "EARRAYOFOBJECTS", "{refused}");
    let all = Query::from_json(&json!({})).unwrap();
    assert!(root.collection("people").find(&all).unwrap().is_empty());
    root.close().unwrap();
}

#[test]
fn collections_are_created_inspected_and_dropped() {
    let fixture = Fixture::new();
    let root = fixture.open();
    assert!(root.create_collection("hd-events").unwrap());
    assert!(!root.create_collection("hd-events").unwrap());
    root.collection("hd-events")
        .put(fields(&json!({"n": 1})))
        .unwrap();
    let inspected = root.inspect_collection("hd-events").unwrap();
    assert_eq!(inspected.collection, "hd-events");
    assert_eq!(inspected.document_count, 1);
    root.drop_collection("hd-events").unwrap();
    assert!(root.inspect_collection("hd-events").is_err());
    root.close().unwrap();
}

#[test]
fn queue_deliveries_are_published_claimed_and_settled() {
    let fixture = Fixture::new();
    let root = fixture.open();
    let mail = root.queue("mail");
    let keyed = PublishOptions {
        idempotency_key: Some("mail-1".into()),
        ..PublishOptions::default()
    };
    let first = mail.publish(json!({"to": "ada"}), &keyed).unwrap();
    let again = mail.publish(json!({"to": "ada"}), &keyed).unwrap();
    assert_eq!(first.id, again.id);
    assert!(again.deduplicated);
    let claimed = mail.claim("sender", ClaimOptions::default()).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].payload, json!({"to": "ada"}));
    mail.nack("sender", &claimed[0].id, &claimed[0].receipt, 0, "retry")
        .unwrap();
    let retried = mail.claim("sender", ClaimOptions::default()).unwrap();
    assert_eq!(retried[0].attempt, 2);
    let stale = mail
        .ack("sender", &claimed[0].id, &claimed[0].receipt)
        .unwrap_err();
    assert_eq!(stale.code(), "EQUEUE_RECEIPT", "{stale}");
    mail.ack("sender", &retried[0].id, &retried[0].receipt)
        .unwrap();
    assert!(
        mail.claim("sender", ClaimOptions::default())
            .unwrap()
            .is_empty()
    );
    root.close().unwrap();
}

#[test]
fn one_root_serves_calls_from_many_threads() {
    let fixture = Fixture::new();
    let root = Arc::new(fixture.open());
    root.create_collection("counts").unwrap();
    let workers: Vec<_> = (0..4)
        .map(|worker| {
            let root = Arc::clone(&root);
            std::thread::spawn(move || {
                for n in 0..10 {
                    root.collection("counts")
                        .put(fields(&json!({"worker": worker, "n": n})))
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let all = Query::from_json(&json!({})).unwrap();
    assert_eq!(root.collection("counts").find(&all).unwrap().len(), 40);
    Arc::into_inner(root).unwrap().close().unwrap();
}

#[cfg(unix)]
#[test]
fn a_root_opened_through_a_symlink_leases_its_target_as_the_machine_protocol_does() {
    let fixture = Fixture::new();
    let link = fixture.0.with_extension("link");
    std::os::unix::fs::symlink(&fixture.0, &link).unwrap();
    let through_link = Root::open(&link, OpenOptions::default()).unwrap();
    through_link.check_lease().unwrap();
    let direct = Root::open(&fixture.0, OpenOptions::default());
    through_link.close().unwrap();
    let _ = fs::remove_file(&link);
    assert!(direct.is_err(), "the target is already leased");
}
