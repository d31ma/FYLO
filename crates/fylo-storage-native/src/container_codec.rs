//! Record body compression for `.fxdb` frames (ADR 0016 section 8).
//!
//! Each body is compressed on its own, so a point read decompresses one
//! document, usually against its collection's dictionary. Native writers use
//! zstd level 3 through the C library. The browser cannot carry that library,
//! so it reads zstd with the pure-Rust `ruzstd` and writes LZ4, which native
//! compaction later rewrites as zstd. Every frame names its codec, so a file
//! may mix them.
//!
//! Bodies come from disk and are not trusted: a decode first reads the
//! content size the body declares, refuses it above the caller's limit, and
//! then refuses any body that does not produce exactly that many bytes.

use std::fmt;

use fylo_format::ContainerCodec;

/// The zstd level native writers use.
pub const ZSTD_WRITE_LEVEL: i32 = 3;
/// The zstd level a sealed file is recompressed at.
pub const ZSTD_SEALED_LEVEL: i32 = 19;
/// Largest dictionary trained for a collection.
pub const MAX_DICTIONARY_BYTES: usize = 32 * 1024;
/// Documents read to train a collection's first dictionary.
pub const DICTIONARY_TRAINING_BYTES: usize = 1024 * 1024;

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const LZ4_SIZE_BYTES: usize = 4;

/// Why a body could not be compressed or decompressed.
#[derive(Debug)]
pub enum ContainerCodecError {
    /// The body is damaged or is not what its codec byte says.
    Corrupt,
    /// The body declares more bytes than the caller allows.
    Oversized,
    /// The frame names a dictionary this codec does not hold.
    WrongDictionary,
    /// The dictionary is not a zstd dictionary.
    BadDictionary,
    /// This build cannot write the codec (zstd in WebAssembly).
    Unsupported,
    /// The zstd library failed for another reason.
    Library(std::io::Error),
}

impl fmt::Display for ContainerCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt => formatter.write_str("compressed record body is damaged"),
            Self::Oversized => formatter.write_str("compressed record body is too large"),
            Self::WrongDictionary => {
                formatter.write_str("record body names a dictionary that is not loaded")
            }
            Self::BadDictionary => formatter.write_str("compression dictionary is malformed"),
            Self::Unsupported => formatter.write_str("this build cannot write that codec"),
            Self::Library(error) => write!(formatter, "zstd failed: {error}"),
        }
    }
}

impl std::error::Error for ContainerCodecError {}

/// A collection's compression dictionary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerDictionary {
    /// Its id within the collection, as frames name it; never zero.
    pub id: u32,
    /// A zstd dictionary. LZ4 uses the same bytes as raw history.
    pub bytes: Vec<u8>,
}

/// One compressed body and what a frame records to decompress it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerEncodedBody {
    /// The frame's codec byte.
    pub codec: ContainerCodec,
    /// The frame's dictionary id; zero when no dictionary was used.
    pub dictionary_id: u32,
    /// The stored bytes.
    pub bytes: Vec<u8>,
}

/// Compresses and decompresses the bodies of one collection, with at most
/// one dictionary. A collection whose records span dictionaries keeps one
/// codec per dictionary.
///
/// It holds the zstd contexts between calls, which is what keeps a small
/// body's encode and decode in microseconds, so it takes `&mut self`.
pub struct ContainerFrameCodec {
    dictionary: Option<ContainerDictionary>,
    #[cfg(not(target_family = "wasm"))]
    compressor: zstd::bulk::Compressor<'static>,
    #[cfg(not(target_family = "wasm"))]
    decompressor: zstd::bulk::Decompressor<'static>,
    #[cfg(target_family = "wasm")]
    decoder: browser_zstd::Decoder,
}

impl fmt::Debug for ContainerFrameCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainerFrameCodec")
            .field(
                "dictionary_id",
                &self.dictionary.as_ref().map(|dictionary| dictionary.id),
            )
            .finish_non_exhaustive()
    }
}

