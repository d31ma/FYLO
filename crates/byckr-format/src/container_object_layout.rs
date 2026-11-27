use crate::{ContainerExtentHeader, ContainerObjectFrame, ContainerObjectPlace};

/// Where the chunks of a large object sit in its extents (ADR 0034
/// section 2): a fixed number of equal frame slots per extent, so chunk `i`
/// is found by arithmetic, with no chunk table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerObjectLayout {
    /// Frame slots in one extent.
    pub slots_per_extent: u64,
    /// Bytes of one frame slot.
    pub slot_bytes: u64,
    /// Plain bytes of a full chunk.
    pub chunk_bytes: u64,
}

impl ContainerObjectLayout {
    /// The chunk size a frame slot is sized for.
    pub const CHUNK_TARGET_BYTES: u64 = 1 << 20;

    /// The layout in a file with `extent_bytes` extents, where each stored
    /// chunk carries `tag_bytes` more than its plain bytes (16 when sealed).
    #[must_use]
    pub fn for_extent(extent_bytes: u64, tag_bytes: u64) -> Self {
        let slots_per_extent = (extent_bytes / Self::CHUNK_TARGET_BYTES).max(1);
        let payload = extent_bytes - ContainerExtentHeader::PAYLOAD_OFFSET;
        let slot_bytes = payload / slots_per_extent;
        Self {
            slots_per_extent,
            slot_bytes,
            chunk_bytes: slot_bytes - ContainerObjectFrame::HEADER_BYTES as u64 - tag_bytes,
        }
    }

    /// Chunks of an object of `length` bytes.
    #[must_use]
    pub fn chunk_count(&self, length: u64) -> u64 {
        length.div_ceil(self.chunk_bytes)
    }

    /// Extents an object of `length` bytes takes.
    #[must_use]
    pub fn extent_count(&self, length: u64) -> u64 {
        self.chunk_count(length).div_ceil(self.slots_per_extent)
    }

    /// Where chunk `index` sits.
    #[must_use]
    pub fn place(&self, index: u64) -> ContainerObjectPlace {
        ContainerObjectPlace {
            ordinal: index / self.slots_per_extent,
            offset: (index % self.slots_per_extent) * self.slot_bytes,
        }
    }

    /// Plain bytes of chunk `index` of an object of `length` bytes: a full
    /// chunk, or what is left for the last one.
    #[must_use]
    pub fn chunk_length(&self, length: u64, index: u64) -> u64 {
        let start = index.saturating_mul(self.chunk_bytes).min(length);
        (length - start).min(self.chunk_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerObjectLayout;
    use crate::ContainerObjectPlace;

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_one_mebibyte_extent_holds_one_chunk() {
        let layout = ContainerObjectLayout::for_extent(MIB, 0);
        assert_eq!(layout.slots_per_extent, 1);
        assert_eq!(layout.slot_bytes, MIB - 4096);
        assert_eq!(layout.chunk_bytes, MIB - 4096 - 32);
    }

    #[test]
    fn the_default_extent_holds_two_chunks_of_just_under_a_mebibyte() {
        let layout = ContainerObjectLayout::for_extent(2 * MIB, 16);
        assert_eq!(layout.slots_per_extent, 2);
        assert_eq!(layout.slot_bytes, MIB - 2048);
        assert_eq!(layout.chunk_bytes, MIB - 2048 - 32 - 16);
    }

    #[test]
    fn a_large_extent_holds_one_chunk_per_mebibyte() {
        let layout = ContainerObjectLayout::for_extent(64 * MIB, 0);
        assert_eq!(layout.slots_per_extent, 64);
        assert!(layout.chunk_bytes < MIB);
    }

    #[test]
    fn counts_chunks_and_extents() {
        let layout = ContainerObjectLayout::for_extent(2 * MIB, 0);
        let chunk = layout.chunk_bytes;
        assert_eq!(layout.chunk_count(0), 0);
        assert_eq!(layout.extent_count(0), 0);
        assert_eq!(layout.chunk_count(1), 1);
        assert_eq!(layout.chunk_count(chunk), 1);
        assert_eq!(layout.chunk_count(chunk + 1), 2);
        assert_eq!(layout.extent_count(2 * chunk), 1);
        assert_eq!(layout.extent_count(2 * chunk + 1), 2);
    }

    #[test]
    fn places_chunks_by_arithmetic() {
        let layout = ContainerObjectLayout::for_extent(2 * MIB, 0);
        assert_eq!(
            layout.place(0),
            ContainerObjectPlace {
                ordinal: 0,
                offset: 0
            }
        );
        assert_eq!(
            layout.place(1),
            ContainerObjectPlace {
                ordinal: 0,
                offset: layout.slot_bytes
            }
        );
        assert_eq!(
            layout.place(5),
            ContainerObjectPlace {
                ordinal: 2,
                offset: layout.slot_bytes
            }
        );
    }

    #[test]
    fn the_last_chunk_is_shorter() {
        let layout = ContainerObjectLayout::for_extent(MIB, 0);
        let length = layout.chunk_bytes + 10;
        assert_eq!(layout.chunk_length(length, 0), layout.chunk_bytes);
        assert_eq!(layout.chunk_length(length, 1), 10);
        assert_eq!(layout.chunk_length(length, 2), 0);
    }
}
