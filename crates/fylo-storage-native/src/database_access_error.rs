use std::fmt;

use fylo_identity::IdentityError;

/// Why a protected `.vdbx` database refused an identity or a change
/// (ADR 0021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseAccessError {
    /// The database is protected and no identity was given
    /// (`ENOIDENTITY`).
    NoIdentity,
    /// The policy is missing, unsigned, not signed by the pinned anchor, or
    /// no anchor was pinned (`EPOLICY`).
    Policy(String),
    /// The identity has no grant for what it asked (`EACCES`).
    Denied(String),
    /// The identity's key store failed.
    Identity(IdentityError),
    /// The access document is damaged (`EVDBX_CORRUPT`).
    Damaged(String),
}

impl DatabaseAccessError {
    /// The stable error code machine clients see.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.stable_code()
    }

    fn stable_code(&self) -> &'static str {
        match self {
            Self::NoIdentity => "ENOIDENTITY",
            Self::Policy(_) => "EPOLICY",
            Self::Denied(_) => "EACCES",
            Self::Identity(error) => error.code(),
            Self::Damaged(_) => "EVDBX_CORRUPT",
        }
    }
}

impl fmt::Display for DatabaseAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoIdentity => formatter.write_str(
                "the .vdbx database is protected: open it with an identity (--identity, fylo.identity or FYLO_IDENTITY)",
            ),
            Self::Policy(message) | Self::Denied(message) | Self::Damaged(message) => {
                formatter.write_str(message)
            }
            Self::Identity(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DatabaseAccessError {}

impl From<IdentityError> for DatabaseAccessError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}
