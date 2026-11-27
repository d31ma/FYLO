use crate::android_key::AndroidKey;
use crate::android_keystore_host::AndroidKeystoreHost;
use crate::hardware_key::HardwareKey;
use crate::hardware_key_store::HardwareKeyStore;
use crate::identity_error::IdentityError;

/// `android-keystore:<alias>` (ADR 0026, section 8): a hardware key in the
/// Android Keystore, like `enclave:` and `tpm:`. FYLO signs records with
/// session keys (ADR 0021, section 8), so only opening a database and one
/// session certificate a day use the Keystore.
pub(crate) struct AndroidKeyStore {
    alias: String,
}

impl AndroidKeyStore {
    pub(crate) fn new(alias: String) -> AndroidKeyStore {
        AndroidKeyStore { alias }
    }
}

impl HardwareKeyStore for AndroidKeyStore {
    fn create(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        let keystore = AndroidKeystoreHost::installed()?;
        let point = keystore.create(&self.alias)?;
        Ok(Box::new(AndroidKey::new(
            self.alias.clone(),
            keystore,
            &point,
        )?))
    }

    fn open(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        let keystore = AndroidKeystoreHost::installed()?;
        let point = keystore.public_key(&self.alias)?;
        Ok(Box::new(AndroidKey::new(
            self.alias.clone(),
            keystore,
            &point,
        )?))
    }
}

#[cfg(test)]
#[path = "android_key_store_tests.rs"]
mod tests;
