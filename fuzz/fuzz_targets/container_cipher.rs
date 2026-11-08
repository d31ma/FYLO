#![no_main]

use fylo_format::ContainerExtentKind;
use fylo_storage_native::{
    ContainerCipher, ContainerCipherError, ContainerSealContext, SALT_BYTES,
};
use libfuzzer_sys::fuzz_target;

const CONTEXT_BYTES: usize = 17;

fn read<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    let mut value = [0_u8; N];
    if let Some(tail) = bytes.get(at..) {
        let length = tail.len().min(N);
        value[..length].copy_from_slice(&tail[..length]);
    }
    value
}

fn context(bytes: &[u8]) -> ContainerSealContext {
    let a = u32::from_le_bytes(read(bytes, 1));
    let b = u64::from_le_bytes(read(bytes, 5));
    let c = u32::from_le_bytes(read(bytes, 13));
    match bytes.first().copied().unwrap_or(0) % 4 {
        0 => ContainerSealContext::Record {
            collection: a,
            lsn: b,
            entry: c,
        },
        1 => ContainerSealContext::Directory { block: a },
        2 => ContainerSealContext::Dictionary {
            collection: a,
            id: c,
        },
        _ => ContainerSealContext::Snapshot {
            collection: a,
            checkpoint: b,
            kind: match c % 3 {
                0 => ContainerExtentKind::Locations,
                1 => ContainerExtentKind::Index,
                _ => ContainerExtentKind::Schemas,
            },
        },
    }
}

// A different context for every input context: the same unit sealed for one
// place must never open in another (ADR 0016 section 9).
fn other(context: ContainerSealContext) -> ContainerSealContext {
    match context {
        ContainerSealContext::Record {
            collection,
            lsn,
            entry,
        } => ContainerSealContext::Record {
            collection,
            lsn,
            entry: entry ^ 1,
        },
        ContainerSealContext::Directory { block } => ContainerSealContext::Record {
            collection: block,
            lsn: 0,
            entry: 0,
        },
        ContainerSealContext::Dictionary { collection, id } => ContainerSealContext::Dictionary {
            collection: collection.wrapping_add(1),
            id,
        },
        ContainerSealContext::Snapshot {
            collection,
            checkpoint,
            kind,
        } => ContainerSealContext::Snapshot {
            collection,
            checkpoint: checkpoint ^ (1 << 63),
            kind,
        },
    }
}

// `.vdbx` encrypted units are untrusted on disk: opening arbitrary bytes must
// fail without panicking, and a sealed unit opens only under its own key and
// context.
fuzz_target!(|input: &[u8]| {
    let secret_bytes = usize::from(input.first().copied().unwrap_or(0) % 64);
    let rest = input.get(1..).unwrap_or_default();
    let (secret, rest) = rest.split_at(secret_bytes.min(rest.len()));
    let salt: [u8; SALT_BYTES] = read(rest, 0);
    let rest = rest.get(SALT_BYTES..).unwrap_or_default();
    let context = context(rest);
    let body = rest.get(CONTEXT_BYTES..).unwrap_or_default();

    let cipher = ContainerCipher::derive(secret, &salt);
    assert_eq!(
        cipher.open(context, body).err(),
        Some(ContainerCipherError::Unauthenticated),
        "arbitrary bytes opened"
    );

    let key_check = cipher.key_check();
    assert!(ContainerCipher::open_file(secret, &salt, &key_check).is_ok());
    let mut wrong_check = key_check;
    wrong_check[usize::from(salt[0]) % wrong_check.len()] ^= 1;
    assert_eq!(
        ContainerCipher::open_file(secret, &salt, &wrong_check).err(),
        Some(ContainerCipherError::WrongKey)
    );

    let sealed = cipher.seal(context, body).expect("seal");
    assert_eq!(cipher.open(context, &sealed).as_deref(), Ok(body));
    assert!(
        cipher.open(other(context), &sealed).is_err(),
        "opened elsewhere"
    );
    let mut wrong_secret = secret.to_vec();
    wrong_secret.push(0);
    assert!(
        ContainerCipher::derive(&wrong_secret, &salt)
            .open(context, &sealed)
            .is_err(),
        "opened under another key"
    );
    let mut tampered = sealed.clone();
    let at = usize::from(salt[1]) % tampered.len();
    tampered[at] ^= 0x80;
    assert!(cipher.open(context, &tampered).is_err(), "opened tampered");
    assert!(
        cipher.open(context, &sealed[..sealed.len() - 1]).is_err(),
        "opened truncated"
    );
});
