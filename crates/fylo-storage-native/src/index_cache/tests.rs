use super::{IndexCache, IndexInputs};
use crate::{NativeRoot, NativeStorageErrorCode, NativeWriteRoot};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

struct IndexCacheFixture(PathBuf);

impl IndexCacheFixture {
    const COLLECTION_NAME: &str = "users";
    const SNAPSHOT_FILE: &str = "keys.snapshot";
    const LOG_FILE: &str = "keys.wal";
    const NEXT_SNAPSHOT_FILE: &str = "next.snapshot";
    const FIRST_RECORD: &[u8] = b"name/eq/Ada/4VRNF52JPCO\n";
    const SECOND_RECORD: &[u8] = b"name/eq/Bob/4VRNF52JPCO\n";
    const THIRD_RECORD: &[u8] = b"name/eq/Eve/4VRNF52JPCO\n";
    const SIMPLE_RECORD: &[u8] = b"a/id\n";
    const LARGER_RECORD: &[u8] = b"larger/id\n";
    const CORRUPT_SNAPSHOT: &[u8] = b"z/id\na/id\n";
    const CACHE_COPIES: usize = 2;
    const RESERVED_BYTES: usize = 1024;
    const EMPTY_CONTENT: &[u8] = b"";

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-index-cache-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        NativeWriteRoot::open(&path)
            .unwrap()
            .create_collection(
                IndexCacheFixture::COLLECTION_NAME,
                crate::CollectionKind::Document,
                None,
            )
            .unwrap();
        Self(path)
    }

    fn collection(&self) -> crate::NativeCollection {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection(IndexCacheFixture::COLLECTION_NAME)
            .unwrap()
    }

    fn index(&self, name: &str) -> PathBuf {
        self.0.join(".collections/users/index").join(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.index(name), bytes).unwrap();
    }
}

impl Drop for IndexCacheFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn refreshes_snapshot_and_wal_without_generation_changes() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    fixture.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::FIRST_RECORD,
    );
    let initial = collection.shared_index_snapshot().unwrap();
    fixture.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::SECOND_RECORD,
    );
    assert_eq!(collection.generation().unwrap().generation, 0);
    assert_eq!(
        collection.shared_index_snapshot().unwrap().as_bytes(),
        IndexCacheFixture::SECOND_RECORD
    );
    fixture.write(
        IndexCacheFixture::LOG_FILE,
        b"-\tname/eq/Bob/4VRNF52JPCO\n+\tname/eq/Eve/4VRNF52JPCO\n",
    );
    assert_eq!(
        collection.shared_index_snapshot().unwrap().as_bytes(),
        IndexCacheFixture::THIRD_RECORD
    );
    assert_eq!(initial.as_bytes(), IndexCacheFixture::FIRST_RECORD);
}

#[test]
fn cached_view_does_not_hide_corrupt_or_missing_snapshot() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    collection.shared_index_snapshot().unwrap();
    fixture.write(IndexCacheFixture::SNAPSHOT_FILE, b"z/eq/b/id\na/eq/a/id\n");
    assert_eq!(
        collection.shared_index_snapshot().unwrap_err().code(),
        NativeStorageErrorCode::CorruptIndex
    );
    fs::remove_file(fixture.index(IndexCacheFixture::SNAPSHOT_FILE)).unwrap();
    assert!(collection.shared_index_snapshot().is_err());
}

#[test]
fn refreshes_missing_wal_torn_tail_replacement_and_truncation() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    fs::remove_file(fixture.index(IndexCacheFixture::LOG_FILE)).unwrap();
    assert!(
        collection
            .shared_index_snapshot()
            .unwrap()
            .as_bytes()
            .is_empty()
    );
    fixture.write(IndexCacheFixture::LOG_FILE, b"+\tname/eq/Ada/4VRNF52JPCO");
    assert!(
        collection
            .shared_index_snapshot()
            .unwrap()
            .as_bytes()
            .is_empty()
    );
    fixture.write(IndexCacheFixture::LOG_FILE, b"+\tname/eq/Ada/4VRNF52JPCO\n");
    assert_eq!(
        collection.shared_index_snapshot().unwrap().as_bytes(),
        IndexCacheFixture::FIRST_RECORD
    );
    fixture.write(
        IndexCacheFixture::NEXT_SNAPSHOT_FILE,
        IndexCacheFixture::SECOND_RECORD,
    );
    fs::rename(
        fixture.index(IndexCacheFixture::NEXT_SNAPSHOT_FILE),
        fixture.index(IndexCacheFixture::SNAPSHOT_FILE),
    )
    .unwrap();
    fixture.write(
        IndexCacheFixture::LOG_FILE,
        IndexCacheFixture::EMPTY_CONTENT,
    );
    assert_eq!(
        collection.shared_index_snapshot().unwrap().as_bytes(),
        IndexCacheFixture::SECOND_RECORD
    );
    fixture.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::EMPTY_CONTENT,
    );
    assert!(
        collection
            .shared_index_snapshot()
            .unwrap()
            .as_bytes()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn cached_view_does_not_bypass_wal_symlink_rejection() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    collection.shared_index_snapshot().unwrap();
    fs::remove_file(fixture.index(IndexCacheFixture::LOG_FILE)).unwrap();
    std::os::unix::fs::symlink(
        fixture.index(IndexCacheFixture::SNAPSHOT_FILE),
        fixture.index(IndexCacheFixture::LOG_FILE),
    )
    .unwrap();
    assert_eq!(
        collection.shared_index_snapshot().unwrap_err().code(),
        NativeStorageErrorCode::UnsafePath
    );
}

