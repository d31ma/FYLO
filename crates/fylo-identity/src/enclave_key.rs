//! A Secure Enclave key (ADR 0021, phase 8): P-256, made inside the Secure
//! Enclave and never exported. macOS gives back an opaque token blob that
//! only this Mac's Secure Enclave can use; [`EnclaveKey::restore`] turns the
//! blob back into a key that signs and agrees inside the enclave.
//!
//! The key is made non-permanent, outside the Data Protection keychain: a
//! permanent key there needs the `keychain-access-groups` entitlement, which
//! only a signed application with a provisioning profile has
//! (`errSecMissingEntitlement`, -34018, otherwise). The blob is kept by
//! [`crate::enclave_key_store::EnclaveKeyStore`] instead.
//!
//! The token blob and `SecKeyCreateWithData` have no safe binding in
//! `security-framework`, so this module, like `cng_key_store.rs`, opts in to
//! `unsafe`. Every block is listed in `docs/security/unsafe-inventory.md`.
#![allow(unsafe_code)]

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::error::CFError;
use core_foundation::string::CFString;
use p256::PublicKey;
use p256::ecdsa::Signature;
use p256::elliptic_curve::sec1::ToSec1Point;
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token};
use security_framework_sys::item::{
    kSecAttrKeyClass, kSecAttrKeyClassPrivate, kSecAttrKeyClassPublic, kSecAttrKeyType,
    kSecAttrKeyTypeECSECPrimeRandom, kSecAttrTokenID, kSecAttrTokenIDSecureEnclave,
};
use security_framework_sys::key::SecKeyCreateWithData;
use zeroize::Zeroizing;

use crate::hardware_key::HardwareKey;
use crate::identity_error::IdentityError;

pub(crate) struct EnclaveKey {
    key: SecKey,
    public_key: PublicKey,
    reference: String,
}

impl EnclaveKey {
    const KEY_BITS: u32 = 256;
    const SHARED_BYTES: usize = 32;
    /// `kSecAttrTokenOID`: the attribute that holds a token key's blob. It
    /// is not in the public headers; `SecKeyCopyAttributes` reports it under
    /// this name, and `SecKeyCreateWithData` takes it back.
    const TOKEN_OBJECT: &'static str = "toid";
    const PERMANENT: &'static str = "perm";

    /// Make a new key in the Secure Enclave.
    pub(crate) fn generate(reference: &str) -> Result<EnclaveKey, IdentityError> {
        let mut options = GenerateKeyOptions::default();
        options
            .set_key_type(KeyType::ec_sec_prime_random())
            .set_size_in_bits(Self::KEY_BITS)
            .set_token(Token::SecureEnclave);
        let key = SecKey::new(&options).map_err(|error| Self::failed(reference, &error))?;
        Self::from_key(key, reference)
    }

    /// The key a token blob of [`EnclaveKey::token_blob`] names.
    pub(crate) fn restore(blob: &[u8], reference: &str) -> Result<EnclaveKey, IdentityError> {
        let data = CFData::from_buffer(blob);
        let attributes = Self::private_attributes(&data);
        let key = Self::create_with_data(&data, &attributes)
            .map_err(|error| Self::failed(reference, &error))?;
        Self::from_key(key, reference)
    }

    /// The opaque blob that names this key to this Mac's Secure Enclave.
    pub(crate) fn token_blob(&self) -> Result<Vec<u8>, IdentityError> {
        let attributes = self.key.attributes();
        let name = CFString::from_static_string(Self::TOKEN_OBJECT);
        let value = attributes
            .find(name.as_CFTypeRef())
            .ok_or_else(|| self.store_error("the Secure Enclave key has no token blob"))?;
        // SAFETY: `value` is a live value of `attributes`, which this
        // function owns until it returns; wrapping it under the get rule
        // retains it, so the `CFType` stays valid on its own.
        let value = unsafe { CFType::wrap_under_get_rule(*value) };
        value
            .downcast::<CFData>()
            .map(|data| data.bytes().to_vec())
            .ok_or_else(|| self.store_error("the Secure Enclave key's token blob is not data"))
    }

    fn from_key(key: SecKey, reference: &str) -> Result<EnclaveKey, IdentityError> {
        let point = key
            .public_key()
            .and_then(|public| public.external_representation())
            .ok_or_else(|| {
                IdentityError::Store(format!(
                    "{reference}: the Secure Enclave key has no public key"
                ))
            })?;
        let public_key = PublicKey::from_sec1_bytes(point.bytes()).map_err(|error| {
            IdentityError::InvalidKey(format!("{reference} is not a P-256 key: {error}"))
        })?;
        Ok(EnclaveKey {
            key,
            public_key,
            reference: reference.to_owned(),
        })
    }

