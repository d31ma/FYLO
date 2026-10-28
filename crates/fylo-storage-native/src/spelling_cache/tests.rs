use super::SpellingCache;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct SpellingFixture(PathBuf);

impl SpellingFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-spelling-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("scope/shard")).unwrap();
        Self(path)
    }

    fn shard(&self) -> PathBuf {
        self.0.join("scope/shard")
    }

    fn scope(&self) -> PathBuf {
        self.0.join("scope")
    }
}

impl Drop for SpellingFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn finds_a_name_created_after_the_directory_was_cached() {
    let fixture = SpellingFixture::new();
    let cache = SpellingCache::new();
    assert!(
        !cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
    fs::write(fixture.shard().join("A.json"), b"{}").unwrap();
    assert!(
        cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
}

#[test]
fn accepts_only_the_stored_spelling() {
    let fixture = SpellingFixture::new();
    let cache = SpellingCache::new();
    fs::write(fixture.shard().join("A.json"), b"{}").unwrap();
    assert!(
        cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
    assert!(
        !cache
            .contains(&fixture.shard(), OsStr::new("a.json"))
            .unwrap()
    );
}

#[test]
fn a_new_generation_drops_stale_names_within_its_scope() {
    let fixture = SpellingFixture::new();
    let cache = SpellingCache::new();
    fs::write(fixture.shard().join("A.json"), b"{}").unwrap();
    cache.observe_generation(&fixture.scope(), 1);
    assert!(
        cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
    fs::rename(
        fixture.shard().join("A.json"),
        fixture.shard().join("B.json"),
    )
    .unwrap();
    cache.observe_generation(&fixture.scope(), 1);
    assert!(
        cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
    cache.observe_generation(&fixture.scope(), 2);
    assert!(
        !cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
}

#[test]
fn forgetting_a_directory_reads_it_again() {
    let fixture = SpellingFixture::new();
    let cache = SpellingCache::new();
    fs::write(fixture.shard().join("A.json"), b"{}").unwrap();
    assert!(
        cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
    fs::remove_file(fixture.shard().join("A.json")).unwrap();
    cache.forget(&fixture.shard());
    assert!(
        !cache
            .contains(&fixture.shard(), OsStr::new("A.json"))
            .unwrap()
    );
}
