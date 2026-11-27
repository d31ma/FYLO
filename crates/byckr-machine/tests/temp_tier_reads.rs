//! The temp-disk read tier (ADR 0030 slices S3 and S4): with `--temp-cache
//! on`, a folder root answers document reads from stored records kept in a
//! private temp folder, and every answer equals the one the root gives. Each
//! test runs on a native-profile root and, on Unix, on an EFS-profile root,
//! whose entries also keep the record's manifest. Each session here is a new
//! tier on the same cache folder, as a new process would be.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use byckr_machine::{ApplicationConfig, FrameLimits, serve_profiled};
use byckr_storage_native::{
    FilesystemProfile, NativeRoot, NativeWriteRoot, RootOpenOptions, TempCacheMode,
    TempCacheSettings,
};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

const NOTES: &str = "notes";
const OTHER: &str = "other";
const PII: &str = "pii";
const FIRST: &str = "4VRNF52JPCO";
const SECOND: &str = "4VRNF52JPCP";
const THIRD: &str = "4VRNF52JPCQ";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";
const SECRET: &str = "123-45-6789";

/// A root, a schema folder that encrypts `pii.ssn`, and a base folder for
/// the temp cache, removed when the test ends.
struct Fixture {
    parent: PathBuf,
    profile: FilesystemProfile,
}

impl Fixture {
    fn new() -> Self {
        Self::with_profile(FilesystemProfile::Native)
    }

    #[cfg(unix)]
    fn efs() -> Self {
        Self::with_profile(FilesystemProfile::EfsSidecarV1)
    }

