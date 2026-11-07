use zeroize::Zeroizing;

use crate::DatabaseAccessError;

/// Binds wraps and signatures to one database, by the random id its
/// creation stored in the superblock, so none can be moved into another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessBinding {
    database: [u8; 32],
}

impl AccessBinding {
    const WRAP_LABEL: &'static [u8] = b"fylo.vdbx.key/1\0";
    const POLICY_LABEL: &'static [u8] = b"fylo.policy/1\0";
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

    pub(crate) fn policy_message(self, policy: &[u8]) -> Vec<u8> {
        [Self::POLICY_LABEL, &self.database, policy].concat()
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
