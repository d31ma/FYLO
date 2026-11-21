use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{AeadInOut, KeyInit};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::{ContainerObjectChunk, ContainerObjectError, ContainerObjectTarget};

/// Opens the sealed chunks of one encrypted large object (ADR 0034
/// section 4). It can only open: sealing needs a
/// [`crate::ContainerObjectSealer`], which always draws a fresh salt.
pub struct ContainerObjectOpener {
    cipher: Aes256Gcm,
}

impl ContainerObjectOpener {
    /// Bytes of the tag after each sealed chunk.
    pub const TAG_BYTES: usize = 16;
    const KEY_INFO: &'static [u8] = b"fylo.vdbx.object.v1";
    const NONCE_BYTES: usize = 12;
    const NONCE_INDEX_AT: usize = 4;

    /// The opener of the object whose key comes from `file_key` and `salt`.
    ///
    /// # Errors
    ///
    /// Returns `Damaged` when no key can be derived.
    pub fn new(file_key: &[u8; 32], salt: &[u8; 32]) -> Result<Self, ContainerObjectError> {
        let key = Self::derive_key(file_key, salt)?;
        Ok(Self {
            cipher: Aes256Gcm::new((&*key).into()),
        })
    }

    /// Decrypt one chunk in place and check its tag.
    ///
    /// # Errors
    ///
    /// Returns `Damaged` when the tag does not match: the chunk was changed,
    /// moved to another place or object, or the object was cut short.
    pub fn open_in_place(
        &self,
        target: &ContainerObjectTarget,
        chunk: ContainerObjectChunk,
        buffer: &mut [u8],
        tag: &[u8; 16],
    ) -> Result<(), ContainerObjectError> {
        let nonce = Self::nonce(chunk.index);
        let aad = Self::associated_data(target, chunk);
        self.cipher
            .decrypt_inout_detached((&nonce).into(), &aad, buffer.into(), tag.into())
            .map_err(|_| ContainerObjectError::damaged("an encrypted chunk failed its tag check"))
    }

    /// The object key: HKDF-SHA256 of the file key with the object's salt.
    pub(crate) fn derive_key(
        file_key: &[u8; 32],
        salt: &[u8; 32],
    ) -> Result<Zeroizing<[u8; 32]>, ContainerObjectError> {
        let mut key = Zeroizing::new([0_u8; 32]);
        Hkdf::<Sha256>::new(Some(salt), file_key)
            .expand(Self::KEY_INFO, key.as_mut())
            .map_err(|_| ContainerObjectError::damaged("no object key could be derived"))?;
        Ok(key)
    }

    /// The nonce of chunk `index`: the chunk number, 96 bits, big-endian.
    pub(crate) fn nonce(index: u64) -> [u8; 12] {
        let mut nonce = [0_u8; Self::NONCE_BYTES];
        nonce[Self::NONCE_INDEX_AT..].copy_from_slice(&index.to_be_bytes());
        nonce
    }

    /// What a chunk's tag binds it to besides its bytes.
    pub(crate) fn associated_data(
        target: &ContainerObjectTarget,
        chunk: ContainerObjectChunk,
    ) -> Vec<u8> {
        let mut aad = Vec::with_capacity(21 + target.id.len());
        aad.extend_from_slice(&target.collection.to_le_bytes());
        aad.extend_from_slice(&chunk.index.to_le_bytes());
        aad.extend_from_slice(&chunk.count.to_le_bytes());
        aad.push(u8::from(chunk.is_last()));
        aad.extend_from_slice(target.id.as_bytes());
        aad
    }
}
