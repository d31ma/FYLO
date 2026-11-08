//! CNG key stores on Windows.
//!
//! - `cng:<name>` (ADR 0021, decision P5-2): a P-256 key persisted under
//!   `name` in the Microsoft Software Key Storage Provider, which keeps it
//!   encrypted with the user's DPAPI key. It signs in software (Q8), so the
//!   key is imported with an export policy that lets FYLO read it back as
//!   PKCS#8.
//! - `tpm:<name>` (phase 8, ADR 0021 section 8): a P-256 key the Microsoft
//!   Platform Crypto Provider makes inside the TPM 2.0 and never exports.
//!   FYLO asks the TPM to sign (`NCryptSignHash`) and to agree
//!   (`NCryptSecretAgreement`, raw secret). A machine without a TPM fails
//!   with `EIDENTITY_UNSUPPORTED`.
//!
//! CNG is a C API, reached through Microsoft's `windows-sys` declarations,
//! so this module opts in to `unsafe`. Every call is listed in
//! `docs/security/unsafe-inventory.md`.
#![allow(unsafe_code)]

use std::ptr;

use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use p256::{PublicKey, SecretKey};
use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{
    NTE_BAD_KEYSET, NTE_DEVICE_NOT_FOUND, NTE_DEVICE_NOT_READY, NTE_EXISTS, NTE_PROVIDER_DLL_FAIL,
    TBS_E_SERVICE_NOT_RUNNING, TBS_E_TPM_NOT_FOUND, TPM_E_PCP_DEVICE_NOT_READY,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDH_PUBLIC_P256_MAGIC, BCRYPT_KDF_RAW_SECRET, BCryptBuffer,
    BCryptBufferDesc, MS_KEY_STORAGE_PROVIDER, MS_PLATFORM_CRYPTO_PROVIDER,
    NCRYPT_ALLOW_EXPORT_FLAG, NCRYPT_ALLOW_KEY_AGREEMENT_FLAG, NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG,
    NCRYPT_ALLOW_SIGNING_FLAG, NCRYPT_DO_NOT_FINALIZE_FLAG, NCRYPT_ECDH_P256_ALGORITHM,
    NCRYPT_EXPORT_POLICY_PROPERTY, NCRYPT_KEY_HANDLE, NCRYPT_KEY_USAGE_PROPERTY,
    NCRYPT_PKCS8_PRIVATE_KEY_BLOB, NCRYPT_PROV_HANDLE, NCRYPT_SECRET_HANDLE,
    NCRYPTBUFFER_PKCS_KEY_NAME, NCRYPTBUFFER_VERSION, NCryptCreatePersistedKey, NCryptDeriveKey,
    NCryptExportKey, NCryptFinalizeKey, NCryptFreeObject, NCryptImportKey, NCryptOpenKey,
    NCryptOpenStorageProvider, NCryptSecretAgreement, NCryptSetProperty, NCryptSignHash,
};
use windows_sys::core::{HRESULT, PCWSTR};
use zeroize::Zeroizing;

use crate::hardware_key::HardwareKey;
use crate::hardware_key_store::HardwareKeyStore;
use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;

pub(crate) struct CngKeyStore {
    name: String,
}

/// `tpm:<name>`: a key in the Microsoft Platform Crypto Provider (TPM 2.0).
pub(crate) struct PlatformKeyStore {
    name: String,
}

/// A TPM key, open in the Platform Crypto Provider.
pub(crate) struct PlatformKey {
    provider: CngHandle,
    key: CngHandle,
    public_key: PublicKey,
    reference: String,
}

/// A CNG handle, freed exactly once when it drops.
struct CngHandle(usize);

impl Drop for CngHandle {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the handle came from a successful NCryptOpenStorageProvider,
            // NCryptImportKey, NCryptOpenKey, NCryptCreatePersistedKey or
            // NCryptSecretAgreement, is owned by this value alone, and is freed
            // here once.
            let _ = unsafe { NCryptFreeObject(self.0) };
        }
    }
}

impl CngKeyStore {
    pub(crate) fn new(name: String) -> CngKeyStore {
        CngKeyStore { name }
    }

    fn reference(&self) -> String {
        format!("cng:{}", self.name)
    }

