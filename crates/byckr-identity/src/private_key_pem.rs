use p256::SecretKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`), the text form of a stored
/// private key, which `openssl pkey` also reads.
pub(crate) struct PrivateKeyPem;

impl PrivateKeyPem {
    pub(crate) fn encode(secret_key: &SecretKey) -> Result<Zeroizing<String>, IdentityError> {
        secret_key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|error| IdentityError::InvalidKey(format!("cannot encode the key: {error}")))
    }

    pub(crate) fn decode(text: &str, reference: &str) -> Result<SecretKey, IdentityError> {
        SecretKey::from_pkcs8_pem(text).map_err(|error| {
            IdentityError::InvalidKey(format!(
                "{reference} is not a P-256 private key in PKCS#8 PEM form: {error}"
            ))
        })
    }
}
