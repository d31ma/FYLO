use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::PublicKey;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::elliptic_curve::sec1::ToSec1Point;

use crate::identity_error::IdentityError;
use crate::identity_id::IdentityId;
use crate::key_wrap::KeyWrap;

/// The public half of an identity: what a policy needs to verify its
/// signatures and to wrap keys for it. Written as `fpk1:p256:` and the
/// base64url (no padding) of the SEC1 uncompressed public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicIdentity {
    public_key: PublicKey,
    id: IdentityId,
}

impl PublicIdentity {
    const PREFIX: &'static str = "fpk1:p256:";

    /// The public identity of `public_key`.
    #[must_use]
    pub fn from_public_key(public_key: PublicKey) -> PublicIdentity {
        let id = IdentityId::from_public_key(&public_key);
        PublicIdentity { public_key, id }
    }

    /// Parse `fpk1:p256:<base64url SEC1 point>`.
    ///
    /// # Errors
    /// [`IdentityError::InvalidIdentifier`] for any other text, or a point
    /// that is not on the curve.
    pub fn parse(text: &str) -> Result<PublicIdentity, IdentityError> {
        let point = Self::decode_point(text)?;
        let public_key = PublicKey::from_sec1_bytes(&point).map_err(|_| Self::invalid(text))?;
        Ok(Self::from_public_key(public_key))
    }

    /// The identity's static identifier.
    #[must_use]
    pub fn id(&self) -> &IdentityId {
        &self.id
    }

    /// The SHA-256 of the SEC1 uncompressed public key: the bytes the
    /// identifier is the base32 of.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        use sha2::{Digest as _, Sha256};
        Sha256::digest(self.public_key.to_sec1_point(false).as_bytes()).into()
    }

    /// The P-256 public key.
    #[must_use]
    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    /// Check an ECDSA P-256 (SHA-256) signature this identity made over
    /// `message`: 64 bytes, `r` then `s`.
    ///
    /// # Errors
    /// [`IdentityError::BadSignature`] when it is not.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), IdentityError> {
        let signature = Signature::from_slice(signature).map_err(|_| self.bad_signature())?;
        VerifyingKey::from(&self.public_key)
            .verify(message, &signature)
            .map_err(|_| self.bad_signature())
    }

    /// Wrap `secret` so only this identity's private key opens it, for the
    /// purpose `info` names (HPKE, RFC 9180).
    ///
    /// # Errors
    /// [`IdentityError::Unwrap`] when the system random generator fails.
    pub fn wrap(&self, info: &[u8], secret: &[u8]) -> Result<Vec<u8>, IdentityError> {
        KeyWrap::seal(&self.public_key, info, secret)
    }

    fn decode_point(text: &str) -> Result<Vec<u8>, IdentityError> {
        let encoded = text
            .strip_prefix(Self::PREFIX)
            .ok_or_else(|| Self::invalid(text))?;
        URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Self::invalid(text))
    }

    fn invalid(text: &str) -> IdentityError {
        IdentityError::InvalidIdentifier(format!(
            "{text:?} is not a public identity ({}<base64url P-256 point>)",
            Self::PREFIX
        ))
    }

    fn bad_signature(&self) -> IdentityError {
        IdentityError::BadSignature(format!("the signature is not {}'s", self.id))
    }
}

impl fmt::Display for PublicIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let point = self.public_key.to_sec1_point(false);
        write!(
            formatter,
            "{}{}",
            Self::PREFIX,
            URL_SAFE_NO_PAD.encode(point.as_bytes())
        )
    }
}

impl FromStr for PublicIdentity {
    type Err = IdentityError;

    fn from_str(text: &str) -> Result<PublicIdentity, IdentityError> {
        PublicIdentity::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use p256::SecretKey;

    use super::PublicIdentity;

    fn public(byte: u8) -> PublicIdentity {
        PublicIdentity::from_public_key(SecretKey::from_slice(&[byte; 32]).unwrap().public_key())
    }

    #[test]
    fn parses_what_it_prints() {
        let identity = public(5);
        let text = identity.to_string();
        assert!(text.starts_with("fpk1:p256:"));
        assert_eq!(PublicIdentity::parse(&text).unwrap(), identity);
    }

    #[test]
    fn refuses_an_identifier_in_place_of_a_public_key() {
        let error = PublicIdentity::parse(public(5).id().as_str()).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_ID");
    }

    #[test]
    fn refuses_a_point_off_the_curve() {
        let error = PublicIdentity::parse(&format!("fpk1:p256:B{}", "A".repeat(86))).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_ID");
    }

    #[test]
    fn refuses_a_malformed_signature() {
        let error = public(5).verify(b"message", &[0; 10]).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_SIGNATURE");
    }
}
