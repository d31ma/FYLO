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
    /// Adopts a compacted document segment in place of the segments it
    /// replaces.
    Compaction,
    /// Commits one version of a document segment collection's schema
    /// registry (ADR 0017 section 3). Its manifest is
    /// `fylo.schema-version.v1` and its blob area holds the version.
    Schema,
}

impl LogRecordKind {
    /// Wire value.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Commit => 1,
            Self::Abort => 2,
            Self::Checkpoint => 3,
            Self::Compaction => 4,
            Self::Schema => 5,
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
            4 => Ok(Self::Compaction),
            5 => Ok(Self::Schema),
            _ => Err(LogFormatError::InvalidField),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LogRecordKind;
    use crate::LogFormatError;

    #[test]
    fn every_kind_round_trips_and_an_unknown_one_is_refused() {
        for kind in [
            LogRecordKind::Commit,
            LogRecordKind::Abort,
            LogRecordKind::Checkpoint,
            LogRecordKind::Compaction,
            LogRecordKind::Schema,
        ] {
            assert_eq!(LogRecordKind::from_code(kind.code()), Ok(kind));
        }
        assert_eq!(LogRecordKind::Schema.code(), 5);
        for code in [0, 6, 7, u8::MAX] {
            assert_eq!(
                LogRecordKind::from_code(code),
                Err(LogFormatError::InvalidField)
            );
        }
    }
}
