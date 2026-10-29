use super::{
    ContainerCodecError, ContainerDictionary, ContainerFrameCodec, browser_zstd, train_dictionary,
    zstd_content_size,
};
use fylo_format::ContainerCodec;

const LIMIT: usize = 1024 * 1024;

/// Small JSON documents shaped like an application's, deterministic.
fn documents(count: usize) -> Vec<Vec<u8>> {
    let statuses = ["pending", "paid", "shipped", "delivered", "refunded"];
    let cities = ["Toronto", "Montreal", "Calgary", "Halifax", "Vancouver"];
    (0..count)
        .map(|n| {
            format!(
                "{{\"id\":\"ord_{n:08x}\",\"status\":\"{}\",\"customer\":{{\"name\":\"Customer {}\",\
                 \"city\":\"{}\",\"email\":\"customer{}@example.com\"}},\"items\":[{{\"sku\":\"SKU-{}\",\
                 \"quantity\":{},\"price\":{}.99}}],\"notes\":\"Leave at the side door if nobody answers.\"}}",
                statuses[n % 5],
                n * 7 % 1000,
                cities[n % 5],
                n % 977,
                n % 131,
                n % 4 + 1,
                n % 90 + 5,
            )
            .into_bytes()
        })
        .collect()
}

fn dictionary() -> ContainerDictionary {
    train_dictionary(7, &documents(2000)).expect("train a dictionary")
}

#[test]
fn trains_a_bounded_zstd_dictionary() {
    let trained = dictionary();
    assert_eq!(trained.id, 7);
    assert!(trained.bytes.len() <= super::MAX_DICTIONARY_BYTES);
    assert!(train_dictionary(0, &documents(2000)).is_none());
    assert!(train_dictionary(1, &[b"x"]).is_none());
}

#[test]
fn zstd_round_trips_and_shrinks_with_a_dictionary() {
    let mut plain = ContainerFrameCodec::new(None).expect("codec");
    let mut trained = ContainerFrameCodec::new(Some(dictionary())).expect("codec");
    let (mut plain_bytes, mut trained_bytes, mut raw_bytes) = (0, 0, 0);
    for document in documents(300) {
        let without = plain.encode(&document).expect("encode");
        let with = trained.encode(&document).expect("encode");
        assert_eq!(with.codec, ContainerCodec::Zstd);
        assert_eq!(with.dictionary_id, 7);
        assert_eq!(
            zstd_content_size(&with.bytes).expect("size"),
            document.len()
        );
        assert_eq!(
            trained
                .decode(with.codec, with.dictionary_id, &with.bytes, LIMIT)
                .expect("decode"),
            document
        );
        assert_eq!(
            plain
                .decode(without.codec, without.dictionary_id, &without.bytes, LIMIT)
                .expect("decode"),
            document
        );
        raw_bytes += document.len();
        plain_bytes += without.bytes.len();
        trained_bytes += with.bytes.len();
    }
    assert!(
        trained_bytes * 2 < raw_bytes,
        "{trained_bytes} of {raw_bytes}"
    );
    assert!(trained_bytes < plain_bytes);
}

#[test]
fn lz4_round_trips_with_and_without_a_dictionary() {
    for dictionary in [None, Some(dictionary())] {
        let mut codec = ContainerFrameCodec::new(dictionary).expect("codec");
        for document in documents(100) {
            let encoded = codec.encode_lz4(&document).expect("encode");
            assert_eq!(
                codec
                    .decode(encoded.codec, encoded.dictionary_id, &encoded.bytes, LIMIT)
                    .expect("decode"),
                document
            );
        }
    }
}

#[test]
fn the_browser_decoder_reads_native_zstd() {
    let trained = dictionary();
    let mut native = ContainerFrameCodec::new(Some(trained.clone())).expect("codec");
    let mut browser = browser_zstd::Decoder::new(Some(&trained)).expect("decoder");
    let mut plain_native = ContainerFrameCodec::new(None).expect("codec");
    let mut plain_browser = browser_zstd::Decoder::new(None).expect("decoder");
    let mut large = Vec::new();
    for document in documents(400) {
        large.extend_from_slice(&document);
    }
    for document in documents(50).into_iter().chain([large]) {
        let encoded = native.encode(&document).expect("encode");
        assert_eq!(
            browser
                .decode(&encoded.bytes, document.len())
                .expect("decode"),
            document
        );
        let encoded = plain_native.encode(&document).expect("encode");
        assert_eq!(
            plain_browser
                .decode(&encoded.bytes, document.len())
                .expect("decode"),
            document
        );
    }
}

