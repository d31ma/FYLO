use p256::PublicKey;
use p256::ecdsa::Signature;
use p256::elliptic_curve::sec1::ToSec1Point;
use zeroize::Zeroizing;

use crate::android_keystore::AndroidKeystore;
use crate::hardware_key::HardwareKey;
use crate::identity_error::IdentityError;

/// One key in the Android Keystore, named by its alias. The Keystore signs
/// and agrees; FYLO holds only the public key.
pub(crate) struct AndroidKey {
    alias: String,
    keystore: &'static dyn AndroidKeystore,
    public_key: PublicKey,
}

impl AndroidKey {
    pub(crate) fn new(
        alias: String,
        keystore: &'static dyn AndroidKeystore,
        point: &[u8],
    ) -> Result<AndroidKey, IdentityError> {
        let public_key = PublicKey::from_sec1_bytes(point).map_err(|_| {
            IdentityError::Store(format!(
                "android-keystore:{alias}: the Keystore gave a public key that is not a P-256 point"
            ))
        })?;
        Ok(AndroidKey {
            alias,
            keystore,
            public_key,
        })
    }

    fn store_error(&self, problem: &str) -> IdentityError {
        IdentityError::Store(format!("android-keystore:{}: {problem}", self.alias))
    }
}

impl HardwareKey for AndroidKey {
    fn public_key(&self) -> PublicKey {
        self.public_key
    }

    fn sign(&self, message: &[u8]) -> Result<[u8; 64], IdentityError> {
        let der = self.keystore.sign(&self.alias, message)?;
        let signature = Signature::from_der(&der)
            .map_err(|_| self.store_error("the Keystore gave a malformed signature"))?;
        Ok(signature.to_bytes().into())
    }

    fn agree(&self, peer: &PublicKey) -> Result<Zeroizing<[u8; 32]>, IdentityError> {
        let point = peer.to_sec1_point(false);
        let shared = self.keystore.agree(&self.alias, point.as_bytes())?;
        let shared: [u8; 32] = shared
            .as_slice()
            .try_into()
            .map_err(|_| self.store_error("the Keystore gave a shared secret of the wrong size"))?;
        Ok(Zeroizing::new(shared))
    }
}
