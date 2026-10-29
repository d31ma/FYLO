use super::{ContainerRecordError, ContainerStoredBody};
use crate::{
    ContainerCipher, ContainerCipherError, ContainerCodecError, ContainerFrameCodec,
    ContainerSealContext, train_dictionary,
};
use fylo_format::ContainerCodec;

const LIMIT: usize = 1024 * 1024;
const HERE: ContainerSealContext = ContainerSealContext::Record {
    collection: 2,
    lsn: 17,
    entry: 0,
};

fn document(n: usize) -> Vec<u8> {
    format!(
        "{{\"id\":\"doc_{n:06}\",\"status\":\"shipped\",\"city\":\"Halifax\",\
         \"notes\":\"Leave it at the side door if nobody answers the bell.\"}}"
    )
    .into_bytes()
}

fn cipher() -> ContainerCipher {
    ContainerCipher::derive(b"secret", &[9; 32])
}

#[test]
fn a_plain_body_round_trips_through_each_codec() {
    let body = document(1).repeat(4);
    let mut codec = ContainerFrameCodec::new(None).unwrap();
    for lz4 in [false, true] {
        let stored = ContainerStoredBody::encode(&mut codec, None, HERE, &body, lz4).unwrap();
        assert!(stored.len() < body.len());
        let parsed = ContainerStoredBody::parse(&stored).unwrap();
        let expected = if lz4 {
            ContainerCodec::Lz4
        } else {
            ContainerCodec::Zstd
        };
        assert_eq!(
            (parsed.codec, parsed.dictionary_id, parsed.sealed),
            (expected, 0, false)
        );
        assert_eq!(parsed.decode(&mut codec, None, HERE, LIMIT).unwrap(), body);
    }
}

#[test]
fn a_dictionary_id_travels_with_the_body() {
    let samples: Vec<Vec<u8>> = (0..2000).map(document).collect();
    let dictionary = train_dictionary(5, &samples).expect("train a dictionary");
    let mut codec = ContainerFrameCodec::new(Some(dictionary)).unwrap();
    let body = document(4242);
    let stored = ContainerStoredBody::encode(&mut codec, None, HERE, &body, false).unwrap();
    let parsed = ContainerStoredBody::parse(&stored).unwrap();
    assert_eq!(parsed.dictionary_id, 5);
    assert_eq!(parsed.decode(&mut codec, None, HERE, LIMIT).unwrap(), body);
    let mut without = ContainerFrameCodec::new(None).unwrap();
    assert!(matches!(
        parsed.decode(&mut without, None, HERE, LIMIT),
        Err(ContainerRecordError::Codec(
            ContainerCodecError::WrongDictionary
        ))
    ));
}

#[test]
fn an_encrypted_body_is_sealed_after_compression_and_bound_to_its_record() {
    let cipher = cipher();
    let body = document(7).repeat(8);
    let mut codec = ContainerFrameCodec::new(None).unwrap();
    let stored =
        ContainerStoredBody::encode(&mut codec, Some(&cipher), HERE, &body, false).unwrap();
    // Compressed first: the sealed body is still smaller than the plain one.
    assert!(stored.len() < body.len());
    assert!(!stored.windows(7).any(|window| window == b"Halifax"));
    let parsed = ContainerStoredBody::parse(&stored).unwrap();
    assert!(parsed.sealed);
    assert_eq!(
        parsed
            .decode(&mut codec, Some(&cipher), HERE, LIMIT)
            .unwrap(),
        body
    );

    let elsewhere = ContainerSealContext::Record {
        collection: 2,
        lsn: 18,
        entry: 0,
    };
    let moved = parsed
        .decode(&mut codec, Some(&cipher), elsewhere, LIMIT)
        .unwrap_err();
    assert!(matches!(
        moved,
        ContainerRecordError::Cipher(ContainerCipherError::Unauthenticated)
    ));
    assert_eq!(moved.code(), "EFXDB_CORRUPT");
    let locked = parsed.decode(&mut codec, None, HERE, LIMIT).unwrap_err();
    assert_eq!(locked.code(), "EKEY");
}

#[test]
fn a_plain_body_in_an_encrypted_file_is_refused() {
    let mut codec = ContainerFrameCodec::new(None).unwrap();
    let stored = ContainerStoredBody::encode(&mut codec, None, HERE, &document(3), false).unwrap();
    let parsed = ContainerStoredBody::parse(&stored).unwrap();
    assert!(matches!(
        parsed.decode(&mut codec, Some(&cipher()), HERE, LIMIT),
        Err(ContainerRecordError::NotSealed)
    ));
}

#[test]
fn malformed_headers_are_refused() {
    for stored in [
        &[][..],
        &[0x40, 1, 2],
        &[0x03, 1],
        &[0x11, 1, 0],
        &[0x11, 0, 0, 0, 0, 1],
        &[0x10, 1, 0, 0, 0, 1],
    ] {
        assert!(
            matches!(
                ContainerStoredBody::parse(stored),
                Err(ContainerRecordError::Malformed)
            ),
            "{stored:?}"
        );
    }
    let parsed = ContainerStoredBody::parse(&[0x00, b'x']).unwrap();
    assert_eq!(
        (parsed.codec, parsed.dictionary_id, parsed.sealed),
        (ContainerCodec::None, 0, false)
    );
}