#[test]
fn incompressible_bodies_are_stored_as_is() {
    let mut codec = ContainerFrameCodec::new(Some(dictionary())).expect("codec");
    let body = b"{}";
    for encoded in [
        codec.encode(body).expect("encode"),
        codec.encode_lz4(body).expect("encode"),
    ] {
        assert_eq!(encoded.codec, ContainerCodec::None);
        assert_eq!(encoded.dictionary_id, 0);
        assert_eq!(encoded.bytes, body);
        assert_eq!(
            codec
                .decode(encoded.codec, 0, &encoded.bytes, LIMIT)
                .expect("decode"),
            body
        );
    }
}

#[test]
fn refuses_oversized_wrong_dictionary_and_damaged_bodies() {
    let mut codec = ContainerFrameCodec::new(Some(dictionary())).expect("codec");
    let document = documents(1).remove(0);
    let zstd = codec.encode(&document).expect("encode");
    let lz4 = codec.encode_lz4(&document).expect("encode");
    for encoded in [&zstd, &lz4] {
        assert!(matches!(
            codec.decode(encoded.codec, 7, &encoded.bytes, document.len() - 1),
            Err(ContainerCodecError::Oversized)
        ));
        assert!(matches!(
            codec.decode(encoded.codec, 8, &encoded.bytes, LIMIT),
            Err(ContainerCodecError::WrongDictionary)
        ));
        let truncated = &encoded.bytes[..encoded.bytes.len() - 3];
        assert!(matches!(
            codec.decode(encoded.codec, 7, truncated, LIMIT),
            Err(ContainerCodecError::Corrupt)
        ));
    }
    // The browser decoder stops at the declared size.
    let mut browser = browser_zstd::Decoder::new(Some(&dictionary())).expect("decoder");
    assert!(browser.decode(&zstd.bytes, document.len() - 10).is_err());
    let mut damaged = zstd.bytes.clone();
    damaged[0] ^= 1;
    assert!(matches!(
        codec.decode(ContainerCodec::Zstd, 7, &damaged, LIMIT),
        Err(ContainerCodecError::Corrupt)
    ));
    // An LZ4 body declaring fewer bytes than it decodes to.
    let mut short = lz4.bytes.clone();
    short[..4].copy_from_slice(
        &u32::try_from(document.len() - 10)
            .expect("small")
            .to_le_bytes(),
    );
    assert!(matches!(
        codec.decode(ContainerCodec::Lz4, 7, &short, LIMIT),
        Err(ContainerCodecError::Corrupt)
    ));
    assert!(matches!(
        codec.decode(ContainerCodec::None, 7, b"{}", LIMIT),
        Err(ContainerCodecError::Corrupt)
    ));
}

#[test]
fn refuses_a_dictionary_that_is_not_zstd() {
    for dictionary in [
        ContainerDictionary {
            id: 1,
            bytes: b"just some text".to_vec(),
        },
        ContainerDictionary {
            id: 0,
            bytes: dictionary().bytes,
        },
    ] {
        assert!(matches!(
            ContainerFrameCodec::new(Some(dictionary)),
            Err(ContainerCodecError::BadDictionary)
        ));
    }
}

#[test]
fn reads_every_content_size_field_width() {
    let mut codec = ContainerFrameCodec::new(None).expect("codec");
    for length in [0_usize, 1, 200, 255, 256, 300, 65_791, 65_792, 70_000] {
        let body: Vec<u8> = [0_u8, 1, 2, 3, 4, 5, 6]
            .into_iter()
            .cycle()
            .take(length)
            .collect();
        let bytes = codec.compressor.compress(&body).expect("compress");
        assert_eq!(zstd_content_size(&bytes).expect("size"), length, "{length}");
    }
    assert!(zstd_content_size(b"\x28\xb5\x2f\xfd").is_err());
    assert!(zstd_content_size(b"not zstd").is_err());
}
