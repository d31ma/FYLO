/// CRC-32C (Castagnoli), the checksum that frames every collection log record.
///
/// It detects the torn or partial tail an interrupted append leaves behind. It
/// is not a cryptographic digest: blobs inside a record also carry SHA-256.
pub struct Crc32c;

impl Crc32c {
    const REFLECTED_POLYNOMIAL: u32 = 0x82F6_3B78;
    const TABLE: [u32; 256] = Self::table();

    /// Checksum of `bytes`.
    #[must_use]
    pub fn checksum(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            let index = (crc ^ u32::from(*byte)) & 0xFF;
            crc = (crc >> 8) ^ Self::TABLE[index as usize];
        }
        !crc
    }

    const fn table() -> [u32; 256] {
        let mut table = [0_u32; 256];
        let mut index: u32 = 0;
        while index < 256 {
            table[index as usize] = Self::table_entry(index);
            index += 1;
        }
        table
    }

    const fn table_entry(index: u32) -> u32 {
        let mut value = index;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ Self::REFLECTED_POLYNOMIAL
            } else {
                value >> 1
            };
            bit += 1;
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::Crc32c;

    #[test]
    fn matches_the_published_check_value() {
        assert_eq!(Crc32c::checksum(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn checksums_empty_input_as_zero() {
        assert_eq!(Crc32c::checksum(b""), 0);
    }
}
