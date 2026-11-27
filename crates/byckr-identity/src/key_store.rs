use p256::SecretKey;

use crate::identity_error::IdentityError;

/// Where a key reference keeps its private key.
pub(crate) trait KeyStore {
    /// Store a new key. Fails with [`IdentityError::AlreadyExists`] rather
    /// than replace one.
    fn save(&self, secret_key: &SecretKey) -> Result<(), IdentityError>;

    /// Read the stored key.
    fn load(&self) -> Result<SecretKey, IdentityError>;
}
