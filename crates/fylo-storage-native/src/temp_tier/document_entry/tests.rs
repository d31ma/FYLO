use super::DocumentEntry;

fn entry() -> DocumentEntry {
    DocumentEntry {
        collection: "docs/users".to_owned(),
        identifier: "01JABCDEFGHJKMNPQRSTVWXYZ0".to_owned(),
        collection_stamp: [3_u8; 32],
        document_stamp: [5_u8; 32],
        modified_millis: 1_700_000_000_123,
        relative_path: "Z0/01JABCDEFGHJKMNPQRSTVWXYZ0.json".to_owned(),
        payload: br#"{"name":"Ada"}"#.to_vec(),
        manifest: br#"{"user.fylo.record-identity":"WyJmeWxvIl0="}"#.to_vec(),
    }
}

#[test]
fn an_entry_reads_back_from_its_bytes() {
    let entry = entry();
    assert_eq!(DocumentEntry::decode(&entry.encode()), Some(entry));
}

#[test]
fn an_empty_payload_reads_back() {
    let entry = DocumentEntry {
        payload: Vec::new(),
        ..entry()
    };
    assert_eq!(DocumentEntry::decode(&entry.encode()), Some(entry));
}

#[test]
fn an_entry_without_a_manifest_reads_back() {
    let entry = DocumentEntry {
        manifest: Vec::new(),
        ..entry()
    };
    assert_eq!(DocumentEntry::decode(&entry.encode()), Some(entry));
}

#[test]
fn an_entry_of_the_first_format_is_refused() {
    let mut bytes = entry().encode();
    bytes[4] = 1;
    assert_eq!(DocumentEntry::decode(&bytes), None);
}

#[test]
fn every_truncation_is_refused() {
    let bytes = entry().encode();
    for length in 0..bytes.len() {
        assert_eq!(DocumentEntry::decode(&bytes[..length]), None, "{length}");
    }
}

#[test]
fn every_flipped_byte_is_refused() {
    let bytes = entry().encode();
    for position in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[position] ^= 0x40;
        assert_eq!(DocumentEntry::decode(&damaged), None, "{position}");
    }
}

#[test]
fn trailing_bytes_are_refused() {
    let mut bytes = entry().encode();
    bytes.push(0);
    assert_eq!(DocumentEntry::decode(&bytes), None);
}

#[test]
fn the_header_has_a_fixed_size() {
    let entry = entry();
    let strings = entry.collection.len() + entry.identifier.len() + entry.relative_path.len();
    let body = entry.payload.len() + entry.manifest.len();
    let expected = DocumentEntry::header_bytes() + strings + body + 4;
    assert_eq!(entry.encode().len(), expected);
}

#[test]
fn a_text_too_long_for_its_length_field_does_not_fit() {
    let entry = DocumentEntry {
        relative_path: "a".repeat(usize::from(u16::MAX) + 1),
        ..entry()
    };
    assert!(!entry.fits());
}
