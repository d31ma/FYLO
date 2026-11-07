use std::fmt;

use p256::elliptic_curve::Generate;
use p256::{PublicKey, SecretKey};

use crate::identity_error::IdentityError;
use crate::identity_id::IdentityId;
use crate::key_reference::KeyReference;

/// An application identity: a P-256 private key held in a key store, named
/// by its [`KeyReference`] and identified by its [`IdentityId`].
pub struct Identity {
    reference: KeyReference,
    secret_key: SecretKey,
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
        let secret_key = Self::generate_secret_key()?;
        reference.store().save(&secret_key)?;
        Ok(Self::from_secret_key(reference.clone(), secret_key))
    }

    /// Open the key `reference` names.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] when there is no key,
    /// [`IdentityError::ExposedKeyFile`] for a key file other users can read,
    /// [`IdentityError::InvalidKey`] for a key that is not P-256 PKCS#8, and
    /// the store's own failures.
    pub fn open(reference: &KeyReference) -> Result<Identity, IdentityError> {
        let secret_key = reference.store().load()?;
        Ok(Self::from_secret_key(reference.clone(), secret_key))
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
        self.secret_key.public_key()
    }

    fn from_secret_key(reference: KeyReference, secret_key: SecretKey) -> Identity {
        let id = IdentityId::from_public_key(&secret_key.public_key());
        Identity {
            reference,
            secret_key,
            id,
        }
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
