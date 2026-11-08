use std::fmt;

use zeroize::Zeroizing;

use crate::identity_error::IdentityError;
use crate::identity_id::IdentityId;
use crate::key_reference::KeyReference;
use crate::public_identity_browser::PublicIdentity;

/// An application identity in the browser build (ADR 0021, decision P5-5):
/// a `webcrypto:<name>` key the host holds as non-extractable `CryptoKey`s in
/// the origin's `IndexedDB`. The engine never sees the private key; it asks the
/// host to sign and unwrap through the host table.
pub struct Identity {
    reference: KeyReference,
    public: PublicIdentity,
}

impl Identity {
    /// Open the identity the host holds. The browser's host is configured
    /// with one `webcrypto:` identity; any other store does not exist here.
    ///
    /// # Errors
    /// [`IdentityError::Unsupported`] for another store, and
    /// [`IdentityError::NotFound`] when the host holds no identity.
    pub fn open(reference: &KeyReference) -> Result<Identity, IdentityError> {
        let KeyReference::WebCrypto(_) = reference else {
            return Err(IdentityError::Unsupported(format!(
                "{reference} is not available in the browser; use webcrypto:<name>"
            )));
        };
        let point = fylo_vfs::host_identity_public()
            .map_err(|message| IdentityError::NotFound(format!("{reference}: {message}")))?;
        Ok(Identity {
            reference: reference.clone(),
            public: PublicIdentity::from_sec1(&point)?,
        })
    }

    /// Parse `reference` and open it.
    ///
    /// # Errors
    /// [`IdentityError::InvalidReference`], or any error of [`Identity::open`].
    pub fn from_reference(reference: &str) -> Result<Identity, IdentityError> {
        Self::open(&KeyReference::parse(reference)?)
    }

    /// The static identifier, `fid1:p256:...`.
    #[must_use]
    pub fn id(&self) -> &IdentityId {
        self.public.id()
    }

    /// Where the private key lives.
    #[must_use]
    pub fn reference(&self) -> &KeyReference {
        &self.reference
    }

    /// The public half.
    #[must_use]
    pub fn public(&self) -> PublicIdentity {
        self.public.clone()
    }

    /// Sign `message` with ECDSA P-256 and SHA-256, through the host. A host
    /// that cannot sign yields no signature bytes, which every check refuses.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        fylo_vfs::host_identity_sign(message)
            .map(|signature| signature.to_vec())
            .unwrap_or_default()
    }

    /// Sign `message` through the host, reporting a host that cannot.
    ///
    /// # Errors
    /// [`IdentityError::Store`] when the host refuses.
    pub fn try_sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        fylo_vfs::host_identity_sign(message)
            .map(|signature| signature.to_vec())
            .map_err(|message| IdentityError::Store(format!("{}: {message}", self.reference)))
    }

    /// A `webcrypto:` key is a software key the browser holds.
    #[must_use]
    pub fn is_hardware(&self) -> bool {
        false
    }

    /// Open a key wrapped for this identity under `info`, through the host.
    ///
    /// # Errors
    /// [`IdentityError::Unwrap`] for a wrap made for another identity or
    /// purpose, or a damaged one.
    pub fn unwrap_key(
        &self,
        info: &[u8],
        wrapped: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        fylo_vfs::host_identity_unwrap(info, wrapped)
            .map(|key| Zeroizing::new(key.to_vec()))
            .map_err(|message| IdentityError::Unwrap(message.to_owned()))
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Identity")
            .field("reference", &self.reference)
            .field("id", self.id())
            .finish_non_exhaustive()
    }
}
