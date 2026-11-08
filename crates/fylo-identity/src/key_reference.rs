use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use crate::file_key_store::FileKeyStore;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use crate::hardware_key_store::HardwareKeyStore;
use crate::identity_error::IdentityError;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use crate::key_store::KeyStore;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use crate::unsupported_key_store::UnsupportedKeyStore;

/// Where an identity's private key lives, named without the key itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyReference {
    /// `file:<path>`: a PKCS#8 PEM file only the process's user can read.
    File(PathBuf),
    /// `keychain:<label>`: a macOS keychain item.
    Keychain(String),
    /// `enclave:<label>`: a macOS Secure Enclave key that never leaves the
    /// enclave (phase 8).
    Enclave(String),
    /// `cng:<name>`: a Windows CNG key.
    Cng(String),
    /// `tpm:<name>`: a Windows TPM 2.0 key that never leaves the TPM
    /// (phase 8).
    Tpm(String),
    /// `creds:<name>`: a `systemd-creds` credential of a Linux service.
    Creds(String),
    /// `webcrypto:<name>`: a non-extractable `CryptoKey` in the browser.
    WebCrypto(String),
}

impl KeyReference {
    const FILE: &'static str = "file";
    const KEYCHAIN: &'static str = "keychain";
    const ENCLAVE: &'static str = "enclave";
    const CNG: &'static str = "cng";
    const TPM: &'static str = "tpm";
    const CREDS: &'static str = "creds";
    const WEB_CRYPTO: &'static str = "webcrypto";

    /// Parse `<store>:<name>`.
    ///
    /// # Errors
    /// [`IdentityError::InvalidReference`] for an unknown store, an empty
    /// name, or a store name that is not letters, digits, `.`, `_` and `-`.
    pub fn parse(text: &str) -> Result<KeyReference, IdentityError> {
        let (store, name) = Self::split(text)?;
        Self::from_parts(store, name)
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn store(&self) -> Box<dyn KeyStore> {
        match self {
            Self::File(path) => Box::new(FileKeyStore::new(path.clone())),
            Self::Keychain(label) => Self::keychain_store(label),
            Self::Enclave(label) => Self::hardware_only(&format!("enclave:{label}")),
            Self::Cng(name) => Self::cng_store(name),
            Self::Tpm(name) => Self::hardware_only(&format!("tpm:{name}")),
            Self::Creds(name) => Self::credential_store(name),
            Self::WebCrypto(name) => Box::new(UnsupportedKeyStore::new(format!(
                "webcrypto:{name} keys live in a browser origin's IndexedDB; only FYLO's browser runtime opens them"
            ))),
        }
    }

    /// The hardware store a hardware reference names; `None` for the
    /// software stores, whose keys [`Self::store`] reads.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn hardware_store(&self) -> Option<Box<dyn HardwareKeyStore>> {
        match self {
            Self::Enclave(label) => Some(Self::enclave_store(label)),
            Self::Tpm(name) => Some(Self::platform_store(name)),
            _ => None,
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    fn hardware_only(reference: &str) -> Box<dyn KeyStore> {
        Box::new(UnsupportedKeyStore::new(format!(
            "{reference} is a hardware key; it is made and used where it lives, never read"
        )))
    }

    fn split(text: &str) -> Result<(&str, &str), IdentityError> {
        text.split_once(':')
            .filter(|(_, name)| !name.is_empty())
            .ok_or_else(|| Self::invalid(text))
    }

    fn from_parts(store: &str, name: &str) -> Result<KeyReference, IdentityError> {
        match store {
            Self::FILE => Ok(Self::File(PathBuf::from(name))),
            Self::KEYCHAIN => Self::checked_name(name).map(Self::Keychain),
            Self::ENCLAVE => Self::checked_name(name).map(Self::Enclave),
            Self::CNG => Self::checked_name(name).map(Self::Cng),
            Self::TPM => Self::checked_name(name).map(Self::Tpm),
            Self::CREDS => Self::checked_name(name).map(Self::Creds),
            Self::WEB_CRYPTO => Self::checked_name(name).map(Self::WebCrypto),
            _ => Err(Self::invalid(&format!("{store}:{name}"))),
        }
    }

    fn checked_name(name: &str) -> Result<String, IdentityError> {
        Some(name)
            .filter(|name| Self::is_store_name(name))
            .map(str::to_owned)
            .ok_or_else(|| {
                IdentityError::InvalidReference(format!(
                    "{name:?} is not a key name: use letters, digits, '.', '_' and '-', and not '.' or '..'"
                ))
            })
    }

    fn is_store_name(name: &str) -> bool {
        name.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            && name != "."
            && name != ".."
    }

    fn invalid(text: &str) -> IdentityError {
        IdentityError::InvalidReference(format!(
            "{text:?} is not a key reference: use file:<path>, keychain:<label>, enclave:<label>, cng:<name>, tpm:<name>, creds:<name> or webcrypto:<name>"
        ))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(target_os = "macos")]
    fn keychain_store(label: &str) -> Box<dyn KeyStore> {
        Box::new(crate::keychain_key_store::KeychainKeyStore::new(
            label.to_owned(),
        ))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(not(target_os = "macos"))]
    fn keychain_store(label: &str) -> Box<dyn KeyStore> {
        Box::new(UnsupportedKeyStore::new(format!(
            "keychain:{label} needs macOS; use file:<path>, or cng:<name> on Windows and creds:<name> for a Linux service"
        )))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(target_os = "macos")]
    fn enclave_store(label: &str) -> Box<dyn HardwareKeyStore> {
        Box::new(crate::enclave_key_store::EnclaveKeyStore::new(
            label.to_owned(),
        ))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(not(target_os = "macos"))]
    fn enclave_store(label: &str) -> Box<dyn HardwareKeyStore> {
        Box::new(UnsupportedKeyStore::new(format!(
            "enclave:{label} needs a Mac with a Secure Enclave; use tpm:<name> on Windows, or a software store"
        )))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(target_os = "linux")]
    fn credential_store(name: &str) -> Box<dyn KeyStore> {
        Box::new(crate::credential_key_store::CredentialKeyStore::new(
            name.to_owned(),
        ))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(not(target_os = "linux"))]
    fn credential_store(name: &str) -> Box<dyn KeyStore> {
        Box::new(UnsupportedKeyStore::new(format!(
            "creds:{name} needs Linux and systemd; use file:<path> or keychain:<label> here"
        )))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(windows)]
    fn platform_store(name: &str) -> Box<dyn HardwareKeyStore> {
        Box::new(crate::cng_key_store::PlatformKeyStore::new(name.to_owned()))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(not(windows))]
    fn platform_store(name: &str) -> Box<dyn HardwareKeyStore> {
        Box::new(UnsupportedKeyStore::new(format!(
            "tpm:{name} needs Windows with a TPM 2.0; use enclave:<label> on a Mac, or a software store"
        )))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(windows)]
    fn cng_store(name: &str) -> Box<dyn KeyStore> {
        Box::new(crate::cng_key_store::CngKeyStore::new(name.to_owned()))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[cfg(not(windows))]
    fn cng_store(name: &str) -> Box<dyn KeyStore> {
        Box::new(UnsupportedKeyStore::new(format!(
            "cng:{name} needs Windows; use file:<path> or keychain:<label> here"
        )))
    }
}

impl fmt::Display for KeyReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(formatter, "{}:{}", Self::FILE, path.display()),
            Self::Keychain(label) => write!(formatter, "{}:{label}", Self::KEYCHAIN),
            Self::Enclave(label) => write!(formatter, "{}:{label}", Self::ENCLAVE),
            Self::Cng(name) => write!(formatter, "{}:{name}", Self::CNG),
            Self::Tpm(name) => write!(formatter, "{}:{name}", Self::TPM),
            Self::Creds(name) => write!(formatter, "{}:{name}", Self::CREDS),
            Self::WebCrypto(name) => write!(formatter, "{}:{name}", Self::WEB_CRYPTO),
        }
    }
}