#[test]
fn reuses_identical_inputs_from_distinct_roots() {
    let left = IndexCacheFixture::new();
    let right = IndexCacheFixture::new();
    left.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::FIRST_RECORD,
    );
    right.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::FIRST_RECORD,
    );
    let cache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
    let initial = left
        .collection()
        .index_snapshot_with_cache(&cache, crate::MAX_INDEX_WAL_BYTES)
        .unwrap();
    let repeated = right
        .collection()
        .index_snapshot_with_cache(&cache, crate::MAX_INDEX_WAL_BYTES)
        .unwrap();
    assert!(Arc::ptr_eq(&initial, &repeated));
    assert_eq!(
        left.collection().index_snapshot().unwrap().as_bytes(),
        initial.as_bytes()
    );
}

#[test]
fn reloads_after_root_replacement_at_the_same_path() {
    let left = IndexCacheFixture::new();
    let right = IndexCacheFixture::new();
    left.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::FIRST_RECORD,
    );
    right.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::SECOND_RECORD,
    );
    let collection = left.collection();
    let cache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
    let initial = collection
        .index_snapshot_with_cache(&cache, crate::MAX_INDEX_WAL_BYTES)
        .unwrap();
    let temporary = left.0.with_extension("swapping");
    fs::rename(&left.0, &temporary).unwrap();
    fs::rename(&right.0, &left.0).unwrap();
    fs::rename(&temporary, &right.0).unwrap();
    let replacement = collection
        .index_snapshot_with_cache(&cache, crate::MAX_INDEX_WAL_BYTES)
        .unwrap();
    assert_eq!(replacement.as_bytes(), IndexCacheFixture::SECOND_RECORD);
    assert_eq!(initial.as_bytes(), IndexCacheFixture::FIRST_RECORD);
}

fn inputs(bytes: &[u8]) -> IndexInputs {
    IndexInputs {
        snapshot: bytes.to_vec(),
        log: None,
    }
}

#[test]
fn bounds_admission_by_inputs_and_merged_bytes() {
    let bytes = IndexCacheFixture::FIRST_RECORD;
    let admitted = IndexCache::new(bytes.len() * IndexCacheFixture::CACHE_COPIES);
    let initial = admitted.load(inputs(bytes)).unwrap();
    assert!(Arc::ptr_eq(
        &initial,
        &admitted.load(inputs(bytes)).unwrap()
    ));
    let bypassed = IndexCache::new(bytes.len() * IndexCacheFixture::CACHE_COPIES - 1);
    let initial = bypassed.load(inputs(bytes)).unwrap();
    assert!(!Arc::ptr_eq(
        &initial,
        &bypassed.load(inputs(bytes)).unwrap()
    ));
    let buffered = IndexCache::new(bytes.len() * IndexCacheFixture::CACHE_COPIES);
    let mut oversized = inputs(bytes);
    oversized
        .snapshot
        .reserve(IndexCacheFixture::RESERVED_BYTES);
    buffered.load(oversized).unwrap();
    assert!(buffered.entry.lock().unwrap().is_none());
}

#[test]
fn replaces_one_entry_without_invalidating_existing_readers() {
    let cache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
    let first = cache.load(inputs(IndexCacheFixture::FIRST_RECORD)).unwrap();
    cache
        .load(inputs(IndexCacheFixture::SECOND_RECORD))
        .unwrap();
    let reloaded = cache.load(inputs(IndexCacheFixture::FIRST_RECORD)).unwrap();
    assert!(!Arc::ptr_eq(&first, &reloaded));
    assert_eq!(first.as_bytes(), reloaded.as_bytes());
}

