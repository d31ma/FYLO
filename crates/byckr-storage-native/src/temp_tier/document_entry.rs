use byckr_format::Crc32c;

use super::entry_lengths::EntryLengths;
use super::entry_reader::EntryReader;

/// One stored document record in the temp tier (ADR 0030 section 4.4).
///
/// The file holds a fixed header (magic `FYLC`, format version, kind, the
/// digests of the collection stamp and of the document file's stamp, the
/// file's modification time, and the lengths), the collection, the id and
/// the document's path below the collection's `docs` folder, a CRC-32C of
/// all of that, then the stored payload exactly as read from the root, the
/// record's verified manifest (the EFS profile's `.fylo-attrs` file, empty
/// in the native profile) and a CRC-32C of both. Nothing in it is decoded
/// or decrypted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DocumentEntry {
    pub(crate) collection: String,
    pub(crate) identifier: String,
    pub(crate) collection_stamp: [u8; 32],
    pub(crate) document_stamp: [u8; 32],
    pub(crate) modified_millis: u64,
    pub(crate) relative_path: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) manifest: Vec<u8>,
}

impl DocumentEntry {
    const MAGIC: [u8; 4] = *b"FYLC";
    const VERSION: u16 = 2;
    const DOCUMENT_KIND: u8 = 1;
    const FIXED_BYTES: usize = 94;
    pub(crate) const CHECKSUM_BYTES: usize = 4;
    const DIGEST_BYTES: usize = 32;

    /// The bytes of the entry's file.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = self.header();
        let header_checksum = Crc32c::checksum(&bytes);
        bytes.extend_from_slice(&header_checksum.to_le_bytes());
        let body_start = bytes.len();
        bytes.extend_from_slice(&self.payload);
        bytes.extend_from_slice(&self.manifest);
        let body_checksum = Crc32c::checksum(&bytes[body_start..]);
        bytes.extend_from_slice(&body_checksum.to_le_bytes());
        bytes
    }

    /// The bytes the entry keeps from the root: the payload and the
    /// manifest. The budget's limits count these.
    pub(crate) fn stored_bytes(&self) -> usize {
        self.payload.len().saturating_add(self.manifest.len())
    }

    /// Whether every text of the entry fits its header's length fields.
    pub(crate) fn fits(&self) -> bool {
        let texts = [&self.collection, &self.identifier, &self.relative_path];
        let lengths = texts.map(|text| u16::try_from(text.len()).is_ok());
        let bodies = [&self.payload, &self.manifest].map(|body| u32::try_from(body.len()).is_ok());
        !lengths.contains(&false) && !bodies.contains(&false)
    }

    /// The entry `bytes` hold, or `None` when they are torn, damaged, of
    /// another version or kind, or not an entry at all.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = EntryReader::new(bytes);
        let fixed = reader.take(Self::FIXED_BYTES)?;
        let lengths = Self::read_fixed(fixed)?;
        let strings = reader.take(lengths.strings())?;
        let header_checksum = reader.take_u32()?;
        let header = &bytes[..Self::FIXED_BYTES + lengths.strings()];
        Self::require(Crc32c::checksum(header) == header_checksum)?;
        let body = reader.take(lengths.body()?)?;
        let body_checksum = reader.take_u32()?;
        Self::require(Crc32c::checksum(body) == body_checksum)?;
        Self::require(reader.is_empty())?;
        Self::assemble(fixed, strings, &lengths, body)
    }

    fn header(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::FIXED_BYTES);
        bytes.extend_from_slice(&Self::MAGIC);
        bytes.extend_from_slice(&Self::VERSION.to_le_bytes());
        bytes.push(Self::DOCUMENT_KIND);
        bytes.push(0);
        bytes.extend_from_slice(&self.collection_stamp);
        bytes.extend_from_slice(&self.document_stamp);
        bytes.extend_from_slice(&self.modified_millis.to_le_bytes());
        bytes.extend_from_slice(&Self::short_length(&self.collection).to_le_bytes());
        bytes.extend_from_slice(&Self::short_length(&self.identifier).to_le_bytes());
        bytes.extend_from_slice(&Self::short_length(&self.relative_path).to_le_bytes());
        bytes.extend_from_slice(&Self::long_length(&self.payload).to_le_bytes());
        bytes.extend_from_slice(&Self::long_length(&self.manifest).to_le_bytes());
        bytes.extend_from_slice(self.collection.as_bytes());
        bytes.extend_from_slice(self.identifier.as_bytes());
        bytes.extend_from_slice(self.relative_path.as_bytes());
        bytes
    }

    fn short_length(text: &str) -> u16 {
        u16::try_from(text.len()).unwrap_or(u16::MAX)
    }

    fn long_length(bytes: &[u8]) -> u32 {
        u32::try_from(bytes.len()).unwrap_or(u32::MAX)
    }

    fn read_fixed(fixed: &[u8]) -> Option<EntryLengths> {
        Self::require(fixed[0..4] == Self::MAGIC)?;
        Self::require(u16::from_le_bytes([fixed[4], fixed[5]]) == Self::VERSION)?;
        Self::require(fixed[6] == Self::DOCUMENT_KIND)?;
        Some(EntryLengths {
            collection: usize::from(u16::from_le_bytes([fixed[80], fixed[81]])),
            identifier: usize::from(u16::from_le_bytes([fixed[82], fixed[83]])),
            path: usize::from(u16::from_le_bytes([fixed[84], fixed[85]])),
            payload: Self::long_at(fixed, 86)?,
            manifest: Self::long_at(fixed, 90)?,
        })
    }

    fn long_at(fixed: &[u8], start: usize) -> Option<usize> {
        let bytes = [
            fixed[start],
            fixed[start + 1],
            fixed[start + 2],
            fixed[start + 3],
        ];
        usize::try_from(u32::from_le_bytes(bytes)).ok()
    }

    fn assemble(fixed: &[u8], strings: &[u8], lengths: &EntryLengths, body: &[u8]) -> Option<Self> {
        let (payload, manifest) = body.split_at(lengths.payload);
        let (collection, rest) = strings.split_at(lengths.collection);
        let (identifier, path) = rest.split_at(lengths.identifier);
        Some(Self {
            collection: String::from_utf8(collection.to_vec()).ok()?,
            identifier: String::from_utf8(identifier.to_vec()).ok()?,
            collection_stamp: Self::digest_at(fixed, 8),
            document_stamp: Self::digest_at(fixed, 8 + Self::DIGEST_BYTES),
            modified_millis: u64::from_le_bytes(Self::eight_at(fixed, 72)),
            relative_path: String::from_utf8(path.to_vec()).ok()?,
            payload: payload.to_vec(),
            manifest: manifest.to_vec(),
        })
    }

    fn digest_at(fixed: &[u8], start: usize) -> [u8; 32] {
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(&fixed[start..start + Self::DIGEST_BYTES]);
        digest
    }

    fn eight_at(fixed: &[u8], start: usize) -> [u8; 8] {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&fixed[start..start + 8]);
        bytes
    }

    fn require(condition: bool) -> Option<()> {
        condition.then_some(())
    }

    #[cfg(test)]
    pub(crate) const fn header_bytes() -> usize {
        Self::FIXED_BYTES + Self::CHECKSUM_BYTES
    }
}

#[cfg(test)]
mod tests;
