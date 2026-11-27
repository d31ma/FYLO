use std::fmt;
use std::io;

use byckr_format::ContainerFormatError;

use crate::ContainerError;

/// Why a large-object operation in a `.vdbx` file failed (ADR 0034).
#[derive(Debug)]
pub enum ContainerObjectError {
    /// The object is over the 512 MiB limit; nothing was written.
    TooLarge {
        /// The refused length.
        length: u64,
    },
    /// The object would need more extent runs than a record may name.
    TooManyRuns {
        /// The runs it would need.
        runs: usize,
    },
    /// The object's extents now hold something else: it was deleted,
    /// replaced or moved since its runs were read.
    NotFound,
    /// A chunk or the whole object failed its check.
    Damaged(String),
    /// The byte range is outside the object.
    RangeOutside {
        /// The range's first byte.
        start: u64,
        /// The byte after the range.
        end: u64,
        /// The object's length.
        length: u64,
    },
    /// The bytes written do not match the declared length.
    LengthMismatch {
        /// The declared length.
        declared: u64,
        /// The bytes written, so far or in all.
        written: u64,
    },
    /// A chunk number was about to be sealed a second time under the same
    /// object key. BYCKR refuses it.
    NonceReuse {
        /// The chunk number.
        index: u64,
    },
    /// The object is encrypted and no file key was given.
    Locked,
    /// The container refused an operation.
    Container(ContainerError),
}

impl ContainerObjectError {
    /// The stable error code machine clients see.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "ENATIVE_FILE_SIZE",
            Self::TooManyRuns { .. } => "ELAYOUTUNSUPPORTED",
            Self::NotFound => "ENATIVE_NOT_FOUND",
            Self::Damaged(_) | Self::NonceReuse { .. } => "EVDBX_CORRUPT",
            Self::RangeOutside { .. } | Self::LengthMismatch { .. } => "ENATIVE_UNSUPPORTED",
            Self::Locked => "EKEY",
            Self::Container(error) => error.code(),
        }
    }

    /// The object error an I/O error from a reader carries, or the I/O
    /// error itself as a container error.
    #[must_use]
    pub fn from_read(error: io::Error) -> Self {
        match error.downcast::<Self>() {
            Ok(inner) => inner,
            Err(other) => Self::Container(ContainerError::Io(other)),
        }
    }

    /// A damaged object, for a failed check.
    pub(crate) fn damaged(what: &str) -> Self {
        Self::Damaged(what.to_owned())
    }
}

impl fmt::Display for ContainerObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { length } => write!(
                formatter,
                "the file is {length} bytes; a file in a .vdbx database may be at most 512 MiB"
            ),
            Self::TooManyRuns { runs } => write!(
                formatter,
                "the file would need {runs} extent runs, more than 4096; run `fylo compact --shrink` to make free space contiguous"
            ),
            Self::NotFound => formatter.write_str("the file is no longer in the database"),
            Self::Damaged(what) => write!(formatter, "the stored file is damaged: {what}"),
            Self::RangeOutside { start, end, length } => write!(
                formatter,
                "the byte range {start}..{end} is outside the file of {length} bytes"
            ),
            Self::LengthMismatch { declared, written } => write!(
                formatter,
                "the file was declared as {declared} bytes, but {written} bytes were written"
            ),
            Self::NonceReuse { index } => write!(
                formatter,
                "chunk {index} would be encrypted twice under the same key; refused"
            ),
            Self::Locked => formatter.write_str("the file is encrypted and needs its key"),
            Self::Container(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ContainerObjectError {}

impl From<ContainerError> for ContainerObjectError {
    fn from(error: ContainerError) -> Self {
        Self::Container(error)
    }
}

impl From<io::Error> for ContainerObjectError {
    fn from(error: io::Error) -> Self {
        Self::Container(ContainerError::Io(error))
    }
}

impl From<ContainerFormatError> for ContainerObjectError {
    fn from(error: ContainerFormatError) -> Self {
        Self::Damaged(error.to_string())
    }
}
