use std::path::PathBuf;

use fylo_format::{ContainerDictionaryHeader, ContainerExtentKind, ContainerSuperblock};

use crate::container_file::{ContainerError, ContainerFile};
use crate::{ContainerDictionary, ContainerSealContext};

const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const USERS: u32 = 1;
const ORDERS: u32 = 2;
const SECRET: &[u8] = b"dictionary test secret";

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn created(name: &str, encrypted: bool) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("test.vdbx");
    if encrypted {
        ContainerFile::create_encrypted(&path, SHIFT, SECRET).expect("create container");
    } else {
        ContainerFile::create(&path, SHIFT).expect("create container");
    }
    (Database(dir), path)
}

fn dictionary(id: u32) -> ContainerDictionary {
    ContainerDictionary {
        id,
        bytes: (0..4000_u32)
            .map(|at| u8::try_from((at * 7 + id) % 251).expect("byte"))
            .collect(),
    }
}

#[test]
fn dictionaries_round_trip_and_are_found_by_id() {
    for encrypted in [false, true] {
        let (_guard, path) = created("vdbx-dictionary-round-trip", encrypted);
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        if encrypted {
            container.unlock(SECRET).expect("unlock");
        }
        let (first, id, length) = container
            .write_dictionary(USERS, &dictionary(1))
            .expect("write dictionary");
        assert_eq!(id, 1);
        let expected_length = if encrypted { 4000 + 28 } else { 4000 };
        assert_eq!(length, expected_length);
        let (second, _, _) = container
            .write_dictionary(USERS, &dictionary(2))
            .expect("write dictionary");
        container
            .write_dictionary(ORDERS, &dictionary(1))
            .expect("write dictionary");
        container.checkpoint().expect("checkpoint");
        assert!(container.allocator().sealed_live_bytes(first).is_some());

        let reader = ContainerFile::open_reader(&path).expect("open reader");
        let mut reader = reader;
        if encrypted {
            assert!(matches!(
                reader.read_dictionary(USERS, first),
                Err(ContainerError::Locked)
            ));
            reader.unlock(SECRET).expect("unlock");
        }
        assert_eq!(
            reader.dictionary_extents(USERS).expect("list"),
            vec![(first, 1), (second, 2)]
        );
        assert_eq!(
            reader.read_dictionary(USERS, second).expect("read"),
            dictionary(2)
        );
        assert_eq!(
            reader.find_dictionary(USERS, 1).expect("find"),
            Some(dictionary(1))
        );
        assert_eq!(reader.find_dictionary(USERS, 3).expect("find"), None);
        // Another collection's extent is not this collection's dictionary.
        assert!(reader.read_dictionary(ORDERS, first).is_err());
        let stored = std::fs::read(&path).expect("read file");
        let probe = &dictionary(2).bytes[100..140];
        assert_eq!(
            stored.windows(probe.len()).any(|window| window == probe),
            !encrypted,
            "an encrypted file stores its dictionaries sealed"
        );
    }
}

#[test]
fn a_damaged_or_misplaced_dictionary_is_refused() {
    let (_guard, path) = created("vdbx-dictionary-damage", false);
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let (extent, _, _) = container
        .write_dictionary(USERS, &dictionary(4))
        .expect("write dictionary");
    container.checkpoint().expect("checkpoint");
    let payload = extent * (1 << SHIFT) + fylo_format::ContainerExtentHeader::PAYLOAD_OFFSET;
    let mut bytes = std::fs::read(&path).expect("read file");
    let at =
        usize::try_from(payload).expect("offset") + ContainerDictionaryHeader::ENCODED_BYTES + 9;
    bytes[at] ^= 0x40;
    std::fs::write(&path, &bytes).expect("damage");
    let reader = ContainerFile::open_reader(&path).expect("open reader");
    let error = reader.read_dictionary(USERS, extent).unwrap_err();
    assert_eq!(error.code(), "EVDBX_CORRUPT", "{error}");
    // The header still names it, so it is listed, and found as damaged.
    assert_eq!(
        reader.dictionary_extents(USERS).expect("list"),
        vec![(extent, 4)]
    );
    assert!(reader.find_dictionary(USERS, 4).is_err());

    // A sealed unit opened for another dictionary id fails to open.
    let (_guard, path) = created("vdbx-dictionary-misplaced", true);
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    container.unlock(SECRET).expect("unlock");
    let sealed = container
        .cipher()
        .expect("key")
        .seal(
            ContainerSealContext::Dictionary {
                collection: USERS,
                id: 9,
            },
            &dictionary(5).bytes,
        )
        .expect("seal");
    let (extent, _, _) = container
        .write_dictionary(USERS, &dictionary(5))
        .expect("write dictionary");
    let header = ContainerDictionaryHeader::for_stored(5, true, &sealed).expect("header");
    container
        .write_payload(extent, 0, &header.encode())
        .expect("write header");
    container
        .write_payload(
            extent,
            ContainerDictionaryHeader::ENCODED_BYTES as u64,
            &sealed,
        )
        .expect("write sealed");
    let error = container.read_dictionary(USERS, extent).unwrap_err();
    assert_eq!(error.code(), "EVDBX_CORRUPT", "{error}");
}

#[test]
fn only_a_dictionary_extent_of_the_collection_is_released() {
    let (_guard, path) = created("vdbx-dictionary-release", false);
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let (extent, _, _) = container
        .write_dictionary(USERS, &dictionary(1))
        .expect("write dictionary");
    let segment = container
        .allocate(ContainerExtentKind::Segment, USERS)
        .expect("allocate");
    assert!(container.release_dictionary(USERS, segment.index).is_err());
    assert!(container.release_dictionary(ORDERS, extent).is_err());
    container
        .release_dictionary(USERS, extent)
        .expect("release");
    assert!(
        container
            .dictionary_extents(USERS)
            .expect("list")
            .is_empty()
    );
    assert!(
        ContainerFile::open_reader(&path)
            .expect("open reader")
            .write_dictionary(USERS, &dictionary(2))
            .is_err()
    );
}