#[test]
fn cache_poisoning_does_not_hide_validated_inputs() {
    let cache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
    let bytes = IndexCacheFixture::FIRST_RECORD;
    let initial = cache.load(inputs(bytes)).unwrap();
    assert!(
        std::panic::catch_unwind(|| {
            let _guard = cache.entry.lock().unwrap();
            panic!("interrupt cache access");
        })
        .is_err()
    );
    let repeated = cache.load(inputs(bytes)).unwrap();
    assert!(Arc::ptr_eq(&initial, &repeated));
    assert!(
        cache
            .load(inputs(IndexCacheFixture::CORRUPT_SNAPSHOT))
            .is_err()
    );
    let replacement = cache
        .load(inputs(IndexCacheFixture::SECOND_RECORD))
        .unwrap();
    assert_eq!(replacement.as_bytes(), IndexCacheFixture::SECOND_RECORD);
    assert_eq!(initial.as_bytes(), bytes);
}

#[test]
fn replays_complete_wal_records_and_ignores_malformed_records() {
    let cache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
    let snapshot = cache
        .load(IndexInputs {
            snapshot: b"a/id\nc/id\n".to_vec(),
            log: Some(
                b"\n+\n+\t\n+xb/id\n?\tb/id\n-\ta/id\n-\tmissing/id\n+\tb/id\n+\tb/id\n-\tc/id\n+\tc/id\n+\ttorn/id"
                    .to_vec(),
            ),
        })
        .unwrap();
    assert_eq!(snapshot.as_bytes(), b"b/id\nc/id\n");
}

#[test]
fn bypassed_inputs_preserve_the_last_admitted_snapshot() {
    let bytes = IndexCacheFixture::SIMPLE_RECORD;
    let cache = IndexCache::new(bytes.len() * IndexCacheFixture::CACHE_COPIES);
    let initial = cache.load(inputs(bytes)).unwrap();
    assert_eq!(
        cache
            .load(inputs(IndexCacheFixture::LARGER_RECORD))
            .unwrap()
            .as_bytes(),
        IndexCacheFixture::LARGER_RECORD
    );
    let mut buffered_log = Vec::with_capacity(IndexCacheFixture::RESERVED_BYTES);
    buffered_log.extend_from_slice(b"-\ta/id\n");
    assert!(
        cache
            .load(IndexInputs {
                snapshot: bytes.to_vec(),
                log: Some(buffered_log),
            })
            .unwrap()
            .as_bytes()
            .is_empty()
    );
    assert!(Arc::ptr_eq(&initial, &cache.load(inputs(bytes)).unwrap()));
}

#[test]
fn rejects_corrupt_snapshot_before_replaying_repairing_wal() {
    let cache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
    let bytes = IndexCacheFixture::SIMPLE_RECORD;
    let initial = cache.load(inputs(bytes)).unwrap();
    let error = cache
        .load(IndexInputs {
            snapshot: IndexCacheFixture::CORRUPT_SNAPSHOT.to_vec(),
            log: Some(b"-\tz/id\n".to_vec()),
        })
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptIndex);
    assert!(
        error
            .to_string()
            .contains("prefix index snapshot is corrupt:")
    );
    assert!(Arc::ptr_eq(&initial, &cache.load(inputs(bytes)).unwrap()));
}

struct GeneratedIndex {
    snapshot: Vec<u8>,
    log: Vec<u8>,
}

impl GeneratedIndex {
    const FIELDS: u64 = 5;
    const VALUES: u64 = 400;
    const SNAPSHOT_KEYS: usize = 3000;
    const LOG_LINES: usize = 900;
    const EMPTY_LINE_EVERY: usize = 97;

    fn new() -> Self {
        let mut seed = 7_u64;
        let mut keys = std::collections::BTreeSet::new();
        while keys.len() < Self::SNAPSHOT_KEYS {
            keys.insert(Self::key(&mut seed));
        }
        let mut snapshot = Vec::new();
        for (position, key) in keys.iter().enumerate() {
            if position % Self::EMPTY_LINE_EVERY == 0 {
                snapshot.push(b'\n');
            }
            snapshot.extend_from_slice(key.as_bytes());
            snapshot.push(b'\n');
        }
        snapshot.pop();
        let existing: Vec<&String> = keys.iter().collect();
        let mut log = Vec::new();
        for line in 0..Self::LOG_LINES {
            let index = Self::next(&mut seed, existing.len() as u64);
            let removed = existing[usize::try_from(index).unwrap()];
            let entry = match line % 3 {
                0 => format!("-\t{removed}\n"),
                1 => format!("+\t{}\n", Self::key(&mut seed)),
                _ => format!("+\t{removed}\n"),
            };
            log.extend_from_slice(entry.as_bytes());
        }
        log.extend_from_slice(b"+\tf2/eq/00150/torn");
        Self { snapshot, log }
    }

