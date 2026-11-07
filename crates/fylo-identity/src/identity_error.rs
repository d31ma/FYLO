use std::fmt;

/// Why an identity could not be created or opened. [`IdentityError::code`]
/// is the stable code from `api/errors/v1.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The text is not a key reference.
    InvalidReference(String),
    /// The text is not an identity identifier.
    InvalidIdentifier(String),
    /// The key reference names no key.
    NotFound(String),
    /// A key is already stored there; FYLO never replaces a key.
    AlreadyExists(String),
    /// A key file another user owns, or that other users can read or write.
    ExposedKeyFile(String),
    /// The stored key is not a P-256 private key in PKCS#8 form.
    InvalidKey(String),
    /// The key store does not exist on this platform or in this build.
    Unsupported(String),
    /// The key store failed.
    Store(String),
}

impl IdentityError {
    /// The stable error code machine clients see.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.stable_code()
    }

    fn stable_code(&self) -> &'static str {
        match self {
            Self::InvalidReference(_) => "EIDENTITY_REFERENCE",
            Self::InvalidIdentifier(_) => "EIDENTITY_ID",
            Self::NotFound(_) => "EIDENTITY_NOT_FOUND",
            Self::AlreadyExists(_) => "EIDENTITY_EXISTS",
            Self::ExposedKeyFile(_) => "EIDENTITY_EXPOSED",
            Self::InvalidKey(_) => "EIDENTITY_KEY",
            Self::Unsupported(_) => "EIDENTITY_UNSUPPORTED",
            Self::Store(_) => "EIDENTITY_STORE",
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::InvalidReference(message)
            | Self::InvalidIdentifier(message)
            | Self::NotFound(message)
            | Self::AlreadyExists(message)
            | Self::ExposedKeyFile(message)
            | Self::InvalidKey(message)
            | Self::Unsupported(message)
            | Self::Store(message) => message,
        }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for IdentityError {}
