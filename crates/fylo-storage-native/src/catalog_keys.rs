use std::sync::Arc;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::KeyInit;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use zeroize::Zeroizing;

use crate::access_binding::AccessBinding;
use crate::{AccessDocument, ContainerCipher, ContainerSealContext, DatabaseAccessError};

/// The catalog keys of a protected database as one identity holds them
/// (ADR 0021 section 7): the current one, which seals the directory and
/// the policy; the previous one while a rotation is still resealing the
/// directory; and the original one, whose check value the superblock
/// holds. The access document keeps the previous and the original sealed
/// under the current key, so only an identity granted the current key
/// learns them.
#[derive(Clone)]
pub(crate) struct CatalogKeys {
    current: Zeroizing<[u8; 32]>,
    previous: Option<Zeroizing<[u8; 32]>>,
    original: Zeroizing<[u8; 32]>,
    ciphers: Vec<Arc<Aes256Gcm>>,
}

impl CatalogKeys {
    const PREVIOUS: u64 = 1;
    const ORIGINAL: u64 = 2;

    /// The keys of a database whose catalog key never rotated.
    #[cfg(test)]
    pub(crate) fn new(current: Zeroizing<[u8; 32]>) -> CatalogKeys {
        Self::of(current.clone(), None, current)
    }

    /// The keys `document` holds, given the current key unwrapped from it.
    pub(crate) fn from_document(
        current: Zeroizing<[u8; 32]>,
        document: &AccessDocument,
    ) -> Result<CatalogKeys, DatabaseAccessError> {
        let cipher = ContainerCipher::from_key(&current);
        let previous = document
            .previous
            .as_deref()
            .map(|sealed| Self::open_key(&cipher, Self::PREVIOUS, sealed))
            .transpose()?;
        let original = match document.original.as_deref() {
            Some(sealed) => Self::open_key(&cipher, Self::ORIGINAL, sealed)?,
            None => current.clone(),
        };
        Ok(Self::of(current, previous, original))
    }

    fn of(
        current: Zeroizing<[u8; 32]>,
        previous: Option<Zeroizing<[u8; 32]>>,
        original: Zeroizing<[u8; 32]>,
    ) -> CatalogKeys {
        let mut ciphers = vec![Arc::new(Aes256Gcm::new((&*current).into()))];
        if let Some(previous) = &previous {
            ciphers.push(Arc::new(Aes256Gcm::new((&**previous).into())));
        }
        CatalogKeys {
            current,
            previous,
            original,
            ciphers,
        }
    }

    /// The key that seals the directory and the policy now.
    pub(crate) fn current(&self) -> &Zeroizing<[u8; 32]> {
        &self.current
    }

    /// The key whose check value the superblock holds.
    pub(crate) fn original(&self) -> &Zeroizing<[u8; 32]> {
        &self.original
    }

    /// Whether a rotation is still resealing the directory.
    pub(crate) fn rotation_pending(&self) -> bool {
        self.previous.is_some()
    }

    /// The cipher that seals a directory block.
    pub(crate) fn sealing(&self) -> Arc<Aes256Gcm> {
        Arc::clone(&self.ciphers[0])
    }

    /// The ciphers that may open a directory block, current first.
    pub(crate) fn opening(&self) -> Vec<Arc<Aes256Gcm>> {
        self.ciphers.clone()
    }

    /// These keys after a rotation to `next`: the current key becomes the
    /// previous one until the directory is resealed.
    pub(crate) fn rotated(&self, next: Zeroizing<[u8; 32]>) -> CatalogKeys {
        Self::of(next, Some(self.current.clone()), self.original.clone())
    }

    /// These keys once the directory is resealed.
    pub(crate) fn settled(&self) -> CatalogKeys {
        Self::of(self.current.clone(), None, self.original.clone())
    }

    /// A random new catalog key.
    pub(crate) fn fresh() -> Result<Zeroizing<[u8; 32]>, DatabaseAccessError> {
        AccessBinding::random_key()
    }

    /// The access document's `previous` and `original` fields for these
    /// keys: each sealed under the current key, or absent when it is not
    /// needed.
    pub(crate) fn document_fields(
        &self,
    ) -> Result<(Option<String>, Option<String>), DatabaseAccessError> {
        let cipher = ContainerCipher::from_key(&self.current);
        let previous = self
            .previous
            .as_ref()
            .map(|previous| Self::seal_key(&cipher, Self::PREVIOUS, previous))
            .transpose()?;
        let original = Some(&self.original)
            .filter(|original| **original != self.current)
            .map(|original| Self::seal_key(&cipher, Self::ORIGINAL, original))
            .transpose()?;
        Ok((previous, original))
    }

    fn seal_key(
        cipher: &ContainerCipher,
        role: u64,
        key: &[u8; 32],
    ) -> Result<String, DatabaseAccessError> {
        cipher
            .seal(ContainerSealContext::CatalogKey { role }, key)
            .map(|sealed| STANDARD.encode(sealed))
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))
    }

    fn open_key(
        cipher: &ContainerCipher,
        role: u64,
        sealed: &str,
    ) -> Result<Zeroizing<[u8; 32]>, DatabaseAccessError> {
        let bytes = STANDARD
            .decode(sealed)
            .map_err(|error| DatabaseAccessError::Damaged(error.to_string()))?;
        let key = cipher
            .open(ContainerSealContext::CatalogKey { role }, &bytes)
            .map_err(|_| {
                DatabaseAccessError::Policy(
                    "an earlier catalog key in the access document does not open with the current one"
                        .into(),
                )
            })?;
        AccessBinding::key_bytes(&key)
    }
}
