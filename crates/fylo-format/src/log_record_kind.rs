use crate::LogFormatError;

/// What a collection log record does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogRecordKind {
    /// A committed mutation to replay.
    Commit,
    /// Cancels one earlier commit that a live writer rolled back.
    Abort,
    /// Marks the state every earlier record has been made durable in.
    Checkpoint,
}

impl LogRecordKind {
    /// Wire value.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Commit => 1,
            Self::Abort => 2,
            Self::Checkpoint => 3,
        }
    }

    /// Parse a wire value.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for an undefined value.
    pub const fn from_code(code: u8) -> Result<Self, LogFormatError> {
        match code {
            1 => Ok(Self::Commit),
            2 => Ok(Self::Abort),
            3 => Ok(Self::Checkpoint),
            _ => Err(LogFormatError::InvalidField),
        }
    }
}
