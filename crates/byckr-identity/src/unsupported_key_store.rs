use p256::SecretKey;

use crate::hardware_key::HardwareKey;
use crate::hardware_key_store::HardwareKeyStore;
use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;

/// A key store this platform or build does not have: every call fails with
/// [`IdentityError::Unsupported`] and the reason.
pub(crate) struct UnsupportedKeyStore {
    reason: String,
}

impl UnsupportedKeyStore {
    pub(crate) fn new(reason: String) -> UnsupportedKeyStore {
        UnsupportedKeyStore { reason }
    }
}

impl KeyStore for UnsupportedKeyStore {
    fn save(&self, _secret_key: &SecretKey) -> Result<(), IdentityError> {
        Err(IdentityError::Unsupported(self.reason.clone()))
    }

    fn load(&self) -> Result<SecretKey, IdentityError> {
        Err(IdentityError::Unsupported(self.reason.clone()))
    }
}

impl HardwareKeyStore for UnsupportedKeyStore {
    fn create(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        Err(IdentityError::Unsupported(self.reason.clone()))
    }

    fn open(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        Err(IdentityError::Unsupported(self.reason.clone()))
    }
}
