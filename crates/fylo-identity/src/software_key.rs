use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::{PublicKey, SecretKey};
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;
use crate::key_wrap::KeyWrap;
use crate::private_key::PrivateKey;

/// A private key FYLO holds in memory: what the `file:`, `keychain:`,
/// `cng:` and `creds:` stores give back.
pub(crate) struct SoftwareKey {
    secret_key: SecretKey,
    signing_key: SigningKey,
}

impl SoftwareKey {
    pub(crate) fn new(secret_key: SecretKey) -> SoftwareKey {
        let signing_key = SigningKey::from(&secret_key);
        SoftwareKey {
            secret_key,
            signing_key,
        }
    }
}

impl PrivateKey for SoftwareKey {
    fn public_key(&self) -> PublicKey {
        self.secret_key.public_key()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        let signature: Signature = self.signing_key.sign(message);
        Ok(signature.to_bytes().to_vec())
    }

    fn unwrap_key(&self, info: &[u8], wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        KeyWrap::open(&self.secret_key, info, wrapped)
    }

    fn is_hardware(&self) -> bool {
        false
    }
}
