/// Why a collection log segment or record could not be decoded.
///
/// A scan stops at the first error. Everything before it is valid; the error
/// marks where an interrupted append ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFormatError {
    /// Fewer bytes remain than the frame or header needs.
    Truncated,
    /// The segment header does not start with the log magic.
    BadMagic,
    /// The segment or record uses a format version this build does not know.
    UnsupportedVersion,
    /// A checksum does not match its bytes.
    ChecksumMismatch,
    /// A length field is larger than the format allows.
    Oversized,
    /// A field holds a value the format does not define.
    InvalidField,
    /// A record's sequence number does not follow the previous record.
    SequenceGap,
    /// A whole frame names a compression dictionary that is missing or
    /// damaged, so its body cannot be read.
    MissingDictionary,
}

impl LogFormatError {
    /// Whether the frame is whole but this reader cannot read its body: it
    /// uses a newer format, or names a dictionary that cannot be loaded. A
    /// scan that stops there has not found a torn tail, and nothing after
    /// it may be cut off.
    #[must_use]
    pub const fn is_unreadable(self) -> bool {
        matches!(self, Self::UnsupportedVersion | Self::MissingDictionary)
    }
}

impl std::fmt::Display for LogFormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Truncated => "log bytes end inside a frame",
            Self::BadMagic => "log segment has the wrong magic",
            Self::UnsupportedVersion => "log segment version is not supported",
            Self::ChecksumMismatch => "log checksum does not match",
            Self::Oversized => "log length exceeds the format limit",
            Self::InvalidField => "log field holds an undefined value",
            Self::SequenceGap => "log sequence number does not follow the previous record",
            Self::MissingDictionary => {
                "log record names a compression dictionary that is missing or damaged"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LogFormatError {}
