use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{AeadInOut, KeyInit};

use crate::{
    ContainerObjectChunk, ContainerObjectError, ContainerObjectOpener, ContainerObjectTarget,
};

/// Seals the chunks of one new large object write (ADR 0034 section 4).
///
/// A key is never used twice with the same nonce: every sealer draws a new
/// random salt, so every object write has its own key, and a sealer seals
/// each chunk number at most once, in increasing order.
pub struct ContainerObjectSealer {
    cipher: Aes256Gcm,
    salt: [u8; 32],
    next_index: u64,
}

impl ContainerObjectSealer {
    /// A sealer for a new object write under `file_key`, with a fresh salt.
    ///
    /// # Errors
    ///
    /// Returns `Container(Cipher(Random))` when no salt can be drawn.
    pub fn new(file_key: &[u8; 32]) -> Result<Self, ContainerObjectError> {
        let salt = crate::ContainerCipher::new_salt().map_err(crate::ContainerError::Cipher)?;
        let key = ContainerObjectOpener::derive_key(file_key, &salt)?;
        Ok(Self {
            cipher: Aes256Gcm::new((&*key).into()),
            salt,
            next_index: 0,
        })
    }

    /// The salt the object's record keeps.
    #[must_use]
    pub fn salt(&self) -> [u8; 32] {
        self.salt
    }

    /// Encrypt one chunk in place and return its tag.
    ///
    /// # Errors
    ///
    /// Returns `NonceReuse` when this chunk number, or a later one, was
    /// sealed already, and `Damaged` when the cipher refuses the chunk.
    pub fn seal_in_place(
        &mut self,
        target: &ContainerObjectTarget,
        chunk: ContainerObjectChunk,
        buffer: &mut [u8],
    ) -> Result<[u8; 16], ContainerObjectError> {
        self.claim_nonce(chunk.index)?;
        let nonce = ContainerObjectOpener::nonce(chunk.index);
        let aad = ContainerObjectOpener::associated_data(target, chunk);
        let tag = self
            .cipher
            .encrypt_inout_detached((&nonce).into(), &aad, buffer.into())
            .map_err(|_| ContainerObjectError::damaged("a chunk could not be encrypted"))?;
        Ok(tag.into())
    }

    fn claim_nonce(&mut self, index: u64) -> Result<(), ContainerObjectError> {
        if index < self.next_index {
            return Err(ContainerObjectError::NonceReuse { index });
        }
        self.next_index = index + 1;
        Ok(())
    }
}
