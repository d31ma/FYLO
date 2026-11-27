use p256::PublicKey;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// A P-256 private key held by hardware that never lets it out (ADR 0021,
/// phase 8): a Secure Enclave key on macOS or a TPM key on Windows. FYLO
/// asks the hardware to sign and to agree on a shared secret; it never sees
/// the key.
pub(crate) trait HardwareKey: Send + Sync {
    /// The public half.
    fn public_key(&self) -> PublicKey;

    /// An ECDSA P-256 signature over the SHA-256 of `message`: 64 bytes,
    /// `r` then `s`.
    fn sign(&self, message: &[u8]) -> Result<[u8; 64], IdentityError>;

    /// The ECDH shared secret with `peer`: the x-coordinate of the shared
    /// point, 32 bytes big-endian.
    fn agree(&self, peer: &PublicKey) -> Result<Zeroizing<[u8; 32]>, IdentityError>;
}
