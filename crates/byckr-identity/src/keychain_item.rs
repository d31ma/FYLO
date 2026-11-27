use std::env;
use std::ffi::OsString;

use security_framework::base::Error as SecurityError;
use security_framework::os::macos::keychain::SecKeychain;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// A generic-password item in a macOS keychain, found by its service (the
/// key reference's label) and account. macOS lets the application that
/// created the item read it without asking; any other application needs the
/// user's consent.
///
/// The item lives in the default keychain, or in the keychain file
/// `FYLO_KEYCHAIN` names (CI and tests use a throwaway one).
pub(crate) struct KeychainItem {
    label: String,
    account: &'static str,
    reference: String,
}

impl KeychainItem {
    const KEYCHAIN_VARIABLE: &'static str = "FYLO_KEYCHAIN";
    const ITEM_NOT_FOUND: i32 = -25300;
    const DUPLICATE_ITEM: i32 = -25299;

    pub(crate) fn new(label: String, account: &'static str, reference: String) -> KeychainItem {
        KeychainItem {
            label,
            account,
            reference,
        }
    }

    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    /// Store `data`; an item already there is never replaced.
    pub(crate) fn add(&self, data: &[u8]) -> Result<(), IdentityError> {
        self.keychain()?
            .add_generic_password(&self.label, self.account, data)
            .map_err(|error| self.keychain_error(error))
    }

    /// The stored data.
    pub(crate) fn find(&self) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let (password, _item) = self
            .keychain()?
            .find_generic_password(&self.label, self.account)
            .map_err(|error| self.keychain_error(error))?;
        Ok(Zeroizing::new(password.as_ref().to_vec()))
    }

    /// Fail with [`IdentityError::AlreadyExists`] when an item is there.
    pub(crate) fn refuse_existing(&self) -> Result<(), IdentityError> {
        match self.find() {
            Ok(_) => Err(self.already_exists()),
            Err(IdentityError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn keychain(&self) -> Result<SecKeychain, IdentityError> {
        Self::open_keychain(env::var_os(Self::KEYCHAIN_VARIABLE))
            .map_err(|error| self.keychain_error(error))
    }

    fn open_keychain(path: Option<OsString>) -> Result<SecKeychain, SecurityError> {
        match path {
            Some(path) => SecKeychain::open(path),
            None => SecKeychain::default(),
        }
    }

    fn keychain_error(&self, error: SecurityError) -> IdentityError {
        match error.code() {
            Self::ITEM_NOT_FOUND => {
                IdentityError::NotFound(format!("{} has no key", self.reference))
            }
            Self::DUPLICATE_ITEM => self.already_exists(),
            _ => IdentityError::Store(format!("{}: {error}", self.reference)),
        }
    }

    fn already_exists(&self) -> IdentityError {
        IdentityError::AlreadyExists(format!(
            "{} already has a key; FYLO never replaces a key",
            self.reference
        ))
    }
}
