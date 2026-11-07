use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::identity_error::IdentityError;
use crate::identity_id::IdentityId;

/// The public half of an identity in the browser build (ADR 0021, decision
/// P5-5): the SEC1 uncompressed point, with signatures checked and keys
/// wrapped by the host's `WebCrypto` through the host table. Written as
/// `fpk1:p256:` and the base64url (no padding) of the point, as natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicIdentity {
    point: [u8; 65],
    id: IdentityId,
}

impl PublicIdentity {
    const PREFIX: &'static str = "fpk1:p256:";
    const UNCOMPRESSED: u8 = 4;

    /// The public identity of a 65-byte SEC1 uncompressed point.
    ///
    /// # Errors
    /// [`IdentityError::InvalidIdentifier`] for any other bytes. Whether the
    /// point is on the curve is checked by the host when it is first used.
    pub fn from_sec1(point: &[u8]) -> Result<PublicIdentity, IdentityError> {
        let point: [u8; 65] = point
            .try_into()
            .ok()
            .filter(|point: &[u8; 65]| point[0] == Self::UNCOMPRESSED)
            .ok_or_else(|| {
                IdentityError::InvalidIdentifier("not a SEC1 uncompressed P-256 point".into())
            })?;
        Ok(PublicIdentity {
            id: IdentityId::from_sec1(&point),
            point,
        })
    }

    /// Parse `fpk1:p256:<base64url SEC1 point>`.
    ///
    /// # Errors
    /// [`IdentityError::InvalidIdentifier`] for any other text.
    pub fn parse(text: &str) -> Result<PublicIdentity, IdentityError> {
        let point = text
            .strip_prefix(Self::PREFIX)
            .and_then(|encoded| URL_SAFE_NO_PAD.decode(encoded).ok())
            .ok_or_else(|| Self::invalid(text))?;
        Self::from_sec1(&point).map_err(|_| Self::invalid(text))
    }

    /// The identity's static identifier.
    #[must_use]
    pub fn id(&self) -> &IdentityId {
        &self.id
    }

    /// The SHA-256 of the SEC1 point.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(self.point).into()
    }

    /// Check an ECDSA P-256 (SHA-256) signature, 64 bytes, over `message`.
    ///
    /// # Errors
    /// [`IdentityError::BadSignature`] when it does not verify.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), IdentityError> {
        let signature: &[u8; 64] = signature.try_into().map_err(|_| self.bad_signature())?;
        fylo_vfs::host_identity_verify(&self.point, message, signature)
            .ok()
            .filter(|valid| *valid)
            .map(|_| ())
            .ok_or_else(|| self.bad_signature())
    }

    /// Wrap `secret` for this identity under `info` (HPKE, RFC 9180).
    ///
    /// # Errors
    /// [`IdentityError::Unwrap`] when the host cannot wrap.
    pub fn wrap(&self, info: &[u8], secret: &[u8]) -> Result<Vec<u8>, IdentityError> {
        fylo_vfs::host_identity_wrap(&self.point, info, secret)
            .map_err(|message| IdentityError::Unwrap(message.to_owned()))
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
        write!(
            formatter,
            "{}{}",
            Self::PREFIX,
            URL_SAFE_NO_PAD.encode(self.point)
        )
    }
}

impl FromStr for PublicIdentity {
    type Err = IdentityError;

    fn from_str(text: &str) -> Result<PublicIdentity, IdentityError> {
        PublicIdentity::parse(text)
    }
}
