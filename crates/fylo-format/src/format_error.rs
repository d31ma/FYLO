use std::fmt;

use crate::FormatErrorCode;

/// A bounded, machine-testable format failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatError {
    code: FormatErrorCode,
    message: String,
}

impl FormatError {
    pub(crate) fn new(code: FormatErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Return the stable error code.
    #[must_use]
    pub const fn code(&self) -> FormatErrorCode {
        self.code
    }

    /// Return the safe diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for FormatError {}
