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
    /// A whole frame's sealed body does not open with the file's key: it
    /// was altered, or belongs to another collection or record. Its
    /// checksum matched, so it is no torn write.
    Unauthenticated,
    /// A whole frame belongs to a collection of a protected database whose
    /// key the opening identity was not granted, or the identity may not
    /// write it (ADR 0021). Never damage, and never a torn tail.
    Denied,
    /// A whole frame of a protected database carries no signature, or one
    /// that does not verify, or one by an identity that could not write
    /// the collection at that point of its log (ADR 0021). Never damage
    /// to cut, and never a torn tail.
    BadSignature,
}

impl LogFormatError {
    /// Whether the frame is whole but this reader cannot read its body: it
    /// uses a newer format, names a dictionary that cannot be loaded, or
    /// does not open with the file's key. A scan that stops there has not
    /// found a torn tail, and nothing after it may be cut off.
    #[must_use]
    pub const fn is_unreadable(self) -> bool {
        matches!(
            self,
            Self::UnsupportedVersion
                | Self::MissingDictionary
                | Self::Unauthenticated
                | Self::Denied
                | Self::BadSignature
        )
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
            Self::Unauthenticated => {
                "log record is sealed for another place, or was altered after sealing"
            }
            Self::Denied => "log record belongs to a collection this identity has no grant for",
            Self::BadSignature => {
                "log record is not signed by an identity that could write its collection"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LogFormatError {}