    fn with_profile(profile: FilesystemProfile) -> Self {
        let parent = byckr_test_support::TestFolder::path().join(format!(
            "fylo-temp-tier-reads-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(parent.join("root")).unwrap();
        fs::create_dir_all(parent.join("base")).unwrap();
        let schemas = parent.join("schemas").join(PII);
        fs::create_dir_all(schemas.join("history")).unwrap();
        fs::write(
            schemas.join("manifest.json"),
            br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
        )
        .unwrap();
        fs::write(
            schemas.join("history/v1.schema.json"),
            br#"{"$encrypted":["ssn"],"name":"^.+$"}"#,
        )
        .unwrap();
        Self {
            parent: fs::canonicalize(parent).unwrap(),
            profile,
        }
    }

    fn root(&self) -> PathBuf {
        self.parent.join("root")
    }

    fn base(&self) -> PathBuf {
        self.parent.join("base")
    }

    fn application(&self, mode: TempCacheMode, bytes: Option<u64>) -> ApplicationConfig {
        ApplicationConfig {
            schema: Some(self.parent.join("schemas")),
            credentials: Some((KEY.into(), "fixture-salt".into())),
            temp_cache: TempCacheSettings {
                mode,
                bytes,
                dir: Some(self.base()),
            },
            ..ApplicationConfig::default()
        }
    }

    /// Answer `requests` in one new session against `root`, opened with
    /// `profile`.
    fn session_at(
        (root, profile): (&Path, FilesystemProfile),
        application: ApplicationConfig,
        requests: &[Value],
    ) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        serve_profiled(
            (&mut Cursor::new(input.into_bytes()), &mut output),
            RootOpenOptions::new(root).with_profile(profile),
            application,
            (FrameLimits::default(), false, true),
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn session(&self, mode: TempCacheMode, requests: &[Value]) -> Vec<Value> {
        Self::session_at(
            (&self.root(), self.profile),
            self.application(mode, None),
            requests,
        )
    }

    /// Run `requests` with the tier on and expect each to succeed.
    fn succeed(&self, requests: &[Value]) -> Vec<Value> {
        let replies = self.session(TempCacheMode::On, requests);
        for (reply, request) in replies.iter().zip(requests) {
            assert_eq!(reply["ok"], json!(true), "{request} failed: {reply}");
        }
        replies
    }

    /// The tier's counters after `requests`, in the same session.
    fn counters_after(&self, requests: &[Value]) -> Value {
        let mut requests = requests.to_vec();
        requests.push(json!({"op":"handshake"}));
        let replies = self.session(TempCacheMode::On, &requests);
        replies.last().unwrap()["result"]["capabilities"]["tempCache"].clone()
    }

    /// The reply bodies of `requests` with the tier in `mode`, without the
    /// handshake.
    fn answers(&self, mode: TempCacheMode, requests: &[Value]) -> Vec<Value> {
        self.session(mode, requests)
            .into_iter()
            .map(|reply| json!({"ok": reply["ok"], "result": reply["result"], "code": reply["error"]["code"]}))
            .collect()
    }

    fn cache_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut folders = vec![self.base()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    fn entries(&self) -> Vec<PathBuf> {
        self.cache_files()
            .into_iter()
            .filter(|path| path.extension().is_some_and(|extension| extension == "fyc"))
            .collect()
    }

    fn only_entry(&self) -> PathBuf {
        let entries = self.entries();
        assert_eq!(entries.len(), 1, "{entries:?}");
        entries[0].clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.parent.join("app.vdbx"));
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn create(collection: &str) -> Value {
    json!({"op":"createCollection","collection":collection})
}

fn put(collection: &str, id: &str, data: &Value) -> Value {
    json!({"op":"putData","collection":collection,"id":id,"data":data})
}

fn get(collection: &str, id: &str) -> Value {
    json!({"op":"getDoc","collection":collection,"id":id})
}

fn patch(collection: &str, id: &str, data: &Value) -> Value {
    json!({"op":"patchDoc","collection":collection,"id":id,"newDoc":data})
}

fn find(collection: &str, query: &Value) -> Value {
    json!({"op":"findDocs","collection":collection,"query":query})
}

fn seed(fixture: &Fixture) {
    fixture.succeed(&[
        create(NOTES),
        put(NOTES, FIRST, &json!({"title":"one","n":1})),
        put(NOTES, SECOND, &json!({"title":"two","n":2})),
        put(NOTES, THIRD, &json!({"title":"three","n":3})),
    ]);
}

fn reads() -> Vec<Value> {
    vec![
        get(NOTES, FIRST),
        get(NOTES, SECOND),
        get(NOTES, THIRD),
        find(NOTES, &json!({})),
        find(NOTES, &json!({"$ops":[{"n":{"$gte":2}}]})),
        json!({"op":"getMeta","collection":NOTES,"id":SECOND}),
    ]
}

fn counter(counters: &Value, name: &str) -> u64 {
    counters[name]
        .as_u64()
        .unwrap_or_else(|| panic!("{name}: {counters}"))
}

fn check_a_document_read_once_is_answered_from_the_tier_by_the_next_session(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&[get(NOTES, FIRST)]);
    let counters = fixture.counters_after(&[get(NOTES, FIRST)]);
    assert_eq!(counters["active"], true, "{counters}");
    assert_eq!(counter(&counters, "hits"), 1, "{counters}");
    assert_eq!(counter(&counters, "misses"), 0, "{counters}");
    assert_eq!(counter(&counters, "revalidations"), 0, "{counters}");
    assert_eq!(fixture.entries().len(), 1);
}

#[test]
fn a_document_read_once_is_answered_from_the_tier_by_the_next_session() {
    check_a_document_read_once_is_answered_from_the_tier_by_the_next_session(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_document_read_once_is_answered_from_the_tier_by_the_next_session_on_efs() {
    check_a_document_read_once_is_answered_from_the_tier_by_the_next_session(&Fixture::efs());
}

fn check_every_read_with_the_tier_equals_the_read_without_it_cold_and_warm(fixture: &Fixture) {
    seed(fixture);
    let expected = fixture.answers(TempCacheMode::Off, &reads());
    let cold = fixture.answers(TempCacheMode::On, &reads());
    let warm = fixture.answers(TempCacheMode::On, &reads());
    assert_eq!(cold, expected);
    assert_eq!(warm, expected);
    let counters = fixture.counters_after(&reads());
    assert!(counter(&counters, "hits") >= 3, "{counters}");
}

#[test]
fn every_read_with_the_tier_equals_the_read_without_it_cold_and_warm() {
    check_every_read_with_the_tier_equals_the_read_without_it_cold_and_warm(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn every_read_with_the_tier_equals_the_read_without_it_cold_and_warm_on_efs() {
    check_every_read_with_the_tier_equals_the_read_without_it_cold_and_warm(&Fixture::efs());
}

fn check_encrypted_documents_read_the_same_and_reach_the_disk_only_as_ciphertext(
    fixture: &Fixture,
) {
    fixture.succeed(&[
        create(PII),
        put(PII, FIRST, &json!({"name":"Ada","ssn":SECRET})),
        put(PII, SECOND, &json!({"name":"Bo","ssn":"987-65-4321"})),
    ]);
    let requests = [get(PII, FIRST), get(PII, SECOND), find(PII, &json!({}))];
    let expected = fixture.answers(TempCacheMode::Off, &requests);
    assert_eq!(fixture.answers(TempCacheMode::On, &requests), expected);
    assert_eq!(fixture.answers(TempCacheMode::On, &requests), expected);
    assert_eq!(expected[0]["result"][FIRST]["ssn"], SECRET, "{expected:?}");
    assert_eq!(fixture.entries().len(), 2);
    for file in fixture.cache_files() {
        let bytes = fs::read(&file).unwrap();
        for plain in [SECRET.as_bytes(), b"987-65-4321".as_slice()] {
            let found = bytes.windows(plain.len()).any(|window| window == plain);
            assert!(!found, "{} holds plaintext", file.display());
        }
    }
}

#[test]
fn encrypted_documents_read_the_same_and_reach_the_disk_only_as_ciphertext() {
    check_encrypted_documents_read_the_same_and_reach_the_disk_only_as_ciphertext(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn encrypted_documents_read_the_same_and_reach_the_disk_only_as_ciphertext_on_efs() {
    check_encrypted_documents_read_the_same_and_reach_the_disk_only_as_ciphertext(&Fixture::efs());
}

fn check_a_write_by_another_session_is_seen_and_unchanged_documents_are_revalidated(
    fixture: &Fixture,
) {
    seed(fixture);
    fixture.succeed(&[get(NOTES, FIRST), get(NOTES, SECOND)]);
    fixture.succeed(&[patch(NOTES, FIRST, &json!({"title":"changed"}))]);
    let replies = fixture.succeed(&[get(NOTES, FIRST), get(NOTES, SECOND)]);
    assert_eq!(
        replies[0]["result"][FIRST]["title"], "changed",
        "{replies:?}"
    );
    assert_eq!(replies[1]["result"][SECOND]["title"], "two", "{replies:?}");
    let counters = fixture.counters_after(&[get(NOTES, SECOND)]);
    assert_eq!(counter(&counters, "hits"), 1, "{counters}");
    assert_eq!(counter(&counters, "revalidations"), 0, "{counters}");
}

#[test]
fn a_write_by_another_session_is_seen_and_unchanged_documents_are_revalidated() {
    check_a_write_by_another_session_is_seen_and_unchanged_documents_are_revalidated(
        &Fixture::new(),
    );
}

#[cfg(unix)]
#[test]
fn a_write_by_another_session_is_seen_and_unchanged_documents_are_revalidated_on_efs() {
    check_a_write_by_another_session_is_seen_and_unchanged_documents_are_revalidated(
        &Fixture::efs(),
    );
}

fn check_a_changed_document_is_refused_on_revalidation(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&[get(NOTES, FIRST), get(NOTES, SECOND)]);
    fixture.succeed(&[patch(NOTES, FIRST, &json!({"title":"changed"}))]);
    let counters = fixture.counters_after(&[get(NOTES, FIRST), get(NOTES, SECOND)]);
    assert_eq!(counter(&counters, "revalidations"), 2, "{counters}");
    assert_eq!(counter(&counters, "rejected"), 1, "{counters}");
    assert_eq!(counter(&counters, "hits"), 1, "{counters}");
}

#[test]
fn a_changed_document_is_refused_on_revalidation() {
    check_a_changed_document_is_refused_on_revalidation(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_changed_document_is_refused_on_revalidation_on_efs() {
    check_a_changed_document_is_refused_on_revalidation(&Fixture::efs());
}

fn check_a_deleted_document_is_not_found_with_the_tier(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&[get(NOTES, FIRST)]);
    fixture.succeed(&[json!({"op":"delDoc","collection":NOTES,"id":FIRST})]);
    let expected = fixture.answers(TempCacheMode::Off, &[get(NOTES, FIRST)]);
    assert_eq!(expected[0]["result"], json!({}), "{expected:?}");
    assert_eq!(
        fixture.answers(TempCacheMode::On, &[get(NOTES, FIRST)]),
        expected
    );
}

#[test]
fn a_deleted_document_is_not_found_with_the_tier() {
    check_a_deleted_document_is_not_found_with_the_tier(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_deleted_document_is_not_found_with_the_tier_on_efs() {
    check_a_deleted_document_is_not_found_with_the_tier(&Fixture::efs());
}

fn check_a_collection_dropped_and_created_again_does_not_answer_from_its_old_life(
    fixture: &Fixture,
) {
    seed(fixture);
    fixture.succeed(&[get(NOTES, FIRST), find(NOTES, &json!({}))]);
    fixture.succeed(&[
        json!({"op":"dropCollection","collection":NOTES}),
        create(NOTES),
    ]);
    let requests = [get(NOTES, FIRST), find(NOTES, &json!({}))];
    let expected = fixture.answers(TempCacheMode::Off, &requests);
    assert_eq!(expected[0]["result"], json!({}), "{expected:?}");
    assert_eq!(fixture.answers(TempCacheMode::On, &requests), expected);
}

#[test]
fn a_collection_dropped_and_created_again_does_not_answer_from_its_old_life() {
    check_a_collection_dropped_and_created_again_does_not_answer_from_its_old_life(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_collection_dropped_and_created_again_does_not_answer_from_its_old_life_on_efs() {
    check_a_collection_dropped_and_created_again_does_not_answer_from_its_old_life(&Fixture::efs());
}

fn check_a_rebuilt_collection_reads_the_same(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&reads());
    fixture.succeed(&[json!({"op":"rebuildCollection","collection":NOTES})]);
    let expected = fixture.answers(TempCacheMode::Off, &reads());
    assert_eq!(fixture.answers(TempCacheMode::On, &reads()), expected);
}

#[test]
fn a_rebuilt_collection_reads_the_same() {
    check_a_rebuilt_collection_reads_the_same(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_rebuilt_collection_reads_the_same_on_efs() {
    check_a_rebuilt_collection_reads_the_same(&Fixture::efs());
}

fn check_a_generation_that_comes_back_does_not_bring_back_an_old_document(fixture: &Fixture) {
    seed(fixture);
    let state = find_file(&fixture.root().join(".fylo-transactions"), "state.json");
    fixture.succeed(&[get(NOTES, FIRST)]);
    let old_state = fs::read(&state).unwrap();
    fixture.succeed(&[patch(NOTES, FIRST, &json!({"title":"changed"}))]);
    let copy = state.with_extension("json.copy");
    fs::write(&copy, &old_state).unwrap();
    fs::rename(&copy, &state).unwrap();
    let expected = fixture.answers(TempCacheMode::Off, &[get(NOTES, FIRST)]);
    assert_eq!(
        fixture.answers(TempCacheMode::On, &[get(NOTES, FIRST)]),
        expected
    );
    assert_ne!(expected[0]["result"][FIRST]["title"], "one", "{expected:?}");
}

fn find_file(folder: &Path, name: &str) -> PathBuf {
    let mut folders = vec![folder.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else if path.file_name().is_some_and(|file| file == name) {
                return path;
            }
        }
    }
    panic!("no {name} below {}", folder.display());
}

fn refused_after(fixture: &Fixture, damage: impl FnOnce(&Path)) {
    damage(&fixture.only_entry());
    let expected = fixture.answers(TempCacheMode::Off, &[get(NOTES, FIRST)]);
    let counters = fixture.counters_after(&[get(NOTES, FIRST)]);
    assert_eq!(counter(&counters, "rejected"), 1, "{counters}");
    assert_eq!(counter(&counters, "hits"), 0, "{counters}");
    assert_eq!(
        fixture.answers(TempCacheMode::On, &[get(NOTES, FIRST)]),
        expected
    );
}

fn seed_one(fixture: &Fixture) {
    fixture.succeed(&[create(NOTES), put(NOTES, FIRST, &json!({"title":"one"}))]);
    fixture.succeed(&[get(NOTES, FIRST)]);
}

#[test]
fn a_generation_that_comes_back_does_not_bring_back_an_old_document() {
    check_a_generation_that_comes_back_does_not_bring_back_an_old_document(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_generation_that_comes_back_does_not_bring_back_an_old_document_on_efs() {
    check_a_generation_that_comes_back_does_not_bring_back_an_old_document(&Fixture::efs());
}

fn check_a_truncated_entry_is_refused(fixture: &Fixture) {
    seed_one(fixture);
    refused_after(fixture, |entry| {
        let bytes = fs::read(entry).unwrap();
        fs::write(entry, &bytes[..bytes.len() - 3]).unwrap();
    });
}

#[test]
fn a_truncated_entry_is_refused() {
    check_a_truncated_entry_is_refused(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_truncated_entry_is_refused_on_efs() {
    check_a_truncated_entry_is_refused(&Fixture::efs());
}

fn check_an_entry_with_a_flipped_byte_is_refused(fixture: &Fixture) {
    seed_one(fixture);
    refused_after(fixture, |entry| {
        let mut bytes = fs::read(entry).unwrap();
        let last = bytes.len() - 6;
        bytes[last] ^= 0x01;
        fs::write(entry, &bytes).unwrap();
    });
}

#[test]
fn an_entry_with_a_flipped_byte_is_refused() {
    check_an_entry_with_a_flipped_byte_is_refused(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn an_entry_with_a_flipped_byte_is_refused_on_efs() {
    check_an_entry_with_a_flipped_byte_is_refused(&Fixture::efs());
}

fn check_an_entry_of_another_document_is_refused(fixture: &Fixture) {
    seed_one(fixture);
    let first = fixture.only_entry();
    fixture.succeed(&[put(NOTES, SECOND, &json!({"title":"two"}))]);
    fs::remove_file(&first).unwrap();
    fixture.succeed(&[get(NOTES, SECOND)]);
    let second = fixture.only_entry();
    fs::rename(&second, &first).unwrap();
    let expected = fixture.answers(TempCacheMode::Off, &[get(NOTES, FIRST)]);
    let counters = fixture.counters_after(&[get(NOTES, FIRST)]);
    assert_eq!(counter(&counters, "rejected"), 1, "{counters}");
    assert_eq!(
        fixture.answers(TempCacheMode::On, &[get(NOTES, FIRST)]),
        expected
    );
}

#[test]
fn an_entry_of_another_document_is_refused() {
    check_an_entry_of_another_document_is_refused(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn an_entry_of_another_document_is_refused_on_efs() {
    check_an_entry_of_another_document_is_refused(&Fixture::efs());
}

fn check_an_entry_of_another_collection_is_refused(fixture: &Fixture) {
    fixture.succeed(&[
        create(NOTES),
        create(OTHER),
        put(NOTES, FIRST, &json!({"title":"note"})),
        put(OTHER, FIRST, &json!({"title":"other"})),
    ]);
    fixture.succeed(&[get(OTHER, FIRST)]);
    let other = fixture.only_entry();
    let bytes = fs::read(&other).unwrap();
    fs::remove_file(&other).unwrap();
    fixture.succeed(&[get(NOTES, FIRST)]);
    let note = fixture.only_entry();
    fs::write(&note, bytes).unwrap();
    let counters = fixture.counters_after(&[get(NOTES, FIRST)]);
    assert_eq!(counter(&counters, "rejected"), 1, "{counters}");
    let replies = fixture.succeed(&[get(NOTES, FIRST)]);
    assert_eq!(replies[0]["result"][FIRST]["title"], "note", "{replies:?}");
}

#[test]
fn an_entry_of_another_collection_is_refused() {
    check_an_entry_of_another_collection_is_refused(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn an_entry_of_another_collection_is_refused_on_efs() {
    check_an_entry_of_another_collection_is_refused(&Fixture::efs());
}

fn check_an_entry_from_before_a_change_is_refused(fixture: &Fixture) {
    seed_one(fixture);
    let entry = fixture.only_entry();
    let old = fs::read(&entry).unwrap();
    fixture.succeed(&[patch(NOTES, FIRST, &json!({"title":"changed"}))]);
    fixture.succeed(&[get(NOTES, FIRST)]);
    fs::write(&entry, old).unwrap();
    let counters = fixture.counters_after(&[get(NOTES, FIRST)]);
    assert_eq!(counter(&counters, "rejected"), 1, "{counters}");
    let replies = fixture.succeed(&[get(NOTES, FIRST)]);
    assert_eq!(
        replies[0]["result"][FIRST]["title"], "changed",
        "{replies:?}"
    );
}

#[test]
fn an_entry_from_before_a_change_is_refused() {
    check_an_entry_from_before_a_change_is_refused(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn an_entry_from_before_a_change_is_refused_on_efs() {
    check_an_entry_from_before_a_change_is_refused(&Fixture::efs());
}

fn check_a_document_over_its_share_of_the_budget_is_not_kept(fixture: &Fixture) {
    let large = "x".repeat(4096);
    let application = fixture.application(TempCacheMode::On, Some(65_536));
    let requests = [
        create(NOTES),
        put(NOTES, FIRST, &json!({"title":large})),
        get(NOTES, FIRST),
        get(NOTES, FIRST),
        json!({"op":"handshake"}),
    ];
    let replies = Fixture::session_at((&fixture.root(), fixture.profile), application, &requests);
    let counters = &replies[4]["result"]["capabilities"]["tempCache"];
    assert_eq!(counter(counters, "writes"), 0, "{counters}");
    assert!(fixture.entries().is_empty());
}

#[test]
fn a_document_over_its_share_of_the_budget_is_not_kept() {
    check_a_document_over_its_share_of_the_budget_is_not_kept(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_document_over_its_share_of_the_budget_is_not_kept_on_efs() {
    check_a_document_over_its_share_of_the_budget_is_not_kept(&Fixture::efs());
}

#[test]
fn the_tier_is_off_unless_asked_for_on_a_local_root() {
    let fixture = Fixture::new();
    seed(&fixture);
    fs::remove_dir_all(fixture.base()).unwrap();
    fs::create_dir_all(fixture.base()).unwrap();
    let replies = fixture.session(
        TempCacheMode::Auto,
        &[get(NOTES, FIRST), json!({"op":"handshake"})],
    );
    let capability = &replies[1]["result"]["capabilities"]["tempCache"];
    assert_eq!(capability["active"], false, "{capability}");
    assert_eq!(capability["reason"], "root is local", "{capability}");
    assert!(fixture.cache_files().is_empty());
}

#[test]
fn a_vdbx_database_never_uses_the_tier() {
    let fixture = Fixture::new();
    seed(&fixture);
    let database = fixture.parent.join("app.vdbx");
    NativeWriteRoot::open(fixture.root())
        .unwrap()
        .pack_database(&database)
        .unwrap();
    fs::remove_dir_all(fixture.base()).unwrap();
    fs::create_dir_all(fixture.base()).unwrap();
    let application = fixture.application(TempCacheMode::On, None);
    let requests = [
        get(NOTES, FIRST),
        get(NOTES, FIRST),
        json!({"op":"handshake"}),
    ];
    let replies = Fixture::session_at(
        (&database, FilesystemProfile::Native),
        application,
        &requests,
    );
    assert_eq!(replies[0]["ok"], true, "{replies:?}");
    let capability = &replies[2]["result"]["capabilities"]["tempCache"];
    assert_eq!(capability["reason"], "vdbx", "{capability}");
    assert!(fixture.cache_files().is_empty());
}

fn check_writes_never_read_through_the_tier(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&[
        get(NOTES, FIRST),
        get(NOTES, SECOND),
        find(NOTES, &json!({})),
    ]);
    let counters = fixture.counters_after(&[
        patch(NOTES, FIRST, &json!({"title":"patched"})),
        json!({"op":"delDoc","collection":NOTES,"id":SECOND}),
        json!({"op":"executeSQL","sql":"UPDATE notes SET n = 9"}),
    ]);
    for name in ["hits", "misses", "revalidations", "rejected"] {
        assert_eq!(counter(&counters, name), 0, "{name}: {counters}");
    }
    let expected = fixture.answers(TempCacheMode::Off, &reads());
    assert_eq!(fixture.answers(TempCacheMode::On, &reads()), expected);
}

#[test]
fn writes_never_read_through_the_tier() {
    check_writes_never_read_through_the_tier(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn writes_never_read_through_the_tier_on_efs() {
    check_writes_never_read_through_the_tier(&Fixture::efs());
}

const FOURTH: &str = "4VRNF52JPCR";
const INDEX_ENTRY: &str = "index.fyc";

fn index_entry(fixture: &Fixture) -> PathBuf {
    let entries: Vec<_> = fixture
        .entries()
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == INDEX_ENTRY))
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    entries[0].clone()
}

fn queries() -> Vec<Value> {
    vec![
        find(NOTES, &json!({})),
        find(NOTES, &json!({"$ops":[{"n":{"$gte":2}}]})),
        find(NOTES, &json!({"$ops":[{"title":{"$eq":"two"}}]})),
    ]
}

fn check_a_query_reads_the_index_from_the_tier(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&queries());
    index_entry(fixture);
    let expected = fixture.answers(TempCacheMode::Off, &queries());
    assert_eq!(fixture.answers(TempCacheMode::On, &queries()), expected);
    let counters = fixture.counters_after(&queries());
    assert!(counter(&counters, "hits") >= 3, "{counters}");
    assert_eq!(counter(&counters, "rejected"), 0, "{counters}");
}

#[test]
fn a_query_reads_the_index_from_the_tier() {
    check_a_query_reads_the_index_from_the_tier(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_query_reads_the_index_from_the_tier_on_efs() {
    check_a_query_reads_the_index_from_the_tier(&Fixture::efs());
}

fn check_a_document_written_after_the_index_was_kept_is_found(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&queries());
    fixture.succeed(&[put(NOTES, FOURTH, &json!({"title":"two","n":4}))]);
    let expected = fixture.answers(TempCacheMode::Off, &queries());
    assert_eq!(fixture.answers(TempCacheMode::On, &queries()), expected);
    assert_eq!(
        expected[1]["result"].as_object().map(serde_json::Map::len),
        Some(3),
        "{expected:?}"
    );
}

#[test]
fn a_document_written_after_the_index_was_kept_is_found() {
    check_a_document_written_after_the_index_was_kept_is_found(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_document_written_after_the_index_was_kept_is_found_on_efs() {
    check_a_document_written_after_the_index_was_kept_is_found(&Fixture::efs());
}

fn check_a_damaged_index_entry_is_refused(fixture: &Fixture) {
    seed(fixture);
    fixture.succeed(&queries());
    let entry = index_entry(fixture);
    let mut bytes = fs::read(&entry).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    fs::write(&entry, bytes).unwrap();
    let expected = fixture.answers(TempCacheMode::Off, &queries());
    let counters = fixture.counters_after(&queries()[1..2]);
    assert_eq!(counter(&counters, "rejected"), 1, "{counters}");
    assert_eq!(fixture.answers(TempCacheMode::On, &queries()), expected);
}

#[test]
fn a_damaged_index_entry_is_refused() {
    check_a_damaged_index_entry_is_refused(&Fixture::new());
}

#[cfg(unix)]
#[test]
fn a_damaged_index_entry_is_refused_on_efs() {
    check_a_damaged_index_entry_is_refused(&Fixture::efs());
}

#[test]
fn word_keys_added_after_the_index_was_kept_answer_match_the_same() {
    let fixture = Fixture::new();
    fixture.succeed(&[
        create(NOTES),
        put(NOTES, FIRST, &json!({"body":"meet tom at noon"})),
        put(NOTES, SECOND, &json!({"body":"lunch room agenda"})),
        put(NOTES, THIRD, &json!({"body":"meeting notes"})),
    ]);
    let matches = [
        find(NOTES, &json!({"$ops":[{"body":{"$match":"meet"}}]})),
        find(NOTES, &json!({"$ops":[{"body":{"$match":"agenda room"}}]})),
    ];
    fixture.succeed(&matches);
    NativeWriteRoot::open(fixture.root())
        .unwrap()
        .set_word_fields(NOTES, &["body".to_owned()], &[])
        .unwrap();
    let expected = fixture.answers(TempCacheMode::Off, &matches);
    assert_eq!(fixture.answers(TempCacheMode::On, &matches), expected);
    assert_eq!(fixture.answers(TempCacheMode::On, &matches), expected);
    assert_ne!(expected[0]["result"], json!({}), "{expected:?}");
}

#[test]
fn an_index_over_its_share_of_the_budget_is_not_kept() {
    let fixture = Fixture::new();
    let application = fixture.application(TempCacheMode::On, Some(65_536));
    let mut requests = vec![create(NOTES)];
    for number in 0..40 {
        let id = format!("4VRNF52J{number:03}");
        requests.push(put(
            NOTES,
            &id,
            &json!({"title":format!("title {number} {}", "x".repeat(40)), "n":number}),
        ));
    }
    requests.push(find(NOTES, &json!({"$ops":[{"n":{"$gte":2}}]})));
    let replies = Fixture::session_at((&fixture.root(), fixture.profile), application, &requests);
    assert_eq!(replies.last().unwrap()["ok"], true, "{replies:?}");
    let kept_index = fixture
        .entries()
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == INDEX_ENTRY));
    assert!(!kept_index);
}