    fn provider(&self) -> Result<CngHandle, IdentityError> {
        let (provider, status) = CngHandle::open_provider(MS_KEY_STORAGE_PROVIDER);
        self.check(status)?;
        Ok(provider)
    }

    fn import(&self, provider: &CngHandle, der: &[u8]) -> Result<CngHandle, IdentityError> {
        let mut name = CngHandle::wide(&self.name);
        let mut buffer = BCryptBuffer {
            cbBuffer: CngHandle::length(name.len() * 2)?,
            BufferType: NCRYPTBUFFER_PKCS_KEY_NAME,
            pvBuffer: name.as_mut_ptr().cast(),
        };
        let parameters = BCryptBufferDesc {
            ulVersion: NCRYPTBUFFER_VERSION,
            cBuffers: 1,
            pBuffers: &raw mut buffer,
        };
        let mut key: NCRYPT_KEY_HANDLE = 0;
        // SAFETY: the provider handle is live; `parameters` points at one
        // initialized buffer holding the NUL-terminated key name, and both
        // outlive the call; `der` is read for exactly its length; `key` is a
        // live, exclusively borrowed handle slot. CNG copies what it keeps.
        let status = unsafe {
            NCryptImportKey(
                provider.0,
                0,
                NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
                &raw const parameters,
                &raw mut key,
                der.as_ptr(),
                CngHandle::length(der.len())?,
                NCRYPT_DO_NOT_FINALIZE_FLAG,
            )
        };
        self.check(status)?;
        Ok(CngHandle(key))
    }

    fn allow_export(&self, key: &CngHandle) -> Result<(), IdentityError> {
        let policy: u32 = NCRYPT_ALLOW_EXPORT_FLAG | NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG;
        self.check(key.set_flags(NCRYPT_EXPORT_POLICY_PROPERTY, policy))
    }

    fn finalize(&self, key: &CngHandle) -> Result<(), IdentityError> {
        self.check(key.finalize())
    }

    fn open(&self, provider: &CngHandle) -> Result<CngHandle, IdentityError> {
        let (key, status) = provider.open_key(&self.name);
        self.check(status)?;
        Ok(key)
    }

