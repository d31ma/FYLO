use std::fmt;
use std::str::FromStr;

#[cfg(not(target_family = "wasm"))]
use p256::PublicKey;
#[cfg(not(target_family = "wasm"))]
use p256::elliptic_curve::sec1::ToSec1Point;
use sha2::{Digest, Sha256};

use crate::base32::Base32;
use crate::identity_error::IdentityError;

/// The static identifier of an identity: `fid1:p256:` and the RFC 4648
/// base32 (upper case, no padding) of the SHA-256 of the public key in SEC1
/// uncompressed form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdentityId(String);

impl IdentityId {
    const PREFIX: &'static str = "fid1:p256:";
    const ENCODED_LENGTH: usize = 52;

    /// The identifier of `public_key`.
    #[must_use]
    #[cfg(not(target_family = "wasm"))]
    pub fn from_public_key(public_key: &PublicKey) -> IdentityId {
        Self::from_sec1(public_key.to_sec1_point(false).as_bytes())
    }

    /// The identifier of a public key in SEC1 uncompressed form.
    #[must_use]
    pub fn from_sec1(point: &[u8]) -> IdentityId {
        let digest = Sha256::digest(point);
        IdentityId(format!("{}{}", Self::PREFIX, Base32::encode(&digest)))
    }

    /// Parse an identifier written as `fid1:p256:<52 base32 characters>`.
    ///
    /// # Errors
    /// [`IdentityError::InvalidIdentifier`] for any other text.
    pub fn parse(text: &str) -> Result<IdentityId, IdentityError> {
        let encoded = Self::encoded_part(text)?;
        Ok(IdentityId(format!("{}{encoded}", Self::PREFIX)))
    }

    /// The identifier as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn encoded_part(text: &str) -> Result<&str, IdentityError> {
        text.strip_prefix(Self::PREFIX)
            .filter(|encoded| Self::is_fingerprint(encoded))
            .ok_or_else(|| {
                IdentityError::InvalidIdentifier(format!(
                    "{text:?} is not an identity identifier ({}<52 base32 characters>)",
                    Self::PREFIX
                ))
            })
    }

    fn is_fingerprint(encoded: &str) -> bool {
        encoded.len() == Self::ENCODED_LENGTH && Base32::is_encoded(encoded)
    }
}

impl fmt::Display for IdentityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for IdentityId {
    type Err = IdentityError;

    fn from_str(text: &str) -> Result<IdentityId, IdentityError> {
        IdentityId::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use p256::SecretKey;

    use super::IdentityId;

    const SECRET: [u8; 32] = [7; 32];

    #[test]
    fn fingerprints_the_uncompressed_public_key() {
        let public_key = SecretKey::from_slice(&SECRET).unwrap().public_key();
        let id = IdentityId::from_public_key(&public_key);
        assert!(id.as_str().starts_with("fid1:p256:"));
        assert_eq!(id.as_str().len(), "fid1:p256:".len() + 52);
        assert_eq!(IdentityId::parse(id.as_str()).unwrap(), id);
    }

    #[test]
    fn the_same_key_always_has_the_same_identifier() {
        let first = SecretKey::from_slice(&SECRET).unwrap().public_key();
        let second = SecretKey::from_slice(&SECRET).unwrap().public_key();
        assert_eq!(
            IdentityId::from_public_key(&first),
            IdentityId::from_public_key(&second)
        );
    }

    #[test]
    fn refuses_a_wrong_prefix() {
        let error = IdentityId::parse(&format!("fid2:p256:{}", "A".repeat(52))).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_ID");
    }

    #[test]
    fn refuses_a_short_fingerprint() {
        let error = IdentityId::parse(&format!("fid1:p256:{}", "A".repeat(51))).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_ID");
    }

    #[test]
    fn refuses_lower_case_base32() {
        let error = IdentityId::parse(&format!("fid1:p256:{}", "a".repeat(52))).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_ID");
    }
}
