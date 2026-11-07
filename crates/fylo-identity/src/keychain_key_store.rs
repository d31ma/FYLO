use std::env;
use std::ffi::OsString;

use p256::SecretKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use security_framework::base::Error as SecurityError;
use security_framework::os::macos::keychain::SecKeychain;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;

/// `keychain:<label>`: a generic-password item in a macOS keychain whose
/// service is the label and whose data is the PKCS#8 DER key. macOS lets the
/// application that created the item read it without asking; any other
/// application needs the user's consent.
///
/// The item lives in the default keychain, or in the keychain file
/// `FYLO_KEYCHAIN` names (CI and tests use a throwaway one).
pub(crate) struct KeychainKeyStore {
    label: String,
}

impl KeychainKeyStore {
    const ACCOUNT: &'static str = "fylo-identity";
    const KEYCHAIN_VARIABLE: &'static str = "FYLO_KEYCHAIN";
    const ITEM_NOT_FOUND: i32 = -25300;
    const DUPLICATE_ITEM: i32 = -25299;

    pub(crate) fn new(label: String) -> KeychainKeyStore {
        KeychainKeyStore { label }
    }

    fn reference(&self) -> String {
        format!("keychain:{}", self.label)
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
                IdentityError::NotFound(format!("{} has no key", self.reference()))
            }
            Self::DUPLICATE_ITEM => IdentityError::AlreadyExists(format!(
                "{} already has a key; FYLO never replaces a key",
                self.reference()
            )),
            _ => IdentityError::Store(format!("{}: {error}", self.reference())),
        }
    }
}

impl KeyStore for KeychainKeyStore {
    fn save(&self, secret_key: &SecretKey) -> Result<(), IdentityError> {
        let document = secret_key.to_pkcs8_der().map_err(|error| {
            IdentityError::InvalidKey(format!("cannot encode the key: {error}"))
        })?;
        self.keychain()?
            .add_generic_password(&self.label, Self::ACCOUNT, document.as_bytes())
            .map_err(|error| self.keychain_error(error))
    }

    fn load(&self) -> Result<SecretKey, IdentityError> {
        let (password, _item) = self
            .keychain()?
            .find_generic_password(&self.label, Self::ACCOUNT)
            .map_err(|error| self.keychain_error(error))?;
        let der = Zeroizing::new(password.as_ref().to_vec());
        SecretKey::from_pkcs8_der(&der).map_err(|error| {
            IdentityError::InvalidKey(format!(
                "{} is not a P-256 private key in PKCS#8 form: {error}",
                self.reference()
            ))
        })
    }
}
