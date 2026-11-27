use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use hkdf::Hkdf;
use p256::PublicKey;
use p256::elliptic_curve::sec1::ToSec1Point;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::hardware_key::HardwareKey;
use crate::identity_error::IdentityError;

/// Opens a [`crate::key_wrap::KeyWrap`] wrap with a hardware key, which
/// gives FYLO its ECDH shared secret but never its private key: RFC 9180
/// HPKE base mode, DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM,
/// single shot, with the KEM's Diffie-Hellman step done by the hardware.
/// The `hpke` crate takes only a private key, so the key schedule is
/// written out here; the tests open the crate's wraps with it.
pub(crate) struct HardwareKeyWrap;

impl HardwareKeyWrap {
    const ENCAPSULATED_BYTES: usize = 65;
    const VERSION_LABEL: &'static [u8] = b"HPKE-v1";
    const KEM_SUITE: &'static [u8] = b"KEM\x00\x10";
    const HPKE_SUITE: &'static [u8] = b"HPKE\x00\x10\x00\x01\x00\x02";
    const BASE_MODE: u8 = 0;
    const KEY_BYTES: usize = 32;
    const NONCE_BYTES: usize = 12;

    pub(crate) fn open(
        key: &dyn HardwareKey,
        info: &[u8],
        wrapped: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let (encapsulated, ciphertext) = wrapped
            .split_at_checked(Self::ENCAPSULATED_BYTES)
            .ok_or_else(Self::refused)?;
        let sender = PublicKey::from_sec1_bytes(encapsulated).map_err(|_| Self::refused())?;
        let shared = key.agree(&sender)?;
        let recipient = key.public_key().to_sec1_point(false);
        let kem_context = [encapsulated, recipient.as_bytes()].concat();
        let secret = Self::kem_secret(shared.as_slice(), &kem_context)?;
        Self::decrypt(&secret, info, ciphertext)
    }

    fn kem_secret(shared: &[u8], kem_context: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let prk = Self::labeled_extract(Self::KEM_SUITE, &[], b"eae_prk", shared);
        Self::labeled_expand(
            Self::KEM_SUITE,
            &prk,
            b"shared_secret",
            kem_context,
            Self::KEY_BYTES,
        )
    }

    fn decrypt(
        shared_secret: &[u8],
        info: &[u8],
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let context = Self::key_schedule_context(info);
        let secret = Self::labeled_extract(Self::HPKE_SUITE, shared_secret, b"secret", &[]);
        let key =
            Self::labeled_expand(Self::HPKE_SUITE, &secret, b"key", &context, Self::KEY_BYTES)?;
        let nonce = Self::labeled_expand(
            Self::HPKE_SUITE,
            &secret,
            b"base_nonce",
            &context,
            Self::NONCE_BYTES,
        )?;
        let nonce: &[u8; Self::NONCE_BYTES] =
            nonce.as_slice().try_into().map_err(|_| Self::refused())?;
        Aes256Gcm::new_from_slice(&key)
            .map_err(|_| Self::refused())?
            .decrypt(nonce.into(), ciphertext)
            .map(Zeroizing::new)
            .map_err(|_| Self::refused())
    }

    fn key_schedule_context(info: &[u8]) -> Vec<u8> {
        let psk_id_hash = Self::labeled_extract(Self::HPKE_SUITE, &[], b"psk_id_hash", &[]);
        let info_hash = Self::labeled_extract(Self::HPKE_SUITE, &[], b"info_hash", info);
        [&[Self::BASE_MODE][..], &psk_id_hash, &info_hash].concat()
    }

