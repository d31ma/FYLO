use p256::SecretKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};

use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;
use crate::keychain_item::KeychainItem;

/// `keychain:<label>`: a generic-password item in a macOS keychain
/// ([`KeychainItem`]) whose service is the label and whose data is the
/// PKCS#8 DER key.
pub(crate) struct KeychainKeyStore {
    item: KeychainItem,
}

impl KeychainKeyStore {
    const ACCOUNT: &'static str = "fylo-identity";

    pub(crate) fn new(label: String) -> KeychainKeyStore {
        let reference = format!("keychain:{label}");
        KeychainKeyStore {
            item: KeychainItem::new(label, Self::ACCOUNT, reference),
        }
    }
}

impl KeyStore for KeychainKeyStore {
    fn save(&self, secret_key: &SecretKey) -> Result<(), IdentityError> {
        let document = secret_key.to_pkcs8_der().map_err(|error| {
            IdentityError::InvalidKey(format!("cannot encode the key: {error}"))
        })?;
        self.item.add(document.as_bytes())
    }

    fn load(&self) -> Result<SecretKey, IdentityError> {
        let der = self.item.find()?;
        SecretKey::from_pkcs8_der(&der).map_err(|error| {
            IdentityError::InvalidKey(format!(
                "{} is not a P-256 private key in PKCS#8 form: {error}",
                self.item.reference()
            ))
        })
    }
}
