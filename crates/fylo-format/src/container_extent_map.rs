use crate::container_bytes::{
    all_zero, read_u16, read_u32, read_u64, write_u16, write_u32, write_u64,
};
use crate::{ContainerExtentKind, ContainerFormatError, Crc32c};

/// The state of one extent in the extent map (ADR 0016 section 3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerExtentState {
    /// Holds nothing; may be reused after the checkpoint that freed it.
    Free,
    /// Allocated and still being written (an active segment).
    Owned {
        /// What it holds.
        kind: ContainerExtentKind,
        /// Owning collection; zero for database-level extents.
        owner: u32,
        /// Allocation sequence number, as in the extent header.
        sequence: u64,
    },
    /// Allocated and immutable.
    Sealed {
        /// What it holds.
        kind: ContainerExtentKind,
        /// Owning collection; zero for database-level extents.
        owner: u32,
        /// Allocation sequence number, as in the extent header.
        sequence: u64,
        /// Bytes still referenced by the location table; compaction picks
        /// extents with few.
        live_bytes: u64,
    },
}

/// One 4 KiB page of the extent map: the states of up to
/// [`Self::ENTRIES_PER_PAGE`] consecutive extents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerExtentMapPage {
    /// Index of the extent the first entry describes.
    pub first_extent: u64,
    /// One state per extent, in index order.
    pub entries: Vec<ContainerExtentState>,
}

impl ContainerExtentMapPage {
    /// Encoded size of one page.
    pub const ENCODED_BYTES: usize = 4096;
    /// Entries one page holds.
    pub const ENTRIES_PER_PAGE: usize = 126;
    /// Current version.
    pub const VERSION: u16 = 1;
    const MAGIC: [u8; 8] = *b"VDXMAP\0\0";
    const HEADER_BYTES: usize = 32;
    const ENTRY_BYTES: usize = 32;
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Encode the page with its checksum.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the page has more than
    /// [`Self::ENTRIES_PER_PAGE`] entries.
    pub fn encode(&self) -> Result<Vec<u8>, ContainerFormatError> {
        let count = u16::try_from(self.entries.len())
            .ok()
            .filter(|count| usize::from(*count) <= Self::ENTRIES_PER_PAGE)
            .ok_or(ContainerFormatError::Oversized)?;
        let mut bytes = vec![0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&Self::MAGIC);
        write_u16(&mut bytes, 8, Self::VERSION);
        write_u16(&mut bytes, 10, count);
        write_u64(&mut bytes, 16, self.first_extent);
        for (position, entry) in self.entries.iter().enumerate() {
            let at = Self::HEADER_BYTES + position * Self::ENTRY_BYTES;
            let slot = &mut bytes[at..at + Self::ENTRY_BYTES];
            match *entry {
                ContainerExtentState::Free => {}
                ContainerExtentState::Owned {
                    kind,
                    owner,
                    sequence,
                } => {
                    slot[0] = 1;
                    slot[1] = kind.code();
                    write_u32(slot, 4, owner);
                    write_u64(slot, 8, sequence);
                }
                ContainerExtentState::Sealed {
                    kind,
                    owner,
                    sequence,
                    live_bytes,
                } => {
                    slot[0] = 2;
                    slot[1] = kind.code();
                    write_u32(slot, 4, owner);
                    write_u64(slot, 8, sequence);
                    write_u64(slot, 16, live_bytes);
                }
            }
        }
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        Ok(bytes)
    }