impl ContainerFrameCodec {
    /// A codec writing at [`ZSTD_WRITE_LEVEL`].
    ///
    /// # Errors
    ///
    /// Returns `BadDictionary` for a dictionary zstd refuses, or a zero id.
    pub fn new(dictionary: Option<ContainerDictionary>) -> Result<Self, ContainerCodecError> {
        Self::with_level(dictionary, ZSTD_WRITE_LEVEL)
    }

    /// A codec writing zstd at `level`. The browser ignores the level: it
    /// writes LZ4.
    ///
    /// # Errors
    ///
    /// Returns `BadDictionary` for a dictionary zstd refuses, or a zero id.
    pub fn with_level(
        dictionary: Option<ContainerDictionary>,
        level: i32,
    ) -> Result<Self, ContainerCodecError> {
        if dictionary.as_ref().is_some_and(|dictionary| {
            dictionary.id == 0 || !dictionary.bytes.starts_with(&ZSTD_DICTIONARY_MAGIC)
        }) {
            return Err(ContainerCodecError::BadDictionary);
        }
        #[cfg(not(target_family = "wasm"))]
        {
            use zstd::zstd_safe::CParameter;
            let raw = dictionary
                .as_ref()
                .map_or(&[][..], |dictionary| &dictionary.bytes[..]);
            let bad = |_| ContainerCodecError::BadDictionary;
            let mut compressor =
                zstd::bulk::Compressor::with_dictionary(level, raw).map_err(bad)?;
            // The frame carries the dictionary id and the CRC; the zstd frame
            // carries neither, but must carry its content size.
            for parameter in [
                CParameter::DictIdFlag(false),
                CParameter::ChecksumFlag(false),
                CParameter::ContentSizeFlag(true),
            ] {
                compressor
                    .set_parameter(parameter)
                    .map_err(ContainerCodecError::Library)?;
            }
            let decompressor = zstd::bulk::Decompressor::with_dictionary(raw).map_err(bad)?;
            Ok(Self {
                dictionary,
                compressor,
                decompressor,
            })
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = level;
            let decoder = browser_zstd::Decoder::new(dictionary.as_ref())?;
            Ok(Self {
                dictionary,
                decoder,
            })
        }
    }

    /// The dictionary's id, or zero.
    #[must_use]
    pub fn dictionary_id(&self) -> u32 {
        self.dictionary
            .as_ref()
            .map_or(0, |dictionary| dictionary.id)
    }

    fn dictionary_bytes(&self) -> &[u8] {
        self.dictionary
            .as_ref()
            .map_or(&[][..], |dictionary| &dictionary.bytes[..])
    }

    /// Compress a body the way this build writes: zstd natively, LZ4 in the
    /// browser. A body compression would not shrink is stored as is.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` for a body of 4 GiB or more, or a zstd failure.
    pub fn encode(&mut self, body: &[u8]) -> Result<ContainerEncodedBody, ContainerCodecError> {
        #[cfg(not(target_family = "wasm"))]
        {
            let bytes = self
                .compressor
                .compress(body)
                .map_err(ContainerCodecError::Library)?;
            Ok(self.smaller_of(body, ContainerCodec::Zstd, bytes))
        }
        #[cfg(target_family = "wasm")]
        {
            self.encode_lz4(body)
        }
    }

