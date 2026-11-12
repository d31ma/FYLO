use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::base::Error as SecurityError;
use security_framework::passwords::{self, PasswordOptions};
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// One generic-password item in the iOS Data Protection keychain (ADR 0026
/// section 8): readable after the device's first unlock, on this device
/// only, never synchronized to iCloud.
pub(crate) struct IosKeychainItem {
    service: String,
    reference: String,
}

impl IosKeychainItem {
    const ACCOUNT: &'static str = "fylo-identity";
    const ITEM_NOT_FOUND: i32 = -25300;

    pub(crate) fn new(service: String, reference: String) -> IosKeychainItem {
        IosKeychainItem { service, reference }
    }

    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    /// Store `data` as a new item. The keychain call replaces an existing
    /// item, so an existing one is refused first.
    pub(crate) fn add(&self, data: &[u8]) -> Result<(), IdentityError> {
        self.refuse_existing()?;
        let options = self.options(true)?;
        passwords::set_generic_password_options(data, options)
            .map_err(|error| self.keychain_error(error))
    }

    pub(crate) fn find(&self) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let options = self.options(false)?;
        passwords::generic_password(options)
            .map(Zeroizing::new)
            .map_err(|error| self.keychain_error(error))
    }

    fn refuse_existing(&self) -> Result<(), IdentityError> {
        match self.find() {
            Ok(_) => Err(IdentityError::AlreadyExists(format!(
                "{} already holds a key",
                self.reference
            ))),
            Err(IdentityError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The item's query; for a new item also its protection.
    fn options(&self, new_item: bool) -> Result<PasswordOptions, IdentityError> {
        let mut options = PasswordOptions::new_generic_password(&self.service, Self::ACCOUNT);
        options.use_protected_keychain();
        options.set_access_synchronized(Some(false));
        if new_item {
            options.set_access_control(self.access_control()?);
        }
        Ok(options)
    }

    fn access_control(&self) -> Result<SecAccessControl, IdentityError> {
        SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
            0,
        )
        .map_err(|error| self.keychain_error(error))
    }

    fn keychain_error(&self, error: SecurityError) -> IdentityError {
        if error.code() == Self::ITEM_NOT_FOUND {
            return IdentityError::NotFound(format!("{} holds no key", self.reference));
        }
        IdentityError::Store(format!("{}: the keychain refused: {error}", self.reference))
    }
}
