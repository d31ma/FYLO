use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::{DatabaseAccessError, FieldKey};

/// Binds wraps and signatures to one database, by the random id its
/// creation stored in the superblock, so none can be moved into another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessBinding {
    database: [u8; 32],
}

impl AccessBinding {
    const WRAP_LABEL: &'static [u8] = b"fylo.vdbx.key/1\0";
    const POLICY_LABEL: &'static [u8] = b"fylo.policy/1\0";
    const FRAME_LABEL: &'static [u8] = b"fylo.frame/1\0";
    const FIELD_LABEL: &'static [u8] = b"fylo.field/1\0";
    const CATALOG_SCOPE: u8 = 1;
    const COLLECTION_SCOPE: u8 = 2;

    pub(crate) fn new(database: [u8; 32]) -> AccessBinding {
        AccessBinding { database }
    }

    pub(crate) fn catalog_info(self) -> Vec<u8> {
        self.wrap_info(Self::CATALOG_SCOPE, 0, 0)
    }

    pub(crate) fn collection_info(self, collection: u32, epoch: u32) -> Vec<u8> {
        self.wrap_info(Self::COLLECTION_SCOPE, collection, epoch)
    }

    /// What a record frame's signature covers: this database, the
    /// collection, the record's LSN and the SHA-256 of the frame's stored
    /// bytes before the signature.
    pub(crate) fn frame_message(self, collection: u32, lsn: u64, digest: &[u8; 32]) -> Vec<u8> {
        [
            Self::FRAME_LABEL,
            &self.database,
            &collection.to_le_bytes(),
            &lsn.to_le_bytes(),
            digest,
        ]
        .concat()
    }

    pub(crate) fn policy_message(self, policy: &[u8]) -> Vec<u8> {
        [Self::POLICY_LABEL, &self.database, policy].concat()
    }

    /// The `$encrypted` field key of `collection` at `epoch`: 64 bytes of
    /// HKDF-SHA256 from that epoch's collection key, salted with this
    /// database's id, so it changes whenever the collection key does.
    pub(crate) fn field_key(
        self,
        collection: u32,
        epoch: u32,
        key: &[u8; 32],
    ) -> Result<FieldKey, DatabaseAccessError> {
        let info = [
            Self::FIELD_LABEL,
            &collection.to_le_bytes(),
            &epoch.to_le_bytes(),
        ]
        .concat();
        let mut material = Zeroizing::new([0_u8; 64]);
        Hkdf::<Sha256>::new(Some(&self.database), key)
            .expand(&info, material.as_mut())
            .map_err(|_| DatabaseAccessError::Damaged("a field key could not be derived".into()))?;
        Ok(FieldKey::new(epoch, &material))
    }

    pub(crate) fn random_key() -> Result<Zeroizing<[u8; 32]>, DatabaseAccessError> {
        let mut key = Zeroizing::new([0_u8; 32]);
        getrandom::fill(key.as_mut()).map_err(|_| {
            DatabaseAccessError::Damaged("the system random generator failed".into())
        })?;
        Ok(key)
    }

    pub(crate) fn key_bytes(bytes: &[u8]) -> Result<Zeroizing<[u8; 32]>, DatabaseAccessError> {
        <[u8; 32]>::try_from(bytes)
            .map(Zeroizing::new)
            .map_err(|_| DatabaseAccessError::Damaged("a wrapped key is not 32 bytes".into()))
    }

    fn wrap_info(self, scope: u8, collection: u32, epoch: u32) -> Vec<u8> {
        [
            Self::WRAP_LABEL,
            &self.database,
            &[scope],
            &collection.to_le_bytes(),
            &epoch.to_le_bytes(),
        ]
        .concat()
    }
}
