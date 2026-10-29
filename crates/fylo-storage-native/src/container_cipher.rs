//! Opt-in encryption of a `.vdbx` file (ADR 0016 section 9). Off by default.
//!
//! The file key is derived with HKDF-SHA256 from the configured secret and a
//! random 32-byte salt kept in the superblock. A key-check value, derived
//! alongside it but revealing nothing about it, is kept there too, so a wrong
//! secret is reported at open instead of as damage. Every sealed unit takes a
//! fresh random 96-bit nonce and AES-256-GCM, with associated data that binds
//! it to where it belongs: a record to its collection, LSN and entry index.
//! Bodies are compressed before they are sealed.

use std::fmt;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use fylo_format::ContainerExtentKind;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Bytes of the nonce at the start of a sealed unit.
pub const NONCE_BYTES: usize = 12;
/// Bytes of the authentication tag at its end.
pub const TAG_BYTES: usize = 16;
/// Bytes of the superblock's salt.
pub const SALT_BYTES: usize = 32;
/// Bytes of the superblock's key-check value.
pub const KEY_CHECK_BYTES: usize = 16;

const FILE_KEY_INFO: &[u8] = b"fylo.vdbx.file-key.v1";
const KEY_CHECK_INFO: &[u8] = b"fylo.vdbx.key-check.v1";

/// Why sealing or opening failed.
#[derive(Debug, Eq, PartialEq)]
pub enum ContainerCipherError {
    /// The secret does not match the file's key-check value (`EKEY`).
    WrongKey,
    /// The unit is damaged, or belongs somewhere else.
    Unauthenticated,
    /// The system could not supply random bytes.
    Random,
}

impl fmt::Display for ContainerCipherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongKey => "the encryption key does not match this database",
            Self::Unauthenticated => "an encrypted unit is damaged or misplaced",
            Self::Random => "no random bytes for an encryption nonce",
        })
    }
}

impl std::error::Error for ContainerCipherError {}

/// Where a sealed unit belongs. Opening it anywhere else fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerSealContext {
    /// A record body.
    Record {
        /// Its collection's id.
        collection: u32,
        /// Its LSN.
        lsn: u64,
        /// Its entry's index in the commit.
        entry: u32,
    },
    /// A collection block in the directory.
    Directory {
        /// The block number.
        block: u32,
    },
    /// A compression dictionary.
    Dictionary {
        /// Its collection's id.
        collection: u32,
        /// The dictionary id.
        id: u32,
    },
    /// A location or index snapshot.
    Snapshot {
        /// Its collection's id.
        collection: u32,
        /// The checkpoint that wrote it.
        checkpoint: u64,
        /// Which snapshot: `Locations`, `Index` or `Schemas`.
        kind: ContainerExtentKind,
    },
    /// A block of a sealed file's image (ADR 0016 sections 8 and 11).
    Block {
        /// The block number: its offset in the image over the block size.
        number: u64,
    },
}

impl ContainerSealContext {
    fn associated_data(self) -> [u8; 28] {
        let (tag, a, b, c) = match self {
            Self::Record {
                collection,
                lsn,
                entry,
            } => (1_u8, collection, lsn, entry),
            Self::Directory { block } => (2, block, 0, 0),
            Self::Dictionary { collection, id } => (3, collection, 0, id),
            Self::Snapshot {
                collection,
                checkpoint,
                kind,
            } => (4, collection, checkpoint, u32::from(kind.code())),
            Self::Block { number } => (5, 0, number, 0),
        };
        let mut bytes = [0_u8; 28];
        bytes[..8].copy_from_slice(b"VDXSEAL\x01");
        bytes[8] = tag;
        bytes[12..16].copy_from_slice(&a.to_le_bytes());
        bytes[16..24].copy_from_slice(&b.to_le_bytes());
        bytes[24..28].copy_from_slice(&c.to_le_bytes());
        bytes
    }
}