    fn next(seed: &mut u64, below: u64) -> u64 {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*seed >> 33) % below
    }

    fn key(seed: &mut u64) -> String {
        let field = Self::next(seed, Self::FIELDS);
        let value = Self::next(seed, Self::VALUES);
        let id = Self::next(seed, 1_000_000);
        format!("f{field}/eq/{value:05}/4V{id:06}")
    }

    fn ranges() -> Vec<fylo_query::IndexKeyRange> {
        use fylo_query::IndexKeyRange;
        vec![
            IndexKeyRange::prefix("f2/eq/00150/"),
            IndexKeyRange::prefix("f0/eq/00000/"),
            IndexKeyRange::prefix("f4/eq/00399/"),
            IndexKeyRange::prefix("f1/"),
            IndexKeyRange::prefix("f3/eq/001"),
            IndexKeyRange::prefix(""),
            IndexKeyRange::prefix("a"),
            IndexKeyRange::prefix("zzz"),
            IndexKeyRange::new(b"f2/eq/00100".to_vec(), b"f2/eq/00200".to_vec()),
            IndexKeyRange::new(b"f2/eq/00200".to_vec(), b"f2/eq/00100".to_vec()),
            IndexKeyRange::new(b"f0".to_vec(), Vec::new()),
        ]
    }
}

fn keys_in(snapshot: &[u8], ranges: &[fylo_query::IndexKeyRange]) -> Vec<Vec<u8>> {
    snapshot
        .split(|byte| *byte == b'\n')
        .filter(|key| ranges.iter().any(|range| range.contains(key)))
        .map(<[u8]>::to_vec)
        .collect()
}

fn lines(snapshot: &[u8]) -> Vec<Vec<u8>> {
    keys_in(snapshot, &[fylo_query::IndexKeyRange::prefix("")])
        .into_iter()
        .filter(|key| !key.is_empty())
        .collect()
}

#[test]
fn ranges_read_exactly_the_keys_the_full_index_holds_in_them() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    let generated = GeneratedIndex::new();
    fixture.write(IndexCacheFixture::SNAPSHOT_FILE, &generated.snapshot);
    fixture.write(IndexCacheFixture::LOG_FILE, &generated.log);
    let full = collection.index_snapshot().unwrap();
    let ranges = GeneratedIndex::ranges();
    for range in &ranges {
        let read = collection
            .index_snapshot_in_ranges(std::slice::from_ref(range))
            .unwrap();
        let expected = keys_in(full.as_bytes(), std::slice::from_ref(range));
        assert_eq!(
            lines(read.as_bytes()),
            lines(&expected.join(&b'\n')),
            "{range:?}"
        );
    }
    let several = &ranges[..4];
    assert_eq!(
        lines(
            collection
                .index_snapshot_in_ranges(several)
                .unwrap()
                .as_bytes()
        ),
        lines(&keys_in(full.as_bytes(), several).join(&b'\n'))
    );
}

#[test]
fn ranges_apply_the_log_without_a_snapshot_run() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    fixture.write(
        IndexCacheFixture::SNAPSHOT_FILE,
        IndexCacheFixture::SECOND_RECORD,
    );
    fixture.write(
        IndexCacheFixture::LOG_FILE,
        b"-\tname/eq/Bob/4VRNF52JPCO\n+\tname/eq/Eve/4VRNF52JPCO\n+\tname/eq/Ada/torn",
    );
    let names = [fylo_query::IndexKeyRange::prefix("name/eq/")];
    assert_eq!(
        collection
            .index_snapshot_in_ranges(&names)
            .unwrap()
            .as_bytes(),
        IndexCacheFixture::THIRD_RECORD
    );
    fs::remove_file(fixture.index(IndexCacheFixture::LOG_FILE)).unwrap();
    assert_eq!(
        collection
            .index_snapshot_in_ranges(&names)
            .unwrap()
            .as_bytes(),
        IndexCacheFixture::SECOND_RECORD
    );
}

#[test]
fn ranges_report_an_overlong_key_they_read() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    let mut snapshot = b"a/eq/1/id\nb/eq/".to_vec();
    snapshot.extend(std::iter::repeat_n(b'x', 9000));
    snapshot.extend_from_slice(b"/id\nc/eq/1/id\n");
    fixture.write(IndexCacheFixture::SNAPSHOT_FILE, &snapshot);
    let error = collection
        .index_snapshot_in_ranges(&[fylo_query::IndexKeyRange::prefix("b/")])
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptIndex);
}

#[test]
fn ranges_report_a_missing_snapshot() {
    let fixture = IndexCacheFixture::new();
    let collection = fixture.collection();
    fs::remove_file(fixture.index(IndexCacheFixture::SNAPSHOT_FILE)).unwrap();
    assert!(
        collection
            .index_snapshot_in_ranges(&[fylo_query::IndexKeyRange::prefix("a/")])
            .is_err()
    );
}
