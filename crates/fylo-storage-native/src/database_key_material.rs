use zeroize::Zeroizing;

use crate::{KEY_CHECK_BYTES, SALT_BYTES};

/// What a [`crate::DatabaseKey`] holds: a secret every file key is derived
/// from, or one file's key that the host already derived for that file's
/// salt (ADR 0025).
pub(crate) enum DatabaseKeyMaterial {
    Secret(Zeroizing<Vec<u8>>),
    Derived {
        salt: [u8; SALT_BYTES],
        file_key: Zeroizing<[u8; 32]>,
        key_check: [u8; KEY_CHECK_BYTES],
    },
}