    fn export(&self, key: &CngHandle) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let mut length = 0_u32;
        // SAFETY: the key handle is live; a null output with zero capacity asks
        // only for the size, written to the live `length`.
        let status = unsafe {
            NCryptExportKey(
                key.0,
                0,
                NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
                ptr::null(),
                ptr::null_mut(),
                0,
                &raw mut length,
                0,
            )
        };
        self.check(status)?;
        let mut der = Zeroizing::new(vec![0_u8; length as usize]);
        // SAFETY: the key handle is live; `der` is an exclusively borrowed
        // buffer of exactly `length` bytes, the size CNG asked for; CNG writes
        // at most that many and reports how many in `length`.
        let status = unsafe {
            NCryptExportKey(
                key.0,
                0,
                NCRYPT_PKCS8_PRIVATE_KEY_BLOB,
                ptr::null(),
                der.as_mut_ptr(),
                length,
                &raw mut length,
                0,
            )
        };
        self.check(status)?;
        der.truncate(length as usize);
        Ok(der)
    }

    /// The Microsoft Software Key Storage Provider replaces a named key on
    /// import instead of failing with `NTE_EXISTS`, so `save` looks first.
    fn refuse_existing(&self, provider: &CngHandle) -> Result<(), IdentityError> {
        match self.open(provider) {
            Ok(_) => Err(self.already_exists()),
            Err(IdentityError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn already_exists(&self) -> IdentityError {
        IdentityError::AlreadyExists(format!(
            "{} already has a key; FYLO never replaces a key",
            self.reference()
        ))
    }

    fn check(&self, status: HRESULT) -> Result<(), IdentityError> {
        match status {
            0 => Ok(()),
            NTE_BAD_KEYSET => Err(IdentityError::NotFound(format!(
                "{} has no key",
                self.reference()
            ))),
            NTE_EXISTS => Err(self.already_exists()),
            _ => Err(IdentityError::Store(format!(
                "{}: CNG failed with {status:#010x}",
                self.reference()
            ))),
        }
    }
}

impl CngHandle {
    /// `text` as a NUL-terminated UTF-16 string.
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn length(bytes: usize) -> Result<u32, IdentityError> {
        u32::try_from(bytes)
            .map_err(|_| IdentityError::InvalidKey("the key is too large for CNG".into()))
    }

    /// Open the key storage provider `name`; the handle is null on failure.
    fn open_provider(name: PCWSTR) -> (CngHandle, HRESULT) {
        let mut provider: NCRYPT_PROV_HANDLE = 0;
        // SAFETY: `provider` is a live, exclusively borrowed handle slot; the
        // provider name is a static NUL-terminated string from windows-sys.
        let status = unsafe { NCryptOpenStorageProvider(&raw mut provider, name, 0) };
        (CngHandle(provider), status)
    }

    /// Open the persisted key `name` of this provider.
    fn open_key(&self, name: &str) -> (CngHandle, HRESULT) {
        let name = Self::wide(name);
        let mut key: NCRYPT_KEY_HANDLE = 0;
        // SAFETY: the provider handle is live; the key name is NUL-terminated
        // and outlives the call; `key` is a live, exclusively borrowed slot.
        let status = unsafe { NCryptOpenKey(self.0, &raw mut key, name.as_ptr(), 0, 0) };
        (CngHandle(key), status)
    }

    /// Start a new persisted P-256 key `name` in this provider, not yet
    /// finalized.
    fn create_key(&self, name: &str) -> (CngHandle, HRESULT) {
        let name = Self::wide(name);
        let mut key: NCRYPT_KEY_HANDLE = 0;
        // SAFETY: the provider handle is live; the algorithm is a static
        // NUL-terminated string and the key name a NUL-terminated buffer that
        // outlives the call; `key` is a live, exclusively borrowed slot.
        let status = unsafe {
            NCryptCreatePersistedKey(
                self.0,
                &raw mut key,
                NCRYPT_ECDH_P256_ALGORITHM,
                name.as_ptr(),
                0,
                0,
            )
        };
        (CngHandle(key), status)
    }

    /// Set the four-byte flags property `property` of this unfinalized key.
    fn set_flags(&self, property: PCWSTR, flags: u32) -> HRESULT {
        // SAFETY: the key handle is live and not yet finalized; the property
        // name is a static NUL-terminated string; the input is the four bytes
        // of `flags`, which outlives the call.
        unsafe { NCryptSetProperty(self.0, property, (&raw const flags).cast(), 4, 0) }
    }

    fn finalize(&self) -> HRESULT {
        // SAFETY: the key handle is live and not yet finalized.
        unsafe { NCryptFinalizeKey(self.0, 0) }
    }

    /// Export this key's public half as `BCRYPT_ECCPUBLIC_BLOB` into
    /// `blob`, which has room for a P-256 one; returns the bytes written.
    fn export_public(&self, blob: &mut [u8; PlatformKey::PUBLIC_BLOB_BYTES]) -> (u32, HRESULT) {
        let mut written = 0_u32;
        // SAFETY: the key handle is live; `blob` is an exclusively borrowed
        // buffer of exactly the length passed; CNG writes at most that many
        // bytes and reports how many in the live `written`.
        let status = unsafe {
            NCryptExportKey(
                self.0,
                0,
                BCRYPT_ECCPUBLIC_BLOB,
                ptr::null(),
                blob.as_mut_ptr(),
                PlatformKey::PUBLIC_BLOB_LENGTH,
                &raw mut written,
                0,
            )
        };
        (written, status)
    }

    /// Import a public key blob into this provider, for an agreement.
    fn import_public(&self, blob: &[u8; PlatformKey::PUBLIC_BLOB_BYTES]) -> (CngHandle, HRESULT) {
        let mut key: NCRYPT_KEY_HANDLE = 0;
        // SAFETY: the provider handle is live; the blob type is a static
        // NUL-terminated string; `blob` is read for exactly its length; `key`
        // is a live, exclusively borrowed slot.
        let status = unsafe {
            NCryptImportKey(
                self.0,
                0,
                BCRYPT_ECCPUBLIC_BLOB,
                ptr::null(),
                &raw mut key,
                blob.as_ptr(),
                PlatformKey::PUBLIC_BLOB_LENGTH,
                0,
            )
        };
        (CngHandle(key), status)
    }

    /// Sign the 32-byte `digest` with this key: 64 bytes, `r` then `s`.
    fn sign_digest(&self, digest: &[u8; 32], signature: &mut [u8; 64]) -> (u32, HRESULT) {
        let mut written = 0_u32;
        // SAFETY: the key handle is live; ECDSA takes no padding information,
        // so it is null; `digest` is read for exactly 32 bytes; `signature` is
        // an exclusively borrowed 64-byte buffer, the size passed, and CNG
        // reports how many bytes it wrote in the live `written`.
        let status = unsafe {
            NCryptSignHash(
                self.0,
                ptr::null(),
                digest.as_ptr(),
                32,
                signature.as_mut_ptr(),
                64,
                &raw mut written,
                0,
            )
        };
        (written, status)
    }

    /// Agree a secret between this private key and `peer`.
    fn agree_with(&self, peer: &CngHandle) -> (CngHandle, HRESULT) {
        let mut secret: NCRYPT_SECRET_HANDLE = 0;
        // SAFETY: both key handles are live and belong to the same provider;
        // `secret` is a live, exclusively borrowed handle slot.
        let status = unsafe { NCryptSecretAgreement(self.0, peer.0, &raw mut secret, 0) };
        (CngHandle(secret), status)
    }

    /// This agreed secret's raw bytes, little-endian, into `raw`.
    fn raw_secret(&self, raw: &mut [u8; 32]) -> (u32, HRESULT) {
        let mut written = 0_u32;
        // SAFETY: the secret handle is live; the KDF name is a static
        // NUL-terminated string and takes no parameters; `raw` is an
        // exclusively borrowed 32-byte buffer, the size passed, and CNG
        // reports how many bytes it wrote in the live `written`.
        let status = unsafe {
            NCryptDeriveKey(
                self.0,
                BCRYPT_KDF_RAW_SECRET,
                ptr::null(),
                raw.as_mut_ptr(),
                32,
                &raw mut written,
                0,
            )
        };
        (written, status)
    }
}

impl PlatformKeyStore {
    /// CNG statuses that mean this machine has no usable TPM.
    const NO_TPM: [HRESULT; 6] = [
        TBS_E_TPM_NOT_FOUND,
        TBS_E_SERVICE_NOT_RUNNING,
        TPM_E_PCP_DEVICE_NOT_READY,
        NTE_DEVICE_NOT_FOUND,
        NTE_DEVICE_NOT_READY,
        NTE_PROVIDER_DLL_FAIL,
    ];

    pub(crate) fn new(name: String) -> PlatformKeyStore {
        PlatformKeyStore { name }
    }

    fn reference(&self) -> String {
        format!("tpm:{}", self.name)
    }

    fn provider(&self) -> Result<CngHandle, IdentityError> {
        let (provider, status) = CngHandle::open_provider(MS_PLATFORM_CRYPTO_PROVIDER);
        self.check(status)?;
        Ok(provider)
    }

    fn refuse_existing(&self, provider: &CngHandle) -> Result<(), IdentityError> {
        let (_key, status) = provider.open_key(&self.name);
        match self.check(status) {
            Ok(()) => Err(self.already_exists()),
            Err(IdentityError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn new_key(&self, provider: &CngHandle) -> Result<CngHandle, IdentityError> {
        let (key, status) = provider.create_key(&self.name);
        self.check(status)?;
        let usage = NCRYPT_ALLOW_SIGNING_FLAG | NCRYPT_ALLOW_KEY_AGREEMENT_FLAG;
        self.check(key.set_flags(NCRYPT_KEY_USAGE_PROPERTY, usage))?;
        self.check(key.finalize())?;
        Ok(key)
    }

    fn already_exists(&self) -> IdentityError {
        IdentityError::AlreadyExists(format!(
            "{} already has a key; FYLO never replaces a key",
            self.reference()
        ))
    }

    fn check(&self, status: HRESULT) -> Result<(), IdentityError> {
        match status {
            0 => Ok(()),
            NTE_BAD_KEYSET => Err(IdentityError::NotFound(format!(
                "{} has no key",
                self.reference()
            ))),
            NTE_EXISTS => Err(self.already_exists()),
            _ if Self::NO_TPM.contains(&status) => Err(IdentityError::Unsupported(format!(
                "{} needs a TPM 2.0, and no TPM is available on this machine (CNG {status:#010x}); use cng:<name> for a software key",
                self.reference()
            ))),
            _ => Err(IdentityError::Store(format!(
                "{}: the TPM failed with {status:#010x}",
                self.reference()
            ))),
        }
    }
}

impl HardwareKeyStore for PlatformKeyStore {
    fn create(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        let provider = self.provider()?;
        self.refuse_existing(&provider)?;
        let key = self.new_key(&provider)?;
        Ok(Box::new(PlatformKey::new(provider, key, self.reference())?))
    }

    fn open(&self) -> Result<Box<dyn HardwareKey>, IdentityError> {
        let provider = self.provider()?;
        let (key, status) = provider.open_key(&self.name);
        self.check(status)?;
        Ok(Box::new(PlatformKey::new(provider, key, self.reference())?))
    }
}

impl PlatformKey {
    /// `BCRYPT_ECCKEY_BLOB` (magic and key length, 4 bytes each) and the
    /// point's X and Y, 32 bytes each, big-endian.
    const PUBLIC_BLOB_BYTES: usize = 8 + 32 + 32;
    const PUBLIC_BLOB_LENGTH: u32 = 8 + 32 + 32;
    const COORDINATE_BYTES: u32 = 32;
    const SEC1_UNCOMPRESSED: u8 = 4;

    fn new(
        provider: CngHandle,
        key: CngHandle,
        reference: String,
    ) -> Result<PlatformKey, IdentityError> {
        let mut blob = [0_u8; Self::PUBLIC_BLOB_BYTES];
        let (written, status) = key.export_public(&mut blob);
        Self::checked(&reference, status)?;
        let public_key = Self::public_key_of(&blob, written)
            .ok_or_else(|| IdentityError::InvalidKey(format!("{reference} is not a P-256 key")))?;
        Ok(PlatformKey {
            provider,
            key,
            public_key,
            reference,
        })
    }

    /// The P-256 key a `BCRYPT_ECCPUBLIC_BLOB` holds.
    fn public_key_of(blob: &[u8; Self::PUBLIC_BLOB_BYTES], written: u32) -> Option<PublicKey> {
        let (header, point) = blob.split_first_chunk::<8>()?;
        let length = u32::from_le_bytes(header[4..].try_into().ok()?);
        let whole = written == Self::PUBLIC_BLOB_LENGTH;
        let sec1 = [&[Self::SEC1_UNCOMPRESSED][..], point].concat();
        (whole && length == Self::COORDINATE_BYTES)
            .then(|| PublicKey::from_sec1_bytes(&sec1).ok())
            .flatten()
    }

    /// `peer` as a `BCRYPT_ECCPUBLIC_BLOB` for ECDH.
    fn public_blob(peer: &PublicKey) -> [u8; Self::PUBLIC_BLOB_BYTES] {
        use p256::elliptic_curve::sec1::ToSec1Point as _;
        let point = peer.to_sec1_point(false);
        let mut blob = [0_u8; Self::PUBLIC_BLOB_BYTES];
        blob[..4].copy_from_slice(&BCRYPT_ECDH_PUBLIC_P256_MAGIC.to_le_bytes());
        blob[4..8].copy_from_slice(&Self::COORDINATE_BYTES.to_le_bytes());
        blob[8..].copy_from_slice(&point.as_bytes()[1..]);
        blob
    }

    /// CNG's raw secret is little-endian; ECDH's x-coordinate is big-endian.
    fn big_endian(raw: &[u8; 32]) -> Zeroizing<[u8; 32]> {
        let mut shared = Zeroizing::new(*raw);
        shared.reverse();
        shared
    }

    fn checked(reference: &str, status: HRESULT) -> Result<(), IdentityError> {
        match status {
            0 => Ok(()),
            _ => Err(IdentityError::Store(format!(
                "{reference}: the TPM failed with {status:#010x}"
            ))),
        }
    }

    fn whole(&self, written: u32, expected: u32) -> Result<(), IdentityError> {
        (written == expected).then_some(()).ok_or_else(|| {
            IdentityError::Store(format!(
                "{}: the TPM gave {written} bytes where {expected} were due",
                self.reference
            ))
        })
    }
}

impl HardwareKey for PlatformKey {
    fn public_key(&self) -> PublicKey {
        self.public_key
    }

    fn sign(&self, message: &[u8]) -> Result<[u8; 64], IdentityError> {
        let digest: [u8; 32] = Sha256::digest(message).into();
        let mut signature = [0_u8; 64];
        let (written, status) = self.key.sign_digest(&digest, &mut signature);
        Self::checked(&self.reference, status)?;
        self.whole(written, 64)?;
        Ok(signature)
    }

    fn agree(&self, peer: &PublicKey) -> Result<Zeroizing<[u8; 32]>, IdentityError> {
        let (peer, status) = self.provider.import_public(&Self::public_blob(peer));
        Self::checked(&self.reference, status)?;
        let (secret, status) = self.key.agree_with(&peer);
        Self::checked(&self.reference, status)?;
        let mut raw = Zeroizing::new([0_u8; 32]);
        let (written, status) = secret.raw_secret(&mut raw);
        Self::checked(&self.reference, status)?;
        self.whole(written, 32)?;
        Ok(Self::big_endian(&raw))
    }
}

impl KeyStore for CngKeyStore {
    fn save(&self, secret_key: &SecretKey) -> Result<(), IdentityError> {
        let document = secret_key.to_pkcs8_der().map_err(|error| {
            IdentityError::InvalidKey(format!("cannot encode the key: {error}"))
        })?;
        let provider = self.provider()?;
        self.refuse_existing(&provider)?;
        let key = self.import(&provider, document.as_bytes())?;
        self.allow_export(&key)?;
        self.finalize(&key)
    }

    fn load(&self) -> Result<SecretKey, IdentityError> {
        let provider = self.provider()?;
        let key = self.open(&provider)?;
        let der = self.export(&key)?;
        SecretKey::from_pkcs8_der(&der).map_err(|error| {
            IdentityError::InvalidKey(format!(
                "{} is not a P-256 private key: {error}",
                self.reference()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use p256::SecretKey;
    use p256::elliptic_curve::sec1::ToSec1Point as _;

    use super::PlatformKey;
    use crate::key_reference::KeyReference;

    fn public(byte: u8) -> p256::PublicKey {
        SecretKey::from_slice(&[byte; 32]).unwrap().public_key()
    }

    #[test]
    fn a_public_blob_holds_the_point_cng_expects() {
        let blob = PlatformKey::public_blob(&public(3));
        assert_eq!(&blob[..4], b"ECK1");
        assert_eq!(&blob[4..8], &32_u32.to_le_bytes());
        assert_eq!(&blob[8..], &public(3).to_sec1_point(false).as_bytes()[1..]);
    }

    #[test]
    fn a_public_blob_reads_back_as_its_key() {
        let blob = PlatformKey::public_blob(&public(5));
        let read = PlatformKey::public_key_of(&blob, 72).unwrap();
        assert_eq!(read, public(5));
    }

    #[test]
    fn a_short_public_blob_is_refused() {
        let blob = PlatformKey::public_blob(&public(5));
        assert!(PlatformKey::public_key_of(&blob, 40).is_none());
    }

    #[test]
    fn the_raw_secret_is_turned_big_endian() {
        let raw: [u8; 32] = std::array::from_fn(|index| u8::try_from(index).unwrap());
        let shared = PlatformKey::big_endian(&raw);
        assert_eq!(shared[0], 31);
        assert_eq!(shared[31], 0);
    }

    #[test]
    #[ignore = "needs a Windows machine without a TPM, such as windows-vm"]
    fn without_a_tpm_create_says_so() {
        let reference = KeyReference::Tpm(format!("fylo-test-{}", std::process::id()));
        let error = crate::Identity::create(&reference).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNSUPPORTED", "{error}");
        assert!(error.to_string().contains("no TPM"), "{error}");
    }

    #[test]
    #[ignore = "needs a Windows machine without a TPM, such as windows-vm"]
    fn without_a_tpm_show_says_so() {
        let reference = KeyReference::Tpm(format!("fylo-test-show-{}", std::process::id()));
        let error = crate::Identity::open(&reference).unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_UNSUPPORTED", "{error}");
    }
}
