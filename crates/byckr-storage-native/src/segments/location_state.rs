use serde::{Deserialize, Serialize};

use crate::{NativeStorageError, NativeStorageErrorCode};

/// What a segment entry says about its document.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LocationState {
    /// The document does not exist (a tombstone).
    Absent,
    /// The document is live.
    Live,
    /// The document is soft-deleted and can be restored.
    Deleted,
}

impl LocationState {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Live => 1,
            Self::Deleted => 2,
        }
    }

    pub(crate) fn from_code(code: u8) -> Result<Self, NativeStorageError> {
        match code {
            0 => Ok(Self::Absent),
            1 => Ok(Self::Live),
            2 => Ok(Self::Deleted),
            _ => Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment location has an undefined state",
            )),
        }
    }
}
