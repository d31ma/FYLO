/// RFC 4648 base32, upper case and without padding, for identity
/// fingerprints.
pub(crate) struct Base32;

impl Base32 {
    const ALPHABET: &'static [u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    const BITS_PER_CHARACTER: u32 = 5;
    const CHARACTER_MASK: u64 = 0b1_1111;

    pub(crate) fn encode(bytes: &[u8]) -> String {
        let mut text = String::new();
        let (buffer, buffered_bits) = Self::encode_whole_characters(&mut text, bytes);
        Self::finish(&mut text, buffer, buffered_bits);
        text
    }

    pub(crate) fn is_encoded(text: &str) -> bool {
        text.bytes().all(|byte| Self::ALPHABET.contains(&byte))
    }

    fn encode_whole_characters(text: &mut String, bytes: &[u8]) -> (u64, u32) {
        let mut buffer: u64 = 0;
        let mut buffered_bits: u32 = 0;
        for byte in bytes {
            buffer = (buffer << 8) | u64::from(*byte);
            buffered_bits = Self::drain(text, buffer, buffered_bits + 8);
        }
        (buffer, buffered_bits)
    }

    fn drain(text: &mut String, buffer: u64, mut buffered_bits: u32) -> u32 {
        while buffered_bits >= Self::BITS_PER_CHARACTER {
            buffered_bits -= Self::BITS_PER_CHARACTER;
            text.push(Self::character(buffer >> buffered_bits));
        }
        buffered_bits
    }

    fn finish(text: &mut String, buffer: u64, buffered_bits: u32) {
        if buffered_bits > 0 {
            text.push(Self::character(
                buffer << (Self::BITS_PER_CHARACTER - buffered_bits),
            ));
        }
    }

    fn character(value: u64) -> char {
        let index = usize::try_from(value & Self::CHARACTER_MASK).unwrap_or_default();
        char::from(Self::ALPHABET[index])
    }
}

#[cfg(test)]
mod tests {
    use super::Base32;

    #[test]
    fn encodes_the_rfc_4648_test_vectors_without_padding() {
        assert_eq!(Base32::encode(b""), "");
        assert_eq!(Base32::encode(b"f"), "MY");
        assert_eq!(Base32::encode(b"fo"), "MZXQ");
        assert_eq!(Base32::encode(b"foo"), "MZXW6");
        assert_eq!(Base32::encode(b"foob"), "MZXW6YQ");
        assert_eq!(Base32::encode(b"fooba"), "MZXW6YTB");
        assert_eq!(Base32::encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn accepts_only_the_upper_case_alphabet() {
        assert!(Base32::is_encoded("MZXW6YTBOI234567"));
        assert!(!Base32::is_encoded("mzxw6"));
        assert!(!Base32::is_encoded("MZXW1"));
        assert!(!Base32::is_encoded("MZXW="));
    }
}
