use std::fmt;

use fylo_engine::EngineError;
use fylo_storage_native::{NativeStorageError, NativeStorageErrorCode};

/// A failed call, with the stable code the machine protocol reports for the
/// same failure (`api/errors/v1.json`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    code: String,
    message: String,
}

impl Error {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// The stable error code, such as `ENATIVE_NOT_FOUND`.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The human-readable explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether the named document does not exist.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.code == NativeStorageErrorCode::NotFound.as_str()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl From<NativeStorageError> for Error {
    fn from(error: NativeStorageError) -> Self {
        Self::new(error.code().as_str(), error.to_string())
    }
}

impl From<EngineError> for Error {
    fn from(error: EngineError) -> Self {
        let code = error
            .storage_code()
            .map(NativeStorageErrorCode::as_str)
            .or_else(|| error.format_code_str())
            .or_else(|| error.query_code_str())
            .unwrap_or_else(|| error.code().as_str());
        Self::new(code, error.to_string())
    }
}