    fn labeled_extract(suite: &[u8], salt: &[u8], label: &[u8], ikm: &[u8]) -> Zeroizing<Vec<u8>> {
        let input = Zeroizing::new([Self::VERSION_LABEL, suite, label, ikm].concat());
        let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &input);
        Zeroizing::new(prk.to_vec())
    }

    fn labeled_expand(
        suite: &[u8],
        prk: &[u8],
        label: &[u8],
        info: &[u8],
        length: usize,
    ) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let size = u16::try_from(length).map_err(|_| Self::refused())?;
        let labeled_info = [
            &size.to_be_bytes()[..],
            Self::VERSION_LABEL,
            suite,
            label,
            info,
        ]
        .concat();
        let mut output = Zeroizing::new(vec![0_u8; length]);
        Hkdf::<Sha256>::from_prk(prk)
            .map_err(|_| Self::refused())?
            .expand(&labeled_info, &mut output)
            .map_err(|_| Self::refused())?;
        Ok(output)
    }

    fn refused() -> IdentityError {
        IdentityError::Unwrap(
            "the wrapped key is damaged, or was wrapped for another identity or purpose".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use p256::elliptic_curve::point::AffineCoordinates as _;
    use p256::elliptic_curve::sec1::ToSec1Point as _;
    use p256::{ProjectivePoint, PublicKey, SecretKey};
    use zeroize::Zeroizing;

    use super::HardwareKeyWrap;
    use crate::hardware_key::HardwareKey;
    use crate::identity_error::IdentityError;
    use crate::key_wrap::KeyWrap;

    const SECRET: [u8; 32] = [9; 32];

    /// A software key standing in for hardware: it agrees the way a
    /// Secure Enclave or TPM does, returning only the shared x-coordinate.
    struct SoftwareStandIn(SecretKey);

    impl HardwareKey for SoftwareStandIn {
        fn public_key(&self) -> PublicKey {
            self.0.public_key()
        }

        fn sign(&self, _message: &[u8]) -> Result<[u8; 64], IdentityError> {
            Err(IdentityError::Store("not used".into()))
        }

        fn agree(&self, peer: &PublicKey) -> Result<Zeroizing<[u8; 32]>, IdentityError> {
            let point = (ProjectivePoint::from(*peer.as_affine()) * *self.0.to_nonzero_scalar())
                .to_affine();
            Ok(Zeroizing::new(point.x().into()))
        }
    }

    fn key(byte: u8) -> SoftwareStandIn {
        SoftwareStandIn(SecretKey::from_slice(&[byte; 32]).unwrap())
    }

    #[test]
    fn opens_a_wrap_the_hpke_crate_sealed() {
        let recipient = key(3);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        let opened = HardwareKeyWrap::open(&recipient, b"purpose", &wrapped).unwrap();
        assert_eq!(opened.as_slice(), &SECRET);
    }

    #[test]
    fn opens_a_wrap_of_another_length() {
        let recipient = key(5);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"collection", &[1; 64]).unwrap();
        let opened = HardwareKeyWrap::open(&recipient, b"collection", &wrapped).unwrap();
        assert_eq!(opened.as_slice(), &[1; 64]);
    }

    #[test]
    fn another_identity_cannot_open_it() {
        let wrapped = KeyWrap::seal(&key(3).public_key(), b"purpose", &SECRET).unwrap();
        let error = HardwareKeyWrap::open(&key(4), b"purpose", &wrapped).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn it_opens_only_for_its_purpose() {
        let recipient = key(3);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        let error = HardwareKeyWrap::open(&recipient, b"other", &wrapped).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn a_truncated_wrap_is_refused() {
        let recipient = key(3);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        let error = HardwareKeyWrap::open(&recipient, b"purpose", &wrapped[..40]).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn an_encapsulated_key_off_the_curve_is_refused() {
        let recipient = key(3);
        let mut wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        wrapped[64] ^= 1;
        let error = HardwareKeyWrap::open(&recipient, b"purpose", &wrapped).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn the_stand_in_agrees_with_the_x_coordinate() {
        let first = key(3);
        let second = key(4);
        let one = first.agree(&second.public_key()).unwrap();
        let two = second.agree(&first.public_key()).unwrap();
        assert_eq!(*one, *two);
        assert_ne!(
            first.public_key().to_sec1_point(false).as_bytes()[1..33],
            one[..]
        );
    }
}
