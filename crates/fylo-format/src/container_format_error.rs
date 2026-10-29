/// Why a structure of a `.fxdb` container could not be decoded (ADR 0016).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerFormatError {
    /// Fewer bytes than the structure needs.
    Truncated,
    /// The bytes do not start with the structure's magic.
    BadMagic,
    /// The structure uses a format version this build does not know.
    UnsupportedVersion,
    /// A checksum does not match its bytes.
    ChecksumMismatch,
    /// A field holds a value the format does not define, or reserved bytes
    /// are not zero.
    InvalidField,
    /// The file sets an incompatible feature this build does not know, so it
    /// must not be read at all.
    IncompatibleFeature,
    /// A length or count is larger than the format allows.
    Oversized,
}

impl std::fmt::Display for ContainerFormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Truncated => "container bytes end inside a structure",
            Self::BadMagic => "container structure has the wrong magic",
            Self::UnsupportedVersion => "container format version is not supported",
            Self::ChecksumMismatch => "container checksum does not match",
            Self::InvalidField => "container field holds an undefined value",
            Self::IncompatibleFeature => "container uses an incompatible feature this build lacks",
            Self::Oversized => "container length exceeds the format limit",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ContainerFormatError {}
