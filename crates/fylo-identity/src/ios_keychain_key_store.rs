use p256::SecretKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};

use crate::identity_error::IdentityError;
use crate::ios_keychain_item::IosKeychainItem;
use crate::key_store::KeyStore;

/// `ios-keychain:<label>` (ADR 0026 section 8): a software key, like
/// `keychain:` on macOS, kept as a PKCS#8 DER generic-password item in the
/// iOS Data Protection keychain ([`IosKeychainItem`]) whose service is the
/// label.
pub(crate) struct IosKeychainKeyStore {
    item: IosKeychainItem,
}

impl IosKeychainKeyStore {
    pub(crate) fn new(label: String) -> IosKeychainKeyStore {
        let reference = format!("ios-keychain:{label}");
        IosKeychainKeyStore {
            item: IosKeychainItem::new(label, reference),
        }
    }
}

impl KeyStore for IosKeychainKeyStore {
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
