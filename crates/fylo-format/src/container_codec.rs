use crate::ContainerFormatError;

/// How a record body in a `.fxdb` frame is compressed: the frame header's
/// codec byte (ADR 0016 sections 4 and 8).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContainerCodec {
    /// Stored as is: compression would not have saved a byte.
    None,
    /// A zstd frame with its content size. Native writers produce it.
    Zstd,
    /// An LZ4 block after a 4-byte little-endian content size. The browser
    /// produces it; native compaction rewrites it as zstd.
    Lz4,
}

impl ContainerCodec {
    /// The codec's one-byte code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Zstd => 1,
            Self::Lz4 => 2,
        }
    }

    /// Decode a codec code.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for a code the format does not define.
    pub const fn from_code(code: u8) -> Result<Self, ContainerFormatError> {
        match code {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstd),
            2 => Ok(Self::Lz4),
            _ => Err(ContainerFormatError::InvalidField),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerCodec;

    #[test]
    fn codes_round_trip() {
        for codec in [
            ContainerCodec::None,
            ContainerCodec::Zstd,
            ContainerCodec::Lz4,
        ] {
            assert_eq!(ContainerCodec::from_code(codec.code()), Ok(codec));
        }
        assert!(ContainerCodec::from_code(3).is_err());
    }
}
