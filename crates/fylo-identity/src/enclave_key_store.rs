use crate::enclave_key::EnclaveKey;
use crate::hardware_key::HardwareKey;
use crate::hardware_key_store::HardwareKeyStore;
use crate::identity_error::IdentityError;
use crate::keychain_item::KeychainItem;

/// `enclave:<label>` on macOS (ADR 0021, phase 8): a P-256 key made in the
/// Secure Enclave, which never leaves it. The enclave's token blob for the
/// key is kept as a generic-password item in the keychain
/// ([`KeychainItem`]), so the keychain's access list binds it to the
/// application that created it, and `FYLO_KEYCHAIN` names a keychain file
/// as for `keychain:`. The blob is useless on any other Mac.
pub(crate) struct EnclaveKeyStore {
    item: KeychainItem,
}

impl EnclaveKeyStore {
    const ACCOUNT: &'static str = "fylo-enclave-identity";

    pub(crate) fn new(label: String) -> EnclaveKeyStore {
        let reference = format!("enclave:{label}");
        EnclaveKeyStore {
            item: KeychainItem::new(label, Self::ACCOUNT, reference),
        }
    }
}

impl HardwareKeyStore for EnclaveKeyStore {
    fn create(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        self.item.refuse_existing()?;
        let key = EnclaveKey::generate(self.item.reference())?;
        self.item.add(&key.token_blob()?)?;
        Ok(Box::new(key))
    }

    fn open(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        let blob = self.item.find()?;
        Ok(Box::new(EnclaveKey::restore(&blob, self.item.reference())?))
    }
}