    /// Decode and verify one page.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown version, too many entries, an unknown state or
    /// kind, or non-zero reserved bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContainerFormatError> {
        let bytes = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        if bytes[0..8] != Self::MAGIC {
            return Err(ContainerFormatError::BadMagic);
        }
        if Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]) != read_u32(bytes, Self::CHECKED_BYTES) {
            return Err(ContainerFormatError::ChecksumMismatch);
        }
        if read_u16(bytes, 8) != Self::VERSION {
            return Err(ContainerFormatError::UnsupportedVersion);
        }
        let count = usize::from(read_u16(bytes, 10));
        if count > Self::ENTRIES_PER_PAGE {
            return Err(ContainerFormatError::Oversized);
        }
        let entries_end = Self::HEADER_BYTES + count * Self::ENTRY_BYTES;
        if !all_zero(&bytes[12..16])
            || !all_zero(&bytes[24..Self::HEADER_BYTES])
            || !all_zero(&bytes[entries_end..Self::CHECKED_BYTES])
        {
            return Err(ContainerFormatError::InvalidField);
        }
        let first_extent = read_u64(bytes, 16);
        if first_extent.checked_add(count as u64).is_none() {
            return Err(ContainerFormatError::Oversized);
        }
        let entries = bytes[Self::HEADER_BYTES..entries_end]
            .chunks_exact(Self::ENTRY_BYTES)
            .map(Self::decode_entry)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            first_extent,
            entries,
        })
    }

    fn decode_entry(slot: &[u8]) -> Result<ContainerExtentState, ContainerFormatError> {
        if !all_zero(&slot[2..4]) || !all_zero(&slot[24..32]) {
            return Err(ContainerFormatError::InvalidField);
        }
        match slot[0] {
            0 if all_zero(slot) => Ok(ContainerExtentState::Free),
            1 if all_zero(&slot[16..24]) => Ok(ContainerExtentState::Owned {
                kind: ContainerExtentKind::from_code(slot[1])?,
                owner: read_u32(slot, 4),
                sequence: read_u64(slot, 8),
            }),
            2 => Ok(ContainerExtentState::Sealed {
                kind: ContainerExtentKind::from_code(slot[1])?,
                owner: read_u32(slot, 4),
                sequence: read_u64(slot, 8),
                live_bytes: read_u64(slot, 16),
            }),
            _ => Err(ContainerFormatError::InvalidField),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerExtentMapPage, ContainerExtentState};
    use crate::{ContainerExtentKind, ContainerFormatError};

    fn sample() -> ContainerExtentMapPage {
        ContainerExtentMapPage {
            first_extent: 126,
            entries: vec![
                ContainerExtentState::Free,
                ContainerExtentState::Owned {
                    kind: ContainerExtentKind::Segment,
                    owner: 2,
                    sequence: 10,
                },
                ContainerExtentState::Sealed {
                    kind: ContainerExtentKind::Locations,
                    owner: 2,
                    sequence: 11,
                    live_bytes: 4_000_000,
                },
            ],
        }
    }

    #[test]
    fn round_trips() {
        let page = sample();
        assert_eq!(
            ContainerExtentMapPage::decode(&page.encode().unwrap()),
            Ok(page)
        );
    }

    #[test]
    fn round_trips_a_full_page() {
        let page = ContainerExtentMapPage {
            first_extent: 0,
            entries: vec![
                ContainerExtentState::Owned {
                    kind: ContainerExtentKind::Map,
                    owner: 0,
                    sequence: 1,
                };
                ContainerExtentMapPage::ENTRIES_PER_PAGE
            ],
        };
        assert_eq!(
            ContainerExtentMapPage::decode(&page.encode().unwrap()),
            Ok(page)
        );
    }

    #[test]
    fn refuses_an_overfull_page() {
        let page = ContainerExtentMapPage {
            first_extent: 0,
            entries: vec![ContainerExtentState::Free; ContainerExtentMapPage::ENTRIES_PER_PAGE + 1],
        };
        assert_eq!(page.encode(), Err(ContainerFormatError::Oversized));
    }

    #[test]
    fn rejects_damage() {
        let mut flipped = sample().encode().unwrap();
        flipped[70] ^= 4;
        assert_eq!(
            ContainerExtentMapPage::decode(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        assert_eq!(
            ContainerExtentMapPage::decode(&[0_u8; 4096]),
            Err(ContainerFormatError::BadMagic)
        );
    }
}
