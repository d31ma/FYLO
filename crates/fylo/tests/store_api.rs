//! The Rust Store API (ADR 0035, `api/store/v1`): typed entities, the
//! Repository base, queries, pages, `requireExisting` and one-collection
//! transactions, over the same engine plans as the machine protocol.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo::store::{Direction, Page, Query, ScrollPage};
use fylo::{Database, OpenOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Ticket {
    id: Option<String>,
    status: String,
    priority: i64,
}

impl Ticket {
    fn new(status: &str, priority: i64) -> Self {
        Self {
            id: None,
            status: status.to_owned(),
            priority,
        }
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    const COLLECTION: &str = "tickets";

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = fylo_test_support::TestFolder::path().join(format!(
            "fylo-crate-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn open(&self) -> Database {
        let database = Database::open(&self.root, OpenOptions::default()).unwrap();
        database.collections().create(Self::COLLECTION).unwrap();
        database
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn statuses(tickets: &[Ticket]) -> Vec<&str> {
    tickets
        .iter()
        .map(|ticket| ticket.status.as_str())
        .collect()
}

#[test]
fn save_answers_the_entity_with_its_new_id_and_find_by_id_reads_it() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    let saved = tickets.save(&Ticket::new("open", 3)).unwrap();
    let id = saved.id.clone().unwrap();
    assert_eq!(tickets.find_by_id(&id).unwrap(), Some(saved));
    assert_eq!(tickets.find_by_id("4VRNF52J000").unwrap(), None);
}

#[test]
fn queries_count_find_and_page_in_order() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    for (status, priority) in [("open", 3), ("open", 1), ("closed", 2), ("open", 5)] {
        tickets.save(&Ticket::new(status, priority)).unwrap();
    }
    let open = Query::new()
        .eq("status", "open")
        .order_by("priority", Direction::Descending);
    assert_eq!(tickets.count(&open).unwrap(), 3);
    let page = tickets.find(&open, Page::new(2, 1)).unwrap();
    let priorities: Vec<i64> = page.iter().map(|ticket| ticket.priority).collect();
    assert_eq!(priorities, [3, 1]);
    assert!(
        tickets
            .exists(&Query::new().eq("status", "closed"))
            .unwrap()
    );
    assert!(
        !tickets
            .exists(&Query::new().eq("status", "pending"))
            .unwrap()
    );
    assert_eq!(
        statuses(&tickets.find_all(Page::new(10, 0)).unwrap()),
        ["open", "open", "closed", "open"]
    );
}

#[test]
fn scroll_pages_with_a_cursor() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    for priority in 1..=3 {
        tickets.save(&Ticket::new("open", priority)).unwrap();
    }
    let by_priority = Query::new().order_by("priority", Direction::Ascending);
    let first = tickets
        .scroll(&by_priority, &ScrollPage::new(2, None))
        .unwrap();
    assert_eq!(first.content.len(), 2);
    let second = tickets
        .scroll(&by_priority, &ScrollPage::new(2, first.next_cursor.clone()))
        .unwrap();
    assert_eq!(second.content[0].priority, 3);
    assert_eq!(second.next_cursor, None);
}

#[test]
fn patch_merges_and_delete_answers_counts() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    let id = tickets.save(&Ticket::new("open", 1)).unwrap().id.unwrap();
    let mut fields = Map::new();
    fields.insert("status".to_owned(), json!("closed"));
    let patched = tickets.patch(&id, &fields).unwrap().unwrap();
    assert_eq!(patched.status, "closed");
    assert_eq!(tickets.patch("4VRNF52J000", &fields).unwrap(), None);
    tickets.save(&Ticket::new("open", 2)).unwrap();
    assert_eq!(
        tickets.delete(&Query::new().eq("status", "open")).unwrap(),
        1
    );
    assert_eq!(tickets.delete_by_id(&id).unwrap(), 1);
    assert_eq!(tickets.delete_by_id(&id).unwrap(), 0);
}

#[test]
fn save_existing_refuses_an_unknown_id_and_writes_nothing() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    let mut ghost = Ticket::new("open", 1);
    ghost.id = Some("4VRNF52J000".to_owned());
    let error = tickets.save_existing(&ghost).unwrap_err();
    assert_eq!(error.code(), "ENATIVE_NOT_FOUND");
    assert!(error.message().contains("4VRNF52J000"));
    assert_eq!(tickets.count(&Query::new()).unwrap(), 0);
}

#[test]
fn a_transaction_reads_its_own_writes_and_commits_them_together() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    let kept = tickets.save(&Ticket::new("open", 1)).unwrap();
    let transaction = tickets.begin();
    transaction.save(&Ticket::new("open", 2)).unwrap();
    transaction
        .delete_by_id(kept.id.as_deref().unwrap())
        .unwrap();
    assert_eq!(transaction.count(&Query::new()).unwrap(), 1);
    assert_eq!(tickets.count(&Query::new()).unwrap(), 1);
    assert_eq!(transaction.commit().unwrap(), 2);
    let after = tickets.find_all(Page::new(10, 0)).unwrap();
    let priorities: Vec<i64> = after.iter().map(|ticket| ticket.priority).collect();
    assert_eq!(priorities, [2]);
}

#[test]
fn a_dropped_transaction_writes_nothing() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    let transaction = tickets.begin();
    transaction.save(&Ticket::new("open", 2)).unwrap();
    transaction.rollback();
    assert_eq!(tickets.count(&Query::new()).unwrap(), 0);
}

#[test]
fn plain_json_documents_and_a_custom_id_field_work_too() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let documents = database
        .store::<Map<String, Value>>(Fixture::COLLECTION)
        .with_id_field("key");
    let entity = json!({ "key": null, "title": "alpha" });
    let saved = documents.save(entity.as_object().unwrap()).unwrap();
    assert!(saved["key"].is_string());
    assert_eq!(saved["title"], json!("alpha"));
    assert!(database.collections().exists(Fixture::COLLECTION).unwrap());
    assert!(!database.collections().exists("absent").unwrap());
}

#[test]
fn collections_list_their_names_sorted() {
    let fixture = Fixture::new();
    let database = fixture.open();
    database.collections().create("audit").unwrap();
    let names = database.collections().list().unwrap();
    assert_eq!(names, ["audit", Fixture::COLLECTION]);
}

#[test]
fn a_bad_query_answers_its_code() {
    let fixture = Fixture::new();
    let database = fixture.open();
    let tickets = database.store::<Ticket>(Fixture::COLLECTION);
    let values: Vec<Value> = (0..101).map(Value::from).collect();
    let error = tickets
        .count(&Query::new().is_in("priority", values))
        .unwrap_err();
    assert_eq!(error.code(), "EQUERYLIMIT");
}
