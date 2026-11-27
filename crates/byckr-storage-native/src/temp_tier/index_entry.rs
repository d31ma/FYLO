use byckr_format::Crc32c;

use super::entry_reader::EntryReader;

/// One collection's stored index bytes in the temp tier (ADR 0030 section
/// 4.6): the index snapshot and its write-ahead log exactly as read from the
/// root, under the digest of the collection stamp they were read at.
///
/// The file holds a fixed header (magic `FYLC`, format version, kind 2, a
/// flag for the log, the stamp digest and the lengths), the collection, a
/// CRC-32C of all of that, then the snapshot and log bytes and a CRC-32C of
/// both. It is used only under the same stamp; there is no revalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexEntry {
    pub(crate) collection: String,
    pub(crate) collection_stamp: [u8; 32],
    pub(crate) snapshot: Vec<u8>,
    pub(crate) log: Option<Vec<u8>>,
}

impl IndexEntry {
    const MAGIC: [u8; 4] = *b"FYLC";
    const VERSION: u16 = 2;
    const INDEX_KIND: u8 = 2;
    const HAS_LOG: u8 = 1;
    const FIXED_BYTES: usize = 50;
    const DIGEST_START: usize = 8;
    const DIGEST_BYTES: usize = 32;

    /// The bytes of the entry's file.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = self.header();
        let header_checksum = Crc32c::checksum(&bytes);
        bytes.extend_from_slice(&header_checksum.to_le_bytes());
        let body_start = bytes.len();
        bytes.extend_from_slice(&self.snapshot);
        bytes.extend_from_slice(self.log_bytes());
        let body_checksum = Crc32c::checksum(&bytes[body_start..]);
        bytes.extend_from_slice(&body_checksum.to_le_bytes());
        bytes
    }

    /// The bytes the entry keeps from the root. The budget's limits count
    /// these.
    pub(crate) fn stored_bytes(&self) -> usize {
        self.snapshot.len().saturating_add(self.log_bytes().len())
    }

    /// Whether the collection name and the bodies fit the header's length
    /// fields.
    pub(crate) fn fits(&self) -> bool {
        let name = u16::try_from(self.collection.len()).is_ok();
        let bodies = [self.snapshot.as_slice(), self.log_bytes()]
            .map(|body| u32::try_from(body.len()).is_ok());
        name && !bodies.contains(&false)
    }

    /// The entry `bytes` hold, or `None` when they are torn, damaged, of
    /// another version or kind, or not an entry at all.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = EntryReader::new(bytes);
        let fixed = reader.take(Self::FIXED_BYTES)?;
        Self::require(Self::is_index_header(fixed))?;
        let lengths = Self::lengths(fixed)?;
        let collection = reader.take(lengths[0])?;
        let header_checksum = reader.take_u32()?;
        let header = &bytes[..Self::FIXED_BYTES + lengths[0]];
        Self::require(Crc32c::checksum(header) == header_checksum)?;
        let body = reader.take(lengths[1].checked_add(lengths[2])?)?;
        let body_checksum = reader.take_u32()?;
        Self::require(Crc32c::checksum(body) == body_checksum)?;
        Self::require(reader.is_empty())?;
        let (snapshot, log) = body.split_at(lengths[1]);
        Some(Self {
            collection: String::from_utf8(collection.to_vec()).ok()?,
            collection_stamp: Self::digest(fixed),
            snapshot: snapshot.to_vec(),
            log: Self::log_of(fixed[7], log),
        })
    }

    fn header(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::FIXED_BYTES);
        bytes.extend_from_slice(&Self::MAGIC);
        bytes.extend_from_slice(&Self::VERSION.to_le_bytes());
        bytes.push(Self::INDEX_KIND);
        bytes.push(self.flags());
        bytes.extend_from_slice(&self.collection_stamp);
        bytes.extend_from_slice(&Self::length_u16(self.collection.len()).to_le_bytes());
        bytes.extend_from_slice(&Self::length_u32(self.snapshot.len()).to_le_bytes());
        bytes.extend_from_slice(&Self::length_u32(self.log_bytes().len()).to_le_bytes());
        bytes.extend_from_slice(self.collection.as_bytes());
        bytes
    }

    fn flags(&self) -> u8 {
        if self.log.is_some() {
            return Self::HAS_LOG;
        }
        0
    }

    fn log_bytes(&self) -> &[u8] {
        self.log.as_deref().unwrap_or_default()
    }

    fn log_of(flags: u8, log: &[u8]) -> Option<Vec<u8>> {
        (flags & Self::HAS_LOG == Self::HAS_LOG).then(|| log.to_vec())
    }

    fn is_index_header(fixed: &[u8]) -> bool {
        let version = u16::from_le_bytes([fixed[4], fixed[5]]);
        fixed[0..4] == Self::MAGIC && version == Self::VERSION && fixed[6] == Self::INDEX_KIND
    }

    fn lengths(fixed: &[u8]) -> Option<[usize; 3]> {
        let name = usize::from(u16::from_le_bytes([fixed[40], fixed[41]]));
        let snapshot = Self::long_at(fixed, 42)?;
        let log = Self::long_at(fixed, 46)?;
        Some([name, snapshot, log])
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

    fn digest(fixed: &[u8]) -> [u8; 32] {
        let mut digest = [0_u8; 32];
        let end = Self::DIGEST_START + Self::DIGEST_BYTES;
        digest.copy_from_slice(&fixed[Self::DIGEST_START..end]);
        digest
    }

    fn length_u16(length: usize) -> u16 {
        u16::try_from(length).unwrap_or(u16::MAX)
    }

    fn length_u32(length: usize) -> u32 {
        u32::try_from(length).unwrap_or(u32::MAX)
    }

    fn require(condition: bool) -> Option<()> {
        condition.then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::IndexEntry;

    fn entry(log: Option<Vec<u8>>) -> IndexEntry {
        IndexEntry {
            collection: ".collections/notes".to_owned(),
            collection_stamp: [7_u8; 32],
            snapshot: b"snapshot bytes".to_vec(),
            log,
        }
    }

    #[test]
    fn an_entry_with_a_log_reads_back() {
        let entry = entry(Some(b"log".to_vec()));
        assert_eq!(IndexEntry::decode(&entry.encode()), Some(entry));
    }

    #[test]
    fn an_entry_without_a_log_reads_back() {
        let entry = entry(None);
        assert_eq!(IndexEntry::decode(&entry.encode()), Some(entry));
    }

    #[test]
    fn an_empty_log_is_not_a_missing_log() {
        let entry = entry(Some(Vec::new()));
        assert_eq!(IndexEntry::decode(&entry.encode()), Some(entry));
    }

    #[test]
    fn every_truncation_and_flipped_byte_is_refused() {
        let bytes = entry(Some(b"log".to_vec())).encode();
        for length in 0..bytes.len() {
            assert_eq!(IndexEntry::decode(&bytes[..length]), None, "{length}");
        }
        for position in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[position] ^= 0x40;
            assert_eq!(IndexEntry::decode(&damaged), None, "{position}");
        }
    }
}
