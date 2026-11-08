//! FYLO application identities (ADR 0021).
//!
//! An identity is a P-256 key pair. Its static identifier is the fingerprint
//! of its public key, `fid1:p256:<base32 of SHA-256>`. A process names its
//! private key by a [`KeyReference`] (`file:`, `keychain:`, `enclave:`,
//! `cng:`, `creds:` or `webcrypto:`) and never by the key itself.

mod base32;
#[cfg(windows)]
mod cng_key_store;
#[cfg(target_os = "linux")]
mod credential_key_store;
#[cfg(target_os = "macos")]
mod enclave_key;
#[cfg(target_os = "macos")]
mod enclave_key_store;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod file_key_store;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod hardware_key;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod hardware_key_store;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod hardware_key_wrap;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod hardware_private_key;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod identity;
mod identity_error;
mod identity_id;
mod key_reference;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod key_store;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod key_wrap;
#[cfg(target_os = "macos")]
mod keychain_item;
#[cfg(target_os = "macos")]
mod keychain_key_store;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod private_key;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod private_key_pem;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod public_identity;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod session_key;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod software_key;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod unsupported_key_store;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod identity_browser;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod public_identity_browser;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod session_key_browser;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use identity::Identity;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use identity_browser::Identity;
pub use identity_error::IdentityError;
pub use identity_id::IdentityId;
pub use key_reference::KeyReference;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use public_identity::PublicIdentity;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use public_identity_browser::PublicIdentity;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use session_key::SessionKey;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use session_key_browser::SessionKey;
