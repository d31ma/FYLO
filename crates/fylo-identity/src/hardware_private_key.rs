use p256::PublicKey;
use zeroize::Zeroizing;

use crate::hardware_key::HardwareKey;
use crate::hardware_key_wrap::HardwareKeyWrap;
use crate::identity_error::IdentityError;
use crate::private_key::PrivateKey;

/// A hardware key as an identity's private key: signatures come from the
/// hardware, and wraps open through its ECDH ([`HardwareKeyWrap`]).
pub(crate) struct HardwarePrivateKey {
    key: Box<dyn HardwareKey>,
}

impl HardwarePrivateKey {
    pub(crate) fn new(key: Box<dyn HardwareKey>) -> HardwarePrivateKey {
        HardwarePrivateKey { key }
    }
}

impl PrivateKey for HardwarePrivateKey {
    fn public_key(&self) -> PublicKey {
        self.key.public_key()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        self.key.sign(message).map(|signature| signature.to_vec())
    }

    fn unwrap_key(&self, info: &[u8], wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        HardwareKeyWrap::open(self.key.as_ref(), info, wrapped)
    }

    fn is_hardware(&self) -> bool {
        true
    }
}
