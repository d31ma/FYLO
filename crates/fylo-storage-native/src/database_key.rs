//! The secret an encrypted `.vdbx` database's file key is derived from
//! (ADR 0016 section 9).

use std::fmt;
use std::sync::Arc;

use zeroize::Zeroizing;

/// The secret named by `fylo.encryption.key`, for opening an encrypted
/// `.vdbx` database or creating one ([`crate::NativeRoot::create_encrypted_database`]).
///
/// The file key is derived from it and the salt in the file's superblock,
/// so the same secret opens every database created with it. It is wiped
/// from memory when the last clone drops, and never printed.
#[derive(Clone)]
pub struct DatabaseKey(Arc<Zeroizing<Vec<u8>>>);

impl DatabaseKey {
    /// A key from the configured secret's bytes.
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self(Arc::new(Zeroizing::new(secret.into())))
    }

    /// The secret's bytes.
    pub(crate) fn secret(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for DatabaseKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DatabaseKey(..)")
    }
}
