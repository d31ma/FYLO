use p256::PublicKey;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// An identity's private key as [`crate::Identity`] uses it: held in memory
/// ([`crate::software_key::SoftwareKey`]) or kept by hardware
/// ([`crate::hardware_private_key::HardwarePrivateKey`]).
pub(crate) trait PrivateKey: Send + Sync {
    /// The public half.
    fn public_key(&self) -> PublicKey;

    /// An ECDSA P-256 (SHA-256) signature over `message`: 64 bytes, `r`
    /// then `s`.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError>;

    /// Open a key wrapped for this identity under `info` (HPKE).
    fn unwrap_key(&self, info: &[u8], wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError>;

    /// Whether the key stays in hardware.
    fn is_hardware(&self) -> bool;
}
