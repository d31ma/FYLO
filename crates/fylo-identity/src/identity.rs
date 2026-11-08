use std::fmt;

use p256::elliptic_curve::Generate;
use p256::{PublicKey, SecretKey};
use zeroize::Zeroizing;

use crate::hardware_private_key::HardwarePrivateKey;
use crate::identity_error::IdentityError;
use crate::identity_id::IdentityId;
use crate::key_reference::KeyReference;
use crate::private_key::PrivateKey;
use crate::public_identity::PublicIdentity;
use crate::software_key::SoftwareKey;

/// An application identity: a P-256 private key held in a key store, named
/// by its [`KeyReference`] and identified by its [`IdentityId`]. The key is
/// in memory for the software stores, and stays in the hardware for
/// `enclave:` and `tpm:` (ADR 0021, phase 8).
pub struct Identity {
    reference: KeyReference,
    key: Box<dyn PrivateKey>,
    id: IdentityId,
}

impl Identity {
    /// Make a new key and store it where `reference` names. An existing key
    /// is never replaced.
    ///
    /// # Errors
    /// [`IdentityError::AlreadyExists`] when a key is already there,
    /// [`IdentityError::Unsupported`] for a store this platform does not
    /// have, and [`IdentityError::Store`] when the store fails.
    pub fn create(reference: &KeyReference) -> Result<Identity, IdentityError> {
        let key = Self::created_key(reference)?;
        Ok(Self::from_key(reference.clone(), key))
    }

    /// Open the key `reference` names.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] when there is no key,
    /// [`IdentityError::ExposedKeyFile`] for a key file other users can read,
    /// [`IdentityError::InvalidKey`] for a key that is not P-256 PKCS#8, and
    /// the store's own failures.
    pub fn open(reference: &KeyReference) -> Result<Identity, IdentityError> {
        let key = Self::opened_key(reference)?;
        Ok(Self::from_key(reference.clone(), key))
    }

    /// Parse `reference` (for example `keychain:com.delma.yon`) and open it.
    ///
    /// # Errors
    /// [`IdentityError::InvalidReference`], or any error of [`Identity::open`].
    pub fn from_reference(reference: &str) -> Result<Identity, IdentityError> {
        Self::open(&KeyReference::parse(reference)?)
    }

    /// The static identifier, `fid1:p256:...`.
    #[must_use]
    pub fn id(&self) -> &IdentityId {
        &self.id
    }

    /// Where the private key lives.
    #[must_use]
    pub fn reference(&self) -> &KeyReference {
        &self.reference
    }

    /// The public key policies grant and verify against.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        self.key.public_key()
    }

    /// The public half, which policies list and wrap keys for.
    #[must_use]
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity::from_public_key(self.public_key())
    }

    /// Sign `message` with ECDSA P-256 and SHA-256: 64 bytes, `r` then `s`.
    /// A hardware key that refuses yields no signature bytes, which every
    /// check refuses; [`Identity::try_sign`] says why.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.key.sign(message).unwrap_or_default()
    }

    /// Sign `message` as [`Identity::sign`] does, reporting a hardware key's
    /// failure.
    ///
    /// # Errors
    /// [`IdentityError::Store`] when the Secure Enclave or TPM refuses.
    pub fn try_sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        self.key.sign(message)
    }

    /// Whether the private key stays in hardware (`enclave:`, `tpm:`), so
    /// each signature costs milliseconds. A writer then signs records with
    /// a session key this identity certifies once (ADR 0021, phase 8).
    #[must_use]
    pub fn is_hardware(&self) -> bool {
        self.key.is_hardware()
    }

    /// Open a key wrapped for this identity and the purpose `info`
    /// ([`PublicIdentity::wrap`]).
    ///
    /// # Errors
    /// [`IdentityError::Unwrap`] for a wrap made for another identity or
    /// purpose, or a damaged one.
    pub fn unwrap_key(
        &self,
        info: &[u8],
        wrapped: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        self.key.unwrap_key(info, wrapped)
    }

    fn created_key(reference: &KeyReference) -> Result<Box<dyn PrivateKey>, IdentityError> {
        match reference.hardware_store() {
            Some(store) => Ok(Box::new(HardwarePrivateKey::new(store.create()?))),
            None => Self::create_software(reference),
        }
    }

    fn opened_key(reference: &KeyReference) -> Result<Box<dyn PrivateKey>, IdentityError> {
        match reference.hardware_store() {
            Some(store) => Ok(Box::new(HardwarePrivateKey::new(store.open()?))),
            None => Ok(Box::new(SoftwareKey::new(reference.store().load()?))),
        }
    }

    fn create_software(reference: &KeyReference) -> Result<Box<dyn PrivateKey>, IdentityError> {
        let secret_key = Self::generate_secret_key()?;
        reference.store().save(&secret_key)?;
        Ok(Box::new(SoftwareKey::new(secret_key)))
    }

    fn from_key(reference: KeyReference, key: Box<dyn PrivateKey>) -> Identity {
        let id = IdentityId::from_public_key(&key.public_key());
        Identity { reference, key, id }
    }

    fn generate_secret_key() -> Result<SecretKey, IdentityError> {
        SecretKey::try_generate().map_err(|error| {
            IdentityError::Store(format!("the system random generator failed: {error}"))
        })
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Identity")
            .field("reference", &self.reference)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Identity;
    use crate::key_reference::KeyReference;

    struct TemporaryKey(PathBuf);

    impl TemporaryKey {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("fylo-identity-{name}-{}.pem", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        fn identity(&self) -> Identity {
            Identity::create(&KeyReference::File(self.0.clone())).unwrap()
        }
    }

    impl Drop for TemporaryKey {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn its_public_half_verifies_its_signature() {
        let key = TemporaryKey::new("sign");
        let identity = key.identity();
        let signature = identity.sign(b"policy version 1");
        assert_eq!(signature.len(), 64);
        identity
            .public()
            .verify(b"policy version 1", &signature)
            .unwrap();
    }

    #[test]
    fn a_signature_does_not_verify_another_message() {
        let key = TemporaryKey::new("sign-other");
        let identity = key.identity();
        let signature = identity.sign(b"policy version 1");
        let error = identity
            .public()
            .verify(b"policy version 2", &signature)
            .unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_SIGNATURE");
    }

    #[test]
    fn it_unwraps_a_key_wrapped_for_its_public_half() {
        let key = TemporaryKey::new("wrap");
        let identity = key.identity();
        let wrapped = identity.public().wrap(b"catalog", &[7; 32]).unwrap();
        assert_eq!(
            identity
                .unwrap_key(b"catalog", &wrapped)
                .unwrap()
                .as_slice(),
            &[7; 32]
        );
    }

    #[test]
    fn the_public_half_has_the_same_identifier() {
        let key = TemporaryKey::new("public-id");
        let identity = key.identity();
        assert_eq!(identity.public().id(), identity.id());
    }
}
