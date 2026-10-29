//! The stored body of a record frame in a `.vdbx` file (ADR 0016 section 4).
//!
//! The frame around it is ADR 0015's: sync marker, length, CRC-32C, body.
//! In a `.vdbx` file that body starts with one header byte: the codec in the
//! low four bits, then a flag for a dictionary id and a flag for a sealed
//! body. A 4-byte dictionary id follows when a dictionary was used. The rest
//! is the compressed record, and in an encrypted file it is sealed: the
//! record's LSN in the clear, then nonce, ciphertext, tag. The frame's CRC
//! covers these stored bytes, so a verifier needs no key to find damage.
//!
//! The associated data binds a sealed body to its collection and LSN, so
//! the reader must know the LSN before it opens the body: it stands outside
//! the seal, and a body whose clear LSN was changed does not open.
//!
//! Compression comes before sealing, because ciphertext does not compress.

use std::fmt;

use fylo_format::ContainerCodec;

use crate::{
    ContainerCipher, ContainerCipherError, ContainerCodecError, ContainerFrameCodec,
    ContainerSealContext,
};

const CODEC_MASK: u8 = 0x0F;
const HAS_DICTIONARY: u8 = 0x10;
const SEALED: u8 = 0x20;
const KNOWN_FLAGS: u8 = CODEC_MASK | HAS_DICTIONARY | SEALED;
const DICTIONARY_ID_BYTES: usize = 4;
const LSN_BYTES: usize = 8;

/// Why a stored record body could not be written or read.
#[derive(Debug)]
pub enum ContainerRecordError {
    /// The header byte or its dictionary id is malformed.
    Malformed,
    /// The header byte names a codec or sets a flag this build does not
    /// know: a newer writer's record, refused rather than misread.
    Unsupported,
    /// The body is plain in an encrypted file, which would let a writer
    /// without the key slip records in.
    NotSealed,
    /// The body is sealed and this open has no key (`EKEY`).
    Locked,
    /// Compression or decompression failed.
    Codec(ContainerCodecError),
    /// Sealing or opening failed.
    Cipher(ContainerCipherError),
}

impl fmt::Display for ContainerRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => formatter.write_str("record body header is malformed"),
            Self::Unsupported => {
                formatter.write_str("record body uses a codec or flag this build cannot read")
            }
            Self::NotSealed => {
                formatter.write_str("record body is not sealed in an encrypted file")
            }
            Self::Locked => formatter.write_str("record body is sealed and no key was given"),
            Self::Codec(error) => write!(formatter, "record body: {error}"),
            Self::Cipher(error) => write!(formatter, "record body: {error}"),
        }
    }
}

impl std::error::Error for ContainerRecordError {}

impl ContainerRecordError {
    /// The stable error code machine clients see.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Locked | Self::Cipher(ContainerCipherError::WrongKey) => "EKEY",
            Self::Cipher(ContainerCipherError::Random) => "ENATIVE_IO",
            Self::Unsupported | Self::Codec(ContainerCodecError::Unsupported) => "EVDBX_VERSION",
            Self::Malformed | Self::NotSealed | Self::Codec(_) | Self::Cipher(_) => "EVDBX_CORRUPT",
        }
    }
}

impl From<ContainerCodecError> for ContainerRecordError {
    fn from(error: ContainerCodecError) -> Self {
        Self::Codec(error)
    }
}

impl From<ContainerCipherError> for ContainerRecordError {
    fn from(error: ContainerCipherError) -> Self {
        Self::Cipher(error)
    }
}

/// A stored record body, parsed but not yet decompressed or opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerStoredBody<'a> {
    /// How the record was compressed.
    pub codec: ContainerCodec,
    /// The dictionary it was compressed with; zero for none. The caller
    /// picks the [`ContainerFrameCodec`] holding it.
    pub dictionary_id: u32,
    /// Whether the compressed bytes are sealed.
    pub sealed: bool,
    /// The LSN a sealed body is bound to, kept in the clear before its
    /// nonce; zero for a body that is not sealed.
    pub lsn: u64,
    payload: &'a [u8],
}

