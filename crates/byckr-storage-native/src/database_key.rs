//! The secret an encrypted `.vdbx` database's file key is derived from
//! (ADR 0016 section 9), or a file key the host derived (ADR 0025).

use std::fmt;
use std::sync::Arc;

use zeroize::Zeroizing;

use crate::database_key_material::DatabaseKeyMaterial;
use crate::{ContainerCipher, ContainerCipherError, KEY_CHECK_BYTES, SALT_BYTES};

/// The secret named by `fylo.encryption.key`, for opening an encrypted
/// `.vdbx` database or creating one ([`crate::NativeRoot::create_encrypted_database`]).
///
/// The file key is derived from it and the salt in the file's superblock,
/// so the same secret opens every database created with it. A browser host
/// that keeps the secret in `WebCrypto` derives one file's key itself and
/// hands over only that key ([`Self::derived`]), which opens only that file.
/// Either is wiped from memory when the last clone drops, and never printed.
#[derive(Clone)]
pub struct DatabaseKey(Arc<DatabaseKeyMaterial>);

impl DatabaseKey {
    /// A key from the configured secret's bytes.
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self(Arc::new(DatabaseKeyMaterial::Secret(Zeroizing::new(
            secret.into(),
        ))))
    }

    /// A file key the host derived from its secret and the file's `salt`,
    /// with the key-check value it derived beside it (ADR 0025). It opens a
    /// file only when the file's salt and key-check value are these.
    #[must_use]
    pub fn derived(
        salt: [u8; SALT_BYTES],
        file_key: [u8; 32],
        key_check: [u8; KEY_CHECK_BYTES],
    ) -> Self {
        Self(Arc::new(DatabaseKeyMaterial::Derived {
            salt,
            file_key: Zeroizing::new(file_key),
            key_check,
        }))
    }

    /// The cipher for a file whose superblock holds `salt` and `key_check`.
    pub(crate) fn open_cipher(
        &self,
        salt: &[u8; SALT_BYTES],
        key_check: &[u8; KEY_CHECK_BYTES],
    ) -> Result<ContainerCipher, ContainerCipherError> {
        let cipher = self.cipher(salt)?;
        cipher.check(key_check)?;
        Ok(cipher)
    }

    /// The cipher for a file whose superblock holds `salt`, unchecked.
    pub(crate) fn cipher(
        &self,
        salt: &[u8; SALT_BYTES],
    ) -> Result<ContainerCipher, ContainerCipherError> {
        match &*self.0 {
            DatabaseKeyMaterial::Secret(secret) => Ok(ContainerCipher::derive(secret, salt)),
            DatabaseKeyMaterial::Derived {
                salt: derived_for,
                file_key,
                key_check,
            } => Self::derived_cipher(derived_for == salt, file_key, *key_check),
        }
    }

    /// The salt and key-check value of a new file encrypted with this key.
    pub(crate) fn new_file_header(
        &self,
    ) -> Result<([u8; SALT_BYTES], [u8; KEY_CHECK_BYTES]), ContainerCipherError> {
        match &*self.0 {
            DatabaseKeyMaterial::Secret(secret) => {
                let salt = ContainerCipher::new_salt()?;
                Ok((salt, ContainerCipher::derive(secret, &salt).key_check()))
            }
            DatabaseKeyMaterial::Derived {
                salt, key_check, ..
            } => Ok((*salt, *key_check)),
        }
    }

    /// The cipher and salt of a new file encrypted with this key.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn new_file_cipher(
        &self,
    ) -> Result<(ContainerCipher, [u8; SALT_BYTES]), ContainerCipherError> {
        let (salt, _) = self.new_file_header()?;
        Ok((self.cipher(&salt)?, salt))
    }

    fn derived_cipher(
        same_salt: bool,
        file_key: &[u8; 32],
        key_check: [u8; KEY_CHECK_BYTES],
    ) -> Result<ContainerCipher, ContainerCipherError> {
        if !same_salt {
            return Err(ContainerCipherError::WrongKey);
        }
        Ok(ContainerCipher::from_derived(file_key, key_check))
    }
}

impl fmt::Debug for DatabaseKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DatabaseKey(..)")
    }
}