/// The file key of an encrypted `.vdbx` file.
pub struct ContainerCipher {
    cipher: Aes256Gcm,
    key_check: [u8; KEY_CHECK_BYTES],
}

impl fmt::Debug for ContainerCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContainerCipher { .. }")
    }
}

impl ContainerCipher {
    /// A fresh salt for a new encrypted file.
    ///
    /// # Errors
    ///
    /// Returns `Random` when the system has no random bytes.
    pub fn new_salt() -> Result<[u8; SALT_BYTES], ContainerCipherError> {
        let mut salt = [0_u8; SALT_BYTES];
        getrandom::fill(&mut salt).map_err(|_| ContainerCipherError::Random)?;
        Ok(salt)
    }

    /// Derive the file key from the configured secret and the file's salt.
    #[must_use]
    pub fn derive(secret: &[u8], salt: &[u8; SALT_BYTES]) -> Self {
        // HKDF-SHA256 (RFC 5869): extract once, expand twice with different
        // labels, each a single block.
        let pseudorandom = Zeroizing::new(hmac_sha256(salt, &[secret]));
        let key = Zeroizing::new(hmac_sha256(&pseudorandom[..], &[FILE_KEY_INFO, &[1]]));
        let check = hmac_sha256(&pseudorandom[..], &[KEY_CHECK_INFO, &[1]]);
        let mut key_check = [0_u8; KEY_CHECK_BYTES];
        key_check.copy_from_slice(&check[..KEY_CHECK_BYTES]);
        Self {
            cipher: Aes256Gcm::new((&*key).into()),
            key_check,
        }
    }

    /// Derive the file key and check it against the superblock's value.
    ///
    /// # Errors
    ///
    /// Returns `WrongKey` when the secret is not this file's.
    pub fn open_file(
        secret: &[u8],
        salt: &[u8; SALT_BYTES],
        key_check: &[u8; KEY_CHECK_BYTES],
    ) -> Result<Self, ContainerCipherError> {
        let cipher = Self::derive(secret, salt);
        cipher.check(key_check)?;
        Ok(cipher)
    }

    /// Check this key against a file's key-check value, in constant time.
    ///
    /// # Errors
    ///
    /// Returns `WrongKey` when the key is not that file's.
    pub fn check(&self, key_check: &[u8; KEY_CHECK_BYTES]) -> Result<(), ContainerCipherError> {
        let difference = self
            .key_check
            .iter()
            .zip(key_check)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b));
        if difference == 0 {
            Ok(())
        } else {
            Err(ContainerCipherError::WrongKey)
        }
    }

    /// The key-check value for the superblock.
    #[must_use]
    pub fn key_check(&self) -> [u8; KEY_CHECK_BYTES] {
        self.key_check
    }

    /// Seal `plaintext` for `context`: nonce, ciphertext, tag.
    ///
    /// # Errors
    ///
    /// Returns `Random` when no nonce can be drawn.
    pub fn seal(
        &self,
        context: ContainerSealContext,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, ContainerCipherError> {
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| ContainerCipherError::Random)?;
        let aad = context.associated_data();
        let ciphertext = self
            .cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| ContainerCipherError::Unauthenticated)?;
        let mut sealed = Vec::with_capacity(NONCE_BYTES + ciphertext.len());
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    /// Open a unit sealed for `context`.
    ///
    /// # Errors
    ///
    /// Returns `Unauthenticated` when it is damaged, truncated, or was sealed
    /// for another context or file.
    pub fn open(
        &self,
        context: ContainerSealContext,
        sealed: &[u8],
    ) -> Result<Vec<u8>, ContainerCipherError> {
        if sealed.len() < NONCE_BYTES + TAG_BYTES {
            return Err(ContainerCipherError::Unauthenticated);
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_BYTES);
        let nonce: &[u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| ContainerCipherError::Unauthenticated)?;
        let aad = context.associated_data();
        self.cipher
            .decrypt(
                nonce.into(),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| ContainerCipherError::Unauthenticated)
    }
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(key)
        .expect("HMAC accepts a key of any length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests;
