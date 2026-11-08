use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::DhP256HkdfSha256;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;

/// RFC 9180 HPKE in base mode with DHKEM(P-256, HKDF-SHA256), HKDF-SHA256
/// and AES-256-GCM, single shot: a wrapped secret is the 65-byte
/// encapsulated key followed by the ciphertext. `info` binds a wrap to what
/// it is for, so it opens nowhere else.
pub(crate) struct KeyWrap;

type WrapKem = DhP256HkdfSha256;

impl KeyWrap {
    const ENCAPSULATED_BYTES: usize = 65;

    pub(crate) fn seal(
        public_key: &PublicKey,
        info: &[u8],
        secret: &[u8],
    ) -> Result<Vec<u8>, IdentityError> {
        let recipient = Self::hpke_public_key(public_key)?;
        let (encapsulated, ciphertext) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, WrapKem>(
            &OpModeS::Base,
            &recipient,
            info,
            secret,
            &[],
        )
        .map_err(|error| IdentityError::Unwrap(format!("cannot wrap a key: {error}")))?;
        let mut wrapped = encapsulated.to_bytes().to_vec();
        wrapped.extend_from_slice(&ciphertext);
        Ok(wrapped)
    }

    pub(crate) fn open(
        secret_key: &SecretKey,
        info: &[u8],
        wrapped: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let (encapsulated, ciphertext) = Self::split(wrapped)?;
        let recipient = Self::hpke_private_key(secret_key)?;
        hpke::single_shot_open::<AesGcm256, HkdfSha256, WrapKem>(
            &OpModeR::Base,
            &recipient,
            &encapsulated,
            info,
            ciphertext,
            &[],
        )
        .map(Zeroizing::new)
        .map_err(|_| Self::refused())
    }

    fn split(wrapped: &[u8]) -> Result<(<WrapKem as Kem>::EncappedKey, &[u8]), IdentityError> {
        let (encapsulated, ciphertext) = wrapped
            .split_at_checked(Self::ENCAPSULATED_BYTES)
            .ok_or_else(Self::refused)?;
        let encapsulated =
            <WrapKem as Kem>::EncappedKey::from_bytes(encapsulated).map_err(|_| Self::refused())?;
        Ok((encapsulated, ciphertext))
    }

    fn hpke_public_key(
        public_key: &PublicKey,
    ) -> Result<<WrapKem as Kem>::PublicKey, IdentityError> {
        <WrapKem as Kem>::PublicKey::from_bytes(public_key.to_sec1_point(false).as_bytes())
            .map_err(|error| IdentityError::InvalidKey(format!("not an HPKE public key: {error}")))
    }

    fn hpke_private_key(
        secret_key: &SecretKey,
    ) -> Result<<WrapKem as Kem>::PrivateKey, IdentityError> {
        let bytes = Zeroizing::new(secret_key.to_bytes());
        <WrapKem as Kem>::PrivateKey::from_bytes(bytes.as_slice())
            .map_err(|error| IdentityError::InvalidKey(format!("not an HPKE private key: {error}")))
    }

    fn refused() -> IdentityError {
        IdentityError::Unwrap(
            "the wrapped key is damaged, or was wrapped for another identity or purpose".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use p256::SecretKey;

    use super::KeyWrap;

    const SECRET: [u8; 32] = [9; 32];

    fn key(byte: u8) -> SecretKey {
        SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    #[test]
    fn opens_what_it_sealed() {
        let recipient = key(3);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        let opened = KeyWrap::open(&recipient, b"purpose", &wrapped).unwrap();
        assert_eq!(opened.as_slice(), &SECRET);
    }

    #[test]
    fn another_identity_cannot_open_it() {
        let wrapped = KeyWrap::seal(&key(3).public_key(), b"purpose", &SECRET).unwrap();
        let error = KeyWrap::open(&key(4), b"purpose", &wrapped).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn it_opens_only_for_its_purpose() {
        let recipient = key(3);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        let error = KeyWrap::open(&recipient, b"other", &wrapped).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn a_truncated_wrap_is_refused() {
        let recipient = key(3);
        let wrapped = KeyWrap::seal(&recipient.public_key(), b"purpose", &SECRET).unwrap();
        let error = KeyWrap::open(&recipient, b"purpose", &wrapped[..40]).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNWRAP");
    }

    #[test]
    fn each_wrap_is_different() {
        let public_key = key(3).public_key();
        let first = KeyWrap::seal(&public_key, b"purpose", &SECRET).unwrap();
        let second = KeyWrap::seal(&public_key, b"purpose", &SECRET).unwrap();
        assert_ne!(first, second);
    }
}
