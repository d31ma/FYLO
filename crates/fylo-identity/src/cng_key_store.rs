//! `cng:<name>` on Windows (ADR 0021, decision P5-2): a P-256 key persisted
//! under `name` in the Microsoft Software Key Storage Provider, which keeps
//! it encrypted with the user's DPAPI key. Version 1 signs in software
//! (Q8), so the key is imported with an export policy that lets FYLO read it
//! back as PKCS#8; non-exportable and TPM keys wait for phase 8.
//!
//! This is the crate's one `unsafe` module: CNG is a C API, reached through
//! Microsoft's `windows-sys` declarations. Every call is listed in
//! `docs/security/unsafe-inventory.md`.
#![allow(unsafe_code)]

use std::ptr;

use p256::SecretKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use windows_sys::Win32::Foundation::{NTE_BAD_KEYSET, NTE_EXISTS};
use windows_sys::Win32::Security::Cryptography::{
    BCryptBuffer, BCryptBufferDesc, MS_KEY_STORAGE_PROVIDER, NCRYPT_ALLOW_EXPORT_FLAG,
    NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG, NCRYPT_DO_NOT_FINALIZE_FLAG, NCRYPT_EXPORT_POLICY_PROPERTY,
    NCRYPT_KEY_HANDLE, NCRYPT_PKCS8_PRIVATE_KEY_BLOB, NCRYPT_PROV_HANDLE,
    NCRYPTBUFFER_PKCS_KEY_NAME, NCRYPTBUFFER_VERSION, NCryptExportKey, NCryptFinalizeKey,
    NCryptFreeObject, NCryptImportKey, NCryptOpenKey, NCryptOpenStorageProvider, NCryptSetProperty,
};
use windows_sys::core::HRESULT;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;

pub(crate) struct CngKeyStore {
    name: String,
}

/// A CNG handle, freed exactly once when it drops.
struct CngHandle(usize);

impl Drop for CngHandle {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the handle came from a successful NCryptOpenStorageProvider,
            // NCryptImportKey or NCryptOpenKey, is owned by this value alone, and
            // is freed here once.
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

    /// `text` as a NUL-terminated UTF-16 string.
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn provider(&self) -> Result<CngHandle, IdentityError> {
        let mut provider: NCRYPT_PROV_HANDLE = 0;
        // SAFETY: `provider` is a live, exclusively borrowed handle slot; the
        // provider name is a static NUL-terminated string from windows-sys.
        let status =
            unsafe { NCryptOpenStorageProvider(&raw mut provider, MS_KEY_STORAGE_PROVIDER, 0) };
        self.check(status)?;
        Ok(CngHandle(provider))
    }

    fn import(&self, provider: &CngHandle, der: &[u8]) -> Result<CngHandle, IdentityError> {
        let mut name = Self::wide(&self.name);
        let mut buffer = BCryptBuffer {
            cbBuffer: Self::length(name.len() * 2)?,
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
                Self::length(der.len())?,
                NCRYPT_DO_NOT_FINALIZE_FLAG,
            )
        };
        self.check(status)?;
        Ok(CngHandle(key))
    }

    fn allow_export(&self, key: &CngHandle) -> Result<(), IdentityError> {
        let policy: u32 = NCRYPT_ALLOW_EXPORT_FLAG | NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG;
        // SAFETY: the key handle is live and not yet finalized; the property
        // name is a static NUL-terminated string; the input is the four bytes
        // of `policy`, which outlives the call.
        let status = unsafe {
            NCryptSetProperty(
                key.0,
                NCRYPT_EXPORT_POLICY_PROPERTY,
                (&raw const policy).cast(),
                4,
                0,
            )
        };
        self.check(status)
    }

    fn finalize(&self, key: &CngHandle) -> Result<(), IdentityError> {
        // SAFETY: the key handle is live and not yet finalized.
        let status = unsafe { NCryptFinalizeKey(key.0, 0) };
        self.check(status)
    }

    fn open(&self, provider: &CngHandle) -> Result<CngHandle, IdentityError> {
        let name = Self::wide(&self.name);
        let mut key: NCRYPT_KEY_HANDLE = 0;
        // SAFETY: the provider handle is live; the key name is NUL-terminated
        // and outlives the call; `key` is a live, exclusively borrowed slot.
        let status = unsafe { NCryptOpenKey(provider.0, &raw mut key, name.as_ptr(), 0, 0) };
        self.check(status)?;
        Ok(CngHandle(key))
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

    fn length(bytes: usize) -> Result<u32, IdentityError> {
        u32::try_from(bytes)
            .map_err(|_| IdentityError::InvalidKey("the key is too large for CNG".into()))
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