    fn peer_key(&self, peer: &PublicKey) -> Result<SecKey, IdentityError> {
        let data = CFData::from_buffer(peer.to_sec1_point(false).as_bytes());
        Self::create_with_data(&data, &Self::public_attributes())
            .map_err(|error| Self::failed(&self.reference, &error))
    }

    fn private_attributes(blob: &CFData) -> CFDictionary<CFString, CFType> {
        // SAFETY: each is an immutable `CFStringRef` constant Security.framework
        // exports for the life of the process; wrapping under the get rule
        // retains it.
        let (type_name, ec, class_name, private, token, enclave) = unsafe {
            (
                CFString::wrap_under_get_rule(kSecAttrKeyType),
                CFString::wrap_under_get_rule(kSecAttrKeyTypeECSECPrimeRandom),
                CFString::wrap_under_get_rule(kSecAttrKeyClass),
                CFString::wrap_under_get_rule(kSecAttrKeyClassPrivate),
                CFString::wrap_under_get_rule(kSecAttrTokenID),
                CFString::wrap_under_get_rule(kSecAttrTokenIDSecureEnclave),
            )
        };
        CFDictionary::from_CFType_pairs(&[
            (type_name, ec.as_CFType()),
            (class_name, private.as_CFType()),
            (token, enclave.as_CFType()),
            (
                CFString::from_static_string(Self::TOKEN_OBJECT),
                blob.as_CFType(),
            ),
            (
                CFString::from_static_string(Self::PERMANENT),
                CFBoolean::false_value().as_CFType(),
            ),
        ])
    }

    fn public_attributes() -> CFDictionary<CFString, CFType> {
        // SAFETY: as in `private_attributes`, immutable exported constants.
        let (type_name, ec, class_name, public) = unsafe {
            (
                CFString::wrap_under_get_rule(kSecAttrKeyType),
                CFString::wrap_under_get_rule(kSecAttrKeyTypeECSECPrimeRandom),
                CFString::wrap_under_get_rule(kSecAttrKeyClass),
                CFString::wrap_under_get_rule(kSecAttrKeyClassPublic),
            )
        };
        CFDictionary::from_CFType_pairs(&[
            (type_name, ec.as_CFType()),
            (class_name, public.as_CFType()),
        ])
    }

    fn create_with_data(
        data: &CFData,
        attributes: &CFDictionary<CFString, CFType>,
    ) -> Result<SecKey, CFError> {
        let mut error = std::ptr::null_mut();
        // SAFETY: `data` and `attributes` are live CF objects borrowed for the
        // call; `error` is a live, exclusively borrowed slot that the call
        // fills only on failure, with a reference this code then owns.
        let key = unsafe {
            SecKeyCreateWithData(
                data.as_concrete_TypeRef(),
                attributes.as_concrete_TypeRef(),
                &raw mut error,
            )
        };
        Self::created(key, error)
    }

    fn created(
        key: security_framework_sys::base::SecKeyRef,
        error: core_foundation_sys::error::CFErrorRef,
    ) -> Result<SecKey, CFError> {
        if key.is_null() {
            // SAFETY: on failure `SecKeyCreateWithData` returns a new
            // `CFErrorRef` the caller owns (the create rule).
            return Err(unsafe { CFError::wrap_under_create_rule(error) });
        }
        // SAFETY: on success it returns a new `SecKeyRef` the caller owns.
        Ok(unsafe { SecKey::wrap_under_create_rule(key) })
    }

    fn failed(reference: &str, error: &CFError) -> IdentityError {
        IdentityError::Store(format!(
            "{reference}: the Secure Enclave refused ({}, {})",
            error.code(),
            error.description()
        ))
    }

    fn store_error(&self, message: &str) -> IdentityError {
        IdentityError::Store(format!("{}: {message}", self.reference))
    }
}

impl HardwareKey for EnclaveKey {
    fn public_key(&self) -> PublicKey {
        self.public_key
    }

    fn sign(&self, message: &[u8]) -> Result<[u8; 64], IdentityError> {
        let der = self
            .key
            .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, message)
            .map_err(|error| Self::failed(&self.reference, &error))?;
        let signature = Signature::from_der(&der)
            .map_err(|_| self.store_error("the Secure Enclave gave a malformed signature"))?;
        Ok(signature.to_bytes().into())
    }

    fn agree(&self, peer: &PublicKey) -> Result<Zeroizing<[u8; 32]>, IdentityError> {
        let peer = self.peer_key(peer)?;
        let shared = Zeroizing::new(
            self.key
                .key_exchange(
                    Algorithm::ECDHKeyExchangeStandard,
                    &peer,
                    Self::SHARED_BYTES,
                    None,
                )
                .map_err(|error| Self::failed(&self.reference, &error))?,
        );
        let shared: [u8; 32] = shared.as_slice().try_into().map_err(|_| {
            self.store_error("the Secure Enclave gave a shared secret of the wrong size")
        })?;
        Ok(Zeroizing::new(shared))
    }
}
