use crate::container_bytes::{all_zero, read_u32, read_u64, write_u32, write_u64};
use crate::{ContainerFormatError, Crc32c};

/// The header of one chunk frame of a large object (ADR 0034 section 2).
///
/// A frame is this header followed by the chunk's stored bytes: the plain
/// chunk, or its ciphertext and tag. The checksum covers the rest of the
/// header and the stored bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerObjectFrame {
    /// The hash that names the object write the frame belongs to.
    pub object: u64,
    /// The chunk's number in its object, from zero.
    pub index: u64,
    /// Bytes stored after the header.
    pub length: u32,
    /// Whether this is the object's last chunk.
    pub last: bool,
}

impl ContainerObjectFrame {
    /// Bytes of the header.
    pub const HEADER_BYTES: usize = 32;
    const MAGIC: [u8; 4] = *b"VDXO";
    const LAST_FLAG: u8 = 1;
    const CHECKED_FROM: usize = 4;

    /// Write the header into the first bytes of `frame`, whose stored bytes
    /// already follow it, and seal it with its checksum.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when `frame` is shorter than the header and the
    /// stored bytes it names.
    pub fn write_header(&self, frame: &mut [u8]) -> Result<(), ContainerFormatError> {
        let end = Self::frame_end(self.length)?;
        let frame = frame
            .get_mut(..end)
            .ok_or(ContainerFormatError::Truncated)?;
        frame[..Self::HEADER_BYTES].fill(0);
        frame[4..8].copy_from_slice(&Self::MAGIC);
        write_u32(frame, 8, self.length);
        frame[12] = u8::from(self.last) * Self::LAST_FLAG;
        write_u64(frame, 16, self.object);
        write_u64(frame, 24, self.index);
        let checksum = Crc32c::checksum(&frame[Self::CHECKED_FROM..]);
        write_u32(frame, 0, checksum);
        Ok(())
    }

    /// Read and check the frame at the start of `frame`.
    ///
    /// # Errors
    ///
    /// Returns `Truncated`, `BadMagic`, `ChecksumMismatch`, or
    /// `InvalidField` for unknown flags or non-zero reserved bytes.
    pub fn read(frame: &[u8]) -> Result<Self, ContainerFormatError> {
        let header = frame
            .get(..Self::HEADER_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        Self::require_magic(header)?;
        let length = read_u32(header, 8);
        let checked = frame
            .get(Self::CHECKED_FROM..Self::frame_end(length)?)
            .ok_or(ContainerFormatError::Truncated)?;
        Self::require_checksum(read_u32(header, 0), checked)?;
        Self::require_reserved(header)?;
        Ok(Self {
            object: read_u64(header, 16),
            index: read_u64(header, 24),
            length,
            last: header[12] == Self::LAST_FLAG,
        })
    }

    /// Bytes of the whole frame: the header and the stored bytes.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the length cannot be addressed.
    pub fn frame_end(length: u32) -> Result<usize, ContainerFormatError> {
        usize::try_from(length)
            .ok()
            .and_then(|length| length.checked_add(Self::HEADER_BYTES))
            .ok_or(ContainerFormatError::Truncated)
    }

    fn require_magic(header: &[u8]) -> Result<(), ContainerFormatError> {
        if header[4..8] == Self::MAGIC {
            return Ok(());
        }
        Err(ContainerFormatError::BadMagic)
    }

    fn require_checksum(expected: u32, checked: &[u8]) -> Result<(), ContainerFormatError> {
        if Crc32c::checksum(checked) == expected {
            return Ok(());
        }
        Err(ContainerFormatError::ChecksumMismatch)
    }

    fn require_reserved(header: &[u8]) -> Result<(), ContainerFormatError> {
        if header[12] <= Self::LAST_FLAG && all_zero(&header[13..16]) {
            return Ok(());
        }
        Err(ContainerFormatError::InvalidField)
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerObjectFrame;
    use crate::ContainerFormatError;

    const FRAME: ContainerObjectFrame = ContainerObjectFrame {
        object: 0x0102_0304_0506_0708,
        index: 9,
        length: 5,
        last: true,
    };

    fn written() -> Vec<u8> {
        let mut frame = vec![0_u8; ContainerObjectFrame::HEADER_BYTES + 5];
        frame[ContainerObjectFrame::HEADER_BYTES..].copy_from_slice(b"hello");
        FRAME.write_header(&mut frame).unwrap();
        frame
    }

    #[test]
    fn round_trips() {
        assert_eq!(ContainerObjectFrame::read(&written()), Ok(FRAME));
    }

    #[test]
    fn detects_a_changed_payload_byte() {
        let mut frame = written();
        frame[ContainerObjectFrame::HEADER_BYTES] ^= 1;
        assert_eq!(
            ContainerObjectFrame::read(&frame),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn detects_a_changed_index() {
        let mut frame = written();
        frame[24] ^= 1;
        assert_eq!(
            ContainerObjectFrame::read(&frame),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn refuses_bytes_that_are_not_a_frame() {
        assert_eq!(
            ContainerObjectFrame::read(&[0_u8; 64]),
            Err(ContainerFormatError::BadMagic)
        );
    }

    #[test]
    fn refuses_a_cut_frame() {
        let frame = written();
        assert_eq!(
            ContainerObjectFrame::read(&frame[..frame.len() - 1]),
            Err(ContainerFormatError::Truncated)
        );
    }

    #[test]
    fn refuses_a_short_buffer_to_write_into() {
        let mut frame = vec![0_u8; ContainerObjectFrame::HEADER_BYTES + 4];
        assert_eq!(
            FRAME.write_header(&mut frame),
            Err(ContainerFormatError::Truncated)
        );
    }
}