impl FromStr for KeyReference {
    type Err = IdentityError;

    fn from_str(text: &str) -> Result<KeyReference, IdentityError> {
        KeyReference::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::KeyReference;

    #[test]
    fn parses_every_store() {
        assert_eq!(
            KeyReference::parse("file:/keys/yon.pem").unwrap(),
            KeyReference::File(PathBuf::from("/keys/yon.pem"))
        );
        assert_eq!(
            KeyReference::parse("keychain:com.delma.yon").unwrap(),
            KeyReference::Keychain("com.delma.yon".into())
        );
        assert_eq!(
            KeyReference::parse("enclave:com.delma.yon").unwrap(),
            KeyReference::Enclave("com.delma.yon".into())
        );
        assert_eq!(
            KeyReference::parse("cng:yon").unwrap(),
            KeyReference::Cng("yon".into())
        );
        assert_eq!(
            KeyReference::parse("tpm:yon").unwrap(),
            KeyReference::Tpm("yon".into())
        );
        assert_eq!(
            KeyReference::parse("creds:fylo-yon").unwrap(),
            KeyReference::Creds("fylo-yon".into())
        );
        assert_eq!(
            KeyReference::parse("webcrypto:yon_1").unwrap(),
            KeyReference::WebCrypto("yon_1".into())
        );
    }

    #[test]
    fn a_file_path_keeps_its_colons() {
        assert_eq!(
            KeyReference::parse("file:C:/keys/yon.pem").unwrap(),
            KeyReference::File(PathBuf::from("C:/keys/yon.pem"))
        );
    }

    #[test]
    fn prints_the_text_it_parsed() {
        let text = "keychain:com.delma.yon";
        assert_eq!(KeyReference::parse(text).unwrap().to_string(), text);
    }

    #[test]
    fn refuses_an_unknown_store() {
        let error = KeyReference::parse("vault:yon").unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_REFERENCE");
    }

    #[test]
    fn refuses_a_missing_name() {
        let error = KeyReference::parse("file:").unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_REFERENCE");
    }

    #[test]
    fn refuses_text_without_a_store() {
        let error = KeyReference::parse("yon").unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_REFERENCE");
    }

    #[test]
    fn refuses_a_store_name_with_a_slash() {
        let error = KeyReference::parse("creds:../etc/passwd").unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_REFERENCE");
    }

    #[test]
    fn refuses_a_dot_dot_store_name() {
        let error = KeyReference::parse("creds:..").unwrap_err();
        assert_eq!(error.code(), "EIDENTITY_REFERENCE");
    }
}
