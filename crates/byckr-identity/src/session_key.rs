use p256::SecretKey;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;

use crate::identity_error::IdentityError;

/// A short-lived software P-256 key a writer signs records with, once an
/// identity has certified it (ADR 0021, phase 8). It lives only in the
/// writer's memory, and zeroizes when dropped, so a hardware identity signs
/// once per session rather than once per record.
pub struct SessionKey {
    signing_key: SigningKey,
    point: [u8; 65],
}

impl SessionKey {
    /// A fresh session key from the system random generator.
    ///
    /// # Errors
    /// [`IdentityError::Store`] when the system random generator fails.
    pub fn generate() -> Result<SessionKey, IdentityError> {
        let secret_key = SecretKey::try_generate().map_err(|error| {
            IdentityError::Store(format!("the system random generator failed: {error}"))
        })?;
        let signing_key = SigningKey::from(&secret_key);
        let point = Self::uncompressed_point(&signing_key)?;
        Ok(SessionKey { signing_key, point })
    }

    /// The public key, a 65-byte SEC1 uncompressed point.
    #[must_use]
    pub fn public_point(&self) -> [u8; 65] {
        self.point
    }

    /// An ECDSA P-256 (SHA-256) signature over `message`: 64 bytes, `r`
    /// then `s`.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.signing_key.sign(message);
        signature.to_bytes().to_vec()
    }

    fn uncompressed_point(signing_key: &SigningKey) -> Result<[u8; 65], IdentityError> {
        signing_key
            .verifying_key()
            .as_affine()
            .to_sec1_point(false)
            .as_bytes()
            .try_into()
            .map_err(|_| IdentityError::InvalidKey("a session key is not a P-256 point".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::SessionKey;
    use crate::public_identity::PublicIdentity;

    #[test]
    fn its_public_point_verifies_its_signature() {
        let session = SessionKey::generate().unwrap();
        let signature = session.sign(b"record");
        PublicIdentity::from_sec1(&session.public_point())
            .unwrap()
            .verify(b"record", &signature)
            .unwrap();
    }

    #[test]
    fn two_sessions_have_different_keys() {
        let first = SessionKey::generate().unwrap();
        let second = SessionKey::generate().unwrap();
        assert_ne!(first.public_point(), second.public_point());
    }
}
