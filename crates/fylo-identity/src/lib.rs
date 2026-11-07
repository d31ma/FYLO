//! FYLO application identities (ADR 0021).
//!
//! An identity is a P-256 key pair. Its static identifier is the fingerprint
//! of its public key, `fid1:p256:<base32 of SHA-256>`. A process names its
//! private key by a [`KeyReference`] (`file:`, `keychain:`, `cng:`, `creds:`
//! or `webcrypto:`) and never by the key itself.

mod base32;
#[cfg(target_os = "linux")]
mod credential_key_store;
mod file_key_store;
mod identity;
mod identity_error;
mod identity_id;
mod key_reference;
mod key_store;
mod key_wrap;
#[cfg(target_os = "macos")]
mod keychain_key_store;
mod private_key_pem;
mod public_identity;
mod unsupported_key_store;

pub use identity::Identity;
pub use identity_error::IdentityError;
pub use identity_id::IdentityId;
pub use key_reference::KeyReference;
pub use public_identity::PublicIdentity;
