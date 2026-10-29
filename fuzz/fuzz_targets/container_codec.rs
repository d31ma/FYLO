#![no_main]

use std::cell::RefCell;
use std::sync::OnceLock;

use fylo_format::ContainerCodec;
use fylo_storage_native::{ContainerDictionary, ContainerFrameCodec, train_dictionary};
use libfuzzer_sys::fuzz_target;

const MAX_BYTES: usize = 64 * 1024;
const DICTIONARY_ID: u32 = 7;
const CODECS: [ContainerCodec; 3] = [
    ContainerCodec::None,
    ContainerCodec::Zstd,
    ContainerCodec::Lz4,
];

// Trained once, from samples shaped like the JSON records a collection
// holds, so dictionary frames take the same paths as real ones.
fn dictionary() -> &'static ContainerDictionary {
    static DICTIONARY: OnceLock<ContainerDictionary> = OnceLock::new();
    DICTIONARY.get_or_init(|| {
        let samples: Vec<Vec<u8>> = (0..1024_u32)
            .map(|index| {
                format!(
                    r#"{{"id":"{index:08x}","name":"user-{}","email":"user{}@example.com","active":{},"tags":["alpha","beta","{}"],"score":{}}}"#,
                    index % 37,
                    index * 7,
                    index % 2 == 0,
                    index % 5,
                    index * 13 % 1000,
                )
                .into_bytes()
            })
            .collect();
        train_dictionary(DICTIONARY_ID, &samples).expect("the fixed samples train a dictionary")
    })
}

thread_local! {
    // Reused across inputs, as a collection reuses its codec across frames.
    static FRAME_CODECS: RefCell<[ContainerFrameCodec; 2]> = RefCell::new([
        ContainerFrameCodec::new(None).expect("a codec without a dictionary"),
        ContainerFrameCodec::new(Some(dictionary().clone())).expect("a codec with a dictionary"),
    ]);
}

// Every `.fxdb` record body decoder (ADR 0016) must reject arbitrary bytes
// without panicking, never return more than the caller's limit, and decode
// whatever its encoders produce back to the input.
fuzz_target!(|input: &[u8]| {
    let limit = input
        .first_chunk::<2>()
        .map_or(MAX_BYTES, |bytes| usize::from(u16::from_le_bytes(*bytes)));
    FRAME_CODECS.with_borrow_mut(|frame_codecs| {
        for index in 0..frame_codecs.len() {
            let own_id = frame_codecs[index].dictionary_id();
            let other_id = frame_codecs[1 - index].dictionary_id();
            let codec = &mut frame_codecs[index];
            for kind in CODECS {
                let id = if kind == ContainerCodec::None {
                    0
                } else {
                    own_id
                };
                for max_bytes in [MAX_BYTES, limit] {
                    if let Ok(body) = codec.decode(kind, id, input, max_bytes) {
                        assert!(body.len() <= max_bytes, "{kind:?} decoded past its limit");
                    }
                }
                let foreign = codec.decode(kind, other_id, input, MAX_BYTES);
                if kind != ContainerCodec::None {
                    assert!(
                        foreign.is_err(),
                        "{kind:?} decoded under a foreign dictionary"
                    );
                }
            }

            let encoded = [
                codec.encode(input).expect("zstd encodes any body"),
                codec.encode_lz4(input).expect("lz4 encodes any small body"),
            ];
            for body in encoded {
                assert!(body.bytes.len() <= input.len(), "encoding grew the body");
                let decoded = codec
                    .decode(body.codec, body.dictionary_id, &body.bytes, input.len())
                    .expect("an encoded body decodes within its own length");
                assert_eq!(
                    decoded, input,
                    "{:?} round trip changed the body",
                    body.codec
                );
                if body.codec != ContainerCodec::None {
                    assert!(
                        input.is_empty()
                            || codec
                                .decode(
                                    body.codec,
                                    body.dictionary_id,
                                    &body.bytes,
                                    input.len() - 1
                                )
                                .is_err(),
                        "{:?} decoded past a limit one byte short",
                        body.codec
                    );
                }
            }
        }
    });
});