    /// Compress a body as LZ4, as the browser does. A body compression would
    /// not shrink is stored as is.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` for a body of 4 GiB or more.
    pub fn encode_lz4(&mut self, body: &[u8]) -> Result<ContainerEncodedBody, ContainerCodecError> {
        let size = u32::try_from(body.len()).map_err(|_| ContainerCodecError::Oversized)?;
        let block = lz4_flex::block::compress_with_dict(body, self.dictionary_bytes());
        let mut bytes = Vec::with_capacity(LZ4_SIZE_BYTES + block.len());
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&block);
        Ok(self.smaller_of(body, ContainerCodec::Lz4, bytes))
    }

    fn smaller_of(
        &self,
        body: &[u8],
        codec: ContainerCodec,
        bytes: Vec<u8>,
    ) -> ContainerEncodedBody {
        if bytes.len() < body.len() {
            ContainerEncodedBody {
                codec,
                dictionary_id: self.dictionary_id(),
                bytes,
            }
        } else {
            ContainerEncodedBody {
                codec: ContainerCodec::None,
                dictionary_id: 0,
                bytes: body.to_vec(),
            }
        }
    }

    /// Decompress a stored body of at most `max_bytes` once decompressed.
    ///
    /// # Errors
    ///
    /// Returns `WrongDictionary` when the frame names another dictionary,
    /// `Oversized` when the body declares more than `max_bytes`, and
    /// `Corrupt` when it does not decompress to exactly what it declares.
    pub fn decode(
        &mut self,
        codec: ContainerCodec,
        dictionary_id: u32,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<Vec<u8>, ContainerCodecError> {
        if codec == ContainerCodec::None {
            if dictionary_id != 0 {
                return Err(ContainerCodecError::Corrupt);
            }
            if bytes.len() > max_bytes {
                return Err(ContainerCodecError::Oversized);
            }
            return Ok(bytes.to_vec());
        }
        if dictionary_id != self.dictionary_id() {
            return Err(ContainerCodecError::WrongDictionary);
        }
        match codec {
            ContainerCodec::None => unreachable!("handled above"),
            ContainerCodec::Lz4 => self.decode_lz4(bytes, max_bytes),
            ContainerCodec::Zstd => {
                let size = zstd_content_size(bytes)?;
                if size > max_bytes {
                    return Err(ContainerCodecError::Oversized);
                }
                let body = self.decode_zstd(bytes, size)?;
                if body.len() == size {
                    Ok(body)
                } else {
                    Err(ContainerCodecError::Corrupt)
                }
            }
        }
    }

    fn decode_lz4(&self, bytes: &[u8], max_bytes: usize) -> Result<Vec<u8>, ContainerCodecError> {
        let (size, block) = bytes
            .split_first_chunk::<LZ4_SIZE_BYTES>()
            .ok_or(ContainerCodecError::Corrupt)?;
        let size = usize::try_from(u32::from_le_bytes(*size))
            .map_err(|_| ContainerCodecError::Oversized)?;
        if size > max_bytes {
            return Err(ContainerCodecError::Oversized);
        }
        let body = lz4_flex::block::decompress_with_dict(block, size, self.dictionary_bytes())
            .map_err(|_| ContainerCodecError::Corrupt)?;
        if body.len() == size {
            Ok(body)
        } else {
            Err(ContainerCodecError::Corrupt)
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn decode_zstd(&mut self, bytes: &[u8], size: usize) -> Result<Vec<u8>, ContainerCodecError> {
        self.decompressor
            .decompress(bytes, size)
            .map_err(|_| ContainerCodecError::Corrupt)
    }

    #[cfg(target_family = "wasm")]
    fn decode_zstd(&mut self, bytes: &[u8], size: usize) -> Result<Vec<u8>, ContainerCodecError> {
        self.decoder.decode(bytes, size)
    }
}

const ZSTD_DICTIONARY_MAGIC: [u8; 4] = [0x37, 0xa4, 0x30, 0xec];

/// Train a collection dictionary from sample documents, at most
/// [`MAX_DICTIONARY_BYTES`]. Returns `None` when the samples are too few or
/// too uniform for zstd to train on; the collection then writes without one.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn train_dictionary<S: AsRef<[u8]>>(id: u32, samples: &[S]) -> Option<ContainerDictionary> {
    if id == 0 {
        return None;
    }
    let bytes = zstd::dict::from_samples(samples, MAX_DICTIONARY_BYTES).ok()?;
    bytes
        .starts_with(&ZSTD_DICTIONARY_MAGIC)
        .then_some(ContainerDictionary { id, bytes })
}

/// The content size a single zstd frame declares. A body must be exactly
/// one frame that records its size, which every `.fxdb` writer does.
fn zstd_content_size(bytes: &[u8]) -> Result<usize, ContainerCodecError> {
    let corrupt = ContainerCodecError::Corrupt;
    if bytes.get(..4) != Some(&ZSTD_MAGIC[..]) {
        return Err(corrupt);
    }
    let descriptor = *bytes.get(4).ok_or(ContainerCodecError::Corrupt)?;
    let single_segment = descriptor & 0x20 != 0;
    if descriptor & 0x08 != 0 {
        return Err(corrupt);
    }
    let dictionary_bytes = [0, 1, 2, 4][usize::from(descriptor & 0x03)];
    let size_bytes = match descriptor >> 6 {
        0 if single_segment => 1,
        0 => return Err(corrupt),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let at = 5 + usize::from(!single_segment) + dictionary_bytes;
    let field = bytes
        .get(at..at + size_bytes)
        .ok_or(ContainerCodecError::Corrupt)?;
    let mut little_endian = [0_u8; 8];
    little_endian[..size_bytes].copy_from_slice(field);
    let mut size = u64::from_le_bytes(little_endian);
    if size_bytes == 2 {
        size += 256;
    }
    usize::try_from(size).map_err(|_| ContainerCodecError::Oversized)
}

/// zstd decoding in pure Rust, for the browser. Natively it is built only for
/// the tests that check it reads what the C library writes.
#[cfg(any(target_family = "wasm", test))]
mod browser_zstd {
    use super::{ContainerCodecError, ContainerDictionary};
    use ruzstd::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};

    /// Bytes decoded between checks against the declared size.
    const STEP_BYTES: usize = 128 * 1024;

    pub(super) struct Decoder {
        decoder: FrameDecoder,
        dictionary: Option<u32>,
    }

    impl Decoder {
        pub(super) fn new(
            dictionary: Option<&ContainerDictionary>,
        ) -> Result<Self, ContainerCodecError> {
            let mut decoder = FrameDecoder::new();
            let dictionary = match dictionary {
                Some(dictionary) => {
                    let parsed = Dictionary::decode_dict(&dictionary.bytes)
                        .map_err(|_| ContainerCodecError::BadDictionary)?;
                    let id = parsed.id;
                    decoder
                        .add_dict(parsed)
                        .map_err(|_| ContainerCodecError::BadDictionary)?;
                    Some(id)
                }
                None => None,
            };
            Ok(Self {
                decoder,
                dictionary,
            })
        }

        /// Decode one frame declaring `size` bytes, refusing more output.
        pub(super) fn decode(
            &mut self,
            mut bytes: &[u8],
            size: usize,
        ) -> Result<Vec<u8>, ContainerCodecError> {
            let corrupt = |_| ContainerCodecError::Corrupt;
            self.decoder.set_max_window_size(size.max(1024) as u64);
            self.decoder.init(&mut bytes).map_err(corrupt)?;
            // The frame leaves its dictionary id out; the record frame names it.
            if let Some(id) = self.dictionary {
                self.decoder.force_dict(id).map_err(corrupt)?;
            }
            let mut body = Vec::with_capacity(size);
            loop {
                self.decoder
                    .decode_blocks(&mut bytes, BlockDecodingStrategy::UptoBytes(STEP_BYTES))
                    .map_err(corrupt)?;
                if let Some(chunk) = self.decoder.collect() {
                    if body.len() + chunk.len() > size {
                        return Err(ContainerCodecError::Corrupt);
                    }
                    body.extend_from_slice(&chunk);
                }
                if self.decoder.is_finished() {
                    break;
                }
            }
            if self.decoder.can_collect() != 0 || !bytes.is_empty() {
                return Err(ContainerCodecError::Corrupt);
            }
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests;