impl<'a> ContainerStoredBody<'a> {
    /// Compress `body` and, when `cipher` is given, seal it for `context`,
    /// a record's, whose LSN is stored in the clear in front of the seal.
    /// Natively the codec writes zstd; with `lz4` set, or in WebAssembly, it
    /// writes LZ4. A body compression would not shrink is stored as is.
    ///
    /// # Errors
    ///
    /// Returns `Codec` when compression fails, or `Cipher(Random)` when no
    /// nonce can be drawn.
    pub fn encode(
        codec: &mut ContainerFrameCodec,
        cipher: Option<&ContainerCipher>,
        context: ContainerSealContext,
        body: &[u8],
        lz4: bool,
    ) -> Result<Vec<u8>, ContainerRecordError> {
        let encoded = if lz4 {
            codec.encode_lz4(body)?
        } else {
            codec.encode(body)?
        };
        let mut header = encoded.codec.code();
        if encoded.dictionary_id != 0 {
            header |= HAS_DICTIONARY;
        }
        let (lsn, payload) = match cipher {
            Some(cipher) => {
                header |= SEALED;
                let lsn = match context {
                    ContainerSealContext::Record { lsn, .. } => lsn,
                    _ => 0,
                };
                (Some(lsn), cipher.seal(context, &encoded.bytes)?)
            }
            None => (None, encoded.bytes),
        };
        let mut stored = Vec::with_capacity(1 + DICTIONARY_ID_BYTES + LSN_BYTES + payload.len());
        stored.push(header);
        if encoded.dictionary_id != 0 {
            stored.extend_from_slice(&encoded.dictionary_id.to_le_bytes());
        }
        if let Some(lsn) = lsn {
            stored.extend_from_slice(&lsn.to_le_bytes());
        }
        stored.extend_from_slice(&payload);
        Ok(stored)
    }

    /// Parse the header of a stored body.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for unknown flag bits or an unknown codec, and
    /// `Malformed` for an empty body, a missing or zero dictionary id, a
    /// dictionary named without compression, or a sealed body too short for
    /// its LSN.
    pub fn parse(stored: &'a [u8]) -> Result<Self, ContainerRecordError> {
        let (&header, rest) = stored
            .split_first()
            .ok_or(ContainerRecordError::Malformed)?;
        if header & !KNOWN_FLAGS != 0 {
            return Err(ContainerRecordError::Unsupported);
        }
        let codec = ContainerCodec::from_code(header & CODEC_MASK)
            .map_err(|_| ContainerRecordError::Unsupported)?;
        let (dictionary_id, payload) = if header & HAS_DICTIONARY == 0 {
            (0, rest)
        } else {
            let (id, payload) = rest
                .split_first_chunk::<DICTIONARY_ID_BYTES>()
                .ok_or(ContainerRecordError::Malformed)?;
            let id = u32::from_le_bytes(*id);
            if id == 0 || codec == ContainerCodec::None {
                return Err(ContainerRecordError::Malformed);
            }
            (id, payload)
        };
        let sealed = header & SEALED != 0;
        let (lsn, payload) = if sealed {
            let (lsn, payload) = payload
                .split_first_chunk::<LSN_BYTES>()
                .ok_or(ContainerRecordError::Malformed)?;
            (u64::from_le_bytes(*lsn), payload)
        } else {
            (0, payload)
        };
        Ok(Self {
            codec,
            dictionary_id,
            sealed,
            lsn,
            payload,
        })
    }

    /// The body itself when it is stored as is, neither compressed nor
    /// sealed, so a reader need not copy it.
    #[must_use]
    pub fn plain(&self) -> Option<&'a [u8]> {
        (self.codec == ContainerCodec::None && !self.sealed).then_some(self.payload)
    }

    /// Where a sealed body of collection `collection` must open: its
    /// collection and the LSN it carries in the clear. A frame holds one
    /// whole record, so its entry index is zero.
    #[must_use]
    pub const fn record_context(&self, collection: u32) -> ContainerSealContext {
        ContainerSealContext::Record {
            collection,
            lsn: self.lsn,
            entry: 0,
        }
    }

    /// Open (when sealed) and decompress the body, to at most `max_bytes`.
    /// `cipher` is the file's key: `Some` exactly when the file is
    /// encrypted.
    ///
    /// # Errors
    ///
    /// Returns `NotSealed` for a plain body in an encrypted file, `Locked`
    /// for a sealed body without a key, `Cipher(Unauthenticated)` for a body
    /// sealed for another place, or a `Codec` error, `WrongDictionary`
    /// among them when `codec` holds another dictionary.
    pub fn decode(
        &self,
        codec: &mut ContainerFrameCodec,
        cipher: Option<&ContainerCipher>,
        context: ContainerSealContext,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ContainerRecordError> {
        let opened;
        let compressed = match (self.sealed, cipher) {
            (true, Some(cipher)) => {
                opened = cipher.open(context, self.payload)?;
                opened.as_slice()
            }
            (true, None) => return Err(ContainerRecordError::Locked),
            (false, Some(_)) => return Err(ContainerRecordError::NotSealed),
            (false, None) => self.payload,
        };
        Ok(codec.decode(self.codec, self.dictionary_id, compressed, max_bytes)?)
    }
}

#[cfg(test)]
mod tests;
