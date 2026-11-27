use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// The Android Keystore as FYLO's Android library offers it (ADR 0026,
/// section 8). Its API is Java only, so the library installs this with
/// [`crate::AndroidKeystoreHost::install`] and answers through JNI.
///
/// Keys are P-256, made inside the Keystore (`StrongBox` when the device has
/// one, else the TEE) for signing and key agreement, and they never leave
/// it. Points are 65-byte uncompressed SEC1.
pub trait AndroidKeystore: Send + Sync {
    /// Make a key under `alias` and answer its public key.
    ///
    /// # Errors
    /// [`IdentityError::AlreadyExists`] when `alias` holds a key,
    /// [`IdentityError::Unsupported`] before Android 12 (no ECDH in the
    /// Keystore), or [`IdentityError::Store`].
    fn create(&self, alias: &str) -> Result<Vec<u8>, IdentityError>;

    /// The public key of the key under `alias`.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] when `alias` holds no key, or
    /// [`IdentityError::Store`].
    fn public_key(&self, alias: &str) -> Result<Vec<u8>, IdentityError>;

    /// An ECDSA P-256 signature over the SHA-256 of `message`, DER encoded.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] or [`IdentityError::Store`].
    fn sign(&self, alias: &str, message: &[u8]) -> Result<Vec<u8>, IdentityError>;

    /// The ECDH shared secret with the point `peer`: 32 bytes.
    ///
    /// # Errors
    /// [`IdentityError::NotFound`] or [`IdentityError::Store`].
    fn agree(&self, alias: &str, peer: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError>;
}
