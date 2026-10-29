use super::{ContainerCipher, ContainerCipherError, ContainerSealContext, hmac_sha256};

const SECRET: &[u8] = b"correct horse battery staple";
const RECORD: ContainerSealContext = ContainerSealContext::Record {
    collection: 3,
    lsn: 41,
    entry: 2,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[test]
fn derivation_is_rfc_5869_hkdf_sha256() {
    // RFC 5869 test case 1, first 32 bytes of the output.
    let ikm = [0x0b_u8; 22];
    let salt: Vec<u8> = (0x00..=0x0c).collect();
    let info: Vec<u8> = (0xf0..=0xf9).collect();
    let prk = hmac_sha256(&salt, &[&ikm]);
    assert_eq!(
        hex(&prk),
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
    );
    assert_eq!(
        hex(&hmac_sha256(&prk, &[&info, &[1]])),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
    );
}

#[test]
fn seals_and_opens_in_place() {
    let salt = ContainerCipher::new_salt().expect("salt");
    let cipher = ContainerCipher::derive(SECRET, &salt);
    let body = b"{\"name\":\"Ada\"}";
    let sealed = cipher.seal(RECORD, body).expect("seal");
    assert_eq!(
        sealed.len(),
        super::NONCE_BYTES + body.len() + super::TAG_BYTES
    );
    assert_eq!(cipher.open(RECORD, &sealed).expect("open"), body);
    // A fresh nonce every time.
    assert_ne!(cipher.seal(RECORD, body).expect("seal"), sealed);
}

#[test]
fn a_unit_opens_only_where_it_was_sealed() {
    let salt = ContainerCipher::new_salt().expect("salt");
    let cipher = ContainerCipher::derive(SECRET, &salt);
    let sealed = cipher.seal(RECORD, b"payload").expect("seal");
    for elsewhere in [
        ContainerSealContext::Record {
            collection: 4,
            lsn: 41,
            entry: 2,
        },
        ContainerSealContext::Record {
            collection: 3,
            lsn: 42,
            entry: 2,
        },
        ContainerSealContext::Record {
            collection: 3,
            lsn: 41,
            entry: 3,
        },
        ContainerSealContext::Directory { block: 3 },
        ContainerSealContext::Dictionary {
            collection: 3,
            id: 2,
        },
        ContainerSealContext::Snapshot {
            collection: 3,
            checkpoint: 41,
            kind: fylo_format::ContainerExtentKind::Locations,
        },
        ContainerSealContext::Snapshot {
            collection: 3,
            checkpoint: 41,
            kind: fylo_format::ContainerExtentKind::Index,
        },
    ] {
        assert_eq!(
            cipher.open(elsewhere, &sealed),
            Err(ContainerCipherError::Unauthenticated),
            "{elsewhere:?}"
        );
    }
    let mut flipped = sealed.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert_eq!(
        cipher.open(RECORD, &flipped),
        Err(ContainerCipherError::Unauthenticated)
    );
    assert_eq!(
        cipher.open(RECORD, &sealed[..20]),
        Err(ContainerCipherError::Unauthenticated)
    );
    let other_file = ContainerCipher::derive(SECRET, &ContainerCipher::new_salt().expect("salt"));
    assert_eq!(
        other_file.open(RECORD, &sealed),
        Err(ContainerCipherError::Unauthenticated)
    );
}

#[test]
fn a_wrong_secret_is_reported_at_open() {
    let salt = ContainerCipher::new_salt().expect("salt");
    let key_check = ContainerCipher::derive(SECRET, &salt).key_check();
    let reopened = ContainerCipher::open_file(SECRET, &salt, &key_check).expect("right key");
    let sealed = reopened.seal(RECORD, b"x").expect("seal");
    assert_eq!(
        ContainerCipher::derive(SECRET, &salt)
            .open(RECORD, &sealed)
            .expect("same key"),
        b"x"
    );
    assert_eq!(
        ContainerCipher::open_file(b"wrong", &salt, &key_check).map(|_| ()),
        Err(ContainerCipherError::WrongKey)
    );
    // The key check is not the key: it does not open what the key sealed.
    assert_ne!(&key_check[..], &salt[..16]);
}
